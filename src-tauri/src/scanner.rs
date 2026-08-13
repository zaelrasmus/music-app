use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::Accessor;
use serde::Serialize;
use sqlx::{Row, SqlitePool};
use walkdir::WalkDir;

/// Extensions we consider audio. Compared lowercased -- `.MP3` is common from
/// older rippers.
const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "m4a", "ogg", "opus", "wav"];

/// Rows written per transaction. Committing in batches keeps the WAL bounded
/// and means an interrupted scan keeps the work it already did -- the next scan
/// resumes cheaply because unchanged files are skipped by mtime.
const BATCH_SIZE: usize = 500;

/// Guards against overlapping scans. Two concurrent scans would interleave
/// writes and each other's generation numbers, and one could mark the other's
/// in-flight rows as missing.
pub struct ScanLock(AtomicBool);

impl ScanLock {
    pub fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Returns a guard, or `None` if a scan is already running.
    fn try_acquire(&self) -> Option<ScanGuard<'_>> {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ScanGuard(&self.0))
    }
}

/// Releases the scan lock on drop, including on early return or panic.
struct ScanGuard<'a>(&'a AtomicBool);

impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    /// Audio files found on disk across all scanned folders.
    pub scanned: u64,
    pub added: u64,
    pub updated: u64,
    /// Matched by mtime + size, so metadata was not re-read.
    pub unchanged: u64,
    pub marked_missing: u64,
    /// Files that could not be read or parsed. They are skipped, not fatal.
    pub errors: u64,
    /// Folders whose root was unreachable. Their tracks were left untouched.
    pub skipped_folders: Vec<String>,
}

/// A registered library folder.
struct Folder {
    id: i64,
    path: String,
}

/// What `stat` told us about a file during the walk.
struct FileEntry {
    path: String,
    mtime: i64,
    size: i64,
}

/// Tags read from a file, with the filename fallback already applied.
struct Metadata {
    title: String,
    artist: Option<String>,
    album: Option<String>,
    duration_secs: Option<i64>,
}

/// Scans every registered folder and reconciles the `tracks` table.
///
/// Returns `None` if a scan is already in progress.
pub async fn scan_all(pool: &SqlitePool, lock: &ScanLock) -> Result<Option<ScanSummary>, String> {
    let Some(_guard) = lock.try_acquire() else {
        return Ok(None);
    };

    let folders: Vec<Folder> = sqlx::query("SELECT id, path FROM library_folders")
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|row| Folder {
            id: row.get("id"),
            path: row.get("path"),
        })
        .collect();

    // One generation for the whole run, so a folder scanned early is not
    // reconciled against a later folder's number.
    let generation: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(last_seen_scan), 0) + 1 FROM tracks")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;

    let mut summary = ScanSummary::default();

    for folder in &folders {
        scan_folder(pool, folder, generation, &mut summary).await?;
    }

    Ok(Some(summary))
}

async fn scan_folder(
    pool: &SqlitePool,
    folder: &Folder,
    generation: i64,
    summary: &mut ScanSummary,
) -> Result<(), String> {
    // An unreachable root -- unplugged drive, disconnected share -- yields zero
    // entries from WalkDir, which would otherwise look exactly like "every file
    // was deleted" and wipe the folder's tracks to 'missing'. Absence of the
    // root is not evidence about the files under it.
    let root = folder.path.clone();
    let reachable = tauri::async_runtime::spawn_blocking(move || Path::new(&root).is_dir())
        .await
        .map_err(|e| e.to_string())?;

    if !reachable {
        summary.skipped_folders.push(folder.path.clone());
        return Ok(());
    }

    // Load what we already know before walking, so the mtime comparison can
    // decide whether a file needs re-parsing at all. Reading metadata is ~99%
    // of scan cost, so this is what makes a rescan fast.
    let known: HashMap<String, (i64, i64)> =
        sqlx::query("SELECT local_path, file_mtime, file_size FROM tracks WHERE folder_id = ?")
            .bind(folder.id)
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter_map(|row| {
                let path: Option<String> = row.get("local_path");
                Some((path?, (row.get("file_mtime"), row.get("file_size"))))
            })
            .collect();

    let root = folder.path.clone();
    let (entries, walk_errors) = tauri::async_runtime::spawn_blocking(move || walk(&root))
        .await
        .map_err(|e| e.to_string())?;

    summary.scanned += entries.len() as u64;
    summary.errors += walk_errors;

    // Split by what the filesystem says, before touching the database.
    let mut unchanged = Vec::new();
    let mut needs_parse = Vec::new();
    for entry in entries {
        match known.get(&entry.path) {
            Some(&(mtime, size)) if mtime == entry.mtime && size == entry.size => {
                unchanged.push(entry)
            }
            _ => needs_parse.push(entry),
        }
    }

    summary.unchanged += unchanged.len() as u64;

    // Unchanged files still need their generation stamped, or the reconcile
    // step below would conclude they had vanished. Only the lofty parse is
    // skipped, never the write.
    for batch in unchanged.chunks(BATCH_SIZE) {
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        for entry in batch {
            sqlx::query(
                "UPDATE tracks SET last_seen_scan = ?, state = 'present', folder_id = ? \
                 WHERE local_path = ?",
            )
            .bind(generation)
            .bind(folder.id)
            .bind(&entry.path)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        }
        tx.commit().await.map_err(|e| e.to_string())?;
    }

    for batch in needs_parse.chunks(BATCH_SIZE) {
        let paths: Vec<String> = batch.iter().map(|e| e.path.clone()).collect();

        // Parse a whole batch on one blocking thread rather than spawning a
        // task per file.
        let parsed = tauri::async_runtime::spawn_blocking(move || {
            paths.into_iter().map(|p| read_metadata(&p)).collect::<Vec<_>>()
        })
        .await
        .map_err(|e| e.to_string())?;

        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        for (entry, metadata) in batch.iter().zip(parsed) {
            let Some(metadata) = metadata else {
                summary.errors += 1;
                continue;
            };

            let is_new = !known.contains_key(&entry.path);

            // DO UPDATE deliberately omits date_added, play_count and
            // last_played: a rescan must never reset the user's history.
            sqlx::query(
                "INSERT INTO tracks (
                     source, title, artist, album, duration_secs,
                     local_path, file_mtime, file_size, folder_id,
                     last_seen_scan, state
                 )
                 VALUES ('local', ?, ?, ?, ?, ?, ?, ?, ?, ?, 'present')
                 ON CONFLICT(local_path) DO UPDATE SET
                     title          = excluded.title,
                     artist         = excluded.artist,
                     album          = excluded.album,
                     duration_secs  = excluded.duration_secs,
                     file_mtime     = excluded.file_mtime,
                     file_size      = excluded.file_size,
                     folder_id      = excluded.folder_id,
                     last_seen_scan = excluded.last_seen_scan,
                     state          = 'present'",
            )
            .bind(&metadata.title)
            .bind(&metadata.artist)
            .bind(&metadata.album)
            .bind(metadata.duration_secs)
            .bind(&entry.path)
            .bind(entry.mtime)
            .bind(entry.size)
            .bind(folder.id)
            .bind(generation)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;

            if is_new {
                summary.added += 1;
            } else {
                summary.updated += 1;
            }
        }
        tx.commit().await.map_err(|e| e.to_string())?;
    }

    // Anything under this folder we did not touch this run is gone from disk.
    // Marked, never deleted, so playlist entries and play history survive.
    let missing = sqlx::query(
        "UPDATE tracks SET state = 'missing' \
         WHERE folder_id = ? AND source = 'local' \
           AND last_seen_scan != ? AND state != 'missing'",
    )
    .bind(folder.id)
    .bind(generation)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    summary.marked_missing += missing.rows_affected();

    Ok(())
}

/// Walks `root`, returning audio files and a count of unreadable entries.
///
/// Symlinks are not followed (WalkDir's default): a directory linking to its
/// own ancestor would otherwise loop forever.
fn walk(root: &str) -> (Vec<FileEntry>, u64) {
    let mut entries = Vec::new();
    let mut errors = 0;

    for result in WalkDir::new(root) {
        let Ok(entry) = result else {
            // Permission denied on a subtree, or a race with a delete. The
            // files under it are unknown, not absent -- so this must not feed
            // the missing-detection pass.
            errors += 1;
            continue;
        };

        if !entry.file_type().is_file() || !is_audio(entry.path()) {
            continue;
        }

        let Ok(metadata) = entry.metadata() else {
            errors += 1;
            continue;
        };

        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);

        let Some(path) = entry.path().to_str() else {
            errors += 1;
            continue;
        };

        entries.push(FileEntry {
            path: path.to_string(),
            mtime,
            size: metadata.len() as i64,
        });
    }

    (entries, errors)
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str()))
}

/// Reads tags from one file. `None` means the file could not be parsed at all.
///
/// Dirty and absent tags are the norm in a real library, so a missing or
/// blank title falls back to the file name rather than failing.
fn read_metadata(path: &str) -> Option<Metadata> {
    let tagged = lofty::read_from_path(path).ok()?;

    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let title = tag
        .and_then(|t| t.title())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| filename_title(path));

    let duration = tagged.properties().duration().as_secs();

    Some(Metadata {
        title,
        artist: tag.and_then(|t| t.artist()).and_then(non_empty),
        album: tag.and_then(|t| t.album()).and_then(non_empty),
        // lofty reports a zero duration when it cannot determine one; storing
        // NULL keeps that distinguishable from a genuinely empty file.
        duration_secs: (duration > 0).then_some(duration as i64),
    })
}

/// The file name without its extension -- otherwise every untagged track would
/// be titled "Song.mp3".
fn filename_title(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown")
        .to_string()
}

fn non_empty(value: std::borrow::Cow<'_, str>) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_extensions_are_matched_case_insensitively() {
        assert!(is_audio(Path::new("a.mp3")));
        assert!(is_audio(Path::new("a.MP3")));
        assert!(is_audio(Path::new("a.FlAc")));
    }

    #[test]
    fn non_audio_files_are_rejected() {
        assert!(!is_audio(Path::new("cover.jpg")));
        assert!(!is_audio(Path::new("notes.txt")));
        assert!(!is_audio(Path::new("no_extension")));
    }

    #[test]
    fn the_fallback_title_drops_the_extension() {
        assert_eq!(filename_title(r"D:\music\Song Name.mp3"), "Song Name");
    }

    #[test]
    fn blank_tag_values_are_treated_as_absent() {
        assert_eq!(non_empty(std::borrow::Cow::Borrowed("   ")), None);
        assert_eq!(
            non_empty(std::borrow::Cow::Borrowed("  Artist ")),
            Some("Artist".to_string())
        );
    }

    #[test]
    fn a_second_scan_cannot_start_while_one_is_running() {
        let lock = ScanLock::new();
        let first = lock.try_acquire();
        assert!(first.is_some());
        assert!(lock.try_acquire().is_none());

        drop(first);
        assert!(lock.try_acquire().is_some());
    }

    // --- end-to-end pipeline tests -------------------------------------

    /// A minimal but valid PCM WAV file, so lofty has something real to parse.
    /// It carries no tags, which is exactly the filename-fallback case.
    fn write_wav(path: &Path) {
        const SAMPLES: u32 = 4410; // 0.1s at 44100Hz, mono, 16-bit
        let data_len = SAMPLES * 2;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&44100u32.to_le_bytes());
        bytes.extend_from_slice(&88200u32.to_le_bytes()); // byte rate
        bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
        bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend_from_slice(&vec![0u8; data_len as usize]);

        std::fs::write(path, bytes).expect("should write test wav");
    }

    /// Fresh database plus an empty music folder registered in it.
    async fn fixture(name: &str) -> (crate::db::Db, std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("music-app-scan-{name}"));
        let _ = std::fs::remove_dir_all(&base);

        let db_dir = base.join("data");
        let music_dir = base.join("music");
        std::fs::create_dir_all(&music_dir).expect("should create music dir");

        let db = crate::db::init(&db_dir).await.expect("db should init");

        sqlx::query("INSERT INTO library_folders (path, path_key) VALUES (?, ?)")
            .bind(music_dir.to_str().unwrap())
            .bind(music_dir.to_str().unwrap().to_lowercase())
            .execute(&db.pool)
            .await
            .expect("should register folder");

        (db, music_dir, base)
    }

    async fn track_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_scan_imports_audio_files_and_falls_back_to_the_file_name() {
        let (db, music, base) = fixture("import").await;
        write_wav(&music.join("Some Song.wav"));
        write_wav(&music.join("Another Song.wav"));
        std::fs::write(music.join("cover.jpg"), b"not audio").unwrap();

        let summary = scan_all(&db.pool, &ScanLock::new())
            .await
            .expect("scan should succeed")
            .expect("no scan should be in progress");

        assert_eq!(summary.scanned, 2, "the jpg must not be counted");
        assert_eq!(summary.added, 2);
        assert_eq!(summary.errors, 0);

        let titles: Vec<String> =
            sqlx::query_scalar("SELECT title FROM tracks ORDER BY title")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert_eq!(titles, vec!["Another Song", "Some Song"]);

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn rescanning_does_not_duplicate_tracks_or_reread_unchanged_files() {
        let (db, music, base) = fixture("rescan").await;
        write_wav(&music.join("Track.wav"));

        scan_all(&db.pool, &ScanLock::new()).await.unwrap().unwrap();
        let second = scan_all(&db.pool, &ScanLock::new())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(second.added, 0);
        assert_eq!(second.unchanged, 1, "mtime and size matched, so no reparse");
        assert_eq!(second.marked_missing, 0);
        assert_eq!(track_count(&db.pool).await, 1, "no duplicate row");

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn a_deleted_file_is_marked_missing_rather_than_removed() {
        let (db, music, base) = fixture("missing").await;
        let doomed = music.join("Gone.wav");
        write_wav(&doomed);
        write_wav(&music.join("Kept.wav"));

        scan_all(&db.pool, &ScanLock::new()).await.unwrap().unwrap();
        std::fs::remove_file(&doomed).unwrap();

        let summary = scan_all(&db.pool, &ScanLock::new())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(summary.marked_missing, 1);
        assert_eq!(track_count(&db.pool).await, 2, "the row must survive");

        let state: String =
            sqlx::query_scalar("SELECT state FROM tracks WHERE title = 'Gone'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(state, "missing");

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn play_history_survives_a_rescan() {
        let (db, music, base) = fixture("history").await;
        let file = music.join("Played.wav");
        write_wav(&file);

        scan_all(&db.pool, &ScanLock::new()).await.unwrap().unwrap();
        sqlx::query("UPDATE tracks SET play_count = 42, last_played = 1000")
            .execute(&db.pool)
            .await
            .unwrap();

        // Touch the file so the scan re-reads it and takes the UPSERT path
        // rather than the unchanged shortcut.
        std::fs::write(&file, std::fs::read(&file).unwrap()).unwrap();
        filetime_bump(&file);

        let summary = scan_all(&db.pool, &ScanLock::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.updated, 1, "the file should have been reparsed");

        let (count, played): (i64, Option<i64>) =
            sqlx::query_as("SELECT play_count, last_played FROM tracks")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(count, 42, "a rescan must not reset play_count");
        assert_eq!(played, Some(1000));

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Rewrites the file with an extra byte so mtime *and* size both differ,
    /// which is what the scanner compares.
    fn filetime_bump(path: &Path) {
        let mut bytes = std::fs::read(path).unwrap();
        bytes.push(0);
        std::fs::write(path, bytes).unwrap();
    }

    #[tokio::test]
    async fn an_unreachable_folder_is_skipped_without_touching_its_tracks() {
        let (db, music, base) = fixture("unreachable").await;
        write_wav(&music.join("OnTheDrive.wav"));

        scan_all(&db.pool, &ScanLock::new()).await.unwrap().unwrap();

        // Simulate the drive being unplugged: the root itself disappears.
        std::fs::remove_dir_all(&music).unwrap();

        let summary = scan_all(&db.pool, &ScanLock::new())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(summary.skipped_folders.len(), 1);
        assert_eq!(
            summary.marked_missing, 0,
            "an unplugged drive must not wipe the library to 'missing'"
        );

        let state: String = sqlx::query_scalar("SELECT state FROM tracks")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(state, "present");

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&base);
    }
}
