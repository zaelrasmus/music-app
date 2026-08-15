use serde::Serialize;
use sqlx::FromRow;
use tauri::{AppHandle, Emitter, State};

use crate::db::Db;
use crate::scanner::{self, ScanLock, ScanSummary};

/// Emitted when a scan finishes so the frontend can refetch.
pub const SCAN_FINISHED_EVENT: &str = "scan-finished";

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
}

#[tauri::command]
pub async fn list_tracks(db: State<'_, Db>) -> Result<Vec<Track>, String> {
    sqlx::query_as(
        "SELECT id, source, title, artist, album, duration_secs, state \
         FROM tracks ORDER BY artist, album, title",
    )
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())
}

/// The tracks played most recently, newest first.
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
        "SELECT id, source, title, artist, album, duration_secs, state \
         FROM tracks WHERE last_played IS NOT NULL \
         ORDER BY last_played DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())
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
) -> Result<Option<ScanSummary>, String> {
    let summary = scanner::scan_all(&db.pool, &lock).await?;

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
