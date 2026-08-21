use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::Accessor;
use serde::Serialize;
use sqlx::{Row, SqlitePool};
use walkdir::WalkDir;

use crate::covers::CoverStore;

/// Extensions we consider audio. Compared lowercased -- `.MP3` is common from
/// older rippers.
const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "m4a", "ogg", "opus", "wav"];

/// Rows written per transaction. Committing in batches keeps the WAL bounded
/// and means an interrupted scan keeps the work it already did -- the next scan
/// resumes cheaply because unchanged files are skipped by mtime.
const BATCH_SIZE: usize = 500;

/// Longest a single file may take to read before the scan gives up on it.
///
/// Measured on a real library: a 245 MB file with an `.mp3` extension that is
/// not an MP3 took **341 seconds** for lofty to reject. It has no recognisable
/// header, so the parser searches the entire file and then fails anyway. Four
/// such files turned a scan of a thousand tracks into twenty minutes of what
/// looked, fairly, like a hang.
///
/// The budget is on *time* rather than size, because size is a poor proxy: a
/// genuine two-hour set is hundreds of megabytes and parses instantly, its
/// header being where a header belongs. Only files that make the parser search
/// are slow, and those are exactly the ones worth abandoning.
///
/// Thirty rather than ten, because the failure modes are not symmetrical. A
/// pathological file costs thirty seconds once; a *legitimate* file wrongly
/// abandoned is silently missing from the library, and -- never having been
/// recorded -- is retried and abandoned again on every scan afterwards. So
/// the budget is set where a false positive is implausible rather than where
/// a true positive is cheapest, and every file it gives up on is named in the
/// summary so a wrong call is visible instead of silent.
///
/// An honest parse reads a header and stops. Even a large file over a slow
/// share is far inside this.
const READ_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// How many abandoned paths the summary carries.
///
/// Enough to act on, not so many that a misconfigured folder ships its whole
/// contents through an event payload.
const MAX_REPORTED_SKIPS: usize = 20;

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
    /// Files the scan gave up on -- see `READ_BUDGET` -- by name.
    ///
    /// Named rather than merely counted, because this is the one outcome
    /// that could be wrong: a legitimate file abandoned for being slow is
    /// missing from the library and nothing would otherwise say so.
    pub skipped_files: Vec<String>,
    /// Folders whose root was unreachable. Their tracks were left untouched.
    pub skipped_folders: Vec<String>,
    /// Cover files deleted because nothing points at them any more.
    pub covers_removed: usize,
}

/// A registered library folder.
struct Folder {
    id: i64,
    path: String,
}

/// What the database already knows about a file, for deciding whether it has
/// to be read again.
struct KnownFile {
    mtime: i64,
    size: i64,
    /// Whether artwork has ever been looked for. Not the same as "has a cover":
    /// a file with no picture is still checked, and must not be re-read for it.
    cover_checked: bool,
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
    /// Key into the cover store, if the file embedded a picture we could read.
    cover_key: Option<String>,
}

/// How far a scan has got.
///
/// A thousand files takes long enough that a spinner alone is indistinguishable
/// from a hang -- the honest complaint was "it is eternally loading". This is
/// what turns that into a number that moves.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    /// The folder being walked, for when several are registered.
    pub folder: String,
    /// The file being read right now.
    ///
    /// Carried so that a scan which stops moving says *which file* it
    /// stopped on. Without it, "it froze at 743" is a number and not a
    /// lead.
    pub file: Option<String>,
    pub done: u64,
    /// Files found in this folder. Known before any are read, because the
    /// walk completes first.
    pub total: u64,
}

/// Where progress is reported.
///
/// A callback rather than an `AppHandle`, so the scanner stays testable
/// without a window -- the same seam, and the same reason, as `PlayerEvents`.
///
/// Shared rather than borrowed, because the slow half of a scan happens on
/// a blocking thread and that is exactly where the reporting has to come
/// from: tag-parsing five hundred files is minutes of work, and a progress
/// count that only moves between batches is indistinguishable from a hang.
pub type ProgressSink = Option<std::sync::Arc<dyn Fn(ScanProgress) + Send + Sync>>;

/// Scans every registered folder and reconciles the `tracks` table.
///
/// Returns `None` if a scan is already in progress.
pub async fn scan_all(
    pool: &SqlitePool,
    lock: &ScanLock,
    covers: Option<&CoverStore>,
    progress: &ProgressSink,
) -> Result<Option<ScanSummary>, String> {
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
        scan_folder(pool, folder, generation, &mut summary, covers, progress).await?;
    }

    // Now, rather than on a timer: the set of live keys is a query away at
    // exactly this moment, and nothing else in the app knows when it changed.
    if let Some(covers) = covers {
        summary.covers_removed = sweep_covers(pool, covers).await.unwrap_or(0);
    }

    Ok(Some(summary))
}

/// Deletes cover files no track and no playlist points at any more.
///
/// Failure is not worth surfacing: an orphaned JPEG costs a few kilobytes and
/// the next scan tries again.
pub(crate) async fn sweep_covers(
    pool: &SqlitePool,
    covers: &CoverStore,
) -> Result<usize, String> {
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT cover_key FROM tracks WHERE cover_key IS NOT NULL
         UNION
         SELECT cover_key FROM playlists WHERE cover_key IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(covers.sweep(&keys.into_iter().collect()))
}

async fn scan_folder(
    pool: &SqlitePool,
    folder: &Folder,
    generation: i64,
    summary: &mut ScanSummary,
    covers: Option<&CoverStore>,
    progress: &ProgressSink,
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
    let known: HashMap<String, KnownFile> = sqlx::query(
        "SELECT local_path, file_mtime, file_size, cover_checked \
         FROM tracks WHERE folder_id = ?",
    )
    .bind(folder.id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?
    .into_iter()
    .filter_map(|row| {
        let path: Option<String> = row.get("local_path");
        Some((
            path?,
            KnownFile {
                mtime: row.get("file_mtime"),
                size: row.get("file_size"),
                cover_checked: row.get::<i64, _>("cover_checked") != 0,
            },
        ))
    })
    .collect();

    let root = folder.path.clone();
    let (entries, walk_errors) = tauri::async_runtime::spawn_blocking(move || walk(&root))
        .await
        .map_err(|e| e.to_string())?;

    // The walk is finished, so the total is known. Taken here, before the
    // vector is consumed below.
    let total = entries.len() as u64;

    summary.scanned += total;
    summary.errors += walk_errors;

    // Split by what the filesystem says, before touching the database.
    let mut unchanged = Vec::new();
    let mut needs_parse = Vec::new();
    for entry in entries {
        match known.get(&entry.path) {
            // Unchanged on disk *and* already examined for artwork. The second
            // half is what lets a library that predates cover art pick it up on
            // one rescan without re-reading everything on every rescan after.
            Some(known)
                if known.mtime == entry.mtime
                    && known.size == entry.size
                    && (known.cover_checked || covers.is_none()) =>
            {
                unchanged.push(entry)
            }
            _ => needs_parse.push(entry),
        }
    }

    // Said before any file is read: the walk itself is the slowest silent
    // part on a big library, and "0 of 1019" is the first proof it is alive.
    let mut done = 0u64;
    let report = |done: u64, file: Option<String>| {
        if let Some(sink) = progress {
            sink(ScanProgress {
                folder: folder.path.clone(),
                file,
                done,
                total,
            });
        }
    };
    report(0, None);

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

        done += batch.len() as u64;
        report(done, None);
    }

    for batch in needs_parse.chunks(BATCH_SIZE) {
        let paths: Vec<String> = batch.iter().map(|e| e.path.clone()).collect();

        // Parse a whole batch on one blocking thread rather than spawning a
        // task per file.
        // Cloned in because the closure outlives this frame. `CoverStore` is
        // one path, so the clone is free.
        let store = covers.cloned();

        // Reported from *inside* the blocking work, file by file. Each of
        // these reads a whole tag and may decode and re-encode embedded
        // artwork, so a batch of five hundred is minutes -- and a count that
        // only moved between batches was reported, fairly, as the app being
        // frozen.
        let sink = progress.clone();
        let folder_path = folder.path.clone();
        let start = done;
        let parsed = tauri::async_runtime::spawn_blocking(move || {
            paths
                .into_iter()
                .enumerate()
                .map(|(index, p)| {
                    if let Some(sink) = &sink {
                        sink(ScanProgress {
                            folder: folder_path.clone(),
                            file: Some(p.clone()),
                            done: start + index as u64,
                            total,
                        });
                    }
                    read_metadata_bounded(&p, store.as_ref())
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| e.to_string())?;

        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        for (entry, outcome) in batch.iter().zip(parsed) {
            let metadata = match outcome {
                Read::Parsed(metadata) => metadata,
                Read::Unreadable => {
                    summary.errors += 1;
                    continue;
                }
                Read::Abandoned => {
                    summary.errors += 1;
                    if summary.skipped_files.len() < MAX_REPORTED_SKIPS {
                        summary.skipped_files.push(entry.path.clone());
                    }
                    continue;
                }
            };

            let is_new = !known.contains_key(&entry.path);

            // DO UPDATE deliberately omits date_added, play_count and
            // last_played: a rescan must never reset the user's history.
            //
            // The `WHERE tracks.source = 'local'` guard matters if a downloaded
            // YouTube file ever ends up under a library folder: without it this
            // would set `state='present'` on a YouTube row and claim it as a
            // local track. The schema now rejects that outright, so the guard
            // is what turns a hard error into simply leaving the row alone.
            sqlx::query(
                "INSERT INTO tracks (
                     source, title, artist, album, duration_secs,
                     local_path, file_mtime, file_size, folder_id,
                     last_seen_scan, state, cover_key, cover_checked
                 )
                 VALUES ('local', ?, ?, ?, ?, ?, ?, ?, ?, ?, 'present', ?, ?)
                 ON CONFLICT(local_path) DO UPDATE SET
                     title          = excluded.title,
                     artist         = excluded.artist,
                     album          = excluded.album,
                     duration_secs  = excluded.duration_secs,
                     -- COALESCE, not a plain assignment: a rescan that cannot
                     -- read the artwork this time (no store, unreadable image)
                     -- must leave the working cover alone rather than clear it.
                     cover_key      = COALESCE(excluded.cover_key, tracks.cover_key),
                     -- Sticky: once a file has been examined it stays examined,
                     -- so a later scan without a store cannot undo the work.
                     cover_checked  = MAX(excluded.cover_checked, tracks.cover_checked),
                     file_mtime     = excluded.file_mtime,
                     file_size      = excluded.file_size,
                     folder_id      = excluded.folder_id,
                     last_seen_scan = excluded.last_seen_scan,
                     state          = 'present'
                 WHERE tracks.source = 'local'",
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
            .bind(&metadata.cover_key)
            .bind(i64::from(covers.is_some()))
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

        // The slow half: these are the files whose tags had to be read, so
        // this is where a long scan actually spends its time and where the
        // number needs to keep moving.
        done += batch.len() as u64;
        report(done, None);
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
/// Reads a file's tags, giving up if it takes too long. See [`READ_BUDGET`].
///
/// The worker is not cancellable -- lofty offers no way to interrupt a parse --
/// so an abandoned one is left to finish by itself. It costs a thread and some
/// background reading, and it ends without help. That is the price of the scan
/// staying answerable, and it is much smaller than the alternative.
fn read_metadata_bounded(path: &str, covers: Option<&CoverStore>) -> Read {
    let (tx, rx) = std::sync::mpsc::channel();
    let owned = path.to_string();
    let store = covers.cloned();

    // Detached on purpose: nothing joins it, and it must not hold the scan up
    // once the budget has passed.
    std::thread::Builder::new()
        .name("scan-read".to_string())
        .spawn(move || {
            let _ = tx.send(read_metadata(&owned, store.as_ref()));
        })
        .map_err(|_| ())
        .ok();

    match rx.recv_timeout(READ_BUDGET) {
        Ok(Some(metadata)) => Read::Parsed(metadata),
        Ok(None) => Read::Unreadable,
        Err(_) => {
            eprintln!("scan: gave up reading {path} after {READ_BUDGET:?}");
            Read::Abandoned
        }
    }
}

/// What one attempt at reading a file produced.
enum Read {
    Parsed(Metadata),
    /// Read to the end and made no sense of it.
    Unreadable,
    /// Gave up on it. Distinct from unreadable because this is the verdict
    /// that could be wrong, and the only one worth naming to the user.
    Abandoned,
}

fn read_metadata(path: &str, covers: Option<&CoverStore>) -> Option<Metadata> {
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
        cover_key: covers.and_then(|store| extract_cover(tag, tagged.file_type(), path, store)),
        artist: tag.and_then(|t| t.artist()).and_then(non_empty),
        album: tag.and_then(|t| t.album()).and_then(non_empty),
        // lofty reports a zero duration when it cannot determine one; storing
        // NULL keeps that distinguishable from a genuinely empty file.
        duration_secs: (duration > 0).then_some(duration as i64),
    })
}

/// Stores the file's embedded artwork, returning its key.
///
/// The picture bytes are already in memory -- `read_from_path` parsed the whole
/// tag to get the title -- so the only cost here is the store's, and that is
/// skipped entirely for a cover already seen. An album therefore pays one
/// decode for twelve tracks.
///
/// A file with no picture, or one the decoder rejects, simply has no cover.
/// Failing a scan over unreadable artwork would be losing a library to a
/// broken JPEG.
fn extract_cover(
    tag: Option<&lofty::tag::Tag>,
    file_type: lofty::file::FileType,
    path: &str,
    covers: &CoverStore,
) -> Option<String> {
    if let Some(picture) = tag.and_then(preferred_picture) {
        return store_cover(picture.data(), covers);
    }

    // MP4 keeps its artwork somewhere the unified tag cannot reach; see
    // `mp4_cover`. Only reached when the tag yielded nothing, so files that do
    // expose their picture normally never pay for the second read.
    if file_type == lofty::file::FileType::Mp4 {
        if let Some(bytes) = mp4_cover(path) {
            return store_cover(&bytes, covers);
        }
    }

    None
}

/// The picture to use, out of however many the tag carries.
///
/// Prefer the front cover when the file says which is which. Tags routinely
/// carry a back cover, a disc scan and an artist photo alongside it, and any of
/// those would be a strange thing to show as the track's artwork.
fn preferred_picture(tag: &lofty::tag::Tag) -> Option<&lofty::picture::Picture> {
    use lofty::picture::PictureType;

    let pictures = tag.pictures();
    pictures
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures.first())
}

/// Reads an MP4's cover art from the concrete tag.
///
/// `read_from_path` returns lofty's *unified* tag, and MP4 artwork does not
/// survive the conversion into it: `Ilst::split_tag` looks up an `ItemKey` for
/// each atom before it inspects the atom's data, and `covr` has no `ItemKey`,
/// so the atom is skipped before the picture branch is ever reached. The
/// picture is still in the file and still parsed -- only the unified view drops
/// it -- so reading the concrete `Ilst` recovers it.
///
/// This costs a second parse of the file. It is confined to MP4s whose unified
/// tag came back without a picture, and `cover_checked` means any one file pays
/// it once, ever.
fn mp4_cover(path: &str) -> Option<Vec<u8>> {
    use lofty::config::ParseOptions;
    use lofty::mp4::Mp4File;

    let mut file = std::fs::File::open(path).ok()?;
    let mp4 = Mp4File::read_from(&mut file, ParseOptions::new()).ok()?;

    // Every `covr` picture is typed `Other`, so there is no front cover to
    // prefer here -- the first one is the cover.
    let picture = mp4.ilst()?.pictures()?.next()?;
    Some(picture.data().to_vec())
}

fn store_cover(bytes: &[u8], covers: &CoverStore) -> Option<String> {
    match covers.store(bytes) {
        Ok(key) => Some(key),
        Err(e) => {
            // Expected in a real library, so never loud.
            eprintln!("cover art skipped: {e}");
            None
        }
    }
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

    /// The catch-up pass.
    ///
    /// Cover art shipped after these libraries were built, so every existing
    /// row has `cover_checked = 0` while its file is byte-for-byte unchanged.
    /// Without the flag those files are "unchanged" forever and never gain
    /// artwork -- the feature would only ever work for music added later.
    #[tokio::test]
    async fn files_scanned_before_cover_art_are_examined_once_and_then_left_alone() {
        let (db, music, base) = fixture("cover-catchup").await;
        write_wav(&music.join("Old Song.wav"));

        // A library from before covers existed.
        let first = scan_all(&db.pool, &ScanLock::new(), None, &None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.added, 1);

        let store = CoverStore::new(base.join("covers"));

        // The upgrade. Nothing on disk changed, but the file has never been
        // looked at for artwork, so it must be read again.
        let second = scan_all(&db.pool, &ScanLock::new(), Some(&store), &None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            second.unchanged, 0,
            "an unexamined file must be re-read even though its mtime matches"
        );

        // And exactly once. Re-reading every coverless file on every scan
        // would tax precisely the worst-tagged libraries hardest.
        let third = scan_all(&db.pool, &ScanLock::new(), Some(&store), &None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            third.unchanged, 1,
            "a file already examined must not be read again, even with no art"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The MP4 regression.
    ///
    /// Nearly every file this app downloads is an `.m4a`, and every one of them
    /// lost its artwork silently: lofty parses the `covr` atom correctly, but
    /// the picture does not survive the conversion into the unified tag that
    /// `read_from_path` returns (see `mp4_cover`). Nothing errored -- the
    /// library simply had no covers, which looks exactly like untagged files.
    ///
    /// Built with ffmpeg rather than checked in as a fixture so the atom layout
    /// is a real encoder's, not one we invented to match our own reader.
    #[test]
    fn an_mp4_keeps_its_artwork() {
        let base = std::env::temp_dir().join(format!("music-app-mp4-cover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let art = base.join("art.jpg");
        let song = base.join("song.m4a");

        let jpeg = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=c=red:s=300x300:d=1"])
            .args(["-frames:v", "1"])
            .arg(&art)
            .status();

        match jpeg {
            Ok(status) if status.success() => {}
            _ => {
                eprintln!("SKIP: ffmpeg is not available to build the fixture");
                return;
            }
        }

        let muxed = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=1"])
            .arg("-i")
            .arg(&art)
            .args(["-map", "0:a", "-map", "1:v"])
            .args(["-c:a", "aac", "-c:v", "mjpeg"])
            .args(["-disposition:v", "attached_pic"])
            .arg(&song)
            .status();

        assert!(
            matches!(muxed, Ok(status) if status.success()),
            "the fixture should mux"
        );

        let store = CoverStore::new(base.join("covers"));
        let metadata =
            read_metadata(song.to_str().unwrap(), Some(&store)).expect("the file should parse");

        assert!(
            metadata.cover_key.is_some(),
            "an MP4 with embedded artwork must yield a cover"
        );

        // The key has to name a file that is really there, or every row would
        // point at a broken image.
        let key = metadata.cover_key.unwrap();
        assert!(
            base.join("covers").join(&key).is_file(),
            "the stored cover {key} should exist on disk"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn a_scan_imports_audio_files_and_falls_back_to_the_file_name() {
        let (db, music, base) = fixture("import").await;
        write_wav(&music.join("Some Song.wav"));
        write_wav(&music.join("Another Song.wav"));
        std::fs::write(music.join("cover.jpg"), b"not audio").unwrap();

        let summary = scan_all(&db.pool, &ScanLock::new(), None, &None)
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

        scan_all(&db.pool, &ScanLock::new(), None, &None).await.unwrap().unwrap();
        let second = scan_all(&db.pool, &ScanLock::new(), None, &None)
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

        scan_all(&db.pool, &ScanLock::new(), None, &None).await.unwrap().unwrap();
        std::fs::remove_file(&doomed).unwrap();

        let summary = scan_all(&db.pool, &ScanLock::new(), None, &None)
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

        scan_all(&db.pool, &ScanLock::new(), None, &None).await.unwrap().unwrap();
        sqlx::query("UPDATE tracks SET play_count = 42, last_played = 1000")
            .execute(&db.pool)
            .await
            .unwrap();

        // Touch the file so the scan re-reads it and takes the UPSERT path
        // rather than the unchanged shortcut.
        std::fs::write(&file, std::fs::read(&file).unwrap()).unwrap();
        filetime_bump(&file);

        let summary = scan_all(&db.pool, &ScanLock::new(), None, &None)
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

        scan_all(&db.pool, &ScanLock::new(), None, &None).await.unwrap().unwrap();

        // Simulate the drive being unplugged: the root itself disappears.
        std::fs::remove_dir_all(&music).unwrap();

        let summary = scan_all(&db.pool, &ScanLock::new(), None, &None)
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


#[cfg(test)]
mod budget_tests {
    use super::*;

    /// The real file that caused this, if it is still on this machine.
    #[test]
    #[ignore]
    fn the_reported_file_no_longer_stalls_the_scan() {
        let path = r"D:\kiza2\Music\Chill - Nostalgic\Getaway Spa 1.mp3";
        if !std::path::Path::new(path).exists() {
            eprintln!("SKIP: not on this machine");
            return;
        }

        let started = std::time::Instant::now();
        let got = read_metadata_bounded(path, None);
        eprintln!(
            "245MB file: {:?} -> {}",
            started.elapsed(),
            matches!(got, Read::Parsed(_))
        );

        assert!(
            started.elapsed() < READ_BUDGET * 2,
            "it took {:?}, which was 341s before the budget",
            started.elapsed(),
        );
    }

    /// The scan must not be hostage to one file.
    ///
    /// A file that cannot be parsed quickly is skipped and counted, and the
    /// scan carries on. Exercised through the real bounded reader rather than
    /// a copy of its logic, against a file lofty cannot make sense of at all.
    #[test]
    fn an_unparseable_file_is_skipped_rather_than_waited_on() {
        let dir = std::env::temp_dir().join("music-app-scan-budget");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Named like audio, containing nothing of the sort.
        let path = dir.join("not-really.mp3");
        std::fs::write(&path, vec![0x5au8; 512 * 1024]).unwrap();

        let started = std::time::Instant::now();
        let got = read_metadata_bounded(path.to_str().unwrap(), None);

        assert!(
            matches!(got, Read::Abandoned | Read::Unreadable),
            "nothing should have been parsed from it",
        );
        assert!(
            started.elapsed() < READ_BUDGET * 2,
            "the reader must return within its budget, took {:?}",
            started.elapsed(),
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And an honest file is unaffected: the budget must not cost anything in
    /// the case that matters, which is every other file in the library.
    #[test]
    fn an_ordinary_file_still_reads_its_tags() {
        let dir = std::env::temp_dir().join("music-app-scan-budget-ok");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A minimal but genuine WAV, which lofty parses from its header.
        let path = dir.join("tone.wav");
        let samples = 44_100u32;
        let data_len = samples * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&44_100u32.to_le_bytes());
        wav.extend_from_slice(&88_200u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend_from_slice(&vec![0u8; data_len as usize]);
        std::fs::write(&path, wav).unwrap();

        let started = std::time::Instant::now();
        let got = read_metadata_bounded(path.to_str().unwrap(), None);

        assert!(matches!(got, Read::Parsed(_)), "a real file must still be read");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "and quickly, took {:?}",
            started.elapsed(),
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
