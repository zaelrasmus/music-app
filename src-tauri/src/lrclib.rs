//! lrclib, the lyrics provider.
//!
//! No key, no account, no token to keep alive: one GET returns candidates as
//! JSON. That, and a catalogue contributed under CC0, is why it is the only
//! provider enabled by default.
//!
//! # One request, not two
//!
//! lrclib offers `/api/get`, which takes a duration and matches it server-side
//! within about two seconds, and `/api/search`, which returns up to twenty
//! candidates and leaves the choosing to the caller. Only the second is used.
//!
//! The reason is this library specifically. Its tracks are largely YouTube
//! rips, and two of them were measured carrying **14.6 and 5.6 seconds of
//! trailing silence** -- so a stored duration is regularly that much longer
//! than the released recording it corresponds to. `/api/get` would reject
//! those outright, and every one of those rejections costs a request that
//! teaches us nothing. `/api/search` hands back the same rows and lets
//! [`crate::lyrics::choose`] apply a window shaped like the actual error:
//! generous when our copy is *longer*, tight when it is shorter.
//!
//! Trying `/api/get` first and falling back would be two requests on exactly
//! the tracks most likely to miss, against infrastructure somebody donates.
//!
//! # Standing on someone else's goodwill
//!
//! lrclib is run for free. Everything here is shaped by that: one request in
//! flight at a time, a floor on how often, a real user agent, and a negative
//! cache so a track with no lyrics is asked about once rather than on every
//! play. The pacing lives in [`crate::lyrics`], where the cache is.

use std::time::Duration;

use serde::Deserialize;

use crate::lyrics::{Identity, Match};

/// What goes in `lyrics.provider`.
pub const PROVIDER: &str = "lrclib";

const SEARCH: &str = "https://lrclib.net/api/search";

/// Long enough for a slow answer, short enough that opening the lyrics panel
/// never feels hung.
const TIMEOUT: Duration = Duration::from_secs(12);

/// lrclib asks callers to identify themselves. Doing so honestly is the least
/// this can do, and it is what lets them throttle a misbehaving build rather
/// than the endpoint.
const USER_AGENT: &str = concat!(
    "music-app v",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/zaelrasmus/music-app)"
);

/// More than a page of results is nothing the ranking can use.
const MAX_CANDIDATES: usize = 40;

/// One row from lrclib.
///
/// Every field is `default`ed rather than required: this is somebody else's
/// API and a missing key must degrade a candidate, not fail the whole search.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// lrclib's own row id, and what makes a candidate addressable later.
    ///
    /// Without it a list shown to the user would be a list of things they
    /// cannot pick: the choice has to survive a round trip back to
    /// [`get`], and re-running the search and hoping the same row is in the
    /// same place is not a plan.
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub track_name: String,
    #[serde(default)]
    pub artist_name: String,
    /// Seconds. `f64` because lrclib sends `210.0`, not `210` -- and this one
    /// really is a REAL, unlike every duration in our own schema.
    #[serde(default)]
    pub duration: Option<f64>,
    /// A positive answer with no words in it. Worth as much as a lyric on a
    /// library of game soundtracks, and dropped by every player that treats
    /// "no text" as "no result".
    #[serde(default)]
    pub instrumental: bool,
    #[serde(default)]
    pub synced_lyrics: Option<String>,
    #[serde(default)]
    pub plain_lyrics: Option<String>,
}

impl Candidate {
    pub fn synced(&self) -> Option<&str> {
        non_empty(self.synced_lyrics.as_deref())
    }

    pub fn plain(&self) -> Option<&str> {
        non_empty(self.plain_lyrics.as_deref())
    }

    /// Into the shape the ranking works on.
    ///
    /// `romaji` is always `None`: lrclib has no romanised track, which is the
    /// single concrete reason NetEase is worth a second provider rather than
    /// just more coverage.
    pub fn into_match(self) -> Match {
        Match {
            provider: PROVIDER,
            id: self.id,
            artist: self.artist_name.clone(),
            title: self.track_name.clone(),
            duration_secs: self.duration,
            instrumental: self.instrumental,
            synced: self.synced().map(str::to_string),
            plain: self.plain().map(str::to_string),
            romaji: None,
        }
    }
}

fn non_empty(text: Option<&str>) -> Option<&str> {
    text.filter(|t| !t.trim().is_empty())
}

/// Asks lrclib about a song.
///
/// An empty vector means "asked, nothing matched" and is a fact worth
/// caching. `Err` means the question never got through, and must not be --
/// writing a negative cache row for a flaky connection would hide the song
/// for a fortnight.
pub async fn find(identity: &Identity) -> Result<Vec<Match>, String> {
    let found = fetch(&query_url(identity)).await?;

    // A fielded query that matched nothing is not the end of it.
    //
    // lrclib matches `artist_name` as a prefix, so an artist we hold in a
    // longer form than it does matches *nothing at all* rather than matching
    // loosely — `Ivycomb Music` against a stored `ivycomb` returns zero.
    // `clean_artist` removes the decoration this library actually carries, but
    // it cannot know every way a channel is named, so dropping the artist and
    // asking by title is the last thing worth trying.
    //
    // Deliberately a second request rather than the first one: a title-only
    // search is much weaker, and what comes back is usually several songs that
    // merely share a name. That is why the caller can end up asking the user
    // instead of choosing.
    if found.is_empty() && identity.artist.is_some() {
        return fetch(&format!(
            "{SEARCH}?track_name={}",
            crate::providers::percent_encode(&identity.title)
        ))
        .await;
    }

    Ok(found)
}

/// One request, and the rules for reading its answer.
async fn fetch(url: &str) -> Result<Vec<Match>, String> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("Could not start an HTTPS client: {e}"))?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("lrclib did not answer: {e}"))?;

    // 404 is lrclib's "no such track", which is an answer rather than a fault.
    if response.status().as_u16() == 404 {
        return Ok(Vec::new());
    }
    if !response.status().is_success() {
        return Err(format!("lrclib refused the request: {}", response.status()));
    }

    let mut candidates: Vec<Candidate> = response
        .json()
        .await
        .map_err(|e| format!("lrclib sent something unreadable: {e}"))?;

    candidates.truncate(MAX_CANDIDATES);
    Ok(candidates
        .into_iter()
        .map(Candidate::into_match)
        .filter(Match::is_useful)
        .collect())
}

/// Fetches one row by the id a candidate carried.
///
/// The other half of showing someone a list: they pick a row, and this is what
/// turns that pick back into lyrics. `/api/get/{id}` answers with the same
/// shape a search does.
pub async fn get(id: i64) -> Result<Match, String> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("Could not start an HTTPS client: {e}"))?;

    let response = client
        .get(format!("https://lrclib.net/api/get/{id}"))
        .send()
        .await
        .map_err(|e| format!("lrclib did not answer: {e}"))?;

    if response.status().as_u16() == 404 {
        return Err("Those lyrics are no longer on lrclib.".to_string());
    }
    if !response.status().is_success() {
        return Err(format!("lrclib refused the request: {}", response.status()));
    }

    let candidate: Candidate = response
        .json()
        .await
        .map_err(|e| format!("lrclib sent something unreadable: {e}"))?;

    Ok(candidate.into_match())
}

/// Searches for whatever the user typed, rather than for a track's own tags.
///
/// Free text, because the point of asking is that the tags were wrong. Goes
/// through the same `q=` endpoint the artist-less path uses.
pub async fn search(query: &str) -> Result<Vec<Match>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    fetch(&format!(
        "{SEARCH}?q={}",
        crate::providers::percent_encode(query)
    ))
    .await
}

/// The search URL for a song.
///
/// Two shapes, because the useful query depends on what is known. With an
/// artist, the fielded form asks a precise question. Without one -- which is
/// 94% of this library -- `q=` is the only thing left, and it is a much weaker
/// question: `q=Chop Suey` returns an August Burns Red cover, a fifty-second
/// stub and three System of a Down rows of different lengths. Narrowing that
/// back down is the ranker's job, not this function's.
///
/// Encoding goes through `providers::percent_encode` rather than being spliced
/// in raw. Veluna does `title.replace(' ', "+").replace('&', "%26")`, which
/// this library breaks immediately: a `#` truncates the query at the fragment,
/// and there are titles here carrying `/`, `%` and full-width brackets.
fn query_url(identity: &Identity) -> String {
    use crate::providers::percent_encode;

    match identity.artist.as_deref() {
        Some(artist) => format!(
            "{SEARCH}?artist_name={}&track_name={}",
            percent_encode(artist),
            percent_encode(&identity.title)
        ),
        None => format!("{SEARCH}?q={}", percent_encode(&identity.title)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::identify;

    #[test]
    fn a_known_artist_asks_the_fielded_question() {
        let url = query_url(&identify(Some("Set It Off"), "Duality"));
        assert_eq!(
            url,
            "https://lrclib.net/api/search?artist_name=Set+It+Off&track_name=Duality"
        );
    }

    #[test]
    fn an_unknown_artist_falls_back_to_a_free_text_query() {
        let url = query_url(&identify(None, "Duality"));
        assert_eq!(url, "https://lrclib.net/api/search?q=Duality");
    }

    /// The characters that break a hand-rolled encoder.
    ///
    /// `#` is the dangerous one: spliced in raw it starts a fragment and
    /// silently truncates everything after it, so the query becomes a
    /// different, shorter question that quietly returns wrong rows.
    #[test]
    fn awkward_titles_survive_encoding() {
        let url = query_url(&identify(None, "a#b&c%d/e"));
        assert_eq!(url, "https://lrclib.net/api/search?q=a%23b%26c%25d%2Fe");
        assert!(!url["https://lrclib.net/api/search?q=".len()..].contains('#'));
    }

    #[test]
    fn a_stub_with_no_lyrics_is_not_useful() {
        assert!(!Candidate::default().into_match().is_useful());

        let instrumental = Candidate {
            instrumental: true,
            ..Default::default()
        };
        assert!(
            instrumental.into_match().is_useful(),
            "an instrumental row is an answer"
        );

        let blank = Candidate {
            plain_lyrics: Some("   ".to_string()),
            ..Default::default()
        };
        assert!(
            !blank.into_match().is_useful(),
            "whitespace is not a lyric"
        );
    }

    /// Shape pinned against a real `/api/search` response body.
    #[test]
    fn a_real_response_body_deserialises() {
        let body = r#"[
            {"id":464567,"name":"Chop Suey!","trackName":"Chop Suey!",
             "artistName":"System of a Down","albumName":"Toxicity",
             "duration":210.0,"instrumental":false,
             "plainLyrics":"Wake up","syncedLyrics":"[00:00.63] Wake up"}
        ]"#;

        let parsed: Vec<Candidate> = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].artist_name, "System of a Down");
        assert_eq!(parsed[0].duration, Some(210.0));
        assert_eq!(parsed[0].synced(), Some("[00:00.63] Wake up"));
    }

    /// An unknown field must not fail the parse. lrclib adds keys over time
    /// and a strict struct would turn that into an outage.
    #[test]
    fn an_unexpected_field_does_not_break_the_parse() {
        let body = r#"[{"trackName":"x","artistName":"y","somethingNew":42,
                        "instrumental":true}]"#;
        let parsed: Vec<Candidate> = serde_json::from_str(body).unwrap();
        assert!(parsed[0].instrumental);
    }

    /// Live. Run deliberately, like `ytmusic_probe`: it is a question about
    /// somebody else's server, and the answer can change without this code
    /// changing.
    ///
    /// ```text
    /// cargo test --lib lrclib::tests::live -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "talks to lrclib"]
    async fn live_lrclib_still_answers_the_way_this_expects() {
        for (artist, title) in [
            (Some("Set It Off"), "Duality"),
            (Some("Hideki Taniuchi - Topic"), "Kodoku"),
            (None, "Celeste Original Soundtrack Checking In"),
        ] {
            let identity = identify(artist, title);
            match find(&identity).await {
                Ok(found) => {
                    eprintln!("{:?} / {} -> {} candidates", identity.artist, identity.title, found.len());
                    for candidate in found.iter().take(3) {
                        eprintln!(
                            "    {} | {} | {:?}s | instrumental {} | synced {}",
                            candidate.artist,
                            candidate.title,
                            candidate.duration_secs,
                            candidate.instrumental,
                            candidate.synced.is_some(),
                        );
                    }
                }
                Err(e) => eprintln!("{title} -> ERROR {e}"),
            }
        }
    }
}
