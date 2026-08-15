//! Temporary on-disk copies of streamed audio.
//!
//! Distinct from a download, which is a deliberate, permanent choice the user
//! made and which the library shows as "offline". This is invisible, bounded,
//! and disposable: it exists so that replaying or seeking backwards through a
//! stream does not fetch the same bytes again.
//!
//! The audio is stored **compressed**, exactly as the provider served it.
//! Measured, that is about 0.93 MB per minute against 20.2 MB for decoded PCM
//! -- a factor of twenty-two, and the difference between a gigabyte holding
//! eighteen hours of music or fifty minutes.
//!
//! Filling it is free. ffmpeg writes the cache copy as a second output of the
//! decode that was happening anyway, so one network fetch produces both the
//! audio being played and the file kept for next time.

use std::path::PathBuf;
use std::time::SystemTime;

/// Ceiling on the whole cache. At the measured rate this is roughly eighteen
/// hours of audio.
const DEFAULT_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// Matroska, because `-c copy` has to accept whatever codec the provider sent
/// without us having probed it first -- AAC from YouTube, AAC-in-HLS from
/// SoundCloud, Opus from either. A container-specific extension would make the
/// copy fail for anything it did not expect.
const EXTENSION: &str = "mka";

/// Where a cache write is going, and where it lands if it completes.
///
/// Carried by the decoder because only it knows whether ffmpeg exited cleanly
/// -- a track skipped halfway through leaves a truncated file that must never
/// be mistaken for the whole song.
#[derive(Debug, Clone)]
pub struct PendingCache {
    pub partial: PathBuf,
    complete: PathBuf,
    /// Carried so the entry can enforce the cap the moment it lands.
    ///
    /// Evicting only on the *next* reservation would let the cache sit over
    /// its limit for as long as nobody plays anything new -- which is exactly
    /// when a user is most likely to look at their disk usage.
    dir: PathBuf,
    max_bytes: u64,
}

impl PendingCache {
    /// Publishes the write, then brings the cache back under its cap.
    pub fn commit(self) {
        if std::fs::rename(&self.partial, &self.complete).is_err() {
            let _ = std::fs::remove_file(&self.partial);
            return;
        }
        evict(&self.dir, self.max_bytes);
    }

    pub fn discard(self) {
        let _ = std::fs::remove_file(&self.partial);
    }
}

pub struct AudioCache {
    dir: PathBuf,
    max_bytes: u64,
}

impl AudioCache {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    #[cfg(test)]
    fn with_limit(dir: PathBuf, max_bytes: u64) -> Self {
        Self { dir, max_bytes }
    }

    /// The cached file for a track, if it is there.
    ///
    /// Touches the file so eviction can tell recently played tracks from
    /// forgotten ones.
    pub fn lookup(&self, source: &str, remote_id: &str) -> Option<PathBuf> {
        let path = self.dir.join(file_name(source, remote_id, EXTENSION));
        if !path.is_file() {
            return None;
        }

        // Best effort: failing to touch costs accuracy in eviction order, not
        // correctness, so it must not stop a cache hit.
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) {
            let _ = file.set_modified(SystemTime::now());
        }

        Some(path)
    }

    /// Reserves a place to write this track, evicting first if needed.
    ///
    /// `None` when the cache directory cannot be created, which makes caching
    /// silently inert rather than breaking playback.
    pub fn reserve(&self, source: &str, remote_id: &str) -> Option<PendingCache> {
        std::fs::create_dir_all(&self.dir).ok()?;
        evict(&self.dir, self.max_bytes);

        let complete = self.dir.join(file_name(source, remote_id, EXTENSION));
        // The extension stays last, matching the downloads convention -- and a
        // leftover `.part` from a killed process is never a usable cache hit,
        // because `lookup` only ever names the completed form.
        let partial = self
            .dir
            .join(file_name(source, remote_id, &format!("part.{EXTENSION}")));

        let _ = std::fs::remove_file(&partial);
        Some(PendingCache {
            partial,
            complete,
            dir: self.dir.clone(),
            max_bytes: self.max_bytes,
        })
    }

    #[cfg(test)]
    fn total_bytes(&self) -> u64 {
        std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum()
    }
}

/// Deletes least-recently-used entries until the cache is under its cap.
///
/// A free function because it runs both when reserving space and when an entry
/// lands -- and the second of those happens inside the decoder, which knows
/// nothing about the cache beyond its own reservation.
fn evict(dir: &std::path::Path, max_bytes: u64) {
    {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };

        let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                if !meta.is_file() {
                    return None;
                }
                Some((
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                    entry.path(),
                ))
            })
            .collect();

        let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
        if total <= max_bytes {
            return;
        }

        // Oldest first, so the tracks nobody has played in a while go before
        // the ones that are still being listened to.
        files.sort_by_key(|(modified, _, _)| *modified);

        for (_, size, path) in files {
            if total <= max_bytes {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }
}

/// Names a cache file by provider *and* id, matching the database's
/// `(source, remote_id)` uniqueness -- SoundCloud ids are plain integers, so
/// an id alone could collide with another provider's.
fn file_name(source: &str, remote_id: &str, extension: &str) -> String {
    format!(
        "{}-{}.{extension}",
        sanitize(source),
        sanitize(remote_id)
    )
}

/// These reach the filesystem, and they arrive from the database rather than
/// straight from a validator, so anything that could climb out of the cache
/// directory is stripped rather than trusted.
fn sanitize(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();

    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("music-app-cache-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes an entry the way playback does: to the partial file, then
    /// committed. Going straight to the final path would skip the eviction
    /// that commit performs.
    fn store(cache: &AudioCache, source: &str, id: &str, bytes: usize) {
        let pending = cache.reserve(source, id).unwrap();
        std::fs::write(&pending.partial, vec![0u8; bytes]).unwrap();
        pending.commit();
        // Distinct mtimes, so "least recently used" is well defined.
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    #[test]
    fn a_track_is_found_again_by_its_provider_and_id() {
        let dir = temp_dir("lookup");
        let cache = AudioCache::new(dir.clone());

        assert!(cache.lookup("youtube", "dQw4w9WgXcQ").is_none());

        store(&cache, "youtube", "dQw4w9WgXcQ", 16);

        assert!(cache.lookup("youtube", "dQw4w9WgXcQ").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same reason the database scopes uniqueness to the provider.
    #[test]
    fn two_providers_sharing_an_id_get_separate_files() {
        let dir = temp_dir("collide");
        let cache = AudioCache::new(dir.clone());

        store(&cache, "soundcloud", "199428706", 16);
        store(&cache, "youtube", "199428706", 16);

        assert!(cache.lookup("soundcloud", "199428706").is_some());
        assert!(cache.lookup("youtube", "199428706").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A partial file must never satisfy a lookup: it is a truncated song.
    #[test]
    fn a_partial_write_is_not_a_cache_hit() {
        let dir = temp_dir("partial");
        let cache = AudioCache::new(dir.clone());

        let pending = cache.reserve("youtube", "abc").unwrap();
        std::fs::write(&pending.partial, vec![0u8; 16]).unwrap();

        assert!(
            cache.lookup("youtube", "abc").is_none(),
            "an interrupted download must not be served as the whole track"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cache_stays_under_its_limit() {
        let dir = temp_dir("evict");
        let cache = AudioCache::with_limit(dir.clone(), 300);

        for name in ["a", "b", "c", "d"] {
            store(&cache, "youtube", name, 100);
        }

        assert!(
            cache.total_bytes() <= 300,
            "cache grew to {} bytes",
            cache.total_bytes()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_oldest_entry_is_the_one_evicted() {
        let dir = temp_dir("lru");
        let cache = AudioCache::with_limit(dir.clone(), 250);

        for name in ["old", "mid", "new"] {
            store(&cache, "youtube", name, 100);
        }

        assert!(
            cache.lookup("youtube", "old").is_none(),
            "the least recently used entry should have gone first"
        );
        assert!(cache.lookup("youtube", "new").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hit counts as use, so a track played often is not evicted for one
    /// played once and forgotten.
    #[test]
    fn a_cache_hit_protects_an_entry_from_eviction() {
        let dir = temp_dir("touch");
        let cache = AudioCache::with_limit(dir.clone(), 250);

        for name in ["first", "second"] {
            store(&cache, "youtube", name, 100);
        }

        // Using the oldest entry makes it the newest.
        assert!(cache.lookup("youtube", "first").is_some());
        std::thread::sleep(std::time::Duration::from_millis(20));

        store(&cache, "youtube", "third", 100);

        assert!(
            cache.lookup("youtube", "first").is_some(),
            "the recently played track was evicted instead of the idle one"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_can_escape_the_cache_directory() {
        assert_eq!(file_name("youtube", "../../etc/passwd", "mka"), "youtube-etcpasswd.mka");
        assert_eq!(file_name("../x", "abc", "mka"), "x-abc.mka");
        assert_eq!(file_name("", "", "mka"), "unknown-unknown.mka");
    }
}
