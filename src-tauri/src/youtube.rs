use std::process::Command;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::sidecar::{self, Tool};

/// Metadata for a single video, as `yt-dlp -J` reports it.
///
/// Everything except `id` is optional on purpose: yt-dlp omits fields freely
/// (no duration on a live stream, no view count in flat listings), and a
/// missing field must never fail the whole parse.
/// The renames are direction-specific for a reason: yt-dlp speaks snake_case
/// (`view_count`), the frontend expects camelCase (`viewCount`). A single
/// `rename_all = "camelCase"` would apply to *both* directions, so `view_count`
/// would silently fail to match and, with `#[serde(default)]`, quietly become
/// `None` rather than erroring.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct VideoMetadata {
    #[serde(rename(serialize = "videoId", deserialize = "id"))]
    pub video_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub view_count: Option<u64>,
    #[serde(default)]
    pub thumbnail: Option<String>,
}

/// Runs yt-dlp with `args` and returns stdout.
///
/// Uses `std::process` rather than the shell plugin's sidecar API deliberately:
/// that API delivers output as line-oriented events, which is fine for JSON but
/// unusable for the raw PCM byte stream ffmpeg will produce. Keeping one
/// spawning mechanism for both tools avoids two ways of doing the same thing.
async fn run(app: &AppHandle, args: Vec<String>) -> Result<String, String> {
    let tool = sidecar::resolve(app, Tool::YtDlp)?;
    run_at(tool.path, args).await
}

/// Same, but for callers that already know where yt-dlp lives.
///
/// Playback resolution runs on the coordinator, which deliberately has no
/// `AppHandle`, so it is handed the path at startup instead.
async fn run_at(yt_dlp: std::path::PathBuf, args: Vec<String>) -> Result<String, String> {
    // yt-dlp takes seconds -- ~4 for a search, ~7 to resolve a stream -- and is
    // a blocking process spawn. Running it inline would park a runtime worker
    // for that whole time, stalling every other command including playback.
    let output = tauri::async_runtime::spawn_blocking(move || {
        Command::new(&yt_dlp).args(&args).output()
    })
    .await
    .map_err(|e| format!("yt-dlp task failed: {e}"))?
    .map_err(|e| format!("Could not start yt-dlp: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(explain(&stderr));
    }

    String::from_utf8(output.stdout).map_err(|_| "yt-dlp returned invalid UTF-8.".to_string())
}

/// Turns yt-dlp's stderr into something worth showing a user.
///
/// Raw stderr is long, noisy, and mentions flags the user never typed, so the
/// common failures get a plain sentence and everything else falls back to the
/// last meaningful line.
pub(crate) fn explain(stderr: &str) -> String {
    let lowered = stderr.to_lowercase();

    if lowered.contains("sign in to confirm your age") || lowered.contains("age-restricted") {
        return "That video is age-restricted and needs a signed-in account.".to_string();
    }
    if lowered.contains("video unavailable") || lowered.contains("has been removed") {
        return "That video is no longer available.".to_string();
    }
    if lowered.contains("private video") {
        return "That video is private.".to_string();
    }
    if lowered.contains("not available in your country") || lowered.contains("geo") {
        return "That video is blocked in your region.".to_string();
    }
    if lowered.contains("failed to resolve")
        || lowered.contains("temporary failure in name resolution")
        || lowered.contains("unable to download webpage")
    {
        return "Could not reach YouTube. Check your internet connection.".to_string();
    }

    stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .next_back()
        .unwrap_or("yt-dlp failed.")
        .trim()
        .to_string()
}

/// Which copy of yt-dlp is in use, and its version.
///
/// Temporary: exists to prove the sidecar wiring end to end before any search
/// UI is built. Remove once Part C lands.
#[tauri::command]
pub async fn debug_yt_dlp_version(app: AppHandle) -> Result<String, String> {
    let tool = sidecar::resolve(&app, Tool::YtDlp)?;
    let version = run(&app, vec!["--version".to_string()]).await?;

    Ok(format!(
        "{} ({:?} at {})",
        version.trim(),
        tool.origin,
        tool.path.display()
    ))
}

/// Fetches metadata for one known video id.
///
/// Temporary, same reason as above: the smallest real call that proves yt-dlp
/// runs, reaches YouTube, and that the JSON shape parses.
#[tauri::command]
pub async fn debug_video_metadata(
    app: AppHandle,
    video_id: String,
) -> Result<VideoMetadata, String> {
    // Reject anything that is not a plain video id, so this can never be
    // coaxed into passing arbitrary flags to yt-dlp.
    if !is_video_id(&video_id) {
        return Err("That is not a valid YouTube video id.".to_string());
    }

    let url = format!("https://www.youtube.com/watch?v={video_id}");
    let json = run(
        &app,
        vec![
            "-J".to_string(),
            "--no-warnings".to_string(),
            "--no-playlist".to_string(),
            // Metadata only: no format resolution, which is the slow part.
            "--skip-download".to_string(),
            url,
        ],
    )
    .await?;

    serde_json::from_str(&json).map_err(|e| format!("Could not parse yt-dlp output: {e}"))
}

// --- raw search --------------------------------------------------------

/// Upper bound on results. yt-dlp fetches these serially, so a large number
/// mostly buys waiting.
const MAX_RESULTS: u32 = 25;

/// One row in the results list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub video_id: String,
    /// The raw upload title, untouched. Picking a song out of a ten-hour loop
    /// or a full-album rip depends on seeing exactly what was uploaded.
    pub title: String,
    pub channel: Option<String>,
    pub duration_secs: Option<f64>,
    pub view_count: Option<u64>,
    pub thumbnail_url: Option<String>,
    /// Live streams have no duration, so the UI shows this instead of a blank.
    pub is_live: bool,
}

/// `--flat-playlist` returns a playlist envelope around the entries.
#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    entries: Vec<Option<SearchEntry>>,
}

/// Only `id` is guaranteed. Entries can even be `null` when a video became
/// unavailable between indexing and the query, hence `Vec<Option<_>>` above.
#[derive(Debug, Deserialize)]
struct SearchEntry {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    uploader: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    view_count: Option<u64>,
    #[serde(default)]
    live_status: Option<String>,
    #[serde(default)]
    thumbnails: Vec<Thumbnail>,
}

#[derive(Debug, Deserialize)]
struct Thumbnail {
    url: String,
    #[serde(default)]
    width: Option<u32>,
}

impl SearchEntry {
    fn normalize(self) -> SearchResult {
        let is_live = self.live_status.as_deref() == Some("is_live");

        SearchResult {
            video_id: self.id,
            title: self.title.unwrap_or_else(|| "(untitled)".to_string()),
            // `channel` is the canonical name; `uploader` is the older field
            // and is sometimes the only one present.
            channel: self.channel.or(self.uploader),
            duration_secs: self.duration,
            view_count: self.view_count,
            thumbnail_url: pick_thumbnail(&self.thumbnails),
            is_live,
        }
    }
}

/// Chooses a list-sized thumbnail.
///
/// yt-dlp returns them smallest-first. The widest one under the cap keeps the
/// list crisp without pulling a 1280px image per row.
fn pick_thumbnail(thumbnails: &[Thumbnail]) -> Option<String> {
    const MAX_WIDTH: u32 = 480;

    thumbnails
        .iter()
        .filter(|t| t.width.is_none_or(|w| w <= MAX_WIDTH))
        .next_back()
        .or_else(|| thumbnails.first())
        .map(|t| t.url.clone())
}

/// Raw YouTube search.
///
/// Deliberately `--flat-playlist`: it returns listing metadata only. Resolving
/// each video's formats during a search would multiply the runtime by the
/// result count for data nothing displays.
#[tauri::command]
pub async fn search_youtube(
    app: AppHandle,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<SearchResult>, String> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let limit = limit.unwrap_or(10).clamp(1, MAX_RESULTS);

    // The query is one argv entry embedded after `ytsearchN:`, and no shell is
    // involved, so it cannot become a flag or a second argument.
    let json = run(
        &app,
        vec![
            format!("ytsearch{limit}:{query}"),
            "--flat-playlist".to_string(),
            "-J".to_string(),
            "--no-warnings".to_string(),
        ],
    )
    .await?;

    let response: SearchResponse =
        serde_json::from_str(&json).map_err(|e| format!("Could not parse search results: {e}"))?;

    Ok(response
        .entries
        .into_iter()
        .flatten()
        .map(SearchEntry::normalize)
        .collect())
}

/// Resolves a directly playable audio stream URL for `video_id`.
///
/// Called on every play and never cached. The URLs YouTube hands back carry an
/// `expire` timestamp and are tied to the requesting IP, so storing one would
/// produce a track that plays today and fails tomorrow for no visible reason.
///
/// `bestaudio[ext=m4a]` first because AAC is cheap to decode and widely
/// available; the bare `bestaudio` fallback is usually Opus, which ffmpeg
/// handles anyway.
pub async fn resolve_stream_url(
    yt_dlp: &std::path::Path,
    video_id: &str,
) -> Result<String, String> {
    if !is_video_id(video_id) {
        return Err("That is not a valid YouTube video id.".to_string());
    }

    let stdout = run_at(
        yt_dlp.to_path_buf(),
        vec![
            "-f".to_string(),
            "bestaudio[ext=m4a]/bestaudio".to_string(),
            // Print the URL instead of downloading.
            "-g".to_string(),
            "--no-warnings".to_string(),
            "--no-playlist".to_string(),
            format!("https://www.youtube.com/watch?v={video_id}"),
        ],
    )
    .await?;

    stdout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("http"))
        .map(str::to_string)
        .ok_or_else(|| "yt-dlp returned no playable stream for that video.".to_string())
}

/// YouTube ids are 11 characters of `[A-Za-z0-9_-]`.
fn is_video_id(candidate: &str) -> bool {
    candidate.len() == 11
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_video_ids_are_accepted() {
        assert!(is_video_id("dQw4w9WgXcQ"));
        assert!(is_video_id("_-aB3cD4eF5"));
    }

    #[test]
    fn anything_that_could_smuggle_a_flag_is_rejected() {
        assert!(!is_video_id("--version"));
        assert!(!is_video_id("dQw4w9WgXc"), "too short");
        assert!(!is_video_id("dQw4w9WgXcQQ"), "too long");
        assert!(!is_video_id("dQw4 9WgXcQ"), "contains a space");
        assert!(!is_video_id("../../etc/pw"));
    }

    /// Guards the snake_case/camelCase split. A `rename_all = "camelCase"` on
    /// the container would make `view_count` miss and silently default to
    /// `None`, which is invisible until someone notices every view count is
    /// blank.
    #[test]
    fn real_yt_dlp_field_names_deserialize() {
        let json = r#"{
            "id": "dQw4w9WgXcQ",
            "title": "Never Gonna Give You Up",
            "channel": "Rick Astley",
            "duration": 213.0,
            "view_count": 1803881842,
            "thumbnail": "https://i.ytimg.com/vi/dQw4w9WgXcQ/maxresdefault.jpg"
        }"#;

        let meta: VideoMetadata = serde_json::from_str(json).expect("should parse");

        assert_eq!(meta.video_id, "dQw4w9WgXcQ");
        assert_eq!(meta.channel.as_deref(), Some("Rick Astley"));
        assert_eq!(meta.duration, Some(213.0));
        assert_eq!(
            meta.view_count,
            Some(1_803_881_842),
            "view_count is snake_case in yt-dlp output"
        );
    }

    /// Live streams have no duration, flat listings often have no view count.
    #[test]
    fn missing_optional_fields_do_not_fail_the_parse() {
        let json = r#"{ "id": "dQw4w9WgXcQ", "title": "Live" }"#;
        let meta: VideoMetadata = serde_json::from_str(json).expect("should parse");

        assert_eq!(meta.duration, None);
        assert_eq!(meta.view_count, None);
        assert_eq!(meta.channel, None);
    }

    /// The frontend sees camelCase even though yt-dlp spoke snake_case.
    #[test]
    fn the_frontend_sees_camel_case() {
        let meta = VideoMetadata {
            video_id: "abc".to_string(),
            title: None,
            channel: None,
            duration: None,
            view_count: Some(5),
            thumbnail: None,
        };

        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("\"videoId\""), "got {json}");
        assert!(json.contains("\"viewCount\""), "got {json}");
    }

    #[test]
    fn known_failures_get_a_plain_explanation() {
        assert!(explain("ERROR: Video unavailable").contains("no longer available"));
        assert!(explain("ERROR: Private video, sign in").contains("private"));
        assert!(explain("unable to download webpage: timed out").contains("internet connection"));
    }

    #[test]
    fn an_unrecognised_failure_falls_back_to_the_last_line() {
        let stderr = "some noise\nERROR: something new broke\n\n";
        assert_eq!(explain(stderr), "ERROR: something new broke");
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;

    /// Shaped from real `--flat-playlist` output, including the parts that
    /// differ from a single-video `-J` call: `thumbnails` is an array and there
    /// is no `thumbnail` field at all.
    const FLAT_SEARCH_JSON: &str = r#"{
        "_type": "playlist",
        "entries": [
            {
                "_type": "url",
                "id": "XFkzRNyygfk",
                "title": "Radiohead - Creep",
                "channel": "Radiohead",
                "duration": 237,
                "view_count": 1535000969,
                "live_status": null,
                "thumbnails": [
                    { "url": "https://i.ytimg.com/small.jpg", "height": 202, "width": 360 },
                    { "url": "https://i.ytimg.com/large.jpg", "height": 404, "width": 720 }
                ]
            }
        ]
    }"#;

    fn parse(json: &str) -> Vec<SearchResult> {
        let response: SearchResponse = serde_json::from_str(json).expect("should parse");
        response
            .entries
            .into_iter()
            .flatten()
            .map(SearchEntry::normalize)
            .collect()
    }

    #[test]
    fn a_flat_search_entry_normalizes() {
        let results = parse(FLAT_SEARCH_JSON);
        assert_eq!(results.len(), 1);

        let first = &results[0];
        assert_eq!(first.video_id, "XFkzRNyygfk");
        assert_eq!(first.title, "Radiohead - Creep");
        assert_eq!(first.channel.as_deref(), Some("Radiohead"));
        assert_eq!(first.duration_secs, Some(237.0));
        assert_eq!(first.view_count, Some(1_535_000_969));
        assert!(!first.is_live);
    }

    /// A 720px image per row is wasteful in a list; the smaller one is enough.
    #[test]
    fn the_thumbnail_is_sized_for_a_list_row() {
        let results = parse(FLAT_SEARCH_JSON);
        assert_eq!(
            results[0].thumbnail_url.as_deref(),
            Some("https://i.ytimg.com/small.jpg")
        );
    }

    /// Nothing but `id` is guaranteed, so a bare entry must still come through.
    #[test]
    fn an_entry_with_almost_nothing_still_yields_a_result() {
        let results = parse(r#"{ "entries": [ { "id": "abcdefghijk" } ] }"#);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "(untitled)");
        assert_eq!(results[0].duration_secs, None);
        assert_eq!(results[0].thumbnail_url, None);
    }

    /// yt-dlp emits a null entry when a video vanished between indexing and the
    /// query. One dead result must not discard the whole page.
    #[test]
    fn a_null_entry_is_skipped_rather_than_failing_the_search() {
        let results = parse(r#"{ "entries": [ null, { "id": "abcdefghijk" }, null ] }"#);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn live_streams_are_flagged_since_they_have_no_duration() {
        let results = parse(
            r#"{ "entries": [ { "id": "abcdefghijk", "live_status": "is_live" } ] }"#,
        );

        assert!(results[0].is_live);
        assert_eq!(results[0].duration_secs, None);
    }

    /// Older listings carry `uploader` where newer ones carry `channel`.
    #[test]
    fn uploader_stands_in_when_channel_is_absent() {
        let results = parse(
            r#"{ "entries": [ { "id": "abcdefghijk", "uploader": "Some Uploader" } ] }"#,
        );

        assert_eq!(results[0].channel.as_deref(), Some("Some Uploader"));
    }
}

// --- saving a result as a track ----------------------------------------

/// Turns a chosen search result into a `saved` YouTube track and returns its id.
///
/// Idempotent by `yt_video_id`: choosing the same video twice returns the
/// existing row rather than creating a duplicate, and deliberately does **not**
/// overwrite `title` or `artist`, which the user may have edited.
#[tauri::command]
pub async fn save_youtube_track(
    db: tauri::State<'_, crate::db::Db>,
    result: SearchResult,
) -> Result<i64, String> {
    if !is_video_id(&result.video_id) {
        return Err("That is not a valid YouTube video id.".to_string());
    }

    // YouTube metadata is dirty -- a slowed+reverb upload has no clean artist
    // tag -- so the upload title and channel are kept verbatim in their own
    // columns while `title`/`artist` become the editable display copy.
    let duration_secs = result.duration_secs.map(|d| d.round() as i64);

    let id: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (
             source, title, artist, duration_secs, state,
             yt_video_id, yt_channel, yt_original_title, yt_thumbnail_url
         )
         VALUES ('youtube', ?, ?, ?, 'saved', ?, ?, ?, ?)
         ON CONFLICT(yt_video_id) DO UPDATE SET
             yt_thumbnail_url = excluded.yt_thumbnail_url
         RETURNING id",
    )
    .bind(&result.title)
    .bind(&result.channel)
    .bind(duration_secs)
    .bind(&result.video_id)
    .bind(&result.channel)
    .bind(&result.title)
    .bind(&result.thumbnail_url)
    .fetch_one(&db.pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(id)
}
