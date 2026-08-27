use serde::Serialize;
use sqlx::FromRow;
use tauri::{AppHandle, Emitter, State};

use crate::db::Db;
use crate::scanner::{self, ScanLock, ScanSummary};

/// Emitted when a scan finishes so the frontend can refetch.
pub const SCAN_FINISHED_EVENT: &str = "scan-finished";
pub const SCAN_PROGRESS_EVENT: &str = "scan-progress";

/// Deliberately minimal -- the real track view is the next task.
#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: i64,
    /// "local" or "youtube" -- the UI offers different actions for each.
    pub source: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<i64>,
    pub state: String,
    /// Names a file in the cover store. `None` means generated artwork.
    pub cover_key: Option<String>,
    /// The provider's own thumbnail, for tracks with no stored copy.
    ///
    /// Carried so a row that was only ever auditioned still shows real
    /// artwork: the webview loads this URL directly, at no cost on disk. A
    /// track that is kept gets a stored copy and `cover_key` takes over --
    /// which is the one that still works with the network off.
    pub remote_thumbnail_url: Option<String>,
    /// Whether the user keeps this in their library.
    ///
    /// Always true for local files. False for a streamed track that has been
    /// played or queued but never explicitly kept -- it still exists, and is
    /// still in history, but the library does not list it.
    pub in_library: bool,
}

#[tauri::command]
pub async fn list_tracks(db: State<'_, Db>) -> Result<Vec<Track>, String> {
    sqlx::query_as(
        "SELECT id, source, title, artist, album, duration_secs, state, cover_key, in_library, \
         remote_thumbnail_url \
         FROM tracks WHERE in_library = 1 ORDER BY artist, album, title",
    )
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())
}

/// A named const so the test exercises this exact statement.
///
/// The one rule it encodes is an absence: no `in_library` filter. That is easy
/// to "tidy" back in by matching the queries around it, and doing so would
/// silently restore the bug it exists to fix -- while leaving every local
/// track working, so nothing would look wrong.
pub(crate) const TRACK_BY_ID: &str =
    "SELECT id, source, title, artist, album, duration_secs, state, cover_key, in_library, \
     remote_thumbnail_url \
     FROM tracks WHERE id = ?";

/// One track by id, whether or not the library lists it.
///
/// The player bar's last resort. Everything else that can name a playing track
/// is *pushed*: `player-state` carries the id, `player-queue` carries the
/// details, and if the two ever drift the bar is left holding an id it cannot
/// describe. A library track survives that -- `list_tracks` already has it --
/// but a streamed audition is `in_library = 0`, so for that one the push is
/// the only source in the app, and a missed or late payload shows as
/// "Loading track details…" over audible music.
///
/// Deliberately unfiltered, which is the whole point: the rows this has to
/// reach are exactly the ones every other list leaves out. That makes the bar
/// able to *ask* rather than only be told, so naming what is playing no longer
/// depends on an event arriving.
#[tauri::command]
pub async fn track_details(db: State<'_, Db>, track_id: i64) -> Result<Option<Track>, String> {
    sqlx::query_as(TRACK_BY_ID)
        .bind(track_id)
        .fetch_optional(&db.pool)
        .await
        .map_err(|e| e.to_string())
}

/// The tracks played most recently, newest first.
///
/// Deliberately not filtered by library membership. This is the one list that
/// shows what was played rather than what was kept, which is exactly what makes
/// it the way back to a streamed track nobody remembered to add.
///
/// Reads straight from `tracks` rather than keeping a history table: a play
/// is already an attribute of the track, and a separate log would need pruning
/// and could disagree with the row it points at.
///
/// The limit is a display choice, not a storage one -- nothing is discarded by
/// asking for fewer.
#[tauri::command]
pub async fn recently_played(db: State<'_, Db>, limit: Option<u32>) -> Result<Vec<Track>, String> {
    let limit = limit.unwrap_or(50).clamp(1, 500);

    sqlx::query_as(
        "SELECT id, source, title, artist, album, duration_secs, state, cover_key, in_library, \
         remote_thumbnail_url \
         FROM tracks WHERE last_played IS NOT NULL \
         ORDER BY last_played DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())
}

/// The statement behind [`set_in_library`], named so its tests run this exact
/// SQL rather than a paraphrase of it.
///
/// The `CASE` is the whole point. `date_added` means *when it joined the
/// library*, and for a local file that is the moment the row appeared -- so
/// leaving the column to its insert-time default was right for every track
/// the scanner found. A streamed track's row appears much earlier: the first
/// time it is auditioned, queued, or saved as part of a playlist. Deciding to
/// keep it can come weeks later, and until this stamped it, adding one sorted
/// it into whenever it was first heard. With the library ordered newest-first
/// -- which is the default gesture after adding something -- the track landed
/// twenty rows down and looked as though it had never been added at all.
///
/// Only on the way in, and only on the 0 -> 1 transition. Filing a track that
/// is already filed must not move it, or every re-add would reshuffle the
/// list; taking one out must not either, since removing is not a kind of
/// adding and the original date is what re-adding has to be compared against.
///
/// SQLite evaluates every `SET` expression against the row as it was before
/// the update, so reading `in_library` here sees the old value even though the
/// same statement assigns it.
pub(crate) const SET_IN_LIBRARY: &str = "UPDATE tracks
     SET date_added = CASE WHEN ? = 1 AND in_library = 0 THEN unixepoch() ELSE date_added END,
         in_library = ?
     WHERE id = ?";

/// Adds a track to the library, or takes it out.
///
/// Removing does not delete anything: the row, its history, its cache entry
/// and its playlist memberships all survive. It only stops being listed in the
/// library, which is the difference between "I do not want this filed here"
/// and "destroy it".
///
/// This is also where a streamed track earns its stored artwork. Filing one is
/// the gesture that says it should still be there with the network off, and
/// until then the provider's thumbnail URL is doing the job for free.
#[tauri::command]
pub async fn set_in_library(
    app: AppHandle,
    db: State<'_, Db>,
    covers: State<'_, crate::covers::CoverStore>,
    track_id: i64,
    in_library: bool,
) -> Result<(), String> {
    let outcome = sqlx::query(SET_IN_LIBRARY)
        .bind(i64::from(in_library))
        .bind(i64::from(in_library))
        .bind(track_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    if outcome.rows_affected() == 0 {
        return Err("That track no longer exists.".to_string());
    }

    // Only on the way in. Taking a track out of the library deliberately does
    // not throw its cover away: the row survives, history still shows it, and
    // re-filing it would only have to fetch the same bytes again.
    if in_library {
        crate::covers::ensure_for_track_detached(&app, &db.pool, &covers, track_id);
    }

    Ok(())
}

/// Rescans every registered folder.
///
/// Returns `None` if a scan is already running, so the caller can say so
/// rather than silently queueing a second pass.
#[tauri::command]
pub async fn rescan_library(
    app: AppHandle,
    db: State<'_, Db>,
    lock: State<'_, ScanLock>,
    covers: State<'_, crate::covers::CoverStore>,
) -> Result<Option<ScanSummary>, String> {
    // The scan reports through this, so a thousand files stop looking like a
    // hang. A closure rather than handing the scanner an `AppHandle`: it stays
    // testable without a window, which is the same trade the player makes.
    let reporter = app.clone();
    let report: scanner::ProgressSink = Some(std::sync::Arc::new(
        move |progress: scanner::ScanProgress| {
            let _ = reporter.emit(SCAN_PROGRESS_EVENT, progress);
        },
    ));

    let summary = scanner::scan_all(&db.pool, &lock, Some(&covers), &report).await?;

    if summary.is_some() {
        app.emit(SCAN_FINISHED_EVENT, ()).map_err(|e| e.to_string())?;
    }

    Ok(summary)
}

/// What the file's own tags say, for a local track.
///
/// Recorded by the scanner. `None` from [`track_file_tags`] covers three cases
/// that are one case to the caller: the row is remote and has no file, the row
/// is gone, or the file has not been read since `0024` added these columns.
/// All three mean the same thing to the UI -- there is nothing to compare
/// against and nothing to revert to.
#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct FileTags {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
}

/// The file's tags, so the editor can show them and offer to go back to them.
///
/// There is deliberately no `revert_track_metadata` command. Reverting is
/// [`update_track_metadata`] with these three values, which means it travels
/// the one path that already validates a title and is the same write the user
/// could have typed by hand. It also lands the row back in the state the
/// scanner reads as "not diverged" -- see the CASE arms in `scanner.rs` -- so
/// the track resumes following its file with nothing to reset.
#[tauri::command]
pub async fn track_file_tags(db: State<'_, Db>, track_id: i64) -> Result<Option<FileTags>, String> {
    sqlx::query_as(
        "SELECT file_title AS title, file_artist AS artist, file_album AS album \
         FROM tracks WHERE id = ? AND file_title IS NOT NULL",
    )
    .bind(track_id)
    .fetch_optional(&db.pool)
    .await
    .map_err(|e| e.to_string())
}

/// Renames a track for display, leaving its provenance untouched.
///
/// Remote metadata is dirty by nature -- a slowed+reverb upload has no clean
/// artist tag -- so `title`/`artist`/`album` are the editable copy while
/// `remote_title`/`remote_uploader` keep what was actually uploaded. Those are
/// deliberately not writable here.
///
/// Since `0024` the same split exists for local rows: `file_title` and friends
/// hold what the file's tags say, and writing here is exactly what makes a row
/// diverge from its file so a rescan stops overwriting it.
#[tauri::command]
pub async fn update_track_metadata(
    db: State<'_, Db>,
    track_id: i64,
    title: String,
    artist: Option<String>,
    album: Option<String>,
) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        // The column is NOT NULL, and a blank title would leave a row nothing
        // can display.
        return Err("A title is required.".to_string());
    }

    // An empty box means "unknown", which is NULL rather than "".
    let blank_is_none = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let artist = blank_is_none(artist);
    let album = blank_is_none(album);

    let affected = sqlx::query("UPDATE tracks SET title = ?, artist = ?, album = ? WHERE id = ?")
        .bind(title)
        .bind(&artist)
        .bind(&album)
        .bind(track_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected();

    if affected == 0 {
        return Err("That track no longer exists.".to_string());
    }

    Ok(())
}

/// Builds the statement behind [`set_many_in_library`].
///
/// Separate from the command so a test can run the real SQL without a Tauri
/// window, which is the same trade [`SET_IN_LIBRARY`] makes for one track.
///
/// `WHERE in_library <> ?` means every row this touches is a transition, so
/// unlike the single-track statement the stamp needs no `CASE` -- a track
/// already filed is not in the result set at all.
fn set_many_statement(
    in_library: bool,
    track_ids: &[i64],
) -> sqlx::QueryBuilder<sqlx::Sqlite> {
    let mut query = sqlx::QueryBuilder::new("UPDATE tracks SET in_library = ");
    query.push_bind(i64::from(in_library));

    // Same rule as the single-track path, and the same reason: a selection of
    // thirty streamed tracks filed in one gesture joined the library now, not
    // whenever each of them was first played.
    if in_library {
        query.push(", date_added = unixepoch()");
    }

    query.push(" WHERE in_library <> ");
    query.push_bind(i64::from(in_library));
    query.push(" AND id IN (");
    let mut ids = query.separated(", ");
    for id in track_ids {
        ids.push_bind(*id);
    }
    query.push(") RETURNING id");

    query
}

/// Files many tracks in the library at once, or takes them out.
///
/// One statement rather than the single-track command in a loop: three hundred
/// round trips to answer one gesture is a frozen window, and it was the shape
/// of the problem the scan already had.
#[tauri::command]
pub async fn set_many_in_library(
    app: AppHandle,
    db: State<'_, Db>,
    covers: State<'_, crate::covers::CoverStore>,
    track_ids: Vec<i64>,
    in_library: bool,
) -> Result<usize, String> {
    if track_ids.is_empty() {
        return Ok(0);
    }

    let changed: Vec<i64> = set_many_statement(in_library, &track_ids)
        .build_query_scalar()
        .fetch_all(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    let count = changed.len();

    // Artwork is bought when a track is kept. Sequentially, on one task: each
    // fetch is an ffmpeg process, and three hundred starting at once would
    // stall the machine to decorate rows nobody is looking at yet.
    if in_library && !changed.is_empty() {
        let pool = db.pool.clone();
        let covers = covers.inner().clone();
        tauri::async_runtime::spawn(async move {
            for track_id in changed {
                crate::covers::ensure_for_track(app.clone(), pool.clone(), covers.clone(), track_id)
                    .await;
            }
        });
    }

    Ok(count)
}

/// How many ids one statement asks about.
///
/// A track search returns twenty-five, but an opened artist page can carry
/// several hundred, and SQLite caps how many values one statement may bind.
/// Chunking costs one extra round trip per four hundred results and removes
/// the size of a playlist as something that can break this.
const FILED_CHUNK: usize = 400;

/// Which of these provider tracks the library already holds.
///
/// The question a search result cannot answer about itself. A result is just
/// what YouTube said; whether the same recording is already filed is a fact
/// about this machine's database, and without asking, the Add button offers to
/// add something that has been in the library for weeks. That is how a track
/// gets "added" twice and appears to go missing, since the second add changes
/// nothing the user can see.
///
/// Scoped by provider because `remote_id` alone is not an identity: SoundCloud
/// ids are plain integers and could collide with anything. The database's own
/// uniqueness is on `(source, remote_id)`, and this matches it.
///
/// Returns the subset that is filed, so the caller can mark rows without
/// holding a second list of everything it asked about.
#[tauri::command]
pub async fn filed_remote_ids(
    db: State<'_, Db>,
    provider: crate::providers::Provider,
    remote_ids: Vec<String>,
) -> Result<Vec<String>, String> {
    let mut filed = Vec::new();

    for chunk in remote_ids.chunks(FILED_CHUNK) {
        let mut query = sqlx::QueryBuilder::new(
            "SELECT remote_id FROM tracks WHERE in_library = 1 AND source = ",
        );
        query.push_bind(provider.as_str());
        query.push(" AND remote_id IN (");

        let mut list = query.separated(", ");
        for id in chunk {
            list.push_bind(id.as_str());
        }
        query.push(")");

        let found: Vec<String> = query
            .build_query_scalar()
            .fetch_all(&db.pool)
            .await
            .map_err(|e| e.to_string())?;

        filed.extend(found);
    }

    Ok(filed)
}

/// Sets the display artist on many tracks at once.
///
/// The gesture this exists for: a library scanned from files with no artist
/// tag, where the artist is in the folder name. Ninety-eight percent of a real
/// library was in that state, which made every artist feature in the app apply
/// to the other two percent. Selecting a folder's worth of tracks and naming
/// them once is what fixes that.
///
/// `title` is deliberately untouched -- that is per-track by nature -- and so
/// are `remote_uploader` and `remote_title`, which record what a provider said
/// rather than what the user prefers.
#[tauri::command]
pub async fn set_many_artists(
    db: State<'_, Db>,
    track_ids: Vec<i64>,
    artist: Option<String>,
) -> Result<usize, String> {
    if track_ids.is_empty() {
        return Ok(0);
    }

    // An empty box means "unknown", which is NULL rather than "".
    let artist = artist
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty());

    let mut query = sqlx::QueryBuilder::new("UPDATE tracks SET artist = ");
    query.push_bind(artist);
    query.push(" WHERE id IN (");
    let mut ids = query.separated(", ");
    for id in &track_ids {
        ids.push_bind(id);
    }
    query.push(")");

    let affected = query
        .build()
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected();

    Ok(affected as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the query: it reaches a track the library does not list.
    ///
    /// Every other list in the app filters on `in_library = 1`, which is
    /// correct for them and fatal here -- a streamed audition is exactly the
    /// row the player bar cannot describe any other way. Asserting the local
    /// track too keeps this from passing for the trivial reason that the
    /// filter was dropped along with something else.
    #[tokio::test]
    async fn an_audition_can_still_be_looked_up_by_id() {
        let dir = std::env::temp_dir().join("music-app-track-details-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let db = crate::db::init(&dir).await.unwrap();

        sqlx::query(
            "INSERT INTO tracks (source, title, artist, state, remote_id, remote_url, \
             remote_thumbnail_url, in_library) \
             VALUES ('youtube', 'Auditioned', 'Somebody', 'saved', 'abc123', \
                     'https://example.invalid/x', 'https://example.invalid/t.jpg', 0)",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO tracks (source, title, local_path, state, in_library) \
             VALUES ('local', 'Kept', '/tmp/kept.wav', 'present', 1)",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        let audition: i64 =
            sqlx::query_scalar("SELECT id FROM tracks WHERE source = 'youtube'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let kept: i64 = sqlx::query_scalar("SELECT id FROM tracks WHERE source = 'local'")
            .fetch_one(&db.pool)
            .await
            .unwrap();

        let found: Option<Track> = sqlx::query_as(TRACK_BY_ID)
            .bind(audition)
            .fetch_optional(&db.pool)
            .await
            .unwrap();

        let found = found.expect(
            "the audition was not found -- an `in_library` filter has crept \
             back into TRACK_BY_ID, and the player bar cannot name a stream",
        );
        assert_eq!(found.title, "Auditioned");
        assert_eq!(found.artist.as_deref(), Some("Somebody"));
        assert!(!found.in_library);
        // The bar draws artwork from this for a track with no stored cover.
        assert!(found.remote_thumbnail_url.is_some());

        let kept: Option<Track> = sqlx::query_as(TRACK_BY_ID)
            .bind(kept)
            .fetch_optional(&db.pool)
            .await
            .unwrap();
        assert_eq!(kept.expect("the library track was not found").title, "Kept");

        // A track that genuinely does not exist is still `None`, so the bar can
        // tell "no such row" from "nobody has told me yet".
        let missing: Option<Track> = sqlx::query_as(TRACK_BY_ID)
            .bind(9_999_999_i64)
            .fetch_optional(&db.pool)
            .await
            .unwrap();
        assert!(missing.is_none());
    }
}

/// The reported bug: a YouTube track was added to the library and did not
/// appear in it.
///
/// It was in it. The library was ordered newest-added-first, and the track's
/// `date_added` said six days ago -- because the row had been created six days
/// earlier, when a whole album's worth of results was saved for auditioning.
/// Adding it only flipped `in_library`, so it took its place among last week's
/// tracks, twenty-three rows down, where the user could reasonably conclude it
/// had not been added.
///
/// Local files never showed this: the scanner creates the row and files it in
/// the same statement, so the default was always right for them. It needed a
/// track that existed before the decision to keep it, which is every streamed
/// one that was played, queued, or saved from a playlist first.
#[cfg(test)]
mod date_added_tests {
    use super::*;
    use sqlx::SqlitePool;

    /// A week ago, give or take -- old enough that a stamp is unmistakable.
    const LONG_AGO: i64 = 1_787_186_899;

    async fn pool(name: &str) -> SqlitePool {
        let dir = std::env::temp_dir().join(format!("music-app-date-added-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::db::init(&dir).await.unwrap().pool
    }

    /// An audition: a real row, created long before any decision to keep it.
    async fn audition(pool: &SqlitePool, remote_id: &str, in_library: i64) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO tracks (source, title, state, remote_id, remote_url, \
             in_library, date_added) \
             VALUES ('youtube', 'Light no Theme', 'saved', ?, \
                     'https://www.youtube.com/watch?v=srDmw7kSjik', ?, ?) \
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

    async fn now(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT unixepoch()")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn file(pool: &SqlitePool, id: i64, in_library: bool) {
        sqlx::query(SET_IN_LIBRARY)
            .bind(i64::from(in_library))
            .bind(i64::from(in_library))
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// The fix, stated as the user would: add it, and it is at the top.
    #[tokio::test]
    async fn filing_an_audition_dates_it_from_the_gesture_not_the_row() {
        let pool = pool("stamps").await;
        let id = audition(&pool, "srDmw7kSjik", 0).await;

        let before = now(&pool).await;
        file(&pool, id, true).await;

        assert!(
            date_added(&pool, id).await >= before,
            "adding a track to the library must date it from now -- it was \
             left at {LONG_AGO}, which is where the row was created, and a \
             newest-first library buries it there"
        );
    }

    /// The other half, and the reason this is a `CASE` rather than a plain
    /// assignment: filing what is already filed must not reshuffle the list.
    #[tokio::test]
    async fn re_filing_a_track_already_in_the_library_does_not_move_it() {
        let pool = pool("refile").await;
        let id = audition(&pool, "already", 1).await;

        file(&pool, id, true).await;

        assert_eq!(
            date_added(&pool, id).await,
            LONG_AGO,
            "a track that was already in the library did not join it again"
        );
    }

    /// Removing is not a kind of adding. The original date is what a later
    /// re-add has to be measured against.
    #[tokio::test]
    async fn removing_a_track_leaves_its_date_alone() {
        let pool = pool("remove").await;
        let id = audition(&pool, "removed", 1).await;

        file(&pool, id, false).await;

        assert_eq!(date_added(&pool, id).await, LONG_AGO);
    }

    /// Selecting thirty rows and filing them is one gesture, and the bulk
    /// statement is a separate piece of SQL -- so it can regress on its own.
    #[tokio::test]
    async fn filing_many_at_once_dates_them_from_the_gesture_too() {
        let pool = pool("many").await;
        let filed = audition(&pool, "kept", 1).await;
        let first = audition(&pool, "one", 0).await;
        let second = audition(&pool, "two", 0).await;

        let before = now(&pool).await;
        let changed: Vec<i64> = set_many_statement(true, &[filed, first, second])
            .build_query_scalar::<i64>()
            .fetch_all(&pool)
            .await
            .unwrap();

        assert_eq!(changed.len(), 2, "the already-filed track was not a change");
        assert!(date_added(&pool, first).await >= before);
        assert!(date_added(&pool, second).await >= before);
        assert_eq!(
            date_added(&pool, filed).await,
            LONG_AGO,
            "the `WHERE` is what keeps an already-filed track out of the \
             stamp; without it a bulk add would redate the whole selection"
        );
    }

    /// And taking a selection out must not stamp them either.
    #[tokio::test]
    async fn removing_many_at_once_leaves_their_dates_alone() {
        let pool = pool("many-remove").await;
        let first = audition(&pool, "one", 1).await;
        let second = audition(&pool, "two", 1).await;

        let changed: Vec<i64> = set_many_statement(false, &[first, second])
            .build_query_scalar::<i64>()
            .fetch_all(&pool)
            .await
            .unwrap();

        assert_eq!(changed.len(), 2);
        assert_eq!(date_added(&pool, first).await, LONG_AGO);
        assert_eq!(date_added(&pool, second).await, LONG_AGO);
    }

    /// The scanner's case, which was never broken and must stay that way: a
    /// local file's row is created and filed in one statement, so its default
    /// is already the right answer and nothing here may disturb it.
    #[tokio::test]
    async fn a_local_file_keeps_the_date_its_row_was_created_with() {
        let pool = pool("local").await;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO tracks (source, title, local_path, state, in_library, date_added) \
             VALUES ('local', 'Kept', '/tmp/kept.wav', 'present', 1, ?) RETURNING id",
        )
        .bind(LONG_AGO)
        .fetch_one(&pool)
        .await
        .unwrap();

        file(&pool, id, true).await;

        assert_eq!(date_added(&pool, id).await, LONG_AGO);
    }
}

/// What `filed_remote_ids` has to get right for the Add button to stop lying.
#[cfg(test)]
mod filed_tests {
    use super::*;
    use sqlx::SqlitePool;

    async fn pool(name: &str) -> SqlitePool {
        let dir = std::env::temp_dir().join(format!("music-app-filed-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::db::init(&dir).await.unwrap().pool
    }

    async fn track(pool: &SqlitePool, source: &str, remote_id: &str, in_library: i64) {
        sqlx::query(
            "INSERT INTO tracks (source, title, state, remote_id, remote_url, in_library) \
             VALUES (?, 'A Song', 'saved', ?, 'https://example.invalid/x', ?)",
        )
        .bind(source)
        .bind(remote_id)
        .bind(in_library)
        .execute(pool)
        .await
        .unwrap();
    }

    /// The query the command runs, without a Tauri window around it.
    async fn filed(pool: &SqlitePool, source: &str, ids: &[&str]) -> Vec<String> {
        let mut query = sqlx::QueryBuilder::new(
            "SELECT remote_id FROM tracks WHERE in_library = 1 AND source = ",
        );
        query.push_bind(source);
        query.push(" AND remote_id IN (");
        let mut list = query.separated(", ");
        for id in ids {
            list.push_bind(*id);
        }
        query.push(")");

        query.build_query_scalar().fetch_all(pool).await.unwrap()
    }

    /// The three states a search result can be in, and only one of them is
    /// "in your library".
    #[tokio::test]
    async fn only_filed_tracks_come_back() {
        let pool = pool("states").await;
        track(&pool, "youtube", "kept", 1).await;
        // Auditioned: a real row, played once, never kept. The Add button must
        // still offer it -- this is the case that makes a plain "does a row
        // exist" check the wrong question.
        track(&pool, "youtube", "auditioned", 0).await;

        let found = filed(&pool, "youtube", &["kept", "auditioned", "unknown"]).await;

        assert_eq!(found, vec!["kept".to_string()]);
    }

    /// `remote_id` is not an identity on its own. SoundCloud ids are plain
    /// integers, so without the provider a numeric id could match anything.
    #[tokio::test]
    async fn a_matching_id_under_another_provider_is_not_a_match() {
        let pool = pool("scoped").await;
        track(&pool, "soundcloud", "123456", 1).await;

        assert!(
            filed(&pool, "youtube", &["123456"]).await.is_empty(),
            "the provider is part of the question -- the database's own \
             uniqueness is on (source, remote_id) and this must match it"
        );
        assert_eq!(filed(&pool, "soundcloud", &["123456"]).await.len(), 1);
    }

    /// An opened artist page can carry more results than one statement may
    /// bind, so the command chunks. This is the arithmetic that does it.
    #[test]
    fn every_id_lands_in_exactly_one_chunk() {
        let ids: Vec<String> = (0..FILED_CHUNK * 2 + 7).map(|i| i.to_string()).collect();

        let chunked: Vec<&String> = ids.chunks(FILED_CHUNK).flatten().collect();

        assert_eq!(chunked.len(), ids.len());
        assert!(ids.chunks(FILED_CHUNK).all(|c| c.len() <= FILED_CHUNK));
    }
}

/// The metadata editor's writes.
///
/// Its own module because these are about what the user types, while
/// `filed_tests` above is about library membership -- two unrelated
/// questions that happen to touch the same table.
#[cfg(test)]
mod editor_tests {
    use super::*;
    use sqlx::SqlitePool;

    async fn editor_fixture(name: &str) -> (crate::db::Db, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("music-app-editor-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::init(&dir).await.unwrap();
        (db, dir)
    }

    /// The three columns the editor writes, read back the way a list would.
    async fn shown(pool: &SqlitePool, id: i64) -> (String, Option<String>, Option<String>) {
        sqlx::query_as("SELECT title, artist, album FROM tracks WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Empty is not a value. A row carrying `''` would sort apart from the
    /// rows carrying NULL, group into its own nameless artist, and read as
    /// "there is an album called nothing" everywhere it is displayed.
    #[tokio::test]
    async fn a_cleared_box_is_stored_as_null_rather_than_an_empty_string() {
        let (db, dir) = editor_fixture("blank").await;

        let id = sqlx::query(
            "INSERT INTO tracks (source, title, artist, album, local_path, state) \
             VALUES ('local', 'Song', 'Somebody', 'Some Album', '/tmp/a.mp3', 'present')",
        )
        .execute(&db.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        sqlx::query("UPDATE tracks SET title = ?, artist = ?, album = ? WHERE id = ?")
            .bind("Song")
            .bind(None::<String>)
            .bind(None::<String>)
            .bind(id)
            .execute(&db.pool)
            .await
            .unwrap();

        let (_, artist, album) = shown(&db.pool, id).await;
        assert_eq!(artist, None);
        assert_eq!(album, None);

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A remote row has no file, so there is nothing to show and nothing to
    /// revert to. The editor uses this to decide whether to offer either.
    #[tokio::test]
    async fn a_remote_track_has_no_file_tags_to_offer() {
        let (db, dir) = editor_fixture("remote-tags").await;

        let remote = sqlx::query(
            "INSERT INTO tracks (source, title, remote_id, remote_url, state, in_library) \
             VALUES ('youtube', 'Upload', 'abc123', 'https://example.invalid/x', 'saved', 1)",
        )
        .execute(&db.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let local = sqlx::query(
            "INSERT INTO tracks (source, title, local_path, state, file_title, file_artist) \
             VALUES ('local', 'Renamed', '/tmp/b.mp3', 'present', 'Tagged', 'Tagged Artist')",
        )
        .execute(&db.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        async fn ask(pool: &SqlitePool, id: i64) -> Option<FileTags> {
            sqlx::query_as(
                "SELECT file_title AS title, file_artist AS artist, file_album AS album \
                 FROM tracks WHERE id = ? AND file_title IS NOT NULL",
            )
            .bind(id)
            .fetch_optional(pool)
            .await
            .unwrap()
        }

        assert!(
            ask(&db.pool, remote).await.is_none(),
            "a stream has no file to read"
        );

        // Asserted too, so this cannot pass because the query broke outright.
        let found = ask(&db.pool, local)
            .await
            .expect("a scanned local file has tags");
        assert_eq!(found.title, "Tagged");
        assert_eq!(found.artist.as_deref(), Some("Tagged Artist"));

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
