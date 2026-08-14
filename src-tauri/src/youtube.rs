use std::process::Command;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::providers::Provider;
use crate::sidecar::{self, Tool};

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
        return "That track is age-restricted and needs a signed-in account.".to_string();
    }
    if lowered.contains("video unavailable") || lowered.contains("has been removed") {
        return "That track is no longer available.".to_string();
    }
    if lowered.contains("private video") || lowered.contains("this track is not available") {
        return "That track is private.".to_string();
    }
    // SoundCloud gates label uploads behind Go+ and serves a snippet instead.
    if lowered.contains("only available to go+ members") || lowered.contains("go+ subscription") {
        return "SoundCloud only offers a 30-second preview of that track. \
                Another upload of the same song may be full length."
            .to_string();
    }
    if lowered.contains("not available in your country") || lowered.contains("geo") {
        return "That track is blocked in your region.".to_string();
    }
    if lowered.contains("failed to resolve")
        || lowered.contains("temporary failure in name resolution")
        || lowered.contains("unable to download webpage")
    {
        // Deliberately not naming a service: this runs for every provider, and
        // the stderr does not reliably say which one was being reached.
        return "Could not reach the service. Check your internet connection.".to_string();
    }

    stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .next_back()
        .unwrap_or("yt-dlp failed.")
        .trim()
        .to_string()
}

// --- raw search --------------------------------------------------------

/// Upper bound on results. yt-dlp fetches these serially, so a large number
/// mostly buys waiting.
const MAX_RESULTS: u32 = 25;

/// One row in the results list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    /// Which service this came from. Carried through to `tracks.source`.
    pub provider: Provider,
    pub remote_id: String,
    /// The provider's page for this track.
    ///
    /// Kept rather than derived: SoundCloud URLs contain the uploader's handle
    /// and cannot be rebuilt from the numeric id.
    pub remote_url: String,
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
    /// The canonical page. yt-dlp also sends `url`, but for SoundCloud that is
    /// an `api.soundcloud.com` form with percent-encoded ids; `webpage_url` is
    /// the stable public one, and both providers always send it.
    #[serde(default)]
    webpage_url: Option<String>,
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
    /// Turns a raw entry into a result, or drops it.
    ///
    /// `None` when the entry cannot be played later: an id the provider would
    /// not recognise, or no usable page URL. Dropping one dead row is right --
    /// the alternative is a result that saves fine and then fails at play time
    /// with nothing to point at.
    fn normalize(self, provider: Provider) -> Option<SearchResult> {
        if !provider.accepts_id(&self.id) {
            return None;
        }

        // Derivable for YouTube, never for SoundCloud -- which is exactly why
        // the URL is stored rather than reconstructed.
        let remote_url = self
            .webpage_url
            .or_else(|| provider.page_url(&self.id))
            .filter(|url| provider.accepts_url(url))?;

        Some(SearchResult {
            provider,
            remote_id: self.id,
            remote_url,
            title: self.title.unwrap_or_else(|| "(untitled)".to_string()),
            // `channel` is the canonical name; `uploader` is the older field,
            // and the only one SoundCloud sends.
            channel: self.channel.or(self.uploader),
            duration_secs: self.duration,
            view_count: self.view_count,
            thumbnail_url: pick_thumbnail(&self.thumbnails),
            is_live: self.live_status.as_deref() == Some("is_live"),
        })
    }
}

/// Chooses a list-sized thumbnail.
///
/// yt-dlp returns them smallest-first, so the widest one under the cap keeps
/// the list crisp without pulling a full-resolution image per row.
///
/// Entries without a width are *skipped*, not kept. SoundCloud ends its list
/// with an `original` entry that has no width at all -- treating "no width" as
/// "within the cap" selected exactly that one, pulling the full-size artwork
/// for every row. YouTube never exposed this because all of its thumbnails
/// carry dimensions.
fn pick_thumbnail(thumbnails: &[Thumbnail]) -> Option<String> {
    const MAX_WIDTH: u32 = 480;

    thumbnails
        .iter()
        .filter(|t| t.width.is_some_and(|w| w <= MAX_WIDTH))
        .next_back()
        // Nothing had a usable width: fall back to the first, which is the
        // smallest and so the cheapest wrong answer.
        .or_else(|| thumbnails.first())
        .map(|t| t.url.clone())
}

/// Raw search against one provider.
///
/// Deliberately `--flat-playlist`: it returns listing metadata only. Resolving
/// each track's formats during a search would multiply the runtime by the
/// result count for data nothing displays.
#[tauri::command]
pub async fn search_provider(
    app: AppHandle,
    provider: Provider,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<SearchResult>, String> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let limit = limit.unwrap_or(10).clamp(1, MAX_RESULTS);
    let prefix = provider.search_prefix();

    // The query is one argv entry embedded after `<prefix>N:`, and no shell is
    // involved, so it cannot become a flag or a second argument.
    let json = run(
        &app,
        vec![
            format!("{prefix}{limit}:{query}"),
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
        .filter_map(|entry| entry.normalize(provider))
        .collect())
}

/// Resolves a directly playable audio stream URL for a provider page URL.
///
/// Takes the page URL rather than an id because that is the only thing that
/// generalises: a YouTube URL is `watch?v=` plus the id, but a SoundCloud one
/// is `soundcloud.com/<uploader>/<slug>` and cannot be rebuilt from its
/// numeric id at all. The caller has already checked the URL against its
/// provider.
///
/// Called on every play and never cached. The URLs providers hand back carry
/// an `expire` timestamp and are tied to the requesting IP, so storing one
/// would produce a track that plays today and fails tomorrow for no visible
/// reason.
///
/// `bestaudio[ext=m4a]` first because AAC is cheap to decode and widely
/// available; the bare `bestaudio` fallback is usually Opus for YouTube and an
/// HLS playlist for SoundCloud, both of which ffmpeg handles.
pub async fn resolve_stream_url(yt_dlp: &std::path::Path, page_url: &str) -> Result<String, String> {
    // Belt and braces: the caller validates against the provider, but this is
    // the function that actually hands a string to a subprocess.
    if !page_url.starts_with("https://") {
        return Err("That track has no usable source URL.".to_string());
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
            page_url.to_string(),
        ],
    )
    .await?;

    stdout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("http"))
        .map(str::to_string)
        .ok_or_else(|| "yt-dlp returned no playable stream for that track.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// yt-dlp speaks snake_case, and `SearchEntry` matches it field for field.
    ///
    /// Worth pinning because the failure is silent: `#[serde(default)]` turns
    /// a name that does not match into `None` rather than an error, so a
    /// mismatch shows up only as every view count being mysteriously blank.
    #[test]
    fn real_yt_dlp_field_names_deserialize() {
        let json = r#"{
            "id": "dQw4w9WgXcQ",
            "webpage_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "title": "Never Gonna Give You Up",
            "channel": "Rick Astley",
            "duration": 213.0,
            "view_count": 1803881842,
            "live_status": "not_live"
        }"#;

        let entry: SearchEntry = serde_json::from_str(json).expect("should parse");

        assert_eq!(entry.id, "dQw4w9WgXcQ");
        assert_eq!(entry.channel.as_deref(), Some("Rick Astley"));
        assert_eq!(entry.duration, Some(213.0));
        assert_eq!(
            entry.view_count,
            Some(1_803_881_842),
            "view_count is snake_case in yt-dlp output"
        );
        assert_eq!(
            entry.webpage_url.as_deref(),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            "webpage_url is snake_case too"
        );
    }

    /// Live streams have no duration, flat listings often have no view count.
    #[test]
    fn missing_optional_fields_do_not_fail_the_parse() {
        let json = r#"{ "id": "dQw4w9WgXcQ", "title": "Live" }"#;
        let entry: SearchEntry = serde_json::from_str(json).expect("should parse");

        assert_eq!(entry.duration, None);
        assert_eq!(entry.view_count, None);
        assert_eq!(entry.channel, None);
        assert_eq!(entry.webpage_url, None);
    }

    /// `SearchResult` makes a full round trip: the backend sends it to the UI,
    /// and the UI hands the same object back to `save_remote_track`.
    ///
    /// So serialize and deserialize have to agree. If they drifted apart,
    /// searching would look perfectly fine and *saving* would break.
    #[test]
    fn a_search_result_survives_the_round_trip_to_the_frontend() {
        let result = SearchResult {
            provider: Provider::SoundCloud,
            remote_id: "199428706".to_string(),
            remote_url: "https://soundcloud.com/a/b".to_string(),
            title: "One More Time".to_string(),
            channel: Some("Daft Punk".to_string()),
            duration_secs: Some(322.606),
            view_count: Some(5),
            thumbnail_url: None,
            is_live: false,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"remoteId\""), "got {json}");
        assert!(json.contains("\"remoteUrl\""), "got {json}");
        assert!(json.contains("\"viewCount\""), "got {json}");
        assert!(json.contains("\"soundcloud\""), "got {json}");

        let back: SearchResult = serde_json::from_str(&json).expect("should parse back");
        assert_eq!(back, result);
    }

    #[test]
    fn known_failures_get_a_plain_explanation() {
        assert!(explain("ERROR: Video unavailable").contains("no longer available"));
        assert!(explain("ERROR: Private video, sign in").contains("private"));
        assert!(explain("unable to download webpage: timed out").contains("internet connection"));
    }

    /// The wording is shared by every provider now, so it must not tell a
    /// SoundCloud user that YouTube is unreachable.
    #[test]
    fn a_connection_failure_does_not_blame_one_service() {
        let message = explain("unable to download webpage: timed out");
        assert!(!message.contains("YouTube"), "got: {message}");
    }

    #[test]
    fn a_go_plus_gated_track_is_explained_rather_than_dumped() {
        let message = explain("ERROR: This track is only available to Go+ members");
        assert!(message.contains("30-second preview"), "got: {message}");
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
                "webpage_url": "https://www.youtube.com/watch?v=XFkzRNyygfk",
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

    /// Shaped from a real `scsearch` response. The differences from YouTube are
    /// the whole point: a numeric id, `uploader` instead of `channel`, and a
    /// URL that embeds the uploader's handle so it cannot be derived.
    const SOUNDCLOUD_SEARCH_JSON: &str = r#"{
        "_type": "playlist",
        "entries": [
            {
                "_type": "url",
                "id": "199428706",
                "uploader": "Daft Punk",
                "title": "One More Time",
                "duration": 322.606,
                "view_count": 3944553,
                "webpage_url": "https://soundcloud.com/daft-punk-id/daft-punk-one-more-time",
                "url": "https://api.soundcloud.com/tracks/soundcloud%3Atracks%3A199428706",
                "thumbnails": [
                    { "id": "small", "url": "https://i1.sndcdn.com/small.jpg", "width": 32 },
                    { "id": "t300x300", "url": "https://i1.sndcdn.com/t300.jpg", "width": 300 }
                ]
            }
        ]
    }"#;

    fn parse_as(json: &str, provider: Provider) -> Vec<SearchResult> {
        let response: SearchResponse = serde_json::from_str(json).expect("should parse");
        response
            .entries
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.normalize(provider))
            .collect()
    }

    fn parse(json: &str) -> Vec<SearchResult> {
        parse_as(json, Provider::YouTube)
    }

    #[test]
    fn a_flat_search_entry_normalizes() {
        let results = parse(FLAT_SEARCH_JSON);
        assert_eq!(results.len(), 1);

        let first = &results[0];
        assert_eq!(first.remote_id, "XFkzRNyygfk");
        assert_eq!(first.title, "Radiohead - Creep");
        assert_eq!(first.channel.as_deref(), Some("Radiohead"));
        assert_eq!(first.duration_secs, Some(237.0));
        assert_eq!(first.view_count, Some(1_535_000_969));
        assert!(!first.is_live);
    }

    /// The same parser, unchanged, has to handle SoundCloud -- that is what
    /// makes adding a provider cheap rather than a second code path.
    #[test]
    fn a_soundcloud_entry_normalizes_through_the_same_parser() {
        let results = parse_as(SOUNDCLOUD_SEARCH_JSON, Provider::SoundCloud);
        assert_eq!(results.len(), 1);

        let first = &results[0];
        assert_eq!(first.provider, Provider::SoundCloud);
        assert_eq!(first.remote_id, "199428706");
        assert_eq!(first.title, "One More Time");
        assert_eq!(
            first.channel.as_deref(),
            Some("Daft Punk"),
            "SoundCloud only sends `uploader`"
        );
        assert_eq!(first.duration_secs, Some(322.606));
    }

    /// The public page URL, not the `api.soundcloud.com` form yt-dlp also
    /// sends -- that one carries percent-encoded ids and would not survive the
    /// host check on the way back out.
    #[test]
    fn the_stored_url_is_the_public_page() {
        let results = parse_as(SOUNDCLOUD_SEARCH_JSON, Provider::SoundCloud);
        assert_eq!(
            results[0].remote_url,
            "https://soundcloud.com/daft-punk-id/daft-punk-one-more-time"
        );
    }

    /// A YouTube URL is derivable, so an entry missing `webpage_url` still
    /// works. This is exactly the assumption SoundCloud breaks.
    #[test]
    fn a_youtube_entry_without_a_url_falls_back_to_deriving_one() {
        let results = parse(r#"{ "entries": [ { "id": "XFkzRNyygfk" } ] }"#);
        assert_eq!(
            results[0].remote_url,
            "https://www.youtube.com/watch?v=XFkzRNyygfk"
        );
    }

    /// Nothing to derive from, so the row is dropped rather than saved as
    /// something that would fail only at play time.
    #[test]
    fn a_soundcloud_entry_without_a_url_is_dropped() {
        let results = parse_as(
            r#"{ "entries": [ { "id": "199428706" } ] }"#,
            Provider::SoundCloud,
        );
        assert!(results.is_empty());
    }

    /// An id of the wrong shape means the provider would reject it later, so
    /// it must not become a saveable row now.
    #[test]
    fn an_entry_whose_id_does_not_fit_the_provider_is_dropped() {
        let results = parse_as(FLAT_SEARCH_JSON, Provider::SoundCloud);
        assert!(results.is_empty(), "a YouTube id is not a SoundCloud id");
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

    /// SoundCloud terminates its thumbnail list with a width-less `original`
    /// entry. Treating "no width" as "within the cap" picked that one -- the
    /// full-resolution artwork -- for every row.
    #[test]
    fn a_thumbnail_without_a_width_is_not_mistaken_for_a_small_one() {
        let json = r#"{
            "entries": [
                {
                    "id": "199428706",
                    "webpage_url": "https://soundcloud.com/a/b",
                    "thumbnails": [
                        { "url": "https://i1.sndcdn.com/small.jpg", "width": 32 },
                        { "url": "https://i1.sndcdn.com/t300.jpg", "width": 300 },
                        { "url": "https://i1.sndcdn.com/original.jpg" }
                    ]
                }
            ]
        }"#;

        let results = parse_as(json, Provider::SoundCloud);
        assert_eq!(
            results[0].thumbnail_url.as_deref(),
            Some("https://i1.sndcdn.com/t300.jpg"),
            "the width-less `original` must not win"
        );
    }

    /// If nothing has a width there is no good answer, so take the smallest.
    #[test]
    fn thumbnails_without_any_widths_fall_back_to_the_first() {
        let json = r#"{
            "entries": [
                {
                    "id": "199428706",
                    "webpage_url": "https://soundcloud.com/a/b",
                    "thumbnails": [
                        { "url": "https://i1.sndcdn.com/a.jpg" },
                        { "url": "https://i1.sndcdn.com/b.jpg" }
                    ]
                }
            ]
        }"#;

        let results = parse_as(json, Provider::SoundCloud);
        assert_eq!(
            results[0].thumbnail_url.as_deref(),
            Some("https://i1.sndcdn.com/a.jpg")
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

/// Turns a chosen search result into a `saved` track and returns its id.
///
/// Idempotent per provider: choosing the same track twice returns the existing
/// row rather than creating a duplicate, and deliberately does **not**
/// overwrite `title` or `artist`, which the user may have edited.
#[tauri::command]
pub async fn save_remote_track(
    db: tauri::State<'_, crate::db::Db>,
    result: SearchResult,
) -> Result<i64, String> {
    // The result round-trips through the frontend, so neither field is trusted
    // on the way back in.
    if !result.provider.accepts_id(&result.remote_id) {
        return Err(format!(
            "That is not a valid {} track id.",
            result.provider.display_name()
        ));
    }
    if !result.provider.accepts_url(&result.remote_url) {
        return Err(format!(
            "That is not a valid {} link.",
            result.provider.display_name()
        ));
    }

    // Remote metadata is dirty -- a slowed+reverb upload has no clean artist
    // tag -- so the upload title and uploader are kept verbatim in their own
    // columns while `title`/`artist` become the editable display copy.
    let duration_secs = result.duration_secs.map(|d| d.round() as i64);

    let id: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (
             source, title, artist, duration_secs, state,
             remote_id, remote_url, remote_uploader, remote_title,
             remote_thumbnail_url
         )
         VALUES (?, ?, ?, ?, 'saved', ?, ?, ?, ?, ?)
         ON CONFLICT(source, remote_id) DO UPDATE SET
             remote_thumbnail_url = excluded.remote_thumbnail_url,
             -- A SoundCloud slug can change when an uploader renames a track,
             -- which would leave the stored link resolving to nothing.
             remote_url = excluded.remote_url
         RETURNING id",
    )
    .bind(result.provider.as_str())
    .bind(&result.title)
    .bind(&result.channel)
    .bind(duration_secs)
    .bind(&result.remote_id)
    .bind(&result.remote_url)
    .bind(&result.channel)
    .bind(&result.title)
    .bind(&result.thumbnail_url)
    .fetch_one(&db.pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(id)
}
