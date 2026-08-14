use std::path::PathBuf;

use sqlx::FromRow;

/// The row `get_playable_source` needs. Deliberately narrow -- resolution must
/// not depend on display metadata.
#[derive(Debug, FromRow)]
pub struct PlayableTrack {
    pub source: String,
    pub state: String,
    pub local_path: Option<String>,
}

/// Something the audio thread knows how to play.
///
/// Only a local file today. When YouTube streaming lands it becomes another
/// variant here, and the player learns one new arm -- nothing else moves.
#[derive(Debug, Clone)]
pub enum PlayableSource {
    LocalFile(PathBuf),
}

/// The single seam between "a track in the database" and "audio to play".
///
/// Every playback path routes through here. Keeping it one small function is
/// the point: it is where streaming, transcoding, or a cache lookup will be
/// added later without the player or the commands changing shape.
pub fn get_playable_source(track: &PlayableTrack) -> Result<PlayableSource, String> {
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

            Ok(PlayableSource::LocalFile(PathBuf::from(path)))
        }
        "youtube" => Err("YouTube playback is not implemented yet.".to_string()),
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
        }
    }

    #[test]
    fn a_present_local_track_resolves_to_its_file() {
        let resolved = get_playable_source(&track("local", "present", Some(r"D:\a.mp3")))
            .expect("should resolve");
        let PlayableSource::LocalFile(path) = resolved;
        assert_eq!(path, PathBuf::from(r"D:\a.mp3"));
    }

    #[test]
    fn a_missing_local_track_fails_with_a_clear_message() {
        let error = get_playable_source(&track("local", "missing", Some(r"D:\a.mp3")))
            .expect_err("missing tracks must not resolve");
        assert!(error.contains("missing from disk"), "got: {error}");
    }

    #[test]
    fn youtube_tracks_are_rejected_until_that_phase_lands() {
        let error = get_playable_source(&track("youtube", "saved", None))
            .expect_err("youtube is not playable yet");
        assert!(error.contains("not implemented"), "got: {error}");
    }
}
