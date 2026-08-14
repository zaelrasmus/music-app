use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite};
use tauri::State;

use crate::db::Db;
use crate::tracks::Track;

/// How multiple selected tags combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TagMode {
    /// Tracks carrying every selected tag.
    #[default]
    All,
    /// Tracks carrying any of them.
    Any,
}

/// Tracks grouped under one artist heading.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistGroup {
    pub artist: String,
    pub tracks: Vec<Track>,
}

const TRACK_COLUMNS: &str =
    "SELECT t.id, t.source, t.title, t.artist, t.album, t.duration_secs, t.state FROM tracks t";

/// Turns what the user typed into an FTS5 expression.
///
/// `MATCH` takes an expression, not a literal: a query containing `AND`, `"`,
/// `*` or `AC/DC` is either a syntax error or silently means something else.
/// Each word becomes a quoted prefix term, ANDed together, so typing narrows
/// as you go and nothing the user types can be an operator.
///
/// Returns `None` when there is nothing searchable, so callers can distinguish
/// "no query" from "query that matches nothing".
pub(crate) fn to_fts_expression(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        // `is_alphanumeric` is Unicode-aware, so "canción" stays one term.
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .take(12)
        .map(|term| format!("\"{term}\"*"))
        .collect();

    (!terms.is_empty()).then(|| terms.join(" AND "))
}

/// The library view's one query: optional text search, optional tag filter.
///
/// Both filters are applied in SQL rather than intersected in Rust, so the
/// database does the set logic and only matching rows cross the boundary.
#[tauri::command]
pub async fn query_library(
    db: State<'_, Db>,
    search: Option<String>,
    tag_ids: Vec<i64>,
    mode: Option<TagMode>,
) -> Result<Vec<Track>, String> {
    let mode = mode.unwrap_or_default();
    let expression = search.as_deref().and_then(to_fts_expression);

    // The user typed something, but it was all punctuation.
    if search.as_deref().is_some_and(|s| !s.trim().is_empty()) && expression.is_none() {
        return Ok(Vec::new());
    }

    // sqlx 0.9 refuses non-'static SQL strings outright, so a dynamic IN list
    // has to be built through QueryBuilder rather than `format!`.
    let mut query: QueryBuilder<Sqlite> = QueryBuilder::new(TRACK_COLUMNS);

    if expression.is_some() {
        query.push(" JOIN tracks_fts ON tracks_fts.rowid = t.id");
    }
    if !tag_ids.is_empty() {
        query.push(" JOIN track_tags tt ON tt.track_id = t.id");
    }

    let mut has_where = false;

    if let Some(expression) = &expression {
        query.push(" WHERE tracks_fts MATCH ").push_bind(expression);
        has_where = true;
    }

    if !tag_ids.is_empty() {
        query.push(if has_where { " AND " } else { " WHERE " });
        query.push("tt.tag_id IN (");

        let mut list = query.separated(", ");
        for id in &tag_ids {
            list.push_bind(id);
        }
        query.push(")");
    }

    if !tag_ids.is_empty() {
        query.push(" GROUP BY t.id");

        if mode == TagMode::All {
            // Relational division: a row survives only if it matched every
            // selected tag.
            query
                .push(" HAVING COUNT(DISTINCT tt.tag_id) = ")
                .push_bind(tag_ids.len() as i64);
        }
    }

    if expression.is_some() {
        // Weighted so a title hit outranks an album hit. bm25 returns
        // increasingly negative values for better matches, hence ascending.
        query.push(" ORDER BY bm25(tracks_fts, 10.0, 5.0, 2.0)");
    } else {
        query.push(" ORDER BY t.artist, t.album, t.title");
    }

    query.push(" LIMIT 500");

    query
        .build_query_as::<Track>()
        .fetch_all(&db.pool)
        .await
        .map_err(|e| e.to_string())
}

/// The library grouped under artist headings.
///
/// One query, folded in Rust. Grouping on the lowercased name means "Radiohead"
/// and "radiohead" -- which scanned tags produce all the time -- land together
/// rather than as two headings; the first spelling seen is what gets displayed.
#[tauri::command]
pub async fn group_tracks_by_artist(db: State<'_, Db>) -> Result<Vec<ArtistGroup>, String> {
    let tracks: Vec<Track> = sqlx::query_as(
        "SELECT t.id, t.source, t.title, t.artist, t.album, t.duration_secs, t.state
         FROM tracks t
         ORDER BY LOWER(COALESCE(NULLIF(TRIM(t.artist), ''), 'Unknown Artist')),
                  t.album,
                  t.title",
    )
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())?;

    let mut groups: Vec<ArtistGroup> = Vec::new();

    for track in tracks {
        let display = display_artist(track.artist.as_deref());
        let key = display.to_lowercase();

        match groups.last_mut() {
            // The query ordered by the same key, so equal keys are adjacent.
            Some(group) if group.artist.to_lowercase() == key => group.tracks.push(track),
            _ => groups.push(ArtistGroup {
                artist: display,
                tracks: vec![track],
            }),
        }
    }

    Ok(groups)
}

/// NULL, empty and whitespace-only artists are all "unknown".
fn display_artist(artist: Option<&str>) -> String {
    artist
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .unwrap_or("Unknown Artist")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_word_becomes_a_prefix_term() {
        assert_eq!(
            to_fts_expression("dark side").as_deref(),
            Some(r#""dark"* AND "side"*"#)
        );
    }

    /// The whole reason for sanitising: these are FTS5 operators, not text.
    #[test]
    fn operators_cannot_reach_the_match_expression() {
        let expression = to_fts_expression("AC/DC \"back\" OR *").unwrap();

        assert!(!expression.contains(" OR "), "got: {expression}");
        assert!(!expression.contains("/"), "got: {expression}");
        // Every term is quoted, so none of them can parse as syntax.
        assert!(expression.starts_with('"'), "got: {expression}");
    }

    #[test]
    fn accented_words_stay_whole() {
        let expression = to_fts_expression("canción").unwrap();
        assert_eq!(expression, "\"canción\"*");
    }

    #[test]
    fn a_query_with_nothing_searchable_yields_nothing() {
        assert_eq!(to_fts_expression("   "), None);
        assert_eq!(to_fts_expression("--- ***"), None);
        assert_eq!(to_fts_expression(""), None);
    }

    #[test]
    fn unknown_artists_share_one_heading() {
        assert_eq!(display_artist(None), "Unknown Artist");
        assert_eq!(display_artist(Some("")), "Unknown Artist");
        assert_eq!(display_artist(Some("   ")), "Unknown Artist");
    }

    #[test]
    fn surrounding_space_does_not_split_an_artist() {
        assert_eq!(display_artist(Some("Radiohead ")), "Radiohead");
        assert_eq!(display_artist(Some(" Radiohead")), "Radiohead");
    }
}
