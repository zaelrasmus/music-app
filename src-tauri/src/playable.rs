use std::path::PathBuf;

use sqlx::FromRow;

/// The row `get_playable_source` needs. Deliberately narrow -- resolution must
/// not depend on display metadata.
#[derive(Debug, FromRow)]
pub struct PlayableTrack {
    pub source: String,
    pub state: String,
    pub local_path: Option<String>,
    pub yt_video_id: Option<String>,
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
    /// The URL is resolved fresh on every play and never stored: YouTube's
    /// stream URLs carry an `expire` timestamp and are bound to the requesting
    /// IP, so a cached one is wrong by tomorrow.
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
) -> Result<PlayableSource, String> {
    match track.source.as_str() {
        "local" => {
            if track.state == "missing" {
                return Err(
                    "That file is missing from disk. Rescan the library or reconnect the drive."
                        .to_string(),
                );
            }

            let path = track
                .local_path
                .as_deref()
                // The schema's CHECK guarantees local rows have a path, so this
                // is a corrupted row rather than an expected condition.
                .ok_or("That track has no file path recorded.")?;

            let path = PathBuf::from(path);

            // The seam decides *how* to decode, not just what to play. This is
            // the one place that knowledge lives, so the engine stays dumb and
            // the YouTube branch will slot in beside it.
            if crate::transcode::needs_transcode(&path) {
                Ok(PlayableSource::Transcoded(path))
            } else {
                Ok(PlayableSource::LocalFile(path))
            }
        }
        "youtube" => {
            // Downloaded: there is a real file, so it plays exactly like a
            // local track and needs no network at all.
            if let Some(path) = track.local_path.as_deref() {
                let path = PathBuf::from(path);
                return Ok(if crate::transcode::needs_transcode(&path) {
                    PlayableSource::Transcoded(path)
                } else {
                    PlayableSource::LocalFile(path)
                });
            }

            // Saved: metadata only. The audio has to be fetched now.
            let video_id = track
                .yt_video_id
                .as_deref()
                // The schema's CHECK guarantees this, so a missing id is a
                // corrupted row rather than an expected state.
                .ok_or("That track has no YouTube id recorded.")?;

            let yt_dlp = yt_dlp.ok_or(
                "yt-dlp was not found, so saved YouTube tracks cannot be played. \
                 See src-tauri/binaries/README.md.",
            )?;

            let url = crate::youtube::resolve_stream_url(yt_dlp, video_id).await?;
            Ok(PlayableSource::Stream(url))
        }
        other => Err(format!("Unknown track source \"{other}\".")),
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
            yt_video_id: None,
        }
    }

    fn youtube_track(state: &str, path: Option<&str>, video_id: Option<&str>) -> PlayableTrack {
        PlayableTrack {
            source: "youtube".to_string(),
            state: state.to_string(),
            local_path: path.map(str::to_string),
            yt_video_id: video_id.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn a_present_local_track_resolves_to_its_file() {
        let resolved = get_playable_source(&track("local", "present", Some(r"D:\a.mp3")), None)
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
        let resolved = get_playable_source(&track("local", "present", Some(r"D:\a.opus")), None)
            .await
            .expect("should resolve");

        assert!(
            matches!(resolved, PlayableSource::Transcoded(_)),
            "opus has no rodio codec, got {resolved:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_local_track_fails_with_a_clear_message() {
        let error = get_playable_source(&track("local", "missing", Some(r"D:\a.mp3")), None)
            .await
            .expect_err("missing tracks must not resolve");
        assert!(error.contains("missing from disk"), "got: {error}");
    }

    /// A downloaded YouTube track is just a file, and must never touch the
    /// network -- that is the whole point of downloading it.
    #[tokio::test]
    async fn a_downloaded_youtube_track_plays_from_disk() {
        let resolved = youtube_track("downloaded", Some(r"D:\yt\abc.m4a"), Some("dQw4w9WgXcQ"));
        let resolved = get_playable_source(&resolved, None)
            .await
            .expect("a downloaded track needs no yt-dlp");

        match resolved {
            PlayableSource::LocalFile(path) => assert_eq!(path, PathBuf::from(r"D:\yt\abc.m4a")),
            other => panic!("m4a is native to rodio, got {other:?}"),
        }
    }

    /// A downloaded Opus file still routes through ffmpeg.
    #[tokio::test]
    async fn a_downloaded_opus_track_still_transcodes() {
        let resolved = youtube_track("downloaded", Some(r"D:\yt\abc.opus"), Some("dQw4w9WgXcQ"));
        let resolved = get_playable_source(&resolved, None).await.expect("resolves");

        assert!(matches!(resolved, PlayableSource::Transcoded(_)));
    }

    #[tokio::test]
    async fn a_saved_youtube_track_needs_yt_dlp() {
        let error = get_playable_source(&youtube_track("saved", None, Some("dQw4w9WgXcQ")), None)
            .await
            .expect_err("streaming without yt-dlp cannot work");

        assert!(error.contains("yt-dlp was not found"), "got: {error}");
    }

    #[tokio::test]
    async fn a_youtube_row_without_a_video_id_is_reported_as_corrupt() {
        let error = get_playable_source(&youtube_track("saved", None, None), None)
            .await
            .expect_err("a saved track with no id is unplayable");

        assert!(error.contains("no YouTube id"), "got: {error}");
    }
}
