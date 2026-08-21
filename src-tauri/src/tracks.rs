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
    let outcome = sqlx::query("UPDATE tracks SET in_library = ? WHERE id = ?")
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

/// Renames a track for display, leaving its provenance untouched.
///
/// YouTube metadata is dirty by nature -- a slowed+reverb upload has no clean
/// artist tag -- so `title`/`artist` are the editable copy while
/// `yt_original_title`/`yt_channel` keep what was actually uploaded. Those are
/// deliberately not writable here.
#[tauri::command]
pub async fn update_track_metadata(
    db: State<'_, Db>,
    track_id: i64,
    title: String,
    artist: Option<String>,
) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        // The column is NOT NULL, and a blank title would leave a row nothing
        // can display.
        return Err("A title is required.".to_string());
    }

    // An empty artist box means "unknown", which is NULL rather than "".
    let artist = artist
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty());

    let affected = sqlx::query("UPDATE tracks SET title = ?, artist = ? WHERE id = ?")
        .bind(title)
        .bind(&artist)
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

    let mut query = sqlx::QueryBuilder::new("UPDATE tracks SET in_library = ");
    query.push_bind(i64::from(in_library));
    query.push(" WHERE in_library <> ");
    query.push_bind(i64::from(in_library));
    query.push(" AND id IN (");
    let mut ids = query.separated(", ");
    for id in &track_ids {
        ids.push_bind(id);
    }
    query.push(") RETURNING id");

    let changed: Vec<i64> = query
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
