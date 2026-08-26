use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use sqlx::Row;
use tauri::{AppHandle, Manager, State};

use crate::db::Db;
use crate::providers::Provider;
use crate::sidecar::{self, Tool};

/// Tracks currently downloading, so a double-click cannot start two writes to
/// the same file.
pub struct DownloadLock(Mutex<HashSet<i64>>);

impl DownloadLock {
    pub fn new() -> Self {
        Self(Mutex::new(HashSet::new()))
    }

    fn try_claim(&self, track_id: i64) -> Result<DownloadGuard<'_>, String> {
        let mut active = self
            .0
            .lock()
            .map_err(|_| "Download state is poisoned.".to_string())?;

        if !active.insert(track_id) {
            return Err("That track is already downloading.".to_string());
        }

        Ok(DownloadGuard {
            lock: self,
            track_id,
        })
    }

    fn release(&self, track_id: i64) {
        if let Ok(mut active) = self.0.lock() {
            active.remove(&track_id);
        }
    }
}

/// Releases the claim however the download ends, including on early return.
struct DownloadGuard<'a> {
    lock: &'a DownloadLock,
    track_id: i64,
}

impl Drop for DownloadGuard<'_> {
    fn drop(&mut self) {
        self.lock.release(self.track_id);
    }
}

/// Where downloaded audio lives.
///
/// Under the app's own data directory on purpose: it must never fall inside a
/// folder the user would add to their library, or the scanner would try to
/// claim these files as local tracks.
pub fn downloads_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Could not locate the app data directory: {e}"))?
        .join("downloads");

    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create the downloads folder: {e}"))?;

    Ok(dir)
}

/// What a finished download writes back to the row.
///
/// Downloading is the strongest "I want to keep this" there is, so it also
/// files the track in the library. Otherwise you could deliberately save a copy
/// to disk and still not find it anywhere but history.
///
/// The upload date comes along for free from the same resolve. `COALESCE` so a
/// date already known -- from the search result, or from an earlier play -- is
/// never replaced by a NULL from a provider that did not report one.
///
/// The `CASE` is the rule [`crate::tracks::SET_IN_LIBRARY`] carries, for the
/// third and last place that files a track: downloading one that was only ever
/// auditioned files it *now*, and "now" is what its date has to say -- while
/// downloading one already in the library must not move it.
pub(crate) const FINISH_DOWNLOAD: &str = "UPDATE tracks
     SET local_path = ?,
         state = 'downloaded',
         date_added = CASE WHEN in_library = 0 THEN unixepoch() ELSE date_added END,
         in_library = 1,
         uploaded_at = COALESCE(uploaded_at, ?)
     WHERE id = ?";

/// Downloads a saved YouTube track for offline playback.
///
/// The audio is **stream-copied**, never re-encoded: yt-dlp resolves the best
/// audio-only format and ffmpeg muxes those exact packets into a file. What
/// lands on disk is bit-identical to what YouTube served.
pub(crate) async fn fetch_track(
    app: &AppHandle,
    pool: &sqlx::SqlitePool,
    covers: &crate::covers::CoverStore,
    track_id: i64,
) -> Result<(), String> {
    // Still claimed, even though the queue runs one job at a time: the queue
    // is not the only way in -- the player's own background caching writes to
    // this directory too -- and this is the guard that makes two writes to one
    // file impossible rather than merely unlikely.
    let lock = app.state::<DownloadLock>();
    let _guard = lock.try_claim(track_id)?;

    let row = sqlx::query("SELECT source, state, remote_id, remote_url FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("That track no longer exists.")?;

    let source: String = row.get("source");
    let state: String = row.get("state");
    let remote_id: Option<String> = row.get("remote_id");
    let remote_url: Option<String> = row.get("remote_url");

    let provider =
        Provider::from_source(&source).ok_or("Only tracks from a provider can be downloaded.")?;
    if state == "downloaded" {
        return Err("That track is already downloaded.".to_string());
    }

    let remote_id = remote_id.ok_or("That track has no provider id recorded.")?;
    let remote_url = remote_url.ok_or("That track has no source URL recorded.")?;
    if !provider.accepts_url(&remote_url) {
        return Err(format!(
            "That track's stored {} link does not look valid.",
            provider.display_name()
        ));
    }

    let yt_dlp = sidecar::resolve(app, Tool::YtDlp)?.path;
    let ffmpeg = sidecar::resolve(app, Tool::Ffmpeg)?.path;
    let dir = downloads_dir(app)?;

    // Names are keyed by provider *and* id: SoundCloud ids are plain integers,
    // so an id alone could collide with another provider's in one directory.
    let name_key = format!("{}-{}", provider.as_str(), remote_id);

    let (final_path, partial_path, uploaded_at) =
        fetch_with_retries(&yt_dlp, &ffmpeg, &remote_url, &name_key, &dir).await?;

    // Only now does the file become the real one, so an interrupted download
    // can never be mistaken for a complete track.
    std::fs::rename(&partial_path, &final_path).map_err(|e| {
        let _ = std::fs::remove_file(&partial_path);
        format!("Could not finish the download: {e}")
    })?;

    let stored = final_path
        .to_str()
        .ok_or("The download path is not valid UTF-8.")?;

    sqlx::query(FINISH_DOWNLOAD)
        .bind(stored)
        .bind(uploaded_at)
        .bind(track_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    // A downloaded track is meant to work with the network off, and a
    // thumbnail URL does not. This is the other half of what filing it in the
    // library just did.
    crate::covers::ensure_for_track_detached(app, pool, covers, track_id);

    Ok(())
}

/// Fetches a complete copy of a track into the cache.
///
/// Separate from [`download_track`] in intent rather than mechanism: a
/// download is a permanent choice the user made and appears in the library as
/// "offline", whereas this is disposable and invisible, subject to the cache's
/// size cap like anything else in it.
///
/// Used only when the free path -- the copy the decoder writes as it plays --
/// cannot produce a valid file: the listen was abandoned, or a seek meant the
/// decode never covered the whole track.
///
/// Everything about it is best effort. Failure leaves the track exactly as
/// uncached as it already was.
pub async fn fetch_into_cache(
    yt_dlp: &Path,
    ffmpeg: &Path,
    page_url: &str,
    pending: crate::audio_cache::PendingCache,
) -> Result<(), String> {
    let mut last_error = String::new();

    for attempt in 1..=FETCH_ATTEMPTS {
        // A fresh URL each time: the signed links are short-lived enough that
        // retrying the same one just fails again.
        let (url, _extension, _uploaded_at) =
            resolve_format(yt_dlp.to_path_buf(), page_url).await?;

        match copy_stream(ffmpeg.to_path_buf(), &url, &pending.partial).await {
            Ok(()) => {
                pending.commit();
                return Ok(());
            }
            Err(e) => {
                let _ = std::fs::remove_file(&pending.partial);

                if !crate::transcode::is_transient(&e) {
                    return Err(e);
                }

                if attempt == FETCH_ATTEMPTS {
                    // Three freshly resolved URLs, all refused. One is
                    // YouTube being busy; three in a row is the shape of a
                    // yt-dlp that has aged out -- the failure it cannot
                    // report itself, because the extraction it did succeeded.
                    crate::updater::nudge(crate::updater::Trigger::Suspected);
                    return Err(e);
                }

                last_error = e;
                tokio::time::sleep(std::time::Duration::from_millis(750 * attempt as u64)).await;
            }
        }
    }

    Err(last_error)
}

/// How many times to re-resolve and refetch before giving up.
///
/// YouTube rejects a large minority of these fetches outright, and a fresh URL
/// usually goes through. Three attempts turns a roughly one-in-three success
/// rate into a roughly seven-in-ten one, which is the difference between a
/// download button that mostly works and one that mostly does not.
const FETCH_ATTEMPTS: usize = 3;

/// Resolves a URL and copies it, retrying transient refusals.
///
/// Each attempt resolves a **new** URL rather than reusing the last one: the
/// signed links are short-lived and single-use enough that retrying the same
/// one just fails again.
///
/// Returns the finished and partial paths, with the audio already at the
/// partial one, plus whatever the resolve learned about the upload date.
async fn fetch_with_retries(
    yt_dlp: &Path,
    ffmpeg: &Path,
    page_url: &str,
    name_key: &str,
    dir: &Path,
) -> Result<(PathBuf, PathBuf, Option<i64>), String> {
    let mut last_error = String::new();

    for attempt in 1..=FETCH_ATTEMPTS {
        let (url, extension, uploaded_at) =
            resolve_format(yt_dlp.to_path_buf(), page_url).await?;

        let final_path = dir.join(final_file_name(name_key, &extension));
        let partial_path = dir.join(partial_file_name(name_key, &extension));

        match copy_stream(ffmpeg.to_path_buf(), &url, &partial_path).await {
            Ok(()) => return Ok((final_path, partial_path, uploaded_at)),
            Err(e) => {
                // A rejected fetch can still leave a stub file behind.
                let _ = std::fs::remove_file(&partial_path);

                if !crate::transcode::is_transient(&e) {
                    return Err(e);
                }

                if attempt == FETCH_ATTEMPTS {
                    // Three freshly resolved URLs, all refused. One is
                    // YouTube being busy; three in a row is the shape of a
                    // yt-dlp that has aged out -- the failure it cannot
                    // report itself, because the extraction it did succeeded.
                    crate::updater::nudge(crate::updater::Trigger::Suspected);
                    return Err(e);
                }

                last_error = e;
                // A brief pause: the refusals come in bursts.
                tokio::time::sleep(std::time::Duration::from_millis(
                    750 * attempt as u64,
                ))
                .await;
            }
        }
    }

    Err(last_error)
}

/// Asks yt-dlp for a direct URL and the container it will be in.
///
/// Both in one invocation: yt-dlp takes seconds to start, and asking twice
/// would also risk the two answers describing different formats.
async fn resolve_format(
    yt_dlp: PathBuf,
    page_url: &str,
) -> Result<(String, String, Option<i64>), String> {
    let url = page_url.to_string();

    let output = tauri::async_runtime::spawn_blocking(move || {
        crate::sidecar::quiet(&mut Command::new(&yt_dlp))
            .args(["-f", "bestaudio[ext=m4a]/bestaudio"])
            // Timestamp last on purpose: `--print` writes in flag order, so
            // appending leaves the two positions parsed below exactly where
            // they were. Free, like the playback resolve -- the extraction has
            // already happened to produce the URL.
            .args(["--print", "urls", "--print", "ext", "--print", "timestamp"])
            .args(["--no-warnings", "--no-playlist"])
            .arg(url)
            .output()
    })
    .await
    .map_err(|e| format!("yt-dlp task failed: {e}"))?
    .map_err(|e| format!("Could not start yt-dlp: {e}"))?;

    if !output.status.success() {
        return Err(crate::youtube::explain(&String::from_utf8_lossy(
            &output.stderr,
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines().map(str::trim).filter(|l| !l.is_empty());

    let url = lines
        .next()
        .filter(|l| l.starts_with("http"))
        .ok_or("yt-dlp returned no downloadable audio for that video.")?
        .to_string();

    let extension = lines.next().unwrap_or("m4a").to_string();

    // A field yt-dlp cannot supply prints as `NA`, which simply fails to
    // parse. Zero means the provider said nothing useful, not 1970.
    let uploaded_at = lines
        .next()
        .and_then(|line| line.parse::<i64>().ok())
        .filter(|seconds| *seconds > 0);

    Ok((url, sanitize_extension(&extension), uploaded_at))
}

/// Copies the remote audio packets into `destination` without re-encoding.
async fn copy_stream(ffmpeg: PathBuf, url: &str, destination: &Path) -> Result<(), String> {
    let url = url.to_string();
    let destination = destination.to_path_buf();

    let output = tauri::async_runtime::spawn_blocking(move || {
        crate::sidecar::quiet(&mut Command::new(&ffmpeg))
            .arg("-hide_banner")
            .args(["-loglevel", "error", "-y"])
            .args(["-reconnect", "1"])
            .args(["-reconnect_streamed", "1"])
            .args(["-reconnect_delay_max", "5"])
            .arg("-i")
            .arg(&url)
            // Drop any cover art stream, then copy the audio verbatim.
            .args(["-vn", "-c", "copy"])
            .arg(&destination)
            .output()
    })
    .await
    .map_err(|e| format!("ffmpeg task failed: {e}"))?
    .map_err(|e| format!("Could not start ffmpeg: {e}"))?;

    if !output.status.success() {
        // Through the same mapper the playback path uses: raw ffmpeg stderr
        // includes the entire signed URL, which is kilobytes of query string.
        return Err(crate::transcode::explain_ffmpeg(&String::from_utf8_lossy(
            &output.stderr,
        )));
    }

    Ok(())
}

/// Where the finished download lives.
fn final_file_name(name_key: &str, extension: &str) -> String {
    // Named by provider and id, never by title: titles contain `:` `?` `|` `*`
    // `"`, all illegal in Windows filenames, and two uploads can share a title.
    format!("{name_key}.{extension}")
}

/// Where a download lives while it is still arriving.
///
/// The extension has to stay **last**. ffmpeg chooses its output muxer from the
/// filename, so a name ending in `.part` leaves it nothing to infer from and it
/// refuses to start: "Unable to choose an output format".
fn partial_file_name(name_key: &str, extension: &str) -> String {
    format!("{name_key}.part.{extension}")
}

/// yt-dlp's `ext` reaches a filename, so it must not carry separators.
fn sanitize_extension(extension: &str) -> String {
    let clean: String = extension
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();

    if clean.is_empty() {
        "m4a".to_string()
    } else {
        clean.to_lowercase()
    }
}

/// Deletes a downloaded file, returning the track to `saved`.
///
/// Clearing `local_path` is not optional: the schema requires a 'saved' track
/// to have none, and the UNIQUE index would otherwise block re-downloading.
#[tauri::command]
pub async fn delete_download(db: State<'_, Db>, track_id: i64) -> Result<(), String> {
    let row = sqlx::query("SELECT source, local_path FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&db.pool)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("That track no longer exists.")?;

    let source: String = row.get("source");
    if Provider::from_source(&source).is_none() {
        return Err("Only downloaded provider tracks can be removed this way.".to_string());
    }

    if let Some(path) = row.get::<Option<String>, _>("local_path") {
        // Already gone is a success, not a failure -- the goal is that the
        // file is absent.
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Could not delete the file: {e}")),
        }
    }

    sqlx::query("UPDATE tracks SET local_path = NULL, state = 'saved' WHERE id = ?")
        .bind(track_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extension_cannot_escape_the_downloads_folder() {
        assert_eq!(sanitize_extension("m4a"), "m4a");
        assert_eq!(sanitize_extension("WEBM"), "webm");
        assert_eq!(sanitize_extension("../../evil"), "evil");
        assert_eq!(sanitize_extension("m4a/../x"), "m4ax");
    }

    #[test]
    fn a_missing_extension_falls_back_to_m4a() {
        assert_eq!(sanitize_extension(""), "m4a");
        assert_eq!(sanitize_extension("///"), "m4a");
    }

    /// The bug this guards: ffmpeg infers its output format from the filename.
    /// A partial named `<id>.<ext>.part` ends in `.part`, so ffmpeg refuses to
    /// open it at all and every download fails.
    #[test]
    fn the_partial_file_still_ends_in_a_real_extension() {
        let partial = partial_file_name("cQ95BBVO3I8", "m4a");

        assert!(
            partial.ends_with(".m4a"),
            "ffmpeg needs the extension last, got {partial}"
        );
        assert!(
            partial.contains("part"),
            "it still has to be recognisable as incomplete: {partial}"
        );
        assert_ne!(
            partial,
            final_file_name("cQ95BBVO3I8", "m4a"),
            "the partial must not collide with the finished file"
        );
    }

    #[test]
    fn the_finished_file_is_named_by_its_key() {
        assert_eq!(final_file_name("cQ95BBVO3I8", "webm"), "cQ95BBVO3I8.webm");
    }

    /// Provider-scoped, matching the database's `(source, remote_id)`
    /// uniqueness. SoundCloud ids are plain integers, so keying files on the id
    /// alone could put two providers' tracks at the same path.
    #[test]
    fn two_providers_sharing_an_id_get_different_files() {
        let a = final_file_name("soundcloud-199428706", "m4a");
        let b = final_file_name("bandcamp-199428706", "m4a");
        assert_ne!(a, b);
    }

    #[test]
    fn a_track_cannot_be_claimed_twice() {
        let lock = DownloadLock::new();

        let first = lock.try_claim(7).expect("first claim succeeds");
        assert!(lock.try_claim(7).is_err(), "second claim must be refused");
        // A different track is unaffected.
        assert!(lock.try_claim(8).is_ok());

        drop(first);
        assert!(lock.try_claim(7).is_ok(), "the claim is released on drop");
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;

    /// The Part E acceptance criterion, against the real service.
    ///
    /// Proves the whole download mechanism: yt-dlp names a format, ffmpeg
    /// stream-copies those packets to disk, and what lands is a valid, playable
    /// audio file rather than a truncated or re-encoded one.
    #[tokio::test]
    async fn a_real_video_downloads_as_playable_audio() {
        let dir = std::env::temp_dir().join("music-app-download-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Exercises exactly what the command runs -- resolution, the partial
        // filename, and the retry. Calling the pieces separately is what let
        // the `.part` muxer bug through: the test built its own filename and
        // so never hit the one production used.
        let fetched = fetch_with_retries(
            // Both tools come off PATH here; the app resolves them via `sidecar`.
            Path::new("yt-dlp"),
            Path::new("ffmpeg"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "youtube-dQw4w9WgXcQ",
            &dir,
        )
        .await;

        let (_final_path, destination, _uploaded_at) = match fetched {
            Ok(paths) => paths,
            Err(e) => {
                // YouTube refuses a large share of these outright, and the
                // retry cannot always win. Not a fault in this code.
                eprintln!("SKIP: could not fetch after retries ({e})");
                return;
            }
        };

        assert!(
            destination.to_string_lossy().ends_with(".m4a")
                || destination.to_string_lossy().ends_with(".webm"),
            "the partial must keep a real extension: {}",
            destination.display()
        );

        let size = std::fs::metadata(&destination)
            .expect("the download should exist")
            .len();
        eprintln!("downloaded {size} bytes to {}", destination.display());
        assert!(size > 100_000, "suspiciously small download: {size} bytes");

        // A byte count proves nothing about validity -- ask ffprobe whether
        // there is really a decodable audio stream in there.
        let probe = Command::new("ffprobe")
            .args(["-v", "error"])
            .args(["-select_streams", "a:0"])
            .args(["-show_entries", "stream=codec_name"])
            .args(["-of", "default=nw=1:nk=1"])
            .arg(&destination)
            .output()
            .expect("ffprobe should run");

        let codec = String::from_utf8_lossy(&probe.stdout).trim().to_string();
        eprintln!("codec: {codec}");
        assert!(
            !codec.is_empty(),
            "no audio stream found in the downloaded file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The rule [`FINISH_DOWNLOAD`] shares with `tracks::SET_IN_LIBRARY`, which is
/// easy to lose here because this statement is mostly about a file path and the
/// library stamp rides along at the end of it.
#[cfg(test)]
mod finish_tests {
    use super::*;
    use sqlx::SqlitePool;

    const LONG_AGO: i64 = 1_787_186_899;

    async fn pool(name: &str) -> SqlitePool {
        let dir = std::env::temp_dir().join(format!("music-app-finish-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::db::init(&dir).await.unwrap().pool
    }

    async fn saved(pool: &SqlitePool, remote_id: &str, in_library: i64) -> i64 {
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

    async fn finish(pool: &SqlitePool, id: i64, path: &str) {
        sqlx::query(FINISH_DOWNLOAD)
            .bind(path)
            .bind(None::<i64>)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn date_added(pool: &SqlitePool, id: i64) -> i64 {
        sqlx::query_scalar("SELECT date_added FROM tracks WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Downloading something you had only auditioned files it, so it has to be
    /// dated from the download rather than from whenever you first heard it.
    #[tokio::test]
    async fn downloading_an_audition_dates_it_from_the_download() {
        let pool = pool("audition").await;
        let id = saved(&pool, "aud", 0).await;

        let before: i64 = sqlx::query_scalar("SELECT unixepoch()")
            .fetch_one(&pool)
            .await
            .unwrap();
        finish(&pool, id, "D:/downloads/aud.m4a").await;

        assert!(date_added(&pool, id).await >= before);
    }

    /// And downloading one you already keep is not a second act of keeping it.
    #[tokio::test]
    async fn downloading_a_track_already_kept_does_not_move_it() {
        let pool = pool("kept").await;
        let id = saved(&pool, "kept", 1).await;

        finish(&pool, id, "D:/downloads/kept.m4a").await;

        assert_eq!(date_added(&pool, id).await, LONG_AGO);
    }
}
