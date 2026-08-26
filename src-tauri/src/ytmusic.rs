//! Searching YouTube Music's catalogue instead of YouTube's uploads.
//!
//! Not a provider. Everything found here is an ordinary YouTube video with an
//! ordinary video id, stored with `source = 'youtube'` and streamed down the
//! same path as anything else -- the schema's CHECK allows no third value, and
//! inventing one would be a lie about where the audio comes from. This is a
//! different way of *searching*, and nothing downstream can tell.
//!
//! # What this is actually for
//!
//! The obvious answer -- "cleaner results" -- is the weaker half, and measured
//! against a handful of real queries it does not always hold: a search for
//! "bohemian rhapsody queen" returns Live Aid, a Panic! At The Disco cover and
//! an upload misattributed to "Freddy Mercury", while plain `ytsearch` finds
//! the Queen original. Tidy metadata is not the same as correct metadata, and
//! this catalogue has its own junk -- junk that looks authoritative.
//!
//! The half that does hold is **structure**. A result here carries the title,
//! the artist and the duration as separate fields, because the catalogue knows
//! them:
//!
//! ```text
//! YouTube Music   title "Duality"                           artist "Set It Off"
//! ytsearch        title "Set It Off-Duality (Lyrics Video)"  uploader "Music Terminal"
//! ```
//!
//! `save_remote_track` writes both straight into the library, so the second
//! files that song under an artist called "Music Terminal". That is what this
//! fixes, and it is a fix no amount of ranking would give.
//!
//! So the two searches are peers rather than a preference and a fallback. This
//! one knows what a song is called; `ytsearch` reaches everything, including
//! the user upload of a version that was never released.
//!
//! # Standing on sand
//!
//! `youtubei` is a private API. The client version below is a string somebody
//! read off a web player, and `FILTER_SONGS` is an opaque protobuf blob. Both
//! can stop working without notice -- which is the entire reason yt-dlp
//! exists. Every caller therefore has a yt-dlp path to fall back to, and this
//! module returning an error is an ordinary event rather than a fault.

use serde_json::Value;

use crate::providers::Provider;
use crate::youtube::SearchResult;

const ENDPOINT: &str = "https://music.youtube.com/youtubei/v1/search?prettyPrint=false";

/// The YouTube Music web client.
///
/// Half of what separates this from a plain YouTube search: a different client
/// reaches a different catalogue, whatever filter is applied.
const CLIENT_NAME: &str = "WEB_REMIX";

/// Read off the web player, and the most likely thing here to go stale.
const CLIENT_VERSION: &str = "1.20240801.01.00";

/// The other half: a protobuf selecting the "songs" result type.
///
/// Decoded, it is `12 05 8a 01 02 08 01 ...` -- field 1 set to 1. Without it
/// the same endpoint returns albums, artists, playlists and music videos mixed
/// together, which is most of what makes a YouTube search hard to read.
///
/// Sent url-encoded because that is the form the endpoint is documented (by
/// observation) to accept.
const FILTER_SONGS: &str = "EgWKAQIIAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D";

/// Long enough for a slow connection, short enough that the fallback still
/// feels like part of the same search rather than a second attempt.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Searches the music catalogue.
///
/// `Err` for anything at all -- the endpoint moving, the shape changing, the
/// network being down. The caller falls back to yt-dlp rather than surfacing
/// it, because a listener does not care which of two searches answered.
pub async fn search(query: &str, limit: u32) -> Result<Vec<SearchResult>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let body = serde_json::json!({
        "context": { "client": {
            "clientName": CLIENT_NAME,
            "clientVersion": CLIENT_VERSION,
            "hl": "en",
            "gl": "US",
        }},
        "query": query,
        "params": FILTER_SONGS,
    });

    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| format!("Could not start an HTTPS client: {e}"))?;

    let response = client
        .post(ENDPOINT)
        .header("Content-Type", "application/json")
        .header("Origin", "https://music.youtube.com")
        // Without a browser-shaped agent the endpoint answers, but with a
        // different page layout that none of the paths below match.
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36",
        )
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("YouTube Music did not answer: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("YouTube Music answered {}", response.status()));
    }

    let json: Value = response
        .json()
        .await
        .map_err(|e| format!("YouTube Music sent something unreadable: {e}"))?;

    let mut songs = Vec::new();
    collect(&json, &mut songs);

    if songs.is_empty() {
        // Either the query genuinely matched nothing or the response shape
        // moved. Indistinguishable from here, and both are better served by
        // letting yt-dlp try than by showing an empty list.
        return Err("YouTube Music returned nothing usable.".to_string());
    }

    // Only trimmed when the caller asked for fewer than arrived.
    //
    // One page is twenty rows whatever `limit` says -- the endpoint has no
    // page-size parameter, so a smaller number throws away results already
    // paid for. It was set to fifteen, which discarded five every search and
    // is a large part of why the list felt short next to a real music app.
    if (limit as usize) < songs.len() {
        songs.truncate(limit as usize);
    }
    Ok(songs)
}

/// Walks the response for song rows.
///
/// Recursive on the row type rather than following a fixed path. The tower of
/// renderer objects above a result changes with the page layout; the row's own
/// name is the most stable thing in the document.
fn collect(value: &Value, out: &mut Vec<SearchResult>) {
    match value {
        Value::Object(map) => {
            if let Some(item) = map.get("musicResponsiveListItemRenderer") {
                if let Some(song) = read_song(item) {
                    out.push(song);
                }
            }
            for nested in map.values() {
                collect(nested, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect(item, out)),
        _ => {}
    }
}

/// The visible text of one flex column, with separators dropped.
fn runs(column: &Value) -> Vec<String> {
    column
        .pointer("/musicResponsiveListItemFlexColumnRenderer/text/runs")
        .and_then(Value::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(|run| run.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty() && *text != "•" && *text != "·")
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn read_song(item: &Value) -> Option<SearchResult> {
    let columns = item.get("flexColumns")?.as_array()?;
    let first = columns.first()?;

    let title = runs(first).into_iter().next()?;

    // The id hangs off the title's navigation endpoint on most rows, and off
    // the thumbnail's play button on the rest.
    let video_id = first
        .pointer(
            "/musicResponsiveListItemFlexColumnRenderer/text/runs/0\
             /navigationEndpoint/watchEndpoint/videoId",
        )
        .or_else(|| {
            item.pointer(
                "/overlay/musicItemThumbnailOverlayRenderer/content\
                 /musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId",
            )
        })
        .and_then(Value::as_str)?
        .to_string();

    // Never trusted. This is network data on its way to a URL and a database
    // row, and the same check every other provider result goes through.
    if !Provider::YouTube.accepts_id(&video_id) {
        return None;
    }

    // Second column reads "Artist • Album • 3:31", with any part omitted.
    let rest = columns.get(1).map(runs).unwrap_or_default();
    let artist = rest.first().cloned();
    let duration_secs = rest.iter().rev().find_map(|part| parse_duration(part));

    Some(SearchResult {
        provider: Provider::YouTube,
        remote_url: format!("https://www.youtube.com/watch?v={video_id}"),
        remote_id: video_id,
        title,
        channel: artist,
        duration_secs,
        // The catalogue does not report play counts, and a zero here would be
        // shown as a real figure.
        view_count: None,
        thumbnail_url: best_thumbnail(item),
        channel_url: columns.get(1).and_then(artist_url),
        is_live: false,
        // Not reported either. Never guessed -- an unknown date sorts last
        // rather than as 1970.
        uploaded_at: None,
    })
}

/// The artist's channel, from whichever run carries a link to one.
///
/// The second column is a row of links, not plain text: the artist, the album
/// and the credits each hang a `navigationEndpoint` off their own run. They
/// are told apart by `pageType` rather than by position, because a song with
/// no album has one fewer link and matching on order would then follow the
/// album's endpoint for the artist -- or the other way round.
///
/// This is what makes "go to the artist" work from a music result. An earlier
/// version of this file asserted in a comment that the response carried no
/// artist page at all, which was simply wrong: it carries one per song, and
/// the feature was missing because nobody looked.
fn artist_url(column: &Value) -> Option<String> {
    let runs = column
        .pointer("/musicResponsiveListItemFlexColumnRenderer/text/runs")?
        .as_array()?;

    for run in runs {
        let endpoint = match run.pointer("/navigationEndpoint/browseEndpoint") {
            Some(endpoint) => endpoint,
            None => continue,
        };

        let is_artist = endpoint
            .pointer(
                "/browseEndpointContextSupportedConfigs\
                 /browseEndpointContextMusicConfig/pageType",
            )
            .and_then(Value::as_str)
            .is_some_and(|page| page == "MUSIC_PAGE_TYPE_ARTIST");

        if !is_artist {
            continue;
        }

        let browse_id = endpoint.get("browseId").and_then(Value::as_str)?;
        // Channel ids are `UC` plus 22 more characters. Checked because this
        // becomes a URL handed to yt-dlp.
        if !browse_id.starts_with("UC")
            || browse_id.len() != 24
            || !browse_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return None;
        }

        return Some(format!("https://www.youtube.com/channel/{browse_id}"));
    }

    None
}

/// `"3:31"` or `"1:02:11"` to seconds.
fn parse_duration(text: &str) -> Option<f64> {
    let text = text.trim();
    if !text.contains(':') {
        return None;
    }

    let mut total = 0f64;
    for part in text.split(':') {
        let value: f64 = part.trim().parse().ok()?;
        total = total * 60.0 + value;
    }
    Some(total)
}

/// The largest artwork offered.
///
/// This is album art from the catalogue rather than a video thumbnail, which
/// is the better picture for a song -- and it is served from a host the app's
/// CSP already allows.
fn best_thumbnail(item: &Value) -> Option<String> {
    item.pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(Value::as_array)
        .and_then(|thumbnails| {
            thumbnails
                .iter()
                .max_by_key(|t| t.get("width").and_then(Value::as_u64).unwrap_or(0))
                .and_then(|t| t.get("url").and_then(Value::as_str))
        })
        .map(upscale)
}

/// What the art is requested at, in pixels.
///
/// Comfortably above any size the app draws it, because this is one URL for
/// every use of that image -- a search row now, a player bar cover later.
const ARTWORK_SIZE: u32 = 544;

/// Asks Google's image host for a bigger copy.
///
/// The response only ever offers 60 and 120 pixels, which is a thumbnail for a
/// list on a phone and visibly soft anywhere else. The size lives in the URL
/// as `=w120-h120-l90-rj`, and the host will render whatever is asked for --
/// so the small sizes in the response are a default rather than a limit.
///
/// Left exactly as found if it does not have that shape. A URL from somewhere
/// else is better delivered untouched than mangled by an assumption.
fn upscale(url: &str) -> String {
    let Some((base, params)) = url.rsplit_once('=') else {
        return url.to_string();
    };

    // `w120-h120-l90-rj` -> the size fields replaced, everything else kept,
    // because the trailing flags control cropping and format.
    let rewritten: Vec<String> = params
        .split('-')
        .map(|part| match part.as_bytes().first() {
            Some(b'w') if part[1..].chars().all(|c| c.is_ascii_digit()) => {
                format!("w{ARTWORK_SIZE}")
            }
            Some(b'h') if part[1..].chars().all(|c| c.is_ascii_digit()) => {
                format!("h{ARTWORK_SIZE}")
            }
            _ => part.to_string(),
        })
        .collect();

    // Nothing looked like a size, so this was not the URL shape assumed.
    if rewritten == params.split('-').collect::<Vec<_>>() {
        return url.to_string();
    }

    format!("{base}={}", rewritten.join("-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]

    /// The artist link, which an earlier version of this file wrongly claimed
    /// the response did not contain.
    #[test]
    fn the_artist_channel_is_read_from_its_own_run() {
        let column = serde_json::json!({
            "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                { "text": "Queen", "navigationEndpoint": { "browseEndpoint": {
                    "browseId": "UC5J__UU0Y7ZAVluolcIjrbw",
                    "browseEndpointContextSupportedConfigs": {
                        "browseEndpointContextMusicConfig": {
                            "pageType": "MUSIC_PAGE_TYPE_ARTIST" } } } } },
                { "text": " • " },
                { "text": "A Night at the Opera", "navigationEndpoint": { "browseEndpoint": {
                    "browseId": "MPREb_someAlbumId",
                    "browseEndpointContextSupportedConfigs": {
                        "browseEndpointContextMusicConfig": {
                            "pageType": "MUSIC_PAGE_TYPE_ALBUM" } } } } }
            ]}}
        });

        assert_eq!(
            artist_url(&column).as_deref(),
            Some("https://www.youtube.com/channel/UC5J__UU0Y7ZAVluolcIjrbw"),
        );
    }

    /// Position is not the signal. A song with no album has one link fewer,
    /// and matching on order would follow the wrong endpoint.
    #[test]
    fn an_album_link_is_never_mistaken_for_an_artist() {
        let column = serde_json::json!({
            "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                { "text": "Some Album", "navigationEndpoint": { "browseEndpoint": {
                    "browseId": "MPREb_onlyAnAlbum",
                    "browseEndpointContextSupportedConfigs": {
                        "browseEndpointContextMusicConfig": {
                            "pageType": "MUSIC_PAGE_TYPE_ALBUM" } } } } }
            ]}}
        });
        assert_eq!(artist_url(&column), None);
    }

    /// The id becomes a URL handed to yt-dlp, so it is never trusted.
    #[test]
    fn a_malformed_channel_id_is_refused() {
        let column = serde_json::json!({
            "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                { "text": "Nope", "navigationEndpoint": { "browseEndpoint": {
                    "browseId": "UC../../etc/passwd",
                    "browseEndpointContextSupportedConfigs": {
                        "browseEndpointContextMusicConfig": {
                            "pageType": "MUSIC_PAGE_TYPE_ARTIST" } } } } }
            ]}}
        });
        assert_eq!(artist_url(&column), None);
    }

    #[test]
    fn plain_text_with_no_links_is_not_an_error() {
        let column = serde_json::json!({
            "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                { "text": "Some Artist" }
            ]}}
        });
        assert_eq!(artist_url(&column), None);
    }
    fn durations_are_read_in_both_shapes() {
        assert_eq!(parse_duration("3:31"), Some(211.0));
        assert_eq!(parse_duration("0:45"), Some(45.0));
        assert_eq!(parse_duration("1:02:11"), Some(3731.0));
        assert_eq!(parse_duration("not a time"), None);
        assert_eq!(parse_duration(""), None);
        // An album name that happens to contain a colon must not become a
        // duration, which is why this parses rather than pattern-matches.
        assert_eq!(parse_duration("Volume: One"), None);
    }

    #[test]
    fn the_largest_artwork_is_chosen() {
        let item = serde_json::json!({
            "thumbnail": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [
                { "url": "small", "width": 60, "height": 60 },
                { "url": "large", "width": 544, "height": 544 },
                { "url": "medium", "width": 120, "height": 120 },
            ]}}}
        });
        assert_eq!(best_thumbnail(&item).as_deref(), Some("large"));
    }

    #[test]

    /// The response offers 60 and 120 pixels; both are soft at the size the
    /// app draws artwork.
    #[test]
    fn artwork_is_requested_at_a_usable_size() {
        assert_eq!(
            upscale("https://yt3.googleusercontent.com/abc=w120-h120-l90-rj"),
            "https://yt3.googleusercontent.com/abc=w544-h544-l90-rj",
        );
        // The trailing flags control cropping and format and must survive.
        assert_eq!(
            upscale("https://x/y=w60-h60-l90-rj-flags"),
            "https://x/y=w544-h544-l90-rj-flags",
        );
    }

    /// A URL of another shape is better untouched than mangled.
    #[test]
    fn an_unfamiliar_artwork_url_is_left_alone() {
        for url in [
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg",
            "https://example.invalid/art.png",
            "https://example.invalid/art=notasize",
        ] {
            assert_eq!(upscale(url), url, "{url} should have been left as it was");
        }
    }
    fn a_response_with_no_artwork_is_not_an_error() {
        assert_eq!(best_thumbnail(&serde_json::json!({})), None);
    }

    /// The row shape, parsed from a cut-down copy of a real response.
    #[test]
    fn a_song_row_becomes_a_search_result() {
        let item = serde_json::json!({
            "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                    { "text": "Duality", "navigationEndpoint": { "watchEndpoint": {
                        "videoId": "HJRz4pROLxE" } } }
                ]}}},
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                    { "text": "Set It Off" },
                    { "text": " • " },
                    { "text": "Duality" },
                    { "text": " • " },
                    { "text": "4:03" }
                ]}}}
            ]
        });

        let song = read_song(&item).expect("a well-formed row should parse");
        assert_eq!(song.title, "Duality");
        // The point of the whole module: an artist, not an uploader.
        assert_eq!(song.channel.as_deref(), Some("Set It Off"));
        assert_eq!(song.duration_secs, Some(243.0));
        assert_eq!(song.remote_id, "HJRz4pROLxE");
        assert_eq!(
            song.remote_url,
            "https://www.youtube.com/watch?v=HJRz4pROLxE"
        );
        assert_eq!(song.provider, Provider::YouTube);
    }

    /// The id reaches a URL and a database row, so a bad one is refused here.
    #[test]
    fn a_row_with_an_unusable_id_is_dropped() {
        let item = serde_json::json!({
            "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                    { "text": "Nope", "navigationEndpoint": { "watchEndpoint": {
                        "videoId": "../../etc/passwd" } } }
                ]}}}
            ]
        });
        assert!(read_song(&item).is_none());
    }

    /// Rows carrying the id on the play button instead must still parse.
    #[test]
    fn an_id_on_the_play_button_is_found_too() {
        let item = serde_json::json!({
            "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                    { "text": "Chop Suey!" }
                ]}}}
            ],
            "overlay": { "musicItemThumbnailOverlayRenderer": { "content": {
                "musicPlayButtonRenderer": { "playNavigationEndpoint": {
                    "watchEndpoint": { "videoId": "MlcJQYON2Go" } } } } } }
        });

        let song = read_song(&item).expect("the overlay id should be found");
        assert_eq!(song.remote_id, "MlcJQYON2Go");
        // No second column, so neither is claimed.
        assert_eq!(song.channel, None);
        assert_eq!(song.duration_secs, None);
    }

    /// The walk must find rows wherever the page layout has buried them.
    #[test]
    fn rows_are_found_at_any_depth() {
        let response = serde_json::json!({
            "contents": { "tabbedSearchResultsRenderer": { "tabs": [ { "tabRenderer": {
                "content": { "sectionListRenderer": { "contents": [ { "musicShelfRenderer": {
                    "contents": [ { "musicResponsiveListItemRenderer": {
                        "flexColumns": [
                            { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                                { "text": "Buried", "navigationEndpoint": { "watchEndpoint": {
                                    "videoId": "dQw4w9WgXcQ" } } }
                            ]}}}
                        ]
                    }}]
                }}]}}
            }}]}}
        });

        let mut found = Vec::new();
        collect(&response, &mut found);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "Buried");
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;

    /// The claim this module exists for, against the live endpoint.
    ///
    /// Not "the results are better" -- that was measured and does not reliably
    /// hold. What must hold is that the *fields are separated*: a title that
    /// is only the song, and an artist that is the artist rather than whoever
    /// uploaded it. That is what ends up in the library.
    #[tokio::test]
    #[ignore = "hits the live YouTube Music endpoint"]
    async fn results_carry_a_real_artist_rather_than_an_uploader() {
        let results = match search("duality set it off", 10).await {
            Ok(results) => results,
            Err(e) => {
                // A private API being unavailable is not a test failure; the
                // command falls back to yt-dlp for exactly this.
                eprintln!("SKIP: {e}");
                return;
            }
        };

        assert!(!results.is_empty(), "no rows parsed from a live response");

        let first = &results[0];
        eprintln!(
            "first: {:?} / {:?} / {:?}s / {}",
            first.title, first.channel, first.duration_secs, first.remote_id
        );

        assert_eq!(
            first.title, "Duality",
            "the title should be the song alone, with no artist or suffix",
        );
        assert_eq!(
            first.channel.as_deref(),
            Some("Set It Off"),
            "the artist should be the band, not an uploader",
        );
        assert!(
            first.duration_secs.is_some_and(|d| (200.0..300.0).contains(&d)),
            "a plausible duration should come back, got {:?}",
            first.duration_secs,
        );

        // Every row has to be usable, not just the one inspected above.
        for song in &results {
            assert!(
                Provider::YouTube.accepts_id(&song.remote_id),
                "unusable id survived: {:?}",
                song.remote_id,
            );
            assert!(!song.title.trim().is_empty(), "a row arrived with no title");
        }
    }
}
