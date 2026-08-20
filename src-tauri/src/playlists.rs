use serde::Serialize;
use sqlx::{FromRow, QueryBuilder, Sqlite, SqliteConnection};
use tauri::State;

use crate::db::Db;
use crate::search::{to_fts_expression, TagMode};
use crate::tracks::Track;

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    /// Names a file in the cover store, set by the user. `None` means
    /// artwork generated from the playlist's name.
    pub cover_key: Option<String>,
    pub created_at: i64,
    /// Shown in the list so a playlist's size is visible without opening it.
    pub track_count: i64,
    /// The provider an imported playlist came from, if it was imported.
    ///
    /// Null for every playlist made by hand, which is what tells the two
    /// apart without a flag that could disagree with the URL beside it.
    pub source: Option<String>,
    /// The provider page it was built from.
    pub source_url: Option<String>,
}

/// A playlist together with its tracks, in order.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDetail {
    pub playlist: Playlist,
    pub tracks: Vec<Track>,
}

/// What `add_tracks_to_playlist` did.
///
/// Adding is idempotent, so "nothing happened" is a normal outcome and the UI
/// needs to be able to say so rather than implying a failure.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddOutcome {
    pub added: usize,
    /// Already present, so left alone.
    pub skipped: usize,
}

fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A playlist needs a name.".to_string());
    }
    Ok(name.to_string())
}

#[tauri::command]
pub async fn create_playlist(db: State<'_, Db>, name: String) -> Result<Playlist, String> {
    let name = clean_name(&name)?;

    // Names are deliberately not unique: two playlists called "Chill" is the
    // user's business.
    let id: i64 = sqlx::query_scalar("INSERT INTO playlists (name) VALUES (?) RETURNING id")
        .bind(&name)
        .fetch_one(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    load_playlist(&db.pool, id).await
}

/// Creates a playlist from a provider's, remembering where it came from.
///
/// The tracks are already saved rows by the time this is called; what happens
/// here is only the playlist and its membership.
///
/// Deliberately **not** filed in the library. Importing says "I want this list
/// to be here", not "I want fifty tracks in my library" -- and the difference
/// matters most for the big imports, where the second reading would bury a
/// carefully kept library under an album someone half remembered. Each track
/// can still be added individually, from the playlist, by the same gesture
/// that adds any other.
///
/// A name is taken rather than derived, so the provider's own title can be
/// edited before it lands.
#[tauri::command]
pub async fn import_playlist(
    db: State<'_, Db>,
    name: String,
    source: crate::providers::Provider,
    source_url: String,
    track_ids: Vec<i64>,
) -> Result<Playlist, String> {
    let name = clean_name(&name)?;

    if !source.accepts_url(&source_url) {
        return Err(format!(
            "That does not look like a {} link.",
            source.display_name()
        ));
    }

    let mut tx = db.pool.begin().await.map_err(|e| e.to_string())?;

    let playlist_id: i64 = sqlx::query_scalar(
        "INSERT INTO playlists (name, source, source_url) VALUES (?, ?, ?) RETURNING id",
    )
    .bind(&name)
    .bind(source.as_str())
    .bind(&source_url)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    // Positions are assigned from the given order rather than read back,
    // because the playlist is new: nothing else can be inserting into it, and
    // the order the provider listed is the order the user is looking at.
    for (position, track_id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO playlist_tracks (playlist_id, track_id, position)
             VALUES (?, ?, ?)
             ON CONFLICT (playlist_id, track_id) DO NOTHING",
        )
        .bind(playlist_id)
        .bind(track_id)
        .bind(position as i64)
        .execute(&mut *tx)
        .await
        .map_err(|e| describe_track_error(&e, *track_id))?;
    }

    tx.commit().await.map_err(|e| e.to_string())?;

    load_playlist(&db.pool, playlist_id).await
}

#[tauri::command]
pub async fn rename_playlist(
    db: State<'_, Db>,
    playlist_id: i64,
    name: String,
) -> Result<(), String> {
    let name = clean_name(&name)?;

    let affected = sqlx::query("UPDATE playlists SET name = ? WHERE id = ?")
        .bind(&name)
        .bind(playlist_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected();

    if affected == 0 {
        return Err("That playlist no longer exists.".to_string());
    }

    Ok(())
}

#[tauri::command]
pub async fn delete_playlist(db: State<'_, Db>, playlist_id: i64) -> Result<(), String> {
    // playlist_tracks rows go with it via ON DELETE CASCADE; the tracks
    // themselves are untouched.
    sqlx::query("DELETE FROM playlists WHERE id = ?")
        .bind(playlist_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn list_playlists(db: State<'_, Db>) -> Result<Vec<Playlist>, String> {
    sqlx::query_as(
        "SELECT p.id, p.name, p.cover_key, p.created_at, p.source, p.source_url,
                COUNT(pt.track_id) AS track_count
         FROM playlists p
         LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id
         GROUP BY p.id
         ORDER BY p.created_at DESC, p.id DESC",
    )
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())
}

/// A playlist's tracks, optionally narrowed by text and tags.
///
/// The filter narrows but never reorders. A playlist's order is curated by the
/// user, so ranking its contents by search relevance -- which is right for the
/// library -- would be wrong here: `playlist.trackCount` stays the real size
/// while the returned list is what survived the filter.
#[tauri::command]
pub async fn get_playlist(
    db: State<'_, Db>,
    playlist_id: i64,
    search: Option<String>,
    tag_ids: Option<Vec<i64>>,
    mode: Option<TagMode>,
) -> Result<PlaylistDetail, String> {
    let playlist = load_playlist(&db.pool, playlist_id).await?;

    let tag_ids = tag_ids.unwrap_or_default();
    let mode = mode.unwrap_or_default();
    let expression = search.as_deref().and_then(to_fts_expression);

    // Typed, but nothing searchable survived sanitising.
    if search.as_deref().is_some_and(|s| !s.trim().is_empty()) && expression.is_none() {
        return Ok(PlaylistDetail {
            playlist,
            tracks: Vec::new(),
        });
    }

    let mut query: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT t.id, t.source, t.title, t.artist, t.album, t.duration_secs, t.state, t.cover_key, t.in_library, t.remote_thumbnail_url
         FROM playlist_tracks pt
         JOIN tracks t ON t.id = pt.track_id",
    );

    if expression.is_some() {
        query.push(" JOIN tracks_fts ON tracks_fts.rowid = t.id");
    }
    if !tag_ids.is_empty() {
        query.push(" JOIN track_tags tt ON tt.track_id = t.id");
    }

    query.push(" WHERE pt.playlist_id = ").push_bind(playlist_id);

    if let Some(expression) = &expression {
        query.push(" AND tracks_fts MATCH ").push_bind(expression);
    }

    if !tag_ids.is_empty() {
        query.push(" AND tt.tag_id IN (");
        let mut list = query.separated(", ");
        for id in &tag_ids {
            list.push_bind(id);
        }
        query.push(")");

        // One playlist_tracks row per track, so grouping keeps `pt.position`
        // unambiguous.
        query.push(" GROUP BY t.id");

        if mode == TagMode::All {
            query
                .push(" HAVING COUNT(DISTINCT tt.tag_id) = ")
                .push_bind(tag_ids.len() as i64);
        }
    }

    // Always playlist order. `added_at` breaks ties so it stays deterministic
    // even if positions were ever to collide.
    query.push(" ORDER BY pt.position, pt.added_at, t.id");

    let tracks = query
        .build_query_as::<Track>()
        .fetch_all(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(PlaylistDetail { playlist, tracks })
}

/// Appends tracks, ignoring any already present.
#[tauri::command]
pub async fn add_tracks_to_playlist(
    db: State<'_, Db>,
    playlist_id: i64,
    track_ids: Vec<i64>,
) -> Result<AddOutcome, String> {
    let mut tx = db.pool.begin().await.map_err(|e| e.to_string())?;

    ensure_playlist_exists(&mut tx, playlist_id).await?;

    // Read inside the transaction: two concurrent adds would otherwise pick
    // the same starting position.
    let mut next: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_tracks WHERE playlist_id = ?",
    )
    .bind(playlist_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    let mut outcome = AddOutcome {
        added: 0,
        skipped: 0,
    };

    for track_id in track_ids {
        let inserted = sqlx::query(
            "INSERT INTO playlist_tracks (playlist_id, track_id, position)
             VALUES (?, ?, ?)
             ON CONFLICT (playlist_id, track_id) DO NOTHING",
        )
        .bind(playlist_id)
        .bind(track_id)
        .bind(next)
        .execute(&mut *tx)
        .await
        .map_err(|e| describe_track_error(&e, track_id))?
        .rows_affected();

        if inserted > 0 {
            outcome.added += 1;
            next += 1;
        } else {
            outcome.skipped += 1;
        }
    }

    tx.commit().await.map_err(|e| e.to_string())?;

    Ok(outcome)
}

#[tauri::command]
pub async fn remove_track_from_playlist(
    db: State<'_, Db>,
    playlist_id: i64,
    track_id: i64,
) -> Result<(), String> {
    let mut tx = db.pool.begin().await.map_err(|e| e.to_string())?;

    let Some(position) = position_of(&mut tx, playlist_id, track_id).await? else {
        // Already gone: the goal is that it is absent, and it is.
        return Ok(());
    };

    sqlx::query("DELETE FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?")
        .bind(playlist_id)
        .bind(track_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    // Close the gap, so positions stay dense and a UI ordinal keeps matching a
    // stored position.
    sqlx::query(
        "UPDATE playlist_tracks SET position = position - 1
         WHERE playlist_id = ? AND position > ?",
    )
    .bind(playlist_id)
    .bind(position)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    tx.commit().await.map_err(|e| e.to_string())
}

/// Moves a track to `new_position`, shifting everything between.
#[tauri::command]
pub async fn reorder_playlist_track(
    db: State<'_, Db>,
    playlist_id: i64,
    track_id: i64,
    new_position: i64,
) -> Result<(), String> {
    let mut tx = db.pool.begin().await.map_err(|e| e.to_string())?;

    let Some(old_position) = position_of(&mut tx, playlist_id, track_id).await? else {
        return Err("That track is not in this playlist.".to_string());
    };

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM playlist_tracks WHERE playlist_id = ?")
            .bind(playlist_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;

    let new_position = new_position.clamp(0, (count - 1).max(0));

    if new_position != old_position {
        // One statement for the whole move: the row lands on its new position
        // and everything it passed shifts one step the other way. This is why
        // (playlist_id, position) must not be UNIQUE -- mid-statement, two
        // rows briefly share a position.
        sqlx::query(
            "UPDATE playlist_tracks
             SET position = CASE
                 WHEN position = ?2 THEN ?3
                 WHEN ?2 < ?3 AND position > ?2 AND position <= ?3 THEN position - 1
                 WHEN ?2 > ?3 AND position >= ?3 AND position < ?2 THEN position + 1
                 ELSE position
             END
             WHERE playlist_id = ?1",
        )
        .bind(playlist_id)
        .bind(old_position)
        .bind(new_position)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }

    tx.commit().await.map_err(|e| e.to_string())
}

// --- helpers -----------------------------------------------------------

async fn load_playlist(pool: &sqlx::SqlitePool, playlist_id: i64) -> Result<Playlist, String> {
    sqlx::query_as(
        "SELECT p.id, p.name, p.cover_key, p.created_at, p.source, p.source_url,
                COUNT(pt.track_id) AS track_count
         FROM playlists p
         LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id
         WHERE p.id = ?
         GROUP BY p.id",
    )
    .bind(playlist_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "That playlist no longer exists.".to_string())
}

async fn ensure_playlist_exists(
    tx: &mut SqliteConnection,
    playlist_id: i64,
) -> Result<(), String> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM playlists WHERE id = ?")
        .bind(playlist_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    exists
        .map(|_| ())
        .ok_or_else(|| "That playlist no longer exists.".to_string())
}

async fn position_of(
    tx: &mut SqliteConnection,
    playlist_id: i64,
    track_id: i64,
) -> Result<Option<i64>, String> {
    sqlx::query_scalar(
        "SELECT position FROM playlist_tracks WHERE playlist_id = ? AND track_id = ?",
    )
    .bind(playlist_id)
    .bind(track_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| e.to_string())
}

/// Foreign keys are enforced, so a bad track id surfaces as a constraint
/// violation rather than a silent no-op. Say what actually went wrong.
fn describe_track_error(error: &sqlx::Error, track_id: i64) -> String {
    let text = error.to_string();
    if text.contains("FOREIGN KEY") {
        format!("Track {track_id} no longer exists.")
    } else {
        text
    }
}

/// Importing a provider playlist, against a real database.
///
/// The interesting parts are not the SQL but the promises made around it: that
/// the order survives, that the tracks stay out of the library, and that the
/// playlist remembers where it came from.
#[cfg(test)]
mod import_tests {
    use crate::providers::Provider;
    use sqlx::{Row, SqlitePool};

    async fn pool(name: &str) -> SqlitePool {
        let dir = std::env::temp_dir().join(format!("music-app-import-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::db::init(&dir).await.unwrap().pool
    }

    /// The statements `save_remote_tracks` runs, in one transaction.
    async fn save_many(pool: &SqlitePool, ids: &[&str]) -> Vec<i64> {
        let mut tx = pool.begin().await.unwrap();
        let mut saved = Vec::new();

        for (n, remote_id) in ids.iter().enumerate() {
            let id: i64 = sqlx::query_scalar(crate::youtube::SAVE_REMOTE_TRACK)
                .bind("youtube")
                .bind(format!("Track {n}"))
                .bind(Some("An Uploader"))
                .bind(Some(180i64))
                .bind(remote_id)
                .bind(format!("https://www.youtube.com/watch?v={remote_id}"))
                .bind(Some("An Uploader"))
                .bind(format!("Track {n}"))
                .bind(Some("https://i.ytimg.com/vi/x/hq.jpg"))
                .bind(None::<i64>)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            saved.push(id);
        }

        tx.commit().await.unwrap();
        saved
    }

    /// The statements `import_playlist` runs.
    async fn import(pool: &SqlitePool, name: &str, url: &str, track_ids: &[i64]) -> i64 {
        let playlist_id: i64 = sqlx::query_scalar(
            "INSERT INTO playlists (name, source, source_url) VALUES (?, ?, ?) RETURNING id",
        )
        .bind(name)
        .bind(Provider::YouTube.as_str())
        .bind(url)
        .fetch_one(pool)
        .await
        .unwrap();

        for (position, track_id) in track_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO playlist_tracks (playlist_id, track_id, position)
                 VALUES (?, ?, ?)
                 ON CONFLICT (playlist_id, track_id) DO NOTHING",
            )
            .bind(playlist_id)
            .bind(track_id)
            .bind(position as i64)
            .execute(pool)
            .await
            .unwrap();
        }

        playlist_id
    }

    #[tokio::test]
    async fn an_imported_playlist_keeps_the_providers_order() {
        let pool = pool("order").await;
        // Deliberately not alphabetical, and not the order the ids sort in:
        // the only thing that should decide this is the order given.
        let ids = save_many(&pool, &["ccccccccccc", "aaaaaaaaaaa", "bbbbbbbbbbb"]).await;
        let playlist = import(&pool, "Discovery", "https://www.youtube.com/playlist?list=X", &ids).await;

        let ordered: Vec<i64> = sqlx::query_scalar(
            "SELECT track_id FROM playlist_tracks WHERE playlist_id = ? ORDER BY position",
        )
        .bind(playlist)
        .fetch_all(&pool)
        .await
        .unwrap();

        assert_eq!(ordered, ids, "a playlist's order is its content");
    }

    /// The user's decision, made explicit: keep the list, not fifty tracks.
    #[tokio::test]
    async fn imported_tracks_stay_out_of_the_library() {
        let pool = pool("library").await;
        let ids = save_many(&pool, &["ddddddddddd", "eeeeeeeeeee"]).await;
        import(&pool, "An Album", "https://www.youtube.com/playlist?list=Y", &ids).await;

        let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks WHERE in_library = 1")
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(kept, 0, "importing a list must not file every track in it");
    }

    #[tokio::test]
    async fn an_imported_playlist_remembers_where_it_came_from() {
        let pool = pool("provenance").await;
        let ids = save_many(&pool, &["fffffffffff"]).await;
        let url = "https://www.youtube.com/playlist?list=Z";
        let playlist = import(&pool, "Imported", url, &ids).await;

        let row = sqlx::query("SELECT source, source_url FROM playlists WHERE id = ?")
            .bind(playlist)
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(row.get::<Option<String>, _>("source").as_deref(), Some("youtube"));
        assert_eq!(row.get::<Option<String>, _>("source_url").as_deref(), Some(url));
    }

    /// A playlist made by hand is told apart by having no origin at all,
    /// rather than by a flag that could disagree with the URL beside it.
    #[tokio::test]
    async fn a_handmade_playlist_has_no_origin() {
        let pool = pool("handmade").await;

        let id: i64 = sqlx::query_scalar("INSERT INTO playlists (name) VALUES (?) RETURNING id")
            .bind("Mine")
            .fetch_one(&pool)
            .await
            .unwrap();

        let row = sqlx::query("SELECT source, source_url FROM playlists WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();

        assert!(row.get::<Option<String>, _>("source").is_none());
        assert!(row.get::<Option<String>, _>("source_url").is_none());
    }

    /// Re-importing the same playlist must not multiply its rows.
    #[tokio::test]
    async fn importing_the_same_tracks_twice_reuses_them() {
        let pool = pool("twice").await;
        let first = save_many(&pool, &["ggggggggggg", "hhhhhhhhhhh"]).await;
        let second = save_many(&pool, &["ggggggggggg", "hhhhhhhhhhh"]).await;

        assert_eq!(first, second, "the same remote track is the same row");

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 2);
    }
    /// A migration that failed to apply looks exactly like one that did,
    /// right up until a query mentions a column that is not there.
    #[tokio::test]
    async fn every_column_this_feature_added_exists() {
        let dir = std::env::temp_dir().join("music-app-migration-shape");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let pool = crate::db::init(&dir).await.unwrap().pool;

        for (table, column) in [
            ("tracks", "remote_thumbnail_url"),
            ("playlists", "source"),
            ("playlists", "source_url"),
        ] {
            let found: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?",
            )
            .bind(table)
            .bind(column)
            .fetch_one(&pool)
            .await
            .unwrap();

            assert_eq!(found, 1, "{table}.{column} is missing");
        }
    }
}
