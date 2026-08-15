use std::path::PathBuf;

use sqlx::FromRow;

use crate::providers::Provider;
use crate::stream_urls::StreamUrlCache;

/// The row `get_playable_source` needs. Deliberately narrow -- resolution must
/// not depend on display metadata.
#[derive(Debug, FromRow)]
pub struct PlayableTrack {
    pub source: String,
    pub state: String,
    pub local_path: Option<String>,
    /// The provider's page for this track. Stored rather than derived: a
    /// SoundCloud URL cannot be rebuilt from its numeric id.
    pub remote_url: Option<String>,
}

/// Something the audio thread knows how to play.
///
/// Only a local file today. When YouTube streaming lands it becomes another
/// variant here, and the player learns one new arm -- nothing else moves.
#[derive(Debug, Clone)]
pub enum PlayableSource {
    /// rodio decodes this itself.
    LocalFile(PathBuf),
    /// rodio has no codec for this, so ffmpeg decodes it instead. Today that
    /// means Opus; the same path will carry YouTube streams.
    Transcoded(PathBuf),
    /// A remote audio stream, decoded by ffmpeg.
    ///
    /// Always ffmpeg, never rodio: rodio's decoder needs `Read + Seek` and a
    /// URL is not seekable, so the container format is irrelevant here. (The
    /// preference for m4a still matters when *downloading*, where it avoids a
    /// re-encode.)
    ///
    /// The URL is never persisted: it carries an `expire` timestamp and is
    /// bound to the requesting IP, so one written to the database would be
    /// wrong by tomorrow. It *is* cached in memory for its stated lifetime --
    /// see [`crate::stream_urls`] -- which is what keeps a replay from costing
    /// another seven seconds.
    Stream(String),
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
) -> Result<PlayableSource, String> {
    if track.source != "local" {
        // Any non-local source is a provider. Unknown ones fail here rather
        // than falling through to a "local" path that would try to open a
        // file that does not exist.
        let provider = Provider::from_source(&track.source)
            .ok_or_else(|| format!("Unknown track source \"{}\".", track.source))?;

        // Downloaded: there is a real file, so it plays exactly like a local
        // track and needs no network at all.
        if let Some(path) = track.local_path.as_deref() {
            return Ok(decode_route(PathBuf::from(path)));
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
        let stream = stream_urls.resolve(yt_dlp, url).await?;
        return Ok(PlayableSource::Stream(stream));
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

    Ok(decode_route(PathBuf::from(path)))
}

/// Decides *how* a file is decoded, not just what to play.
///
/// The one place that knowledge lives, which is what keeps the engine dumb --
/// and why a downloaded remote track needs no special handling at all.
fn decode_route(path: PathBuf) -> PlayableSource {
    if crate::transcode::needs_transcode(&path) {
        PlayableSource::Transcoded(path)
    } else {
        PlayableSource::LocalFile(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(source: &str, state: &str, path: Option<&str>) -> PlayableTrack {
        PlayableTrack {
            source: source.to_string(),
            state: state.to_string(),
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
            local_path: path.map(str::to_string),
            remote_url: url.map(str::to_string),
        }
    }

    const YT_URL: &str = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
    const SC_URL: &str = "https://soundcloud.com/daft-punk-id/daft-punk-one-more-time";

    #[tokio::test]
    async fn a_present_local_track_resolves_to_its_file() {
        let resolved = get_playable_source(&track("local", "present", Some(r"D:\a.mp3")), None, &StreamUrlCache::default())
            .await
            .expect("should resolve");

        match resolved {
            PlayableSource::LocalFile(path) => assert_eq!(path, PathBuf::from(r"D:\a.mp3")),
            other => panic!("mp3 is native to rodio, got {other:?}"),
        }
    }

    /// The seam decides the decode route, so an Opus file must come back
    /// marked for ffmpeg rather than handed to rodio.
    #[tokio::test]
    async fn an_opus_track_resolves_to_the_transcode_route() {
        let resolved = get_playable_source(&track("local", "present", Some(r"D:\a.opus")), None, &StreamUrlCache::default())
            .await
            .expect("should resolve");

        assert!(
            matches!(resolved, PlayableSource::Transcoded(_)),
            "opus has no rodio codec, got {resolved:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_local_track_fails_with_a_clear_message() {
        let error = get_playable_source(&track("local", "missing", Some(r"D:\a.mp3")), None, &StreamUrlCache::default())
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

            let resolved = get_playable_source(&track, None, &StreamUrlCache::default())
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

    /// A downloaded Opus file still routes through ffmpeg.
    #[tokio::test]
    async fn a_downloaded_opus_track_still_transcodes() {
        let track = remote_track("youtube", "downloaded", Some(r"D:\yt\abc.opus"), Some(YT_URL));
        let resolved = get_playable_source(&track, None, &StreamUrlCache::default()).await.expect("resolves");

        assert!(matches!(resolved, PlayableSource::Transcoded(_)));
    }

    #[tokio::test]
    async fn a_saved_remote_track_needs_yt_dlp() {
        for (source, url) in [("youtube", YT_URL), ("soundcloud", SC_URL)] {
            let error = get_playable_source(&remote_track(source, "saved", None, Some(url)), None, &StreamUrlCache::default())
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
        )
        .await
        .expect_err("no yt-dlp");

        assert!(error.contains("SoundCloud"), "got: {error}");
    }

    #[tokio::test]
    async fn a_remote_row_without_a_url_is_reported_as_corrupt() {
        let error = get_playable_source(&remote_track("youtube", "saved", None, None), None, &StreamUrlCache::default())
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
        )
        .await
        .expect_err("mismatched URL must not resolve");

        assert!(error.contains("does not look valid"), "got: {error}");
    }

    #[tokio::test]
    async fn an_unknown_source_fails_instead_of_being_treated_as_local() {
        let error = get_playable_source(&remote_track("bandcamp", "saved", None, None), None, &StreamUrlCache::default())
            .await
            .expect_err("an unknown provider cannot resolve");

        assert!(error.contains("Unknown track source"), "got: {error}");
    }
}
