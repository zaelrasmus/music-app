//! NetEase Cloud Music, the second lyrics provider.
//!
//! # Why this one, specifically
//!
//! Not "more coverage" — a concrete thing lrclib does not have. NetEase
//! returns a song's lyrics as several parallel tracks sharing one set of
//! timestamps, and one of them is a **romanisation**:
//!
//! ```text
//! lrc      [00:00.620] 教えて教えてよ その仕組みを
//! romalrc  [00:00.620] o shi e te o shi e te yo so no shi ku mi wo
//! tlyric   [00:00.620] 告诉我 告诉我吧 那其中的构造
//! ```
//!
//! This library is full of `Ōrīōn`, `Misa no Kodoku`, `一滴の影響` and a wall
//! of Japanese doujin releases. Being able to *read along* with those is the
//! whole argument, and lrclib cannot do it at all.
//!
//! `tlyric` is deliberately dropped. It is a Chinese translation, which is the
//! wrong language for this library, and keeping it would mean a third mode in
//! the reader for an audience of nobody.
//!
//! # Two requests, not one
//!
//! Unlike lrclib, a search here returns song *metadata* only — no lyrics — so
//! the words cost a second fetch per song. That is why [`find`] fetches
//! lyrics for a handful of top results rather than all of them, and why this
//! provider is tried second: lrclib answers the same question in one request
//! and holds CC0 data.
//!
//! # Standing on sand
//!
//! `music.163.com/api` is not a documented API. It answers without a key
//! today; it can stop, or start requiring a signed payload, without notice.
//! Every failure here is an ordinary event that falls back to "no lyrics"
//! rather than a fault — which is also why the caller reaches this only after
//! lrclib has already said no.

use std::time::Duration;

use serde::Deserialize;

use crate::lyrics::{Identity, Match};

/// What goes in `lyrics.provider`.
pub const PROVIDER: &str = "netease";

const SEARCH: &str = "https://music.163.com/api/search/get";
const LYRIC: &str = "https://music.163.com/api/song/lyric";

/// Sent on every request. The endpoint answers without it, but a bare request
/// to a site's own internal API is the kind of thing that gets blocked first.
const REFERER: &str = "https://music.163.com";

const TIMEOUT: Duration = Duration::from_secs(12);

/// How many search results are worth fetching lyrics for.
///
/// Each one is a second request, so this is the direct cost of using this
/// provider at all. Five is enough for the ranking to have a real choice --
/// the right song is essentially never sixth when the query carried an artist
/// -- without turning one lookup into a burst.
const FETCH_TOP: usize = 5;

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    result: Option<SearchResult>,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    #[serde(default)]
    songs: Vec<Song>,
}

#[derive(Debug, Deserialize)]
struct Song {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: Vec<Artist>,
    /// **Milliseconds**, unlike lrclib's seconds. Converting this in the wrong
    /// place would put every candidate a thousand times out of range and read
    /// as "nothing matched".
    #[serde(default)]
    duration: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct Artist {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct LyricResponse {
    #[serde(default)]
    lrc: Option<LyricText>,
    #[serde(default)]
    romalrc: Option<LyricText>,
}

#[derive(Debug, Deserialize)]
struct LyricText {
    #[serde(default)]
    lyric: Option<String>,
}

impl LyricText {
    fn text(field: &Option<LyricText>) -> Option<String> {
        field
            .as_ref()
            .and_then(|t| t.lyric.as_deref())
            .map(strip_credits)
            .filter(|text| !text.trim().is_empty())
    }
}

/// Removes NetEase's contributor block from the top of a lyric.
///
/// Their `lrc` track opens with the production credits, stamped at the very
/// start of the song:
///
/// ```text
/// [00:00.000] 制作人 : TK from 凛として時雨
/// [00:00.155] 作词 : TK from 凛として時雨
/// [00:00.620] 教えて教えてよ その仕組みを   <- the actual first line
/// ```
///
/// Left in, those render as the song's opening words and the highlight sits on
/// somebody's job title while the intro plays.
///
/// Only the *leading* run is dropped, and only lines carrying a spaced colon —
/// which is how these are written and how a lyric almost never is. A song that
/// really does open with " : " in its first line loses it; nothing later in
/// the track is touched at all.
fn strip_credits(lyric: &str) -> String {
    let mut lines = lyric.lines().peekable();
    let mut kept = Vec::new();

    while let Some(line) = lines.peek() {
        let text = line
            .rsplit_once(']')
            .map(|(_, text)| text)
            .unwrap_or(line)
            .trim();

        if text.contains(" : ") {
            lines.next();
            continue;
        }
        break;
    }

    kept.extend(lines);
    kept.join("\n")
}

/// Asks NetEase about a song.
pub async fn find(identity: &Identity) -> Result<Vec<Match>, String> {
    let query = match identity.artist.as_deref() {
        Some(artist) => format!("{artist} {}", identity.title),
        None => identity.title.clone(),
    };
    search(&query).await
}

/// Searches for free text, then fetches the words for the best few.
pub async fn search(query: &str) -> Result<Vec<Match>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let client = client()?;

    let response = client
        .post(SEARCH)
        .header("Referer", REFERER)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "s={}&type=1&limit=10&offset=0",
            crate::providers::percent_encode(query)
        ))
        .send()
        .await
        .map_err(|e| format!("NetEase did not answer: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "NetEase refused the request: {}",
            response.status()
        ));
    }

    let found: SearchResponse = response
        .json()
        .await
        .map_err(|e| format!("NetEase sent something unreadable: {e}"))?;

    let songs = found.result.map(|r| r.songs).unwrap_or_default();

    let mut matches = Vec::new();
    for song in songs.into_iter().take(FETCH_TOP) {
        // One song failing to yield lyrics is not the search failing.
        if let Ok(Some(found)) = lyrics_for(&client, &song).await {
            matches.push(found);
        }
    }

    Ok(matches)
}

/// Fetches one song by the id a candidate carried.
pub async fn get(id: i64) -> Result<Match, String> {
    let client = client()?;
    let song = Song {
        id,
        name: String::new(),
        artists: Vec::new(),
        duration: None,
    };

    lyrics_for(&client, &song)
        .await?
        .ok_or_else(|| "NetEase has no lyrics for that song.".to_string())
}

async fn lyrics_for(client: &reqwest::Client, song: &Song) -> Result<Option<Match>, String> {
    // `lv`/`tv`/`rv` select the original, translated and romanised tracks;
    // -1 asks for whichever version exists.
    let response = client
        .get(format!(
            "{LYRIC}?id={}&lv=-1&kv=-1&tv=-1&rv=-1",
            song.id
        ))
        .header("Referer", REFERER)
        .send()
        .await
        .map_err(|e| format!("NetEase did not answer: {e}"))?;

    if !response.status().is_success() {
        return Ok(None);
    }

    let found: LyricResponse = response
        .json()
        .await
        .map_err(|e| format!("NetEase sent something unreadable: {e}"))?;

    let original = LyricText::text(&found.lrc);
    let romaji = LyricText::text(&found.romalrc);

    if original.is_none() && romaji.is_none() {
        return Ok(None);
    }

    Ok(Some(Match {
        provider: PROVIDER,
        id: song.id,
        artist: song
            .artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        title: song.name.clone(),
        // Milliseconds on the wire, seconds everywhere in this crate.
        duration_secs: song.duration.map(|ms| ms as f64 / 1000.0),
        // NetEase has no instrumental flag: a song with no lyric rows simply
        // returns none, which is "we do not know" rather than "it has none".
        instrumental: false,
        synced: original,
        plain: None,
        romaji,
    }))
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| format!("Could not start an HTTPS client: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The credit block is not the first verse.
    #[test]
    fn leading_credits_are_dropped() {
        let lyric = "[00:00.000] 制作人 : TK\n\
                     [00:00.155] 作词 : TK\n\
                     [00:00.620]教えて教えてよ\n\
                     [00:07.410]僕の中に誰がいるの?";

        let cleaned = strip_credits(lyric);
        assert!(!cleaned.contains("制作人"), "a job title survived: {cleaned}");
        assert!(cleaned.starts_with("[00:00.620]"));
        assert_eq!(cleaned.lines().count(), 2);
    }

    /// Only the leading run, so a lyric that happens to contain a spaced colon
    /// later in the song keeps it.
    #[test]
    fn only_the_opening_run_is_treated_as_credits() {
        let lyric = "[00:00.620]a real first line\n\
                     [00:07.410]something : with a colon";

        assert_eq!(strip_credits(lyric).lines().count(), 2);
    }

    #[test]
    fn a_lyric_with_no_credits_is_untouched() {
        let lyric = "[00:00.620]one\n[00:07.410]two";
        assert_eq!(strip_credits(lyric), lyric);
    }

    /// Milliseconds on the wire, seconds in this crate.
    ///
    /// Getting this backwards puts every candidate a thousand times out of the
    /// duration window, which the ranking reports as "nothing matched" — a
    /// silent, total failure of the provider that looks like poor coverage.
    #[test]
    fn durations_arrive_in_milliseconds() {
        let song = Song {
            id: 1,
            name: "x".to_string(),
            artists: Vec::new(),
            duration: Some(240_666),
        };
        let secs = song.duration.map(|ms| ms as f64 / 1000.0);
        assert_eq!(secs, Some(240.666));
    }

    /// Live. Run deliberately, like the lrclib probe.
    ///
    /// ```text
    /// cargo test --lib netease::tests::live -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "talks to NetEase"]
    async fn live_netease_still_returns_romaji() {
        let identity = crate::lyrics::identify(Some("TK from 凛として時雨"), "unravel");
        let found = find(&identity).await.expect("NetEase");

        eprintln!("{} candidates", found.len());
        for candidate in &found {
            eprintln!(
                "  [{}] [{}] {:?}s synced={} romaji={}",
                candidate.artist,
                candidate.title,
                candidate.duration_secs,
                candidate.synced.is_some(),
                candidate.romaji.is_some(),
            );
        }

        assert!(!found.is_empty(), "no candidates at all");
        assert!(
            found.iter().any(|c| c.romaji.is_some()),
            "no romanisation, which is the only reason this provider is here"
        );
    }
}
