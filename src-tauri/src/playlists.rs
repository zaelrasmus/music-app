use serde::Serialize;
use sqlx::{FromRow, QueryBuilder, Sqlite, SqliteConnection};
use tauri::State;

use crate::db::Db;
use crate::search::{to_fts_expression, Direction, Sort, TagMode};
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
    /// The artist names this playlist fills itself from.
    ///
    /// Empty for an ordinary playlist. Non-empty is what makes it an artist
    /// collection -- which is also what the UI draws as a circle rather than a
    /// square, so the shape can never disagree with the behaviour.
    #[sqlx(skip)]
    pub artist_rules: Vec<ArtistRule>,
}

/// One name that counts as a playlist's artist.
#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ArtistRule {
    /// Matched against. Trimmed and lowercased.
    pub artist_key: String,
    /// What the user picked, for the chip.
    pub label: String,
    /// The artist's own picture, found in the background after the rule was
    /// made. `None` until it arrives, and `None` forever if the provider has
    /// nothing to offer -- in which case the playlist keeps its generated art,
    /// which is a better answer than one track's cover standing in for forty.
    pub avatar_url: Option<String>,
}

/// An artist present in the library, for the picker and the browse list.
#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct LibraryArtist {
    pub artist_key: String,
    pub name: String,
    pub track_count: i64,
    /// Which provider their tracks came from, so an avatar can be looked for
    /// in the right place. `None` for an artist known only from local files.
    pub source: Option<String>,
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

/// How the playlist grid is ordered.
///
/// A separate vocabulary from the track `Sort`, because the questions are not
/// the same: a playlist has no artist, no duration and no upload date, and
/// reusing an enum whose options mostly do not apply would put five dead
/// entries in the menu to save one type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlaylistSort {
    /// Which list you put on last. The default, because with seventy of them
    /// that is nearly always the one being looked for.
    #[default]
    LastPlayed,
    MostPlayed,
    Name,
    DateCreated,
}

impl PlaylistSort {
    /// Unknowns sort last in *both* directions. "Never played" is not "longest
    /// ago": letting it lead an ascending sort would fill the top of the grid
    /// with the playlists there is least to say about. `created_at` breaks the
    /// tie, which is the order the grid had before either of these existed.
    fn order_by(self, direction: crate::search::Direction) -> &'static str {
        let desc = direction == crate::search::Direction::Desc;
        match self {
            PlaylistSort::LastPlayed if desc => {
                " ORDER BY p.last_played IS NULL, p.last_played DESC, p.created_at DESC, p.id DESC"
            }
            PlaylistSort::LastPlayed => {
                " ORDER BY p.last_played IS NULL, p.last_played ASC, p.created_at ASC, p.id"
            }
            PlaylistSort::MostPlayed if desc => {
                " ORDER BY p.play_count DESC, p.created_at DESC, p.id DESC"
            }
            PlaylistSort::MostPlayed => " ORDER BY p.play_count ASC, p.created_at ASC, p.id",
            PlaylistSort::Name if desc => " ORDER BY p.name COLLATE NOCASE DESC, p.id",
            PlaylistSort::Name => " ORDER BY p.name COLLATE NOCASE ASC, p.id",
            PlaylistSort::DateCreated if desc => " ORDER BY p.created_at DESC, p.id DESC",
            PlaylistSort::DateCreated => " ORDER BY p.created_at ASC, p.id",
        }
    }
}

/// How many tracks a playlist really holds, as SQL, correlated on `p.id`.
///
/// The `CASE` is a fast path, not a micro-optimisation: without rules the
/// answer is a two-row lookup in `playlist_tracks`, and taking the general
/// branch would scan every track in the library once per playlist just to
/// discover that none of them qualify.
const MEMBER_COUNT: &str = "
    CASE WHEN EXISTS (SELECT 1 FROM playlist_artist_rules r WHERE r.playlist_id = p.id)
         THEN (SELECT COUNT(*) FROM tracks t
               LEFT JOIN playlist_tracks pt
                      ON pt.track_id = t.id AND pt.playlist_id = p.id
               WHERE ((pt.track_id IS NOT NULL AND pt.by_rule = 0)
                      OR (t.in_library = 1
                          AND lower(trim(COALESCE(NULLIF(trim(t.remote_uploader), ''), t.artist)))
                              IN (SELECT artist_key FROM playlist_artist_rules
                                  WHERE playlist_id = p.id)))
                 AND t.id NOT IN (SELECT track_id FROM playlist_excluded_tracks
                                  WHERE playlist_id = p.id))
         ELSE (SELECT COUNT(*) FROM playlist_tracks pt
               WHERE pt.playlist_id = p.id AND pt.by_rule = 0)
    END AS track_count";

/// How a track's artist is identified for matching, as SQL.
///
/// `remote_uploader` first because it is the provider's own name for the
/// channel and nothing in this app ever edits it. `artist` is the *display*
/// copy the user may rename, and matching on that would silently drop a track
/// out of its own artist's playlist the moment they tidied its title.
///
/// One constant rather than the expression written out at each site: it is
/// also the expression the index in migration 0015 is built on, and an index
/// that does not match its query is simply a slower query nobody notices.
const ARTIST_KEY: &str =
    "lower(trim(COALESCE(NULLIF(trim(t.remote_uploader), ''), t.artist)))";

/// The same, for queries that do not alias `tracks` as `t`.
const ARTIST_KEY_BARE: &str =
    "lower(trim(COALESCE(NULLIF(trim(remote_uploader), ''), artist)))";

/// Trimmed and lowercased, so one name in two casings is one artist.
pub(crate) fn artist_key_of(name: &str) -> String {
    name.trim().to_lowercase()
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
pub async fn list_playlists(
    db: State<'_, Db>,
    sort: Option<PlaylistSort>,
    direction: Option<crate::search::Direction>,
) -> Result<Vec<Playlist>, String> {
    // `QueryBuilder` because the count is composed rather than literal, and
    // it owns its string -- `query_as` borrows one, which cannot outlive the
    // statement that built it.
    let order = sort
        .unwrap_or_default()
        .order_by(direction.unwrap_or(crate::search::Direction::Desc));

    let mut query: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
        "SELECT p.id, p.name, p.cover_key, p.created_at, p.source, p.source_url, {MEMBER_COUNT}
         FROM playlists p{order}"
    ));

    let mut playlists: Vec<Playlist> = query
        .build_query_as()
        .fetch_all(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    attach_rules(&db.pool, &mut playlists).await?;
    Ok(playlists)
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
    sort: Option<Sort>,
    direction: Option<Direction>,
) -> Result<PlaylistDetail, String> {
    let playlist = load_playlist(&db.pool, playlist_id).await?;
    let tracks =
        playlist_tracks(&db.pool, playlist_id, search, tag_ids, mode, sort, direction).await?;
    Ok(PlaylistDetail { playlist, tracks })
}

/// Which tracks a playlist holds, in order.
///
/// A free function taking a pool rather than only a command, because this is
/// where membership is actually decided -- rule matches, hand-added rows and
/// exclusions resolved together -- and a test that had to reach it through
/// Tauri would end up re-typing the query instead of running it.
pub async fn playlist_tracks(
    pool: &sqlx::SqlitePool,
    playlist_id: i64,
    search: Option<String>,
    tag_ids: Option<Vec<i64>>,
    mode: Option<TagMode>,
    sort: Option<Sort>,
    direction: Option<Direction>,
) -> Result<Vec<Track>, String> {

    let tag_ids = tag_ids.unwrap_or_default();
    let mode = mode.unwrap_or_default();
    let expression = search.as_deref().and_then(to_fts_expression);

    // Typed, but nothing searchable survived sanitising.
    if search.as_deref().is_some_and(|s| !s.trim().is_empty()) && expression.is_none() {
        return Ok(Vec::new());
    }

    // Membership, resolved here rather than materialised into
    // `playlist_tracks`: a rule is a standing statement about what belongs,
    // and copying its answer into rows would need a sync step that can fall
    // behind and a second copy of the truth that can disagree.
    //
    // `LEFT JOIN` rather than `JOIN`, so a track admitted only by a rule still
    // appears -- and `pt.position` being NULL is exactly what marks it as one.
    let mut query: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT t.id, t.source, t.title, t.artist, t.album, t.duration_secs, t.state, t.cover_key, t.in_library, t.remote_thumbnail_url
         FROM tracks t
         LEFT JOIN playlist_tracks pt
                ON pt.track_id = t.id AND pt.playlist_id = ",
    );
    query.push_bind(playlist_id);

    if expression.is_some() {
        query.push(" JOIN tracks_fts ON tracks_fts.rowid = t.id");
    }
    if !tag_ids.is_empty() {
        query.push(" JOIN track_tags tt ON tt.track_id = t.id");
    }

    // `in_library` on the rule branch and not on the hand-added one, because
    // they are different statements. A rule says "everything by this artist",
    // and in this app the library *is* the keeping: a track auditioned once and
    // not kept, or carried in by an imported playlist, was never claimed. Adding
    // one by hand is claiming it, and outranks the rule.
    //
    // Without this the picker and the rule disagree -- it offers an artist with
    // two tracks and the rule admits thirty-nine, which is the kind of surprise
    // that stops anyone trusting a list they did not enumerate.
    // `pt.by_rule = 0` on the first branch: a row written to remember where
    // a rule match sits is *ordering*, not membership. Without that test the
    // rule would stop deciding anything the moment an order was recorded, and
    // removing it would leave every track it ever matched behind.
    query
        .push(" WHERE ((pt.track_id IS NOT NULL AND pt.by_rule = 0) OR (t.in_library = 1 AND ")
        .push(ARTIST_KEY)
        .push(" IN (SELECT artist_key FROM playlist_artist_rules WHERE playlist_id = ")
        .push_bind(playlist_id)
        .push(")))");

    // Removal has to mean something on a list nobody enumerated: without this
    // the rule would put back whatever the user took out, every time.
    query
        .push(" AND t.id NOT IN (SELECT track_id FROM playlist_excluded_tracks")
        .push(" WHERE playlist_id = ")
        .push_bind(playlist_id)
        .push(")");

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

    // Custom is the playlist's own order and the default: hand-placed tracks
    // keep the order the user gave them, and everything a rule brought in
    // follows, oldest first. `pt.position IS NULL` sorts the two groups --
    // false before true -- which is what keeps a curated top half curated as
    // the bottom half grows on its own.
    //
    // Any other sort is a *view*. It never writes anything, and the caller
    // turns dragging off while one is active, because a row's position on
    // screen has stopped being its position in the playlist.
    match sort.unwrap_or(Sort::Custom) {
        Sort::Custom | Sort::Auto => {
            query.push(
                " ORDER BY (pt.position IS NULL), pt.position, pt.added_at, t.date_added, t.id",
            );
        }
        chosen => {
            query.push(chosen.order_by(direction.unwrap_or_default()));
        }
    }

    query
        .build_query_as::<Track>()
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())
}

/// Appends tracks, ignoring any already present.
#[tauri::command]
pub async fn add_tracks_to_playlist(
    db: State<'_, Db>,
    playlist_id: i64,
    track_ids: Vec<i64>,
) -> Result<AddOutcome, String> {
    add_tracks(&db.pool, playlist_id, track_ids).await
}

/// Appends tracks, ignoring any already present.
///
/// A free function taking a pool, for the same reason `playlist_tracks` is one:
/// this is where "added by hand" is distinguished from "the rule put it there",
/// and a test that had to reach it through Tauri would re-type the statement
/// instead of running it.
pub async fn add_tracks(
    pool: &sqlx::SqlitePool,
    playlist_id: i64,
    track_ids: Vec<i64>,
) -> Result<AddOutcome, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;

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
        // Adding is the opposite decision to removing, so it retracts one.
        // Without this, re-adding a track the user had taken out would insert
        // a row that the exclusion then hides -- a button that appears to do
        // nothing, which is the same trap the exclusion was invented to close.
        sqlx::query("DELETE FROM playlist_excluded_tracks WHERE playlist_id = ? AND track_id = ?")
            .bind(playlist_id)
            .bind(track_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;

        // An existing *ordering* row is promoted rather than skipped:
        // adding a track by hand says it belongs whatever the rules do
        // later, and that is a different claim from where it sits.
        let inserted = sqlx::query(
            "INSERT INTO playlist_tracks (playlist_id, track_id, position, by_rule)
             VALUES (?, ?, ?, 0)
             ON CONFLICT (playlist_id, track_id) DO UPDATE SET by_rule = 0
             WHERE playlist_tracks.by_rule = 1",
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

    // Recorded whatever happens below. On a list nobody enumerated, deleting
    // the row is not enough -- a rule would put the track straight back and
    // the button would appear to do nothing. This is what makes "remove" mean
    // "and stay out".
    //
    // Written even when no rule matches today, because one may be added
    // tomorrow, and the user's decision about this track should outlive that.
    sqlx::query(
        "INSERT INTO playlist_excluded_tracks (playlist_id, track_id)
         VALUES (?, ?) ON CONFLICT DO NOTHING",
    )
    .bind(playlist_id)
    .bind(track_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    let Some(position) = position_of(&mut tx, playlist_id, track_id).await? else {
        // No hand-placed row to remove. Either it was only ever here because a
        // rule said so -- in which case the exclusion above is the whole job --
        // or it was already gone, and the goal is that it is absent.
        return tx.commit().await.map_err(|e| e.to_string());
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
    // Taking hold of the order is what fixes it.
    //
    // A rule's matches have no position -- they are not rows in
    // `playlist_tracks` at all -- so there is nothing to drag them between. The
    // first drag in such a playlist therefore writes down the order currently on
    // screen, and from then on this is an ordinary hand-ordered playlist that
    // the rule still adds to, at the end.
    //
    // The alternative was to give the dragged track a position and leave its
    // neighbours without one, which sorts hand-placed tracks above rule matches
    // -- so dragging a row *downwards* would fling it to the top. Freezing the
    // whole order is the only version where the row lands where it was dropped.
    materialise_order(&db.pool, playlist_id).await?;

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
    let mut query: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
        "SELECT p.id, p.name, p.cover_key, p.created_at, p.source, p.source_url, {MEMBER_COUNT}
         FROM playlists p
         WHERE p.id = "
    ));
    query.push_bind(playlist_id);

    let mut found: Vec<Playlist> = query
        .build_query_as()
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())?;

    attach_rules(pool, &mut found).await?;

    found
        .pop()
        .ok_or_else(|| "That playlist no longer exists.".to_string())
}

/// Fills in each playlist's rules.
///
/// One query for the whole list rather than one per playlist: the rules are
/// what decide whether a row draws as an artist, so every list that shows a
/// playlist needs them and none of them should pay per row for it.
async fn attach_rules(
    pool: &sqlx::SqlitePool,
    playlists: &mut [Playlist],
) -> Result<(), String> {
    if playlists.is_empty() {
        return Ok(());
    }

    let rows: Vec<(i64, String, String, Option<String>)> = sqlx::query_as(
        "SELECT playlist_id, artist_key, label, avatar_url FROM playlist_artist_rules
         ORDER BY added_at, artist_key",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    for (playlist_id, artist_key, label, avatar_url) in rows {
        if let Some(playlist) = playlists.iter_mut().find(|p| p.id == playlist_id) {
            playlist.artist_rules.push(ArtistRule {
                artist_key,
                label,
                avatar_url,
            });
        }
    }

    Ok(())
}

/// Writes down the order a playlist is currently showing.
///
/// A no-op for a playlist without rules, where every member already has a
/// position. For one with rules it turns the resolved order into real rows, so
/// that dragging has something to move.
///
/// Idempotent, and safe to call before any reorder: it only ever fills gaps,
/// never renumbers a track the user already placed.
async fn materialise_order(pool: &sqlx::SqlitePool, playlist_id: i64) -> Result<(), String> {
    // The order as shown, which is the order being taken hold of.
    let shown = playlist_tracks(pool, playlist_id, None, None, None, None, None).await?;

    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;

    let placed: Vec<i64> =
        sqlx::query_scalar("SELECT track_id FROM playlist_tracks WHERE playlist_id = ?")
            .bind(playlist_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;

    if placed.len() == shown.len() {
        // Every member is already a row; nothing to write down.
        return Ok(());
    }

    // Renumbered from the shown order, so positions stay dense and a UI
    // ordinal keeps matching a stored one.
    for (position, track) in shown.iter().enumerate() {
        // `by_rule = 1` on insert only. A track that already has a row keeps
        // whichever kind it was: hand-added rows are written by
        // `add_tracks_to_playlist`, and nothing here may quietly demote one
        // to an ordering row.
        sqlx::query(
            "INSERT INTO playlist_tracks (playlist_id, track_id, position, by_rule)
             VALUES (?1, ?2, ?3, 1)
             ON CONFLICT (playlist_id, track_id) DO UPDATE SET position = ?3",
        )
        .bind(playlist_id)
        .bind(track.id)
        .bind(position as i64)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }

    tx.commit().await.map_err(|e| e.to_string())
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

// --- artist rules ------------------------------------------------------

/// Every artist present in the library, most tracks first.
///
/// Feeds both the chip picker and the browse list, because they are the same
/// question asked from two places: *who is in here?* Only library tracks
/// count -- an artist you auditioned once and did not keep is not someone you
/// are collecting.
///
/// Tracks with no artist at all are excluded rather than gathered under an
/// "Unknown" heading. They are 98% of a local library scanned from files with
/// no tags, and a rule naming them would sweep the entire library into one
/// playlist -- which is never what anybody meant.
#[tauri::command]
pub async fn list_library_artists(db: State<'_, Db>) -> Result<Vec<LibraryArtist>, String> {
    let sql = format!(
        "SELECT {ARTIST_KEY_BARE} AS artist_key,
                MIN(COALESCE(NULLIF(trim(remote_uploader), ''), artist)) AS name,
                COUNT(*) AS track_count,
                MIN(NULLIF(source, 'local')) AS source
         FROM tracks
         WHERE in_library = 1
           AND {ARTIST_KEY_BARE} IS NOT NULL
           AND {ARTIST_KEY_BARE} <> ''
         GROUP BY artist_key
         ORDER BY track_count DESC, name"
    );

    let mut query: QueryBuilder<Sqlite> = QueryBuilder::new(sql);
    query
        .build_query_as()
        .fetch_all(&db.pool)
        .await
        .map_err(|e| e.to_string())
}

/// Makes `label` one of the names this playlist fills itself from.
///
/// Idempotent: naming the same artist twice is a no-op rather than an error,
/// because the user's intent ("this artist counts") is already satisfied.
#[tauri::command]
pub async fn add_playlist_artist_rule(
    app: tauri::AppHandle,
    db: State<'_, Db>,
    playlist_id: i64,
    label: String,
) -> Result<Playlist, String> {
    let key = artist_key_of(&label);
    if key.is_empty() {
        return Err("That is not a name this can match on.".to_string());
    }

    sqlx::query(
        "INSERT INTO playlist_artist_rules (playlist_id, artist_key, label)
         VALUES (?, ?, ?)
         ON CONFLICT(playlist_id, artist_key) DO UPDATE SET label = excluded.label",
    )
    .bind(playlist_id)
    .bind(&key)
    .bind(label.trim())
    .execute(&db.pool)
    .await
    .map_err(|e| e.to_string())?;

    // Adding a rule can readmit tracks the user removed while it was absent,
    // which would be a surprise -- the exclusions were about *this* playlist,
    // and it has just changed its mind about what belongs.
    //
    // Deliberately not cleared: an exclusion is a decision about one track,
    // and a rule is a decision about a name. Neither overrules the other, and
    // silently discarding the more specific one is the wrong way round.

    // The order is written down now rather than on the first drag.
    //
    // Rows for the tracks a rule matches are *ordering* rows, so recording
    // them costs the rule nothing -- it still decides membership -- and it
    // means the playlist can be rearranged from the moment it exists, with
    // no invisible change of behaviour partway through.
    materialise_order(&db.pool, playlist_id).await?;

    // Spawned, not awaited: finding the picture is a provider round trip of
    // several seconds, and naming an artist should take effect immediately.
    // The playlist keeps its generated art until this lands.
    find_avatar(app, db.pool.clone(), playlist_id, key, label);

    load_playlist(&db.pool, playlist_id).await
}

/// Looks for the artist's own picture, and files it against the rule.
///
/// Best effort throughout, and silent when it fails: nothing the user asked
/// for depends on the result. The worst case is a playlist that keeps the art
/// generated from its name, which is what every other playlist has.
///
/// Found by asking the provider the same question the artist search asks --
/// there is no stored artist anywhere in this app, only tracks, and a track's
/// thumbnail is its own cover rather than the person who made it.
fn find_avatar(
    app: tauri::AppHandle,
    pool: sqlx::SqlitePool,
    playlist_id: i64,
    artist_key: String,
    label: String,
) {
    tauri::async_runtime::spawn(async move {
        // Which provider to ask is decided by where the tracks came from. An
        // artist known only from local files has none, and gets no picture.
        let mut probe: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
            "SELECT MIN(NULLIF(source, 'local')) FROM tracks
             WHERE in_library = 1 AND {ARTIST_KEY_BARE} = "
        ));
        probe.push_bind(&artist_key);

        let source: Option<String> = probe
            .build_query_scalar()
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten();

        let Some(provider) = source
            .as_deref()
            .and_then(crate::providers::Provider::from_source)
        else {
            return;
        };

        let found = crate::collections::search_collections(
            app,
            provider,
            crate::providers::SearchKind::Artist,
            label.clone(),
        )
        .await;

        // The first result, and only when its name is the one asked for. A
        // near miss would put a stranger's face on the playlist, which is
        // worse than no face at all.
        let avatar = found.ok().and_then(|collections| {
            collections.into_iter().find_map(|collection| {
                (artist_key_of(&collection.title) == artist_key)
                    .then_some(collection.thumbnail_url)
                    .flatten()
            })
        });

        let Some(avatar) = avatar else {
            return;
        };

        let _ = sqlx::query(
            "UPDATE playlist_artist_rules SET avatar_url = ?
             WHERE playlist_id = ? AND artist_key = ?",
        )
        .bind(avatar)
        .bind(playlist_id)
        .bind(&artist_key)
        .execute(&pool)
        .await;
    });
}

/// Stops this playlist filling itself from `artist_key`.
///
/// Tracks the rule brought in simply stop appearing. Anything the user added
/// by hand stays, because that was a separate decision.
#[tauri::command]
pub async fn remove_playlist_artist_rule(
    db: State<'_, Db>,
    playlist_id: i64,
    artist_key: String,
) -> Result<Playlist, String> {
    sqlx::query("DELETE FROM playlist_artist_rules WHERE playlist_id = ? AND artist_key = ?")
        .bind(playlist_id)
        .bind(artist_key_of(&artist_key))
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    // The ordering rows for tracks no rule matches any more are now saying
    // where something that is not here should sit. Harmless, but they would
    // accumulate for as long as rules were added and dropped, so they go with
    // the rule that justified them. Hand-added rows are untouched: those are a
    // decision, not bookkeeping.
    let mut orphans: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
        "DELETE FROM playlist_tracks
         WHERE by_rule = 1
           AND track_id NOT IN (
             SELECT t.id FROM tracks t
             WHERE {ARTIST_KEY} IN (
                 SELECT artist_key FROM playlist_artist_rules WHERE playlist_id = "
    ));
    orphans.push_bind(playlist_id);
    orphans.push(")) AND playlist_id = ");
    orphans.push_bind(playlist_id);

    orphans
        .build()
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    load_playlist(&db.pool, playlist_id).await
}

/// The statement behind [`add_playlist_to_library`].
///
/// `date_added` is stamped for the same reason it is in
/// [`crate::tracks::SET_IN_LIBRARY`], and this is the site where it matters
/// most: every row an import created has existed since the import, so without
/// the stamp filing a fifty-track playlist scatters all fifty through last
/// month and the library looks unchanged.
///
/// No `CASE` here, unlike the single-track statement -- the `WHERE` already
/// restricts this to tracks that were not filed, so every row it touches is a
/// transition by construction.
const ADD_PLAYLIST_TO_LIBRARY: &str = "UPDATE tracks
     SET in_library = 1, date_added = unixepoch()
     WHERE in_library = 0
       AND id IN (SELECT track_id FROM playlist_tracks WHERE playlist_id = ?)
     RETURNING id";

/// Files every track in a playlist in the library.
///
/// The gesture an imported playlist needs. Importing deliberately does not add
/// its tracks to the library -- "I want this list here" is not "I want fifty
/// tracks in my library" -- but that leaves them invisible to anything keyed on
/// library membership, artist rules above all. This is the one action that says
/// *actually, I want all of these*, without making the import mean something it
/// did not.
///
/// Returns how many were newly filed. Tracks already in the library are left
/// alone rather than counted, so the number is what changed.
///
/// Only `playlist_tracks` rows can be affected: a rule already requires library
/// membership, so anything it admitted is filed by definition.
#[tauri::command]
pub async fn add_playlist_to_library(
    app: tauri::AppHandle,
    db: State<'_, Db>,
    covers: State<'_, crate::covers::CoverStore>,
    playlist_id: i64,
) -> Result<usize, String> {
    let filed: Vec<i64> = sqlx::query_scalar(ADD_PLAYLIST_TO_LIBRARY)
    .bind(playlist_id)
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())?;

    if filed.is_empty() {
        return Ok(0);
    }

    let count = filed.len();

    // Artwork is bought when a track is kept, and this keeps a great many at
    // once. One task walking the list rather than one task per track: each
    // fetch is an ffmpeg process, and fifty of those starting together would
    // stall the machine to decorate a list nobody is looking at yet.
    let pool = db.pool.clone();
    let covers = covers.inner().clone();
    tauri::async_runtime::spawn(async move {
        for track_id in filed {
            crate::covers::ensure_for_track(app.clone(), pool.clone(), covers.clone(), track_id)
                .await;
        }
    });

    Ok(count)
}

/// Notes that a playlist was played.
///
/// Counts plays of the *playlist*, not of the tracks in it. A track played from
/// the library that happens to sit here does not count: "which list did I put on
/// last" is the question, and answering it from track history would push a
/// playlist nobody opened to the top of the grid.
#[tauri::command]
pub async fn mark_playlist_played(db: State<'_, Db>, playlist_id: i64) -> Result<(), String> {
    sqlx::query(
        "UPDATE playlists SET last_played = unixepoch(), play_count = play_count + 1
         WHERE id = ?",
    )
    .bind(playlist_id)
    .execute(&db.pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// The library stamp on [`ADD_PLAYLIST_TO_LIBRARY`], which is the site where
/// losing it does the most visible damage: an imported playlist's rows all
/// share the import's date, so an unstamped "add all to library" puts fifty
/// tracks somewhere in last month at once.
#[cfg(test)]
mod add_to_library_tests {
    use super::*;
    use sqlx::SqlitePool;

    const LONG_AGO: i64 = 1_787_186_899;

    async fn pool(name: &str) -> SqlitePool {
        let dir = std::env::temp_dir().join(format!("music-app-playlist-filing-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::db::init(&dir).await.unwrap().pool
    }

    /// An imported row: created when the playlist was imported, filed or not.
    async fn imported(pool: &SqlitePool, remote_id: &str, in_library: i64) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO tracks (source, title, state, remote_id, remote_url, \
             in_library, date_added) \
             VALUES ('youtube', 'A Song', 'saved', ?, 'https://y.invalid/w', ?, ?) \
             RETURNING id",
        )
        .bind(remote_id)
        .bind(in_library)
        .bind(LONG_AGO)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn date_added(pool: &SqlitePool, id: i64) -> i64 {
        sqlx::query_scalar("SELECT date_added FROM tracks WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn filing_a_playlist_dates_its_tracks_from_the_gesture() {
        let pool = pool("stamp").await;

        let playlist: i64 =
            sqlx::query_scalar("INSERT INTO playlists (name) VALUES ('Imported') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();

        let fresh = imported(&pool, "one", 0).await;
        let already = imported(&pool, "two", 1).await;

        for (position, track) in [fresh, already].into_iter().enumerate() {
            sqlx::query(
                "INSERT INTO playlist_tracks (playlist_id, track_id, position) VALUES (?, ?, ?)",
            )
            .bind(playlist)
            .bind(track)
            .bind(position as i64)
            .execute(&pool)
            .await
            .unwrap();
        }

        let before: i64 = sqlx::query_scalar("SELECT unixepoch()")
            .fetch_one(&pool)
            .await
            .unwrap();

        let filed: Vec<i64> = sqlx::query_scalar(ADD_PLAYLIST_TO_LIBRARY)
            .bind(playlist)
            .fetch_all(&pool)
            .await
            .unwrap();

        assert_eq!(filed, vec![fresh], "only the unfiled track was a change");
        assert!(
            date_added(&pool, fresh).await >= before,
            "a track filed by this gesture joined the library now, not at import"
        );
        assert_eq!(
            date_added(&pool, already).await,
            LONG_AGO,
            "a track already in the library must not be redated by it"
        );
    }
}
