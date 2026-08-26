//! Does the YouTube Music endpoint really return a cleaner list?
//!
//! Veluna's "clean search" claim rests on one request: a POST to YouTube
//! Music's Innertube endpoint as the `WEB_REMIX` client, carrying an opaque
//! `params` blob. Decoded, that blob is a protobuf whose first field selects a
//! result type, and the value used is 1 -- songs. The claim is that this
//! returns official studio recordings where `ytsearch` returns whatever
//! YouTube has: lyric videos, live cuts, covers, sped-up edits, hour loops.
//!
//! That is a testable claim, and worth testing before any of it is built on:
//! Innertube is a private API, the client version is hardcoded, and the params
//! blob is a constant somebody else derived. All three can rot.
//!
//! ```text
//! cargo test -j 1 --lib ytmusic_probe -- --ignored --nocapture
//! ```

use serde_json::Value;

const ENDPOINT: &str = "https://music.youtube.com/youtubei/v1/search?prettyPrint=false";

/// The YouTube Music web client. A different `clientName` reaches a different
/// catalogue -- this is the half of the trick that is not the params blob.
const CLIENT: &str = "WEB_REMIX";
const CLIENT_VERSION: &str = "1.20240801.01.00";

/// `songs`, as a protobuf: `12 05 8a 01 02 08 01 ...`, where the trailing
/// `08 01` is field 1 = 1. Sent url-encoded, which is how it appears in the
/// wild and what the endpoint accepts.
const FILTER_SONGS: &str = "EgWKAQIIAWoQEAMQBBAJEAoQBRAREBAQFQ%3D%3D";

/// Overridable so the filter question can be measured rather than argued.
fn filter() -> Option<String> {
    match std::env::var("MUSIC_APP_YTM_FILTER") {
        Ok(v) if v == "none" => None,
        Ok(v) => Some(v),
        Err(_) => Some(FILTER_SONGS.to_string()),
    }
}

const YT_DLP: &str = "binaries/yt-dlp-x86_64-pc-windows-msvc.exe";

#[derive(Debug)]
struct Song {
    title: String,
    artist: String,
    album: Option<String>,
    duration: String,
    video_id: String,
}

/// Walks the response for song rows.
///
/// Innertube nests results inside a different tower of renderer objects
/// depending on how the page is laid out that day, so this recurses for the
/// row type rather than following a fixed path -- the path is the part most
/// likely to change, and the row name is the part least likely to.
fn collect_songs(value: &Value, out: &mut Vec<Song>) {
    match value {
        Value::Object(map) => {
            if let Some(item) = map.get("musicResponsiveListItemRenderer") {
                if let Some(song) = read_song(item) {
                    out.push(song);
                }
            }
            for v in map.values() {
                collect_songs(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect_songs(v, out);
            }
        }
        _ => {}
    }
}

fn runs_of(column: &Value, kind: &str) -> Vec<String> {
    column
        .pointer(&format!("/{kind}/text/runs"))
        .and_then(|r| r.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                .map(str::to_string)
                .filter(|t| !t.trim().is_empty() && t != "•" && t != "·")
                .collect()
        })
        .unwrap_or_default()
}

fn read_song(item: &Value) -> Option<Song> {
    let flex = item.get("flexColumns")?.as_array()?;

    let first = runs_of(
        flex.first()?,
        "musicResponsiveListItemFlexColumnRenderer",
    );
    let title = first.first()?.clone();

    // The id lives on the title's navigation endpoint, or on the play button
    // in the thumbnail overlay. Rows differ about which they carry.
    let video_id = flex
        .first()
        .and_then(|c| {
            c.pointer(
                "/musicResponsiveListItemFlexColumnRenderer/text/runs/0\
                 /navigationEndpoint/watchEndpoint/videoId",
            )
        })
        .or_else(|| {
            item.pointer(
                "/overlay/musicItemThumbnailOverlayRenderer/content\
                 /musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId",
            )
        })
        .and_then(|v| v.as_str())?
        .to_string();

    // Second column is "Artist • Album • Duration", with parts omitted freely.
    let rest = flex
        .get(1)
        .map(|c| runs_of(c, "musicResponsiveListItemFlexColumnRenderer"))
        .unwrap_or_default();

    let duration = rest
        .iter()
        .rfind(|p| p.contains(':'))
        .cloned()
        .unwrap_or_else(|| "?".to_string());

    let artist = rest.first().cloned().unwrap_or_else(|| "?".to_string());
    let album = if rest.len() > 2 {
        Some(rest[rest.len() - 2].clone())
    } else {
        None
    };

    Some(Song {
        title,
        artist,
        album,
        duration,
        video_id,
    })
}

async fn search_yt_music(query: &str) -> Result<Vec<Song>, String> {
    let mut body = serde_json::json!({
        "context": { "client": {
            "clientName": CLIENT,
            "clientVersion": CLIENT_VERSION,
            "hl": "en",
            "gl": "US",
        }},
        "query": query,
    });
    if let Some(params) = filter() {
        body["params"] = serde_json::Value::String(params);
    }

    let response = reqwest::Client::new()
        .post(ENDPOINT)
        .header("Content-Type", "application/json")
        .header("Origin", "https://music.youtube.com")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36",
        )
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("endpoint answered {}", response.status()));
    }

    let json: Value = response
        .json()
        .await
        .map_err(|e| format!("not json: {e}"))?;

    let mut songs = Vec::new();
    collect_songs(&json, &mut songs);
    Ok(songs)
}

/// What the app does today: yt-dlp's generic YouTube search.
fn search_yt_dlp(query: &str, limit: usize) -> Result<Vec<(String, String)>, String> {
    let out = std::process::Command::new(YT_DLP)
        .args([
            &format!("ytsearch{limit}:{query}"),
            "--flat-playlist",
            "--print",
            "%(title)s====%(uploader)s",
            "--no-warnings",
            "--socket-timeout",
            "15",
        ])
        .output()
        .map_err(|e| format!("yt-dlp: {e}"))?;

    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut parts = l.splitn(2, "====");
            Some((parts.next()?.to_string(), parts.next()?.to_string()))
        })
        .collect())
}

/// Words that mark a result as something other than the studio recording.
///
/// Crude on purpose: this counts obvious cases so the two lists can be
/// compared by a number as well as by eye. It undercounts rather than over --
/// a cover or a re-upload with a clean title is invisible to it.
fn looks_like_not_the_song(title: &str, uploader: &str) -> Option<&'static str> {
    let t = title.to_lowercase();
    let u = uploader.to_lowercase();

    for (needle, label) in [
        ("lyric", "lyric video"),
        ("live", "live"),
        ("cover", "cover"),
        ("remix", "remix"),
        ("sped up", "sped up"),
        ("speed up", "sped up"),
        ("slowed", "slowed"),
        ("reverb", "reverb"),
        ("nightcore", "nightcore"),
        ("8d audio", "8d"),
        ("1 hour", "hour loop"),
        ("10 hours", "hour loop"),
        ("karaoke", "karaoke"),
        ("instrumental", "instrumental"),
        ("reaction", "reaction"),
        ("official video", "music video"),
        ("music video", "music video"),
        ("(video)", "music video"),
    ] {
        if t.contains(needle) {
            return Some(label);
        }
    }

    if u.contains("vevo") && t.contains("official") {
        return Some("music video");
    }

    None
}

#[tokio::test]
#[ignore = "hits the live YouTube Music endpoint"]
async fn yt_music_search_is_compared_with_the_current_one() {
    // Overridable, so a ranking question can be asked without a rebuild.
    let raw = std::env::var("MUSIC_APP_QUERIES").unwrap_or_else(|_| {
        "penny pinched|chop suey system of a down|duality set it off|bohemian rhapsody|blinding lights".to_string()
    });
    let queries: Vec<&str> = raw.split('|').collect();

    let mut ytm_suspect = 0usize;
    let mut ytm_total = 0usize;
    let mut dlp_suspect = 0usize;
    let mut dlp_total = 0usize;
    let mut reached = 0usize;

    for query in queries {
        eprintln!("\n════════ \"{query}\" ════════");

        match search_yt_music(query).await {
            Ok(songs) if songs.is_empty() => {
                eprintln!("  YT MUSIC: answered, but no song rows parsed");
            }
            Ok(songs) => {
                reached += 1;
                eprintln!("  YT MUSIC ({} rows):", songs.len());
                for s in songs.iter().take(8) {
                    let flag = looks_like_not_the_song(&s.title, &s.artist)
                        .map(|f| format!("   <- {f}"))
                        .unwrap_or_default();
                    ytm_total += 1;
                    if flag.is_empty() {
                    } else {
                        ytm_suspect += 1;
                    }
                    eprintln!(
                        "    {:<42} {:<24} {:<8} {}{}",
                        truncate(&s.title, 42),
                        truncate(&s.artist, 24),
                        s.duration,
                        s.video_id,
                        flag,
                    );
                }
                if let Some(first) = songs.first() {
                    if let Some(album) = &first.album {
                        eprintln!("    (album of first row: {album})");
                    }
                }
            }
            Err(e) => eprintln!("  YT MUSIC: {e}"),
        }

        match search_yt_dlp(query, 8) {
            Ok(rows) if rows.is_empty() => eprintln!("  ytsearch: no rows"),
            Ok(rows) => {
                eprintln!("  ytsearch (what the app does today):");
                for (title, uploader) in rows.iter().take(8) {
                    let flag = looks_like_not_the_song(title, uploader)
                        .map(|f| format!("   <- {f}"))
                        .unwrap_or_default();
                    dlp_total += 1;
                    if !flag.is_empty() {
                        dlp_suspect += 1;
                    }
                    eprintln!(
                        "    {:<42} {:<24} {}",
                        truncate(title, 42),
                        truncate(uploader, 24),
                        flag,
                    );
                }
            }
            Err(e) => eprintln!("  ytsearch: {e}"),
        }
    }

    eprintln!("\n════════ tally ════════");
    eprintln!("  YT Music: {ytm_suspect}/{ytm_total} rows look like something other than the song");
    eprintln!("  ytsearch: {dlp_suspect}/{dlp_total} rows look like something other than the song");

    assert!(
        reached > 0,
        "the YouTube Music endpoint answered nothing usable for any query -- \
         the client version or the params blob has rotted, and none of this \
         should be built on until it is understood",
    );
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}
