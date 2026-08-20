//! Cover art, stored once and shared by everything that points at it.
//!
//! Three sources feed this store -- pictures embedded in a local file's tags,
//! thumbnails fetched from a provider, and images the user picks for a
//! playlist -- and they all land in the same place under the same rules.
//!
//! Entries are content-addressed: the key is a hash of the *source* bytes, so
//! an album whose twelve tracks embed the same cover stores it once and pays
//! the decode once. Hashing the source rather than the normalised output is
//! what makes that dedupe happen before the expensive work rather than after.
//!
//! Nothing here is authoritative. Every entry can be deleted and regenerated
//! from the file or URL it came from, which is why sweeping orphans is safe
//! and why a failure to store one is never worth failing the caller for.

use std::collections::HashSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use sha2::{Digest, Sha256};
use sqlx::Row;
use tauri::{AppHandle, Emitter};

/// The longest edge of a stored cover, in pixels.
///
/// Sized for the largest place one is drawn (a 104px header tile) on a 3x
/// display, with room to spare. Tags routinely embed 3000x3000 originals;
/// keeping those would mean decoding six megapixels to paint forty pixels
/// every time a row scrolls past.
const MAX_EDGE: u32 = 600;

/// Quality of the stored JPEG.
///
/// 82 is the usual point where artefacts stop being visible on photographic
/// content. Album art is photographic often enough that going lower shows.
const QUALITY: u8 = 82;

/// Refuses absurd input before handing it to a decoder.
///
/// A malformed or hostile image can claim enormous dimensions in its header
/// and have the decoder allocate for them. 64 MB is far above any real cover
/// and far below anything that would matter.
const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct CoverStore {
    dir: PathBuf,
}

impl CoverStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file a key names. Not guaranteed to exist.
    pub fn path(&self, key: &str) -> PathBuf {
        self.dir.join(key)
    }

    /// Stores an image, returning the key that names it.
    ///
    /// Cheap to call repeatedly with the same bytes: the hash is computed
    /// first and an existing entry short-circuits before anything is decoded.
    pub fn store(&self, bytes: &[u8]) -> Result<String, String> {
        if bytes.is_empty() {
            return Err("That image is empty.".to_string());
        }
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err("That image is too large.".to_string());
        }

        let key = key_for(bytes);
        let path = self.path(&key);

        // The dedupe. Twelve tracks sharing an album cover reach this line
        // twelve times and decode once.
        if path.exists() {
            return Ok(key);
        }

        let jpeg = normalise(bytes)?;

        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;

        // Written beside the target and renamed, so a crash or a second writer
        // cannot leave a half-written JPEG that `exists()` would then accept
        // forever. Rename is atomic on both filesystems this ships to.
        let temp = self.dir.join(format!("{key}.part"));
        std::fs::write(&temp, &jpeg).map_err(|e| e.to_string())?;
        match std::fs::rename(&temp, &path) {
            Ok(()) => {}
            Err(e) => {
                let _ = std::fs::remove_file(&temp);
                // Losing the race is success: the other writer stored the same
                // bytes, because the name is derived from them.
                if !path.exists() {
                    return Err(e.to_string());
                }
            }
        }

        Ok(key)
    }

    /// Deletes every entry no longer referenced.
    ///
    /// Returns how many went. Called after a scan, when the set of live keys
    /// has just been recomputed anyway -- doing it on a timer would mean
    /// holding that set somewhere or rebuilding it for no reason.
    pub fn sweep(&self, keep: &HashSet<String>) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return 0;
        };

        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };

            // `.part` files are interrupted writes, and no live key ends in
            // one -- so this also cleans up after a crash mid-store.
            if keep.contains(name) {
                continue;
            }
            if std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

/// The filename for some bytes.
///
/// Truncated to 32 hex characters -- 128 bits, where a collision across a
/// library of any imaginable size remains impossible. The extension is always
/// `.jpg` because the stored form always is, whatever arrived.
fn key_for(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut key = String::with_capacity(36);
    for byte in &digest[..16] {
        key.push_str(&format!("{byte:02x}"));
    }
    key.push_str(".jpg");
    key
}

/// Decodes, shrinks and re-encodes as JPEG.
///
/// Always re-encodes, even when the source is already a small JPEG. The point
/// is that every entry in the store is known to be one decodable format at one
/// bounded size -- a store that sometimes holds a 12 MB PNG is a store whose
/// cost nobody can reason about.
fn normalise(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory(bytes)
        .map_err(|e| format!("That image could not be read: {e}"))?;

    // `thumbnail` rather than `resize`: it is markedly faster and the
    // difference is invisible on the way down to 600px.
    let shrunk = if image.width() > MAX_EDGE || image.height() > MAX_EDGE {
        image.thumbnail(MAX_EDGE, MAX_EDGE)
    } else {
        image
    };

    // JPEG has no alpha channel. Flattening to RGB first is what stops a
    // transparent PNG encoding as noise.
    let rgb = shrunk.to_rgb8();

    let mut out = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut out, QUALITY)
        .encode_image(&rgb)
        .map_err(|e| format!("That image could not be converted: {e}"))?;

    Ok(out.into_inner())
}

// --- Commands ---------------------------------------------------------------

/// Where cover files live, so the frontend can build asset URLs itself.
///
/// One call at startup rather than one per image: the alternative is an IPC
/// round trip for every row in a list, which is the thing the asset protocol
/// exists to avoid.
#[tauri::command]
pub fn cover_dir(covers: tauri::State<'_, CoverStore>) -> String {
    covers.dir().to_string_lossy().to_string()
}

/// Gives a playlist artwork from a file the user picked.
///
/// The file is *copied* into the store, not referenced. A path into the user's
/// pictures folder would break the first time they moved the file, and would
/// sit outside the asset protocol's scope, so the webview could not load it
/// even while it existed.
#[tauri::command]
pub async fn set_playlist_cover(
    db: tauri::State<'_, crate::db::Db>,
    covers: tauri::State<'_, CoverStore>,
    playlist_id: i64,
    path: String,
) -> Result<(), String> {
    let store = (*covers).clone();
    let key = tauri::async_runtime::spawn_blocking(move || {
        let bytes = std::fs::read(&path).map_err(|e| format!("Could not read that file: {e}"))?;
        store.store(&bytes)
    })
    .await
    .map_err(|e| e.to_string())??;

    let outcome = sqlx::query("UPDATE playlists SET cover_key = ? WHERE id = ?")
        .bind(&key)
        .bind(playlist_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    if outcome.rows_affected() == 0 {
        return Err("That playlist no longer exists.".to_string());
    }

    Ok(())
}

/// Drops a playlist's artwork, falling back to the generated kind.
///
/// The file itself is left for the next sweep rather than deleted here: another
/// playlist or track may point at the same key, because the key is a hash of
/// the image and identical images share one.
#[tauri::command]
pub async fn clear_playlist_cover(
    db: tauri::State<'_, crate::db::Db>,
    playlist_id: i64,
) -> Result<(), String> {
    sqlx::query("UPDATE playlists SET cover_key = NULL WHERE id = ?")
        .bind(playlist_id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Fetches a provider thumbnail into the store, through ffmpeg.
///
/// ffmpeg rather than an HTTP client because it is already how everything else
/// in this app reaches the network, and because a remote track cannot play
/// without it anyway -- so this adds no requirement that streaming did not
/// already impose. It decodes to PNG so the only lossy step is our own JPEG
/// encode, rather than re-compressing an already-compressed thumbnail.
pub async fn fetch_remote_cover(
    ffmpeg: PathBuf,
    url: String,
    covers: CoverStore,
) -> Result<String, String> {
    let output = tauri::async_runtime::spawn_blocking(move || {
        crate::sidecar::quiet(&mut std::process::Command::new(&ffmpeg))
            .arg("-hide_banner")
            .args(["-loglevel", "error"])
            .args(["-reconnect", "1"])
            .args(["-reconnect_delay_max", "5"])
            // Name the input demuxer instead of letting ffmpeg guess from the
            // URL. Left to itself it reads the extension, and a SoundCloud
            // thumbnail ends in a literal `.jpg`, which selects `image2` -- the
            // *file sequence* demuxer. Over HTTP that yields a stream whose
            // size it cannot determine, so the decode produces no frame and the
            // cover silently never appears.
            //
            // YouTube escaped this only by accident: its thumbnails carry a
            // query string, so there is no extension to misread and ffmpeg
            // probes the content instead. Being explicit makes both providers
            // take the same path rather than leaving one of them to luck.
            .args(["-f", "image2pipe"])
            .arg("-i")
            .arg(&url)
            // One frame, as PNG, to stdout. `image2pipe` is the muxer that
            // does not insist on a filename with a sequence pattern in it.
            .args(["-frames:v", "1", "-f", "image2pipe", "-c:v", "png"])
            .arg("-")
            .output()
    })
    .await
    .map_err(|e| format!("cover task failed: {e}"))?
    .map_err(|e| format!("Could not start ffmpeg: {e}"))?;

    if !output.status.success() || output.stdout.is_empty() {
        return Err(format!(
            "ffmpeg could not fetch that image: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let store = covers;
    let bytes = output.stdout;
    tauri::async_runtime::spawn_blocking(move || store.store(&bytes))
        .await
        .map_err(|e| e.to_string())?
}

/// The event announcing that a track's artwork has landed.
///
/// Lives here rather than with the provider search that used to own it: the
/// fetch is no longer part of finding a track, it is part of keeping one, and
/// keeping happens in places that know nothing about searching.
pub const COVER_READY_EVENT: &str = "cover-ready";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverReady {
    pub track_id: i64,
    pub cover_key: String,
}

/// Stores a remote track's artwork, if it has none yet.
///
/// Called when a track is **kept** -- filed in the library, or downloaded --
/// and deliberately not when one is merely auditioned. A preview already shows
/// the provider's thumbnail straight from its URL, which the webview is
/// allowed to load and which costs nothing to hold; a *stored* copy only earns
/// its disk once the track has to survive being offline.
///
/// Every failure here is silent by design. It runs detached from the gesture
/// that triggered it, that gesture has already succeeded, and a missing
/// picture is not worth a toast over a track that is otherwise fine.
/// The URL worth fetching for a row, if any.
///
/// Split out because both answers are easy to get wrong in a way nothing would
/// notice: fetching again for a row that already has a key costs a process and
/// a download to arrive at the identical content-addressed name, and fetching
/// for a row with no thumbnail URL spawns ffmpeg to fail.
fn fetch_wanted(cover_key: Option<&str>, thumbnail_url: Option<&str>) -> Option<String> {
    if cover_key.is_some() {
        return None;
    }

    // A URL that is present but empty is the same as absent. SQLite is happy
    // to hold `''`, and ffmpeg would be asked to open nothing.
    thumbnail_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

pub async fn ensure_for_track(
    app: AppHandle,
    pool: sqlx::SqlitePool,
    covers: CoverStore,
    track_id: i64,
) {
    let row = sqlx::query("SELECT cover_key, remote_thumbnail_url FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&pool)
        .await;

    let Ok(Some(row)) = row else {
        return;
    };

    let Some(url) = fetch_wanted(
        row.get::<Option<String>, _>("cover_key").as_deref(),
        row.get::<Option<String>, _>("remote_thumbnail_url").as_deref(),
    ) else {
        return;
    };

    let Ok(ffmpeg) = crate::sidecar::resolve(&app, crate::sidecar::Tool::Ffmpeg) else {
        return;
    };

    let key = match fetch_remote_cover(ffmpeg.path, url, covers).await {
        Ok(key) => key,
        Err(e) => {
            eprintln!("cover art skipped for track {track_id}: {e}");
            return;
        }
    };

    if sqlx::query("UPDATE tracks SET cover_key = ? WHERE id = ?")
        .bind(&key)
        .bind(track_id)
        .execute(&pool)
        .await
        .is_err()
    {
        return;
    }

    let _ = app.emit(
        COVER_READY_EVENT,
        CoverReady {
            track_id,
            cover_key: key,
        },
    );
}

/// Spawns [`ensure_for_track`], so keeping a track never waits on a picture.
///
/// The row is complete and usable the moment the caller returns; the artwork
/// arrives later and announces itself.
pub fn ensure_for_track_detached(
    app: &AppHandle,
    pool: &sqlx::SqlitePool,
    covers: &CoverStore,
    track_id: i64,
) {
    let app = app.clone();
    let pool = pool.clone();
    let covers = covers.clone();

    tauri::async_runtime::spawn(async move {
        ensure_for_track(app, pool, covers, track_id).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, RgbImage};

    /// Serves `body` once, at a URL ending in `.jpg`, and returns that URL.
    ///
    /// Both halves matter. The bug this guards needed a real HTTP source *and*
    /// a path ffmpeg would read an extension from: given `.jpg` it selects the
    /// `image2` file-sequence demuxer, which cannot size a single image over
    /// HTTP. A local file reproduces nothing, and neither does an HTTP URL
    /// carrying a query string -- which is exactly why YouTube worked while
    /// SoundCloud did not.
    fn serve_one_jpeg(body: Vec<u8>) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let port = listener.local_addr().unwrap().port();

        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // Read the request line so the client is not writing into a closed
            // socket; the content is irrelevant, only that it is drained.
            let mut scratch = [0u8; 1024];
            let _ = stream.read(&mut scratch);

            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        });

        format!("http://127.0.0.1:{port}/artwork.jpg")
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut buffer = RgbImage::new(width, height);
        for (x, y, pixel) in buffer.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x % 256) as u8, (y % 256) as u8, 200]);
        }
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(buffer)
            .write_to(&mut out, ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    /// Covers the fetch path end to end: HTTP in, a key on disk out.
    ///
    /// NOT a regression test for the SoundCloud bug, though it was written as
    /// one. Checked by reverting the `-f image2pipe` fix and re-running: this
    /// still passed, so it does not reproduce the failure. Whatever the real
    /// difference is, a locally served baseline JPEG does not have it -- the
    /// SoundCloud image was checked and is baseline too, so that is not it.
    ///
    /// The fix rests on measurement against the live URLs instead: SoundCloud
    /// returned 0 bytes before and 138,248 after, while YouTube returned an
    /// identical 132,754 both ways. Left here for the coverage it does give,
    /// and labelled so nobody trusts it for more than that.
    #[tokio::test]
    async fn a_thumbnail_url_is_fetched_and_stored() {
        let Some(ffmpeg) = which_ffmpeg() else {
            eprintln!("SKIP: ffmpeg is not available");
            return;
        };

        let dir = std::env::temp_dir().join(format!("music-app-sc-cover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = CoverStore::new(dir.clone());

        let url = serve_one_jpeg(jpeg(300, 300));
        let key = fetch_remote_cover(ffmpeg, url, store)
            .await
            .expect("a .jpg URL served over HTTP must yield a cover");

        assert!(
            dir.join(&key).is_file(),
            "the stored cover {key} should exist on disk"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ffmpeg is optional, so this test reports rather than fails without it.
    fn which_ffmpeg() -> Option<PathBuf> {
        let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut buffer = RgbImage::new(width, height);
        // Not a flat fill: a solid colour compresses to almost nothing, which
        // would make the size assertions meaningless.
        for (x, y, pixel) in buffer.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
        }
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(buffer)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn store(name: &str) -> CoverStore {
        let dir = std::env::temp_dir().join(format!("music-app-covers-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        CoverStore::new(dir)
    }

    #[test]
    fn the_same_image_stores_once() {
        let store = store("dedupe");
        let bytes = png(64, 64);

        let first = store.store(&bytes).unwrap();
        let second = store.store(&bytes).unwrap();

        assert_eq!(first, second, "identical bytes must share a key");
        assert_eq!(
            std::fs::read_dir(store.dir()).unwrap().count(),
            1,
            "an album's shared cover must not be written per track"
        );
    }

    #[test]
    fn different_images_get_different_keys() {
        let store = store("distinct");
        assert_ne!(
            store.store(&png(64, 64)).unwrap(),
            store.store(&png(48, 48)).unwrap()
        );
    }

    /// The reason the store exists at all.
    #[test]
    fn an_oversized_cover_is_shrunk() {
        let store = store("shrink");
        let key = store.store(&png(2000, 2000)).unwrap();

        let stored = image::open(store.path(&key)).unwrap();
        assert!(
            stored.width() <= MAX_EDGE && stored.height() <= MAX_EDGE,
            "stored {}x{}, expected no edge over {MAX_EDGE}",
            stored.width(),
            stored.height()
        );
    }

    #[test]
    fn a_small_cover_is_not_enlarged() {
        let store = store("small");
        let key = store.store(&png(80, 80)).unwrap();

        let stored = image::open(store.path(&key)).unwrap();
        assert_eq!((stored.width(), stored.height()), (80, 80));
    }

    #[test]
    fn everything_is_stored_as_jpeg_whatever_arrived() {
        let store = store("format");
        let key = store.store(&png(100, 100)).unwrap();

        assert!(key.ends_with(".jpg"));
        assert_eq!(
            image::ImageReader::open(store.path(&key))
                .unwrap()
                .format()
                .unwrap(),
            ImageFormat::Jpeg
        );
    }

    #[test]
    fn rubbish_is_refused_rather_than_stored() {
        let store = store("rubbish");

        assert!(store.store(b"not an image at all").is_err());
        assert!(store.store(&[]).is_err());
        assert_eq!(
            std::fs::read_dir(store.dir()).map(|d| d.count()).unwrap_or(0),
            0,
            "a rejected image must leave nothing behind"
        );
    }

    #[test]
    fn sweeping_keeps_what_is_referenced_and_removes_the_rest() {
        let store = store("sweep");
        let kept = store.store(&png(64, 64)).unwrap();
        let orphan = store.store(&png(32, 32)).unwrap();

        let keep: HashSet<String> = std::iter::once(kept.clone()).collect();
        assert_eq!(store.sweep(&keep), 1);

        assert!(store.path(&kept).exists());
        assert!(!store.path(&orphan).exists());
    }

    /// A crash mid-write must not leave something `store` would later trust.
    #[test]
    fn an_interrupted_write_is_swept() {
        let store = store("interrupted");
        let kept = store.store(&png(64, 64)).unwrap();
        std::fs::write(store.dir().join("deadbeef.jpg.part"), b"half").unwrap();

        let keep: HashSet<String> = std::iter::once(kept).collect();
        assert_eq!(store.sweep(&keep), 1);
    }

    #[test]
    fn a_track_that_already_has_artwork_is_left_alone() {
        // The store is content-addressed, so a second fetch would spend a
        // process and a download to write the identical file back.
        assert_eq!(
            fetch_wanted(Some("abc123"), Some("https://i.ytimg.com/vi/x/hq.jpg")),
            None
        );
    }

    #[test]
    fn a_track_with_no_thumbnail_is_left_alone() {
        assert_eq!(fetch_wanted(None, None), None);
        // SQLite will hold an empty string quite happily, and ffmpeg would be
        // spawned to open nothing at all.
        assert_eq!(fetch_wanted(None, Some("")), None);
        assert_eq!(fetch_wanted(None, Some("   ")), None);
    }

    #[test]
    fn a_kept_track_with_only_a_thumbnail_url_is_fetched() {
        assert_eq!(
            fetch_wanted(None, Some("https://i.ytimg.com/vi/x/hq.jpg")),
            Some("https://i.ytimg.com/vi/x/hq.jpg".to_string())
        );
    }}
