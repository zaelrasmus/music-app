//! Short-lived cache of resolved stream URLs.
//!
//! Resolving one costs a yt-dlp process and 6-7 seconds, and it was being paid
//! on *every* play -- including replaying the track that just finished, or
//! coming back to one skipped past a moment ago.
//!
//! The URLs are good for far longer than that: measured against live services,
//! a YouTube link carried five hours of validity and a SoundCloud link ninety
//! minutes. Both state it plainly in their query string, so the lifetime is
//! read rather than guessed.
//!
//! Correctness does not depend on the expiry being right. A link can be
//! revoked early, and a cached one that fails is invalidated and re-resolved,
//! so the worst case is the old behaviour plus one wasted attempt.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;

/// Trimmed off any parsed expiry, so a URL is never handed out moments before
/// it dies -- a stream that starts and then stops is worse than a slow start.
const SAFETY_MARGIN: Duration = Duration::from_secs(300);

/// Used when the URL states no expiry we recognise.
///
/// Short enough to be safe against a policy we cannot see, long enough to
/// still cover the replays and skip-backs that motivate the cache at all.
const DEFAULT_TTL: Duration = Duration::from_secs(600);

/// Which encoding of a track to ask the provider for.
///
/// Both services publish the same audio several times over, and the choice is
/// not purely about quality: ffmpeg's native AAC decoder rejects some streams
/// outright -- a SoundCloud track reported `Number of bands (49) exceeds limit
/// (32)` mid-listen and could not be decoded at all. Nothing about that track
/// is broken; the other encoding of it plays.
///
/// So the encoding is a *choice the caller can revise* rather than a constant.
/// One that will not decode stops being a dead track and becomes one retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// AAC in mp4 where it exists.
    ///
    /// Cheap to decode, widely available, and the highest bitrate SoundCloud
    /// offers (160k, against 128k for its mp3). The bare fallback is Opus on
    /// YouTube and mp3 on SoundCloud.
    Preferred,
    /// Anything except that.
    ///
    /// Deliberately expressed as an exclusion rather than a second list of
    /// preferences: the only thing known at this point is which encoding just
    /// failed, and every remaining option is better than none.
    Alternate,
}

impl Encoding {
    /// The yt-dlp format selector this asks for.
    pub fn selector(self) -> &'static str {
        match self {
            Encoding::Preferred => "bestaudio[ext=m4a]/bestaudio",
            Encoding::Alternate => "bestaudio[ext!=m4a]/bestaudio",
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    url: String,
    good_until: SystemTime,
}

/// Keyed by the provider's page URL and the encoding asked for.
///
/// The page URL is what identifies a track across both services and is what
/// the caller already holds; the encoding is part of the key because the two
/// resolve to different streams and a fallback must not be handed the link
/// that just failed to decode.
#[derive(Default)]
pub struct StreamUrlCache {
    entries: Mutex<HashMap<(String, Encoding), Entry>>,
    /// Absent in tests, which exercise the expiry logic and nothing else.
    pool: Option<SqlitePool>,
}

impl StreamUrlCache {
    /// A cache that also records what a resolve happens to teach it.
    ///
    /// The pool is here rather than in the caller because this is the only
    /// place that observes a resolve actually running -- everything above sees
    /// a URL and cannot tell a fresh one from a cached one. Since the resolve
    /// is the one moment a YouTube upload date is available for free, the
    /// write belongs where that moment is visible.
    pub fn with_pool(pool: SqlitePool) -> Self {
        Self {
            entries: Mutex::default(),
            pool: Some(pool),
        }
    }

    /// Returns a playable stream URL, resolving only when necessary.
    pub async fn resolve(
        &self,
        yt_dlp: &Path,
        page_url: &str,
        encoding: Encoding,
    ) -> Result<String, String> {
        if let Some(url) = self.lookup(page_url, encoding, SystemTime::now()) {
            return Ok(url);
        }

        let resolved = crate::youtube::resolve_stream_url(yt_dlp, page_url, encoding).await?;
        self.store(page_url, encoding, resolved.url.clone(), SystemTime::now());

        if let Some(uploaded_at) = resolved.uploaded_at {
            self.remember_upload_date(page_url, uploaded_at);
        }

        Ok(resolved.url)
    }

    /// Files the upload date against whichever track has this page URL.
    ///
    /// Fire-and-forget, and only fills a gap: `uploaded_at IS NULL` means a
    /// date already recorded at save time -- SoundCloud reports one in search
    /// results -- is never overwritten by a later resolve. Failure costs
    /// nothing, because the next play resolves again.
    fn remember_upload_date(&self, page_url: &str, uploaded_at: i64) {
        let Some(pool) = self.pool.clone() else {
            return;
        };
        let page_url = page_url.to_string();

        tauri::async_runtime::spawn(async move {
            let _ = sqlx::query(
                "UPDATE tracks SET uploaded_at = ? \
                 WHERE remote_url = ? AND uploaded_at IS NULL",
            )
            .bind(uploaded_at)
            .bind(&page_url)
            .execute(&pool)
            .await;
        });
    }

    /// Forgets every cached URL for `page_url`, whatever encoding it was for.
    ///
    /// Returns whether there was one. The caller uses that to decide whether
    /// retrying is worthwhile: a failure with nothing cached was not our
    /// staleness, so retrying would just fail again more slowly.
    ///
    /// All encodings, not just the one that failed: staleness is a property of
    /// the moment rather than of the encoding, so links resolved alongside the
    /// dead one are no more trustworthy than it was.
    pub fn invalidate(&self, page_url: &str) -> bool {
        self.entries
            .lock()
            .map(|mut entries| {
                let before = entries.len();
                entries.retain(|(url, _), _| url != page_url);
                entries.len() != before
            })
            .unwrap_or(false)
    }

    /// `now` is a parameter so the expiry logic is testable without waiting.
    fn lookup(&self, page_url: &str, encoding: Encoding, now: SystemTime) -> Option<String> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(&(page_url.to_string(), encoding))?;
        (entry.good_until > now).then(|| entry.url.clone())
    }

    fn store(&self, page_url: &str, encoding: Encoding, url: String, now: SystemTime) {
        let good_until = usable_until(&url, now);

        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(
                (page_url.to_string(), encoding),
                Entry {
                    url,
                    good_until,
                },
            );
        }
    }
}

/// How long `url` can be trusted for.
fn usable_until(url: &str, now: SystemTime) -> SystemTime {
    match parse_expiry(url) {
        // A link already inside the safety margin is kept only briefly rather
        // than being treated as good for the default -- it really is nearly
        // dead, and `now` is the honest answer.
        Some(expiry) => expiry.checked_sub(SAFETY_MARGIN).unwrap_or(now).max(now),
        None => now + DEFAULT_TTL,
    }
}

/// Reads the expiry both services put in the query string.
///
/// YouTube spells it `expire`, SoundCloud `expires`; both are unix seconds.
/// Anything else falls back to the default TTL rather than being trusted.
fn parse_expiry(url: &str) -> Option<SystemTime> {
    let (_, query) = url.split_once('?')?;

    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if key != "expire" && key != "expires" {
            continue;
        }
        if let Ok(seconds) = value.parse::<u64>() {
            return Some(UNIX_EPOCH + Duration::from_secs(seconds));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// The real shapes, taken from live responses.
    const YT: &str = "https://rr2---sn-x.googlevideo.com/videoplayback?expire=2000000000&ei=abc";
    const SC: &str = "https://playback.media-streaming.soundcloud.cloud/x/playlist.m3u8?expires=2000000000&Policy=abc";

    #[test]
    fn both_providers_state_their_expiry_and_it_is_read() {
        assert_eq!(parse_expiry(YT), Some(at(2_000_000_000)));
        assert_eq!(parse_expiry(SC), Some(at(2_000_000_000)));
    }

    #[test]
    fn a_url_with_no_expiry_yields_none() {
        assert_eq!(parse_expiry("https://example.test/audio.m4a"), None);
        assert_eq!(parse_expiry("https://example.test/a?b=c"), None);
        assert_eq!(parse_expiry("https://example.test/a?expire=soon"), None);
    }

    /// The fallback must not be handed the link that just failed.
    ///
    /// Both encodings of a track share a page URL, so keying on that alone
    /// would return the AAC link when the alternate was asked for -- and the
    /// retry would fail exactly as the first attempt did.
    #[test]
    fn the_two_encodings_are_cached_apart() {
        let cache = StreamUrlCache::default();
        cache.store("page", Encoding::Preferred, YT.to_string(), at(1_000_000_000));

        assert_eq!(
            cache.lookup("page", Encoding::Alternate, at(1_000_000_100)),
            None,
            "asking for the other encoding must not return this one",
        );
        assert_eq!(
            cache.lookup("page", Encoding::Preferred, at(1_000_000_100)),
            Some(YT.to_string()),
        );
    }

    /// Staleness is a property of the moment, not of the encoding.
    #[test]
    fn invalidating_a_page_forgets_every_encoding_of_it() {
        let cache = StreamUrlCache::default();
        cache.store("page", Encoding::Preferred, YT.to_string(), at(1_000_000_000));
        cache.store("page", Encoding::Alternate, SC.to_string(), at(1_000_000_000));
        cache.store("other", Encoding::Preferred, YT.to_string(), at(1_000_000_000));

        assert!(cache.invalidate("page"));

        assert_eq!(cache.lookup("page", Encoding::Preferred, at(1_000_000_100)), None);
        assert_eq!(cache.lookup("page", Encoding::Alternate, at(1_000_000_100)), None);
        assert!(
            cache.lookup("other", Encoding::Preferred, at(1_000_000_100)).is_some(),
            "a different track must be left alone",
        );
    }

    /// The alternate has to actually exclude what the preferred asks for, or
    /// the retry resolves the same stream and fails identically.
    #[test]
    fn the_alternate_encoding_excludes_the_preferred_one() {
        assert!(Encoding::Preferred.selector().contains("ext=m4a"));
        assert!(Encoding::Alternate.selector().contains("ext!=m4a"));
        assert_ne!(Encoding::Preferred.selector(), Encoding::Alternate.selector());
    }

    #[test]
    fn a_cached_url_is_returned_without_resolving() {
        let cache = StreamUrlCache::default();
        cache.store("page", Encoding::Preferred, YT.to_string(), at(1_000_000_000));

        assert_eq!(
            cache.lookup("page", Encoding::Preferred, at(1_000_000_100)),
            Some(YT.to_string())
        );
    }

    /// The whole point: the entry must stop being used before the link dies,
    /// not after.
    #[test]
    fn an_entry_stops_being_used_before_its_link_expires() {
        let cache = StreamUrlCache::default();
        cache.store("page", Encoding::Preferred, YT.to_string(), at(1_000_000_000));

        // Inside the margin, so already treated as unusable.
        let just_before = at(2_000_000_000 - SAFETY_MARGIN.as_secs() + 1);
        assert_eq!(cache.lookup("page", Encoding::Preferred, just_before), None);

        // And comfortably before it, still fine.
        let well_before = at(2_000_000_000 - SAFETY_MARGIN.as_secs() - 60);
        assert_eq!(cache.lookup("page", Encoding::Preferred, well_before), Some(YT.to_string()));
    }

    #[test]
    fn a_url_without_an_expiry_gets_the_default_lifetime() {
        let cache = StreamUrlCache::default();
        let plain = "https://example.test/audio.m4a";
        cache.store("page", Encoding::Preferred, plain.to_string(), at(1_000));

        assert!(cache.lookup("page", Encoding::Preferred, at(1_000 + 60)).is_some());
        assert_eq!(
            cache.lookup("page", Encoding::Preferred, at(1_000 + DEFAULT_TTL.as_secs() + 1)),
            None,
            "an unknown policy must not be trusted indefinitely"
        );
    }

    /// A link that is already within the margin when stored must not be
    /// resurrected by the subtraction underflowing.
    #[test]
    fn an_almost_dead_link_is_not_treated_as_fresh() {
        let cache = StreamUrlCache::default();
        let now = at(2_000_000_000 - 10);
        cache.store("page", Encoding::Preferred, YT.to_string(), now);

        assert_eq!(cache.lookup("page", Encoding::Preferred, now), None);
    }

    #[test]
    fn invalidating_reports_whether_anything_was_cached() {
        let cache = StreamUrlCache::default();
        cache.store("page", Encoding::Preferred, YT.to_string(), at(1_000_000_000));

        assert!(cache.invalidate("page"), "there was an entry");
        assert!(!cache.invalidate("page"), "and now there is not");
        assert_eq!(cache.lookup("page", Encoding::Preferred, at(1_000_000_100)), None);
    }

    /// Two tracks must not share a cached URL.
    #[test]
    fn entries_are_keyed_per_track() {
        let cache = StreamUrlCache::default();
        cache.store("one", Encoding::Preferred, YT.to_string(), at(1_000_000_000));
        cache.store("two", Encoding::Preferred, SC.to_string(), at(1_000_000_000));

        assert_eq!(cache.lookup("one", Encoding::Preferred, at(1_000_000_100)), Some(YT.to_string()));
        assert_eq!(cache.lookup("two", Encoding::Preferred, at(1_000_000_100)), Some(SC.to_string()));
    }
}
