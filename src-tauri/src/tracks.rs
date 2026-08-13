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
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<i64>,
    pub state: String,
}

#[tauri::command]
pub async fn list_tracks(db: State<'_, Db>) -> Result<Vec<Track>, String> {
    sqlx::query_as(
        "SELECT id, title, artist, album, duration_secs, state \
         FROM tracks ORDER BY artist, album, title",
    )
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
