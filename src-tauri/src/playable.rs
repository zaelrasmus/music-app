use std::path::PathBuf;

use sqlx::FromRow;

use crate::audio_cache::{AudioCache, PendingCache};
use crate::providers::Provider;
use crate::stream_urls::{Encoding, StreamUrlCache};

/// The row `get_playable_source` needs. Deliberately narrow -- resolution must
/// not depend on display metadata.
#[derive(Debug, FromRow)]
pub struct PlayableTrack {
    pub source: String,
    pub state: String,
    pub remote_id: Option<String>,
    pub local_path: Option<String>,
    /// The provider's page for this track. Stored rather than derived: a
    /// SoundCloud URL cannot be rebuilt from its numeric id.
    pub remote_url: Option<String>,
}

/// Something the audio thread knows how to play.
///
/// Every variant is decoded by ffmpeg. What separates them is not *how* they
/// are decoded but what they are worth: a file the user owns, a copy this app
/// made and may throw away, or a URL that expires.
///
/// There used to be a fourth -- `Transcoded` -- for the files rodio could not
/// decode natively. It is gone because the distinction it drew is gone: rodio
/// no longer decodes anything.
#[derive(Debug, Clone)]
pub enum PlayableSource {
    /// A file the user owns, wherever it came from.
    ///
    /// The user's, which is the whole of what this variant means: if it will
    /// not decode, that is worth reporting rather than quietly discarding.
    LocalFile(PathBuf),
    /// A disposable copy the app made of a stream it already played.
    ///
    /// Decoded exactly like [`Self::LocalFile`]; the distinction is
    /// *provenance*. This file is the app's own, it is worth nothing, and
    /// it can be wrong -- a copy written from an interrupted stream decodes
    /// happily until it reaches the damage. Naming it separately is what
    /// lets the player throw it away and go back to the provider instead of
    /// treating a track as unplayable forever.
    Cached(PathBuf),
    /// A remote audio stream.
    ///
    /// The container format is irrelevant here, as it now is everywhere. (The
    /// preference for m4a still matters when *downloading*, where it avoids a
    /// re-encode.)
    ///
    /// The URL is never persisted: it carries an `expire` timestamp and is
    /// bound to the requesting IP, so one written to the database would be
    /// wrong by tomorrow. It *is* cached in memory for its stated lifetime --
    /// see [`crate::stream_urls`] -- which is what keeps a replay from costing
    /// another seven seconds.
    Stream {
        url: String,
        /// Where to keep a copy, when this is worth caching.
        cache: Option<PendingCache>,
    },
}

/// The single seam between "a track in the database" and "audio to play".
///
/// Every playback path routes through here. Keeping it one small function is
/// the point: it is where streaming, transcoding, or a cache lookup will be
/// added later without the player or the commands changing shape.
pub async fn get_playable_source(
    track: &PlayableTrack,
    yt_dlp: Option<&std::path::Path>,
    stream_urls: &StreamUrlCache,
    audio_cache: Option<&AudioCache>,
    encoding: Encoding,
) -> Result<PlayableSource, String> {
    if track.source != "local" {
        // Any non-local source is a provider. Unknown ones fail here rather
        // than falling through to a "local" path that would try to open a
        // file that does not exist.
        let provider = Provider::from_source(&track.source)
            .ok_or_else(|| format!("Unknown track source \"{}\".", track.source))?;

        // Downloaded: there is a real file, so it plays exactly like a local
        // track and needs no network at all. The user asked for this one to be
        // kept, so it is theirs -- not something to discard on a bad decode.
        if let Some(path) = track.local_path.as_deref() {
            return Ok(PlayableSource::LocalFile(PathBuf::from(path)));
        }

        // Cached from an earlier play: the bytes are already on disk, so this
        // needs no network at all -- and seeking backwards through it is a
        // local seek rather than another trip to the provider.
        let cached = track
            .remote_id
            .as_deref()
            .zip(audio_cache)
            .and_then(|(id, cache)| cache.lookup(&track.source, id));
        if let Some(path) = cached {
            return Ok(PlayableSource::Cached(path));
        }

        // Saved: metadata only. The audio has to be fetched now.
        let url = track
            .remote_url
            .as_deref()
            // The schema's CHECK guarantees this, so its absence is a
            // corrupted row rather than an expected state.
            .ok_or("That track has no source URL recorded.")?;

        if !provider.accepts_url(url) {
            return Err(format!(
                "That track's stored {} link does not look valid.",
                provider.display_name()
            ));
        }

        let yt_dlp = yt_dlp.ok_or_else(|| {
            format!(
                "yt-dlp was not found, so saved {} tracks cannot be played. \
                 See src-tauri/binaries/README.md.",
                provider.display_name()
            )
        })?;

        // Cached where possible: resolving costs a yt-dlp process and ~7
        // seconds, and the URLs stay valid for hours.
        let stream = stream_urls.resolve(yt_dlp, url, encoding).await?;

        // Reserved now so the decode can write the cache copy as it plays.
        let cache = track
            .remote_id
            .as_deref()
            .zip(audio_cache)
            .and_then(|(id, cache)| cache.reserve(&track.source, id));

        return Ok(PlayableSource::Stream { url: stream, cache });
    }

    // Local file.
    if track.state == "missing" {
        return Err(
            "That file is missing from disk. Rescan the library or reconnect the drive."
                .to_string(),
        );
    }

    let path = track
        .local_path
        .as_deref()
        // The schema's CHECK guarantees local rows have a path, so this is a
        // corrupted row rather than an expected condition.
        .ok_or("That track has no file path recorded.")?;

    // No route to choose any more. ffmpeg decodes every format this app plays,
    // so the extension no longer decides anything -- which is also why a
    // downloaded remote track needs no special handling here.
    Ok(PlayableSource::LocalFile(PathBuf::from(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(source: &str, state: &str, path: Option<&str>) -> PlayableTrack {
        PlayableTrack {
            source: source.to_string(),
            state: state.to_string(),
            remote_id: None,
            local_path: path.map(str::to_string),
            remote_url: None,
        }
    }

    fn remote_track(
        source: &str,
        state: &str,
        path: Option<&str>,
        url: Option<&str>,
    ) -> PlayableTrack {
        PlayableTrack {
            source: source.to_string(),
            state: state.to_string(),
            remote_id: Some("dQw4w9WgXcQ".to_string()),
            local_path: path.map(str::to_string),
            remote_url: url.map(str::to_string),
        }
    }

    const YT_URL: &str = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
    const SC_URL: &str = "https://soundcloud.com/daft-punk-id/daft-punk-one-more-time";

    #[tokio::test]
    async fn a_present_local_track_resolves_to_its_file() {
        let resolved = get_playable_source(&track("local", "present", Some(r"D:\a.mp3")), None, &StreamUrlCache::default(), None, Encoding::Preferred)
            .await
            .expect("should resolve");

        match resolved {
            PlayableSource::LocalFile(path) => assert_eq!(path, PathBuf::from(r"D:\a.mp3")),
            other => panic!("mp3 is native to rodio, got {other:?}"),
        }
    }

    /// Opus is no longer a special case, and that is the point.
    ///
    /// It used to need its own route because rodio had no codec for it, and
    /// the sniffing that decided which files took that route was a standing
    /// source of "this one format does not play". ffmpeg decodes every format
    /// this app plays, so an Opus file resolves exactly like an MP3.
    #[tokio::test]
    async fn an_opus_track_is_no_longer_a_special_case() {
        let resolved = get_playable_source(&track("local", "present", Some(r"D:\a.opus")), None, &StreamUrlCache::default(), None, Encoding::Preferred)
            .await
            .expect("should resolve");

        match resolved {
            PlayableSource::LocalFile(path) => assert_eq!(path, PathBuf::from(r"D:\a.opus")),
            other => panic!("opus should resolve like any other file, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_missing_local_track_fails_with_a_clear_message() {
        let error = get_playable_source(&track("local", "missing", Some(r"D:\a.mp3")), None, &StreamUrlCache::default(), None, Encoding::Preferred)
            .await
            .expect_err("missing tracks must not resolve");
        assert!(error.contains("missing from disk"), "got: {error}");
    }

    /// A downloaded remote track is just a file, and must never touch the
    /// network -- that is the whole point of downloading it.
    #[tokio::test]
    async fn a_downloaded_remote_track_plays_from_disk() {
        for (source, url) in [("youtube", YT_URL), ("soundcloud", SC_URL)] {
            let track = remote_track(source, "downloaded", Some(r"D:\yt\abc.m4a"), Some(url));

            let resolved = get_playable_source(&track, None, &StreamUrlCache::default(), None, Encoding::Preferred)
                .await
                .expect("a downloaded track needs no yt-dlp");

            match resolved {
                PlayableSource::LocalFile(path) => {
                    assert_eq!(path, PathBuf::from(r"D:\yt\abc.m4a"))
                }
                other => panic!("m4a is native to rodio, got {other:?} for {source}"),
            }
        }
    }

    /// A downloaded Opus track plays from its file, not from the network.
    ///
    /// The file is what matters here: a downloaded track must never go back to
    /// the provider, whatever its format. Being `LocalFile` rather than
    /// `Cached` also says it is the user's -- a bad decode is worth reporting,
    /// not silently discarding the way a cache copy would be.
    #[tokio::test]
    async fn a_downloaded_opus_track_plays_from_disk() {
        let track = remote_track("youtube", "downloaded", Some(r"D:\yt\abc.opus"), Some(YT_URL));
        let resolved = get_playable_source(&track, None, &StreamUrlCache::default(), None, Encoding::Preferred).await.expect("resolves");

        match resolved {
            PlayableSource::LocalFile(path) => assert_eq!(path, PathBuf::from(r"D:\yt\abc.opus")),
            other => panic!("a downloaded file must play from disk, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_saved_remote_track_needs_yt_dlp() {
        for (source, url) in [("youtube", YT_URL), ("soundcloud", SC_URL)] {
            let error = get_playable_source(&remote_track(source, "saved", None, Some(url)), None, &StreamUrlCache::default(), None, Encoding::Preferred)
                .await
                .expect_err("streaming without yt-dlp cannot work");

            assert!(error.contains("yt-dlp was not found"), "got: {error}");
        }
    }

    /// The message names the provider, so "SoundCloud tracks cannot be played"
    /// does not read as a YouTube problem.
    #[tokio::test]
    async fn the_missing_tool_message_names_the_provider() {
        let error = get_playable_source(
            &remote_track("soundcloud", "saved", None, Some(SC_URL)),
            None,
            &StreamUrlCache::default(),
            None,
            Encoding::Preferred,
        )
        .await
        .expect_err("no yt-dlp");

        assert!(error.contains("SoundCloud"), "got: {error}");
    }

    #[tokio::test]
    async fn a_remote_row_without_a_url_is_reported_as_corrupt() {
        let error = get_playable_source(&remote_track("youtube", "saved", None, None), None, &StreamUrlCache::default(), None, Encoding::Preferred)
            .await
            .expect_err("a saved track with no URL is unplayable");

        assert!(error.contains("no source URL"), "got: {error}");
    }

    /// A stored URL is checked against its own provider before being handed to
    /// yt-dlp. A row claiming to be SoundCloud but pointing elsewhere is
    /// corrupt, and must not be fetched.
    #[tokio::test]
    async fn a_url_that_does_not_match_its_provider_is_refused() {
        let error = get_playable_source(
            &remote_track("soundcloud", "saved", None, Some(YT_URL)),
            Some(std::path::Path::new("yt-dlp")),
        &StreamUrlCache::default(),
        None,
        Encoding::Preferred,
        )
        .await
        .expect_err("mismatched URL must not resolve");

        assert!(error.contains("does not look valid"), "got: {error}");
    }

    #[tokio::test]
    async fn an_unknown_source_fails_instead_of_being_treated_as_local() {
        let error = get_playable_source(&remote_track("bandcamp", "saved", None, None), None, &StreamUrlCache::default(), None, Encoding::Preferred)
            .await
            .expect_err("an unknown provider cannot resolve");

        assert!(error.contains("Unknown track source"), "got: {error}");
    }
}
