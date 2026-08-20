//! SoundCloud's own search API, for the two questions yt-dlp cannot answer.
//!
//! yt-dlp searches SoundCloud tracks (`scsearch`) and expands any set or user
//! page by URL, but it exposes no *search* for playlists or users at all --
//! `soundcloud.com/search/sets?q=` is routed to the user extractor and 404s.
//! So the only way to find a SoundCloud playlist by name is the API its own
//! web player uses.
//!
//! **The shape this takes, and why.** Searching goes through here; everything
//! afterwards goes back through yt-dlp. A result is a title and a
//! `permalink_url`, and that URL is handed straight to the same expansion that
//! serves YouTube. So this module is confined to discovery, and the worst it
//! can do when it breaks is fail to find things -- it is never on the path to
//! playing one.
//!
//! **What makes it fragile.** `api-v2` is undocumented, and it demands a
//! `client_id` that appears in no documentation either: the web player carries
//! one inside its JavaScript, and it rotates. Both facts are worked around
//! below the way yt-dlp itself works around them, which is the strongest
//! precedent available -- and the same caveat applies as to YouTube's search
//! filters: no yt-dlp update fixes this if it changes, because yt-dlp is not
//! involved.

use std::time::Duration;

use serde::Deserialize;
use tokio::sync::Mutex;

use crate::collections::Collection;
use crate::providers::{Provider, SearchKind};
use crate::youtube::SearchResult;

/// How long to wait on SoundCloud before giving up.
///
/// A search the user is watching; two of these can happen in a row when a
/// stale `client_id` forces a retry, so it stays well inside the patience of
/// someone who has just pressed a key.
const TIMEOUT: Duration = Duration::from_secs(12);

/// The `client_id` in use, once one has been found.
///
/// Cached for the life of the process rather than refetched per search:
/// finding one costs a page load plus a JavaScript bundle. It is dropped and
/// re-found only when SoundCloud rejects it, which is the sole reliable
/// signal that it has rotated -- there is no expiry to read.
///
/// An async lock, held *across* the discovery rather than around the cache
/// read, which makes finding one single-flight: callers that arrive while a
/// search is already looking wait for its answer instead of starting their own
/// scrape. Observed the other way round first -- five concurrent lookups, each
/// fetching the home page and a megabyte of JavaScript, and SoundCloud
/// refusing several of them.
static CLIENT_ID: Mutex<Option<String>> = Mutex::const_new(None);

/// Finds playlists or users matching `query`.
pub async fn search(
    kind: SearchKind,
    query: &str,
    limit: u32,
) -> Result<Vec<Collection>, String> {
    let endpoint = match kind {
        SearchKind::Playlist => "playlists",
        SearchKind::Artist => "users",
        // Tracks go through yt-dlp like every other provider's do; there is
        // nothing this API offers that `scsearch` does not.
        SearchKind::Track => return Ok(Vec::new()),
    };

    let client = client()?;

    // One retry, and only on the failure that means the key rotated. A
    // rejected key is indistinguishable from a wrong one, and both are fixed
    // the same way; anything else is a real failure and is reported as one.
    match fetch(&client, endpoint, kind, query, limit, cached_client_id(&client).await?).await {
        Ok(found) => Ok(found),
        Err(Rejected::Stale) => {
            forget_client_id().await;
            let fresh = cached_client_id(&client).await?;
            fetch(&client, endpoint, kind, query, limit, fresh)
                .await
                .map_err(|e| e.into_message())
        }
        Err(other) => Err(other.into_message()),
    }
}

/// The page to expand for an artist's own songs.
///
/// A bare SoundCloud user URL is the **All** tab, which includes everything
/// they have reposted -- so an artist page built from it fills with other
/// people's music, correctly credited to whoever uploaded it, which is exactly
/// what makes it confusing. `/tracks` is the tab that means "what this person
/// made": measured on one account, 69 entries of which 36 were someone else's,
/// against 30 that were all their own.
///
/// Left alone if the URL already names a tab, so a deliberate `/sets` or
/// `/likes` still means what it says.
pub fn artist_tracks_url(url: &str) -> String {
    const TABS: [&str; 6] = ["/tracks", "/albums", "/sets", "/reposts", "/likes", "/spotlight"];

    let trimmed = url.trim_end_matches('/');
    if TABS.iter().any(|tab| trimmed.ends_with(tab)) {
        return trimmed.to_string();
    }

    format!("{trimmed}/tracks")
}

/// What a set or user page says about itself.
///
/// One `/resolve` call. yt-dlp's envelope cannot answer this: for a user it
/// carries no picture at all, and it titles the page after the tab -- "Daft
/// Punk (All)" -- which is how "All" ended up displayed as an artist's name.
pub async fn describe(url: &str, kind: SearchKind) -> Result<Collection, String> {
    let client = client()?;
    let resolved = resolve(&client, url).await?;

    Ok(resolved.into_collection(kind, url))
}

/// Every track in a set, in order.
///
/// yt-dlp cannot be used for this, and the reason is worth stating exactly
/// because the symptom misleads. SoundCloud returns a set with only its first
/// few tracks filled in and the rest as bare `{id}` stubs. yt-dlp faithfully
/// emits all of them -- but a stub carries no page URL, so every one is
/// dropped as unplayable, and a 33-track playlist arrives as 5. It looks like
/// a fixed limit of five; it is however many SoundCloud happened to fill in.
///
/// The stubs still carry ids, and ids are all this needs: measured on that
/// same set, 5 tracks through yt-dlp against 33 through here.
pub async fn expand_set(url: &str, limit: u32) -> Result<Vec<SearchResult>, String> {
    let client = client()?;
    let resolved = resolve(&client, url).await?;

    let mut tracks: Vec<SearchResult> = resolved
        .tracks
        .into_iter()
        .take(limit as usize)
        .map(|track| track.into_result())
        .collect();

    if tracks.is_empty() {
        return Err("There is nothing in that playlist.".to_string());
    }

    hydrate(&mut tracks).await;

    // A stub that hydration could not fill has no page to play. Dropped here
    // rather than shown as a row that fails when clicked.
    tracks.retain(|track| !track.remote_url.is_empty());

    Ok(tracks)
}

async fn resolve(client: &reqwest::Client, url: &str) -> Result<Resolved, String> {
    let request = |client_id: String| {
        format!(
            "https://api-v2.soundcloud.com/resolve?url={}&client_id={}",
            crate::providers::percent_encode(url),
            crate::providers::percent_encode(&client_id),
        )
    };

    let first = request(cached_client_id(client).await?);
    let response = match send_json::<Resolved>(client, &first).await {
        Ok(resolved) => return Ok(resolved),
        Err(Rejected::Stale) => {
            forget_client_id().await;
            let retry = request(cached_client_id(client).await?);
            send_json::<Resolved>(client, &retry).await
        }
        Err(other) => Err(other),
    };

    response.map_err(|e| e.into_message())
}

async fn send_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T, Rejected> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| Rejected::Other(format!("Could not reach SoundCloud: {e}")))?;

    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(Rejected::Stale);
    }
    if !response.status().is_success() {
        return Err(Rejected::Other(format!(
            "SoundCloud refused that ({}).",
            response.status()
        )));
    }

    response
        .json()
        .await
        .map_err(|e| Rejected::Other(format!("Could not read SoundCloud's answer: {e}")))
}

/// A set or a user, as `/resolve` describes it.
#[derive(Debug, Deserialize)]
struct Resolved {
    /// A set's name.
    #[serde(default)]
    title: Option<String>,
    /// A user's name.
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    track_count: Option<u64>,
    #[serde(default)]
    followers_count: Option<u64>,
    #[serde(default)]
    artwork_url: Option<String>,
    #[serde(default)]
    avatar_url: Option<String>,
    #[serde(default)]
    user: Option<EntryUser>,
    /// A set's contents. Mostly stubs beyond the first few.
    #[serde(default)]
    tracks: Vec<TrackDetail>,
}

impl Resolved {
    fn into_collection(self, kind: SearchKind, url: &str) -> Collection {
        let uploader = self.user.as_ref().and_then(|user| user.username.clone());

        Collection {
            provider: Provider::SoundCloud,
            kind,
            url: url.to_string(),
            title: match kind {
                SearchKind::Artist => self.username.or(self.title),
                _ => self.title.or(self.username),
            }
            .unwrap_or_else(|| "(untitled)".to_string()),
            uploader: match kind {
                SearchKind::Artist => None,
                _ => uploader,
            },
            item_count: self.track_count,
            follower_count: self.followers_count,
            thumbnail_url: self
                .artwork_url
                .or(self.avatar_url)
                .or_else(|| self.user.and_then(|user| user.avatar_url)),
        }
    }
}

/// How many track ids one hydration request carries.
///
/// SoundCloud accepts a list; fifty keeps the URL well short of anything a
/// server or proxy objects to, and a 200-track playlist costs four requests.
const HYDRATE_BATCH: usize = 50;

/// Fills in what yt-dlp's flat listing leaves out.
///
/// `--flat-playlist` is what makes opening a collection take seconds rather
/// than minutes, and the price is that SoundCloud's flat entries are almost
/// empty: a *set* lists its tracks as bare ids with no title at all, and
/// neither sets nor users carry artwork. Every row therefore arrives as
/// "(untitled)" with a generated tile -- which is not a cosmetic problem, it
/// makes a playlist unreadable.
///
/// The ids are the part yt-dlp gets right, and the API turns ids into
/// everything else. So this fills the gaps rather than replacing the
/// extraction: order, URLs and the cap all stay where they already work.
///
/// Best effort throughout. A failure here leaves the listing exactly as
/// yt-dlp produced it, which is worse-looking but still playable, and that is
/// a far better outcome than refusing to open a playlist because its pictures
/// could not be fetched.
pub async fn hydrate(tracks: &mut [SearchResult]) {
    if tracks.is_empty() {
        return;
    }

    let Ok(client) = client() else {
        return;
    };
    let Ok(client_id) = cached_client_id(&client).await else {
        return;
    };

    for batch in tracks.chunks_mut(HYDRATE_BATCH) {
        let ids: Vec<&str> = batch.iter().map(|track| track.remote_id.as_str()).collect();

        let url = format!(
            "https://api-v2.soundcloud.com/tracks?ids={}&client_id={}",
            ids.join("%2C"),
            crate::providers::percent_encode(&client_id),
        );

        // `continue`, not `return`: one refused batch used to abandon every
        // later one, so a 150-track playlist could come back with its first
        // fifty named and the rest left as "(untitled)" by an unknown artist.
        let Ok(response) = client.get(&url).send().await else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let Ok(details) = response.json::<Vec<TrackDetail>>().await else {
            continue;
        };

        // Returned in the API's order, not the one asked for -- measured, and
        // it does not match. Matching by id is the only way to put a title on
        // the right row.
        for track in batch.iter_mut() {
            let Some(detail) = details
                .iter()
                .find(|detail| detail.id.to_string() == track.remote_id)
            else {
                continue;
            };

            if let Some(title) = detail.title.clone() {
                track.title = title;
            }
            if track.channel.is_none() {
                track.channel = detail.user.as_ref().and_then(|user| user.username.clone());
            }
            if track.duration_secs.is_none() {
                // Milliseconds, unlike everything else in this app.
                track.duration_secs = detail.duration.map(|ms| ms as f64 / 1000.0);
            }
            if track.remote_url.is_empty() {
                if let Some(url) = detail
                    .permalink_url
                    .clone()
                    .filter(|url| Provider::SoundCloud.accepts_url(url))
                {
                    track.remote_url = url;
                }
            }
            if track.channel_url.is_none() {
                track.channel_url = detail
                    .user
                    .as_ref()
                    .and_then(|user| user.permalink_url.clone());
            }
            track.thumbnail_url = detail
                .artwork_url
                .clone()
                .or_else(|| detail.user.as_ref().and_then(|user| user.avatar_url.clone()));
        }
    }
}

/// One track as the API describes it.
///
/// Everything but the id is optional because a set lists most of its contents
/// as `{id, kind, policy}` and nothing else -- the stub this exists to fill.
#[derive(Debug, Deserialize)]
struct TrackDetail {
    id: u64,
    #[serde(default)]
    title: Option<String>,
    /// Milliseconds.
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    permalink_url: Option<String>,
    #[serde(default)]
    artwork_url: Option<String>,
    #[serde(default)]
    user: Option<EntryUser>,
}

impl TrackDetail {
    /// The row this track will be, before hydration fills the gaps.
    ///
    /// A stub becomes a result with an id and nothing else, which is exactly
    /// what `hydrate` needs and what it matches on.
    fn into_result(self) -> SearchResult {
        let user = self.user;

        SearchResult {
            provider: Provider::SoundCloud,
            remote_id: self.id.to_string(),
            // Empty when this is a stub. Filled by hydration, and dropped by
            // the caller if it never is -- a row with no page cannot be played.
            remote_url: self
                .permalink_url
                .filter(|url| Provider::SoundCloud.accepts_url(url))
                .unwrap_or_default(),
            title: self.title.unwrap_or_else(|| "(untitled)".to_string()),
            channel: user.as_ref().and_then(|user| user.username.clone()),
            duration_secs: self.duration.map(|ms| ms as f64 / 1000.0),
            view_count: None,
            thumbnail_url: self.artwork_url,
            channel_url: user.and_then(|user| user.permalink_url),
            is_live: false,
            uploaded_at: None,
        }
    }
}

/// Why a request did not produce results.
enum Rejected {
    /// SoundCloud refused the key. The one failure worth retrying.
    Stale,
    Other(String),
}

impl Rejected {
    fn into_message(self) -> String {
        match self {
            Rejected::Stale => {
                "SoundCloud would not accept the app's search key. Try again in a moment."
                    .to_string()
            }
            Rejected::Other(message) => message,
        }
    }
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| format!("Could not start an HTTPS client: {e}"))
}

async fn fetch(
    client: &reqwest::Client,
    endpoint: &str,
    kind: SearchKind,
    query: &str,
    limit: u32,
    client_id: String,
) -> Result<Vec<Collection>, Rejected> {
    // Built rather than assembled by the client: reqwest's own query builder
    // is behind a feature this does not otherwise need, and the encoding rule
    // is already written down once in `providers`. The key is encoded too --
    // it is always alphanumeric, so this changes nothing today and cannot be
    // the reason a stranger value breaks the URL tomorrow.
    let url = format!(
        "https://api-v2.soundcloud.com/search/{endpoint}?q={}&client_id={}&limit={limit}",
        crate::providers::percent_encode(query),
        crate::providers::percent_encode(&client_id),
    );

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| Rejected::Other(format!("Could not reach SoundCloud: {e}")))?;

    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(Rejected::Stale);
    }
    if !response.status().is_success() {
        return Err(Rejected::Other(format!(
            "SoundCloud refused that search ({}).",
            response.status()
        )));
    }

    let page: SearchPage = response
        .json()
        .await
        .map_err(|e| Rejected::Other(format!("Could not read SoundCloud's answer: {e}")))?;

    Ok(page
        .collection
        .into_iter()
        .filter_map(|entry| entry.into_collection(kind))
        .collect())
}

// --- the response ------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SearchPage {
    #[serde(default)]
    collection: Vec<Entry>,
}

/// One playlist or user.
///
/// The two endpoints return different shapes, and this holds the union rather
/// than being split in two: they differ by three fields, and a second type
/// would mean a second parse, a second conversion and a second place for the
/// URL check to be forgotten.
#[derive(Debug, Deserialize)]
struct Entry {
    /// The page, and the only field that must be right: it is what expansion,
    /// playback and import all go through afterwards.
    permalink_url: Option<String>,
    /// A playlist's name.
    #[serde(default)]
    title: Option<String>,
    /// A user's name.
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    track_count: Option<u64>,
    #[serde(default)]
    followers_count: Option<u64>,
    #[serde(default)]
    artwork_url: Option<String>,
    #[serde(default)]
    avatar_url: Option<String>,
    #[serde(default)]
    user: Option<EntryUser>,
    /// A playlist's contents, of which only the first is ever read.
    #[serde(default)]
    tracks: Vec<EntryTrack>,
}

#[derive(Debug, Deserialize)]
struct EntryUser {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    avatar_url: Option<String>,
    /// Their page, so a track inside a playlist still links to its artist.
    #[serde(default)]
    permalink_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EntryTrack {
    #[serde(default)]
    artwork_url: Option<String>,
}

impl Entry {
    fn into_collection(self, kind: SearchKind) -> Option<Collection> {
        // Checked here rather than trusted, because this URL goes on to be a
        // subprocess argument. A host that is not SoundCloud's is dropped.
        let url = self
            .permalink_url
            .filter(|url| Provider::SoundCloud.accepts_url(url))?;

        let uploader = self.user.as_ref().and_then(|user| user.username.clone());

        // Playlists are titled; users are named. Whichever the endpoint sent.
        let title = self.title.or(self.username)?;

        // A playlist with no cover of its own borrows its first track's, which
        // is what SoundCloud's own web player shows -- an empty tile in a grid
        // of covers reads as a broken playlist rather than an unillustrated
        // one. The uploader's avatar is the last resort.
        let thumbnail_url = self
            .artwork_url
            .or_else(|| self.tracks.first().and_then(|track| track.artwork_url.clone()))
            .or(self.avatar_url)
            .or_else(|| self.user.and_then(|user| user.avatar_url));

        Some(Collection {
            provider: Provider::SoundCloud,
            kind,
            url,
            title,
            // An artist row's uploader is its own name, which would read as
            // "Daft Punk, by Daft Punk".
            uploader: match kind {
                SearchKind::Artist => None,
                _ => uploader,
            },
            item_count: self.track_count,
            follower_count: self.followers_count,
            thumbnail_url,
        })
    }
}

// --- the client id -----------------------------------------------------

async fn cached_client_id(client: &reqwest::Client) -> Result<String, String> {
    // Held across the discovery on purpose -- see CLIENT_ID.
    let mut cached = CLIENT_ID.lock().await;

    if let Some(id) = cached.as_ref() {
        return Ok(id.clone());
    }

    let found = discover_client_id(client).await?;
    *cached = Some(found.clone());

    Ok(found)
}

async fn forget_client_id() {
    *CLIENT_ID.lock().await = None;
}

/// Finds a usable `client_id` the way the web player hands one out.
///
/// The home page loads a handful of JavaScript bundles and exactly one of them
/// carries the key. Which one is not stable, so they are tried in order --
/// measured today: the ninth of nine, which is an argument against guessing.
async fn discover_client_id(client: &reqwest::Client) -> Result<String, String> {
    let home = client
        .get("https://soundcloud.com/")
        .send()
        .await
        .map_err(|e| format!("Could not reach SoundCloud: {e}"))?
        .text()
        .await
        .map_err(|e| format!("Could not read SoundCloud's home page: {e}"))?;

    for src in script_urls(&home) {
        let Ok(response) = client.get(&src).send().await else {
            continue;
        };
        let Ok(script) = response.text().await else {
            continue;
        };

        if let Some(found) = client_id_in(&script) {
            return Ok(found);
        }
    }

    Err("Could not find SoundCloud's search key. Searching for tracks still works.".to_string())
}

/// The script bundles a page loads from SoundCloud's asset host.
///
/// Deliberately not a regex, and not an HTML parser: this looks for one
/// attribute on one host, and either dependency would be a large answer to a
/// small question. Anything it mis-parses simply fails to be a URL that
/// returns JavaScript, and the next candidate is tried.
fn script_urls(html: &str) -> Vec<String> {
    const PREFIX: &str = "https://a-v2.sndcdn.com/assets/";

    let mut found = Vec::new();
    let mut rest = html;

    while let Some(start) = rest.find(PREFIX) {
        rest = &rest[start..];

        // The URL ends at the quote that closes the attribute.
        let Some(end) = rest.find(['"', '\'']) else {
            break;
        };

        let url = &rest[..end];
        if url.ends_with(".js") {
            found.push(url.to_string());
        }

        rest = &rest[end..];
    }

    found
}

/// Pulls a `client_id` out of a script bundle.
///
/// The key appears as `client_id:"…"` or `client_id="…"` depending on how the
/// bundle was minified, and is 32 characters of `[A-Za-z0-9]`. Length and
/// alphabet are both checked, so a shorter lookalike elsewhere in a megabyte
/// of JavaScript cannot be mistaken for one.
fn client_id_in(script: &str) -> Option<String> {
    const KEY: &str = "client_id";
    const LENGTH: usize = 32;

    let mut rest = script;

    while let Some(start) = rest.find(KEY) {
        rest = &rest[start + KEY.len()..];

        let mut chars = rest.chars();
        // `client_id` followed by an assignment and an opening quote, in
        // either of the two forms minifiers produce.
        if !matches!(chars.next(), Some(':') | Some('=')) {
            continue;
        }
        if !matches!(chars.next(), Some('"') | Some('\'')) {
            continue;
        }

        let value: String = rest[2..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();

        if value.len() == LENGTH {
            return Some(value);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minified_client_id_is_found_in_either_form() {
        assert_eq!(
            client_id_in(r#"x.client_id:"abcdefghij0123456789ABCDEFGHIJ01",y"#).as_deref(),
            Some("abcdefghij0123456789ABCDEFGHIJ01")
        );
        assert_eq!(
            client_id_in(r#"var client_id="abcdefghij0123456789ABCDEFGHIJ01";"#).as_deref(),
            Some("abcdefghij0123456789ABCDEFGHIJ01")
        );
    }

    #[test]
    fn something_shorter_is_not_mistaken_for_a_key() {
        // A megabyte of minified JavaScript contains the words `client_id` in
        // places that are not the key. Length is what tells them apart.
        assert!(client_id_in(r#"client_id:"short""#).is_none());
        assert!(client_id_in(r#"client_id:"" "#).is_none());
        assert!(client_id_in("client_id").is_none());
        assert!(client_id_in("no key at all").is_none());
    }

    #[test]
    fn a_real_key_is_found_past_an_earlier_mention() {
        // The first occurrence is usually a parameter name, not a value.
        let script = r#"function f(client_id){}...{client_id:"abcdefghij0123456789ABCDEFGHIJ01"}"#;
        assert_eq!(
            client_id_in(script).as_deref(),
            Some("abcdefghij0123456789ABCDEFGHIJ01")
        );
    }

    #[test]
    fn only_javascript_on_the_asset_host_is_collected() {
        let html = r#"
            <script src="https://a-v2.sndcdn.com/assets/0-abc.js"></script>
            <script src="https://a-v2.sndcdn.com/assets/1-def.js"></script>
            <link href="https://a-v2.sndcdn.com/assets/style.css">
            <script src="https://example.com/other.js"></script>
        "#;

        assert_eq!(
            script_urls(html),
            vec![
                "https://a-v2.sndcdn.com/assets/0-abc.js",
                "https://a-v2.sndcdn.com/assets/1-def.js"
            ]
        );
    }

    #[test]
    fn a_playlist_entry_becomes_a_collection() {
        let entry: Entry = serde_json::from_str(
            r#"{
                "title": "Discovery",
                "track_count": 14,
                "permalink_url": "https://soundcloud.com/daftpunkofficialmusic/sets/discovery-8",
                "artwork_url": "https://i1.sndcdn.com/artworks-large.jpg",
                "user": { "username": "Daft Punk", "avatar_url": "https://i1.sndcdn.com/avatars.jpg" }
            }"#,
        )
        .unwrap();

        let collection = entry.into_collection(SearchKind::Playlist).unwrap();
        assert_eq!(collection.title, "Discovery");
        assert_eq!(collection.uploader.as_deref(), Some("Daft Punk"));
        assert_eq!(collection.item_count, Some(14));
        assert_eq!(
            collection.thumbnail_url.as_deref(),
            Some("https://i1.sndcdn.com/artworks-large.jpg")
        );
    }

    #[test]
    fn a_playlist_with_no_cover_borrows_its_first_tracks() {
        let entry: Entry = serde_json::from_str(
            r#"{
                "title": "Untitled",
                "permalink_url": "https://soundcloud.com/u/sets/x",
                "artwork_url": null,
                "tracks": [{ "artwork_url": "https://i1.sndcdn.com/first-track.jpg" }]
            }"#,
        )
        .unwrap();

        assert_eq!(
            entry
                .into_collection(SearchKind::Playlist)
                .unwrap()
                .thumbnail_url
                .as_deref(),
            Some("https://i1.sndcdn.com/first-track.jpg"),
            "an empty tile reads as a broken playlist, not an unillustrated one"
        );
    }

    #[test]
    fn a_user_entry_becomes_an_artist() {
        let entry: Entry = serde_json::from_str(
            r#"{
                "username": "Daft Punk",
                "track_count": 216,
                "permalink_url": "https://soundcloud.com/daftpunkofficialmusic",
                "avatar_url": "https://i1.sndcdn.com/avatars.jpg"
            }"#,
        )
        .unwrap();

        let collection = entry.into_collection(SearchKind::Artist).unwrap();
        assert_eq!(collection.title, "Daft Punk");
        assert_eq!(collection.item_count, Some(216));
        assert_eq!(
            collection.thumbnail_url.as_deref(),
            Some("https://i1.sndcdn.com/avatars.jpg")
        );
    }

    /// The same entry, read two ways.
    ///
    /// Written like this on purpose: a fixture with no `user` at all would
    /// come back with no uploader whatever the rule said, and pass while
    /// proving nothing. The credit has to be *present* for dropping it to
    /// mean something.
    #[test]
    fn only_a_playlist_is_credited_to_someone() {
        const ENTRY: &str = r#"{
            "title": "Discovery",
            "username": "Daft Punk",
            "permalink_url": "https://soundcloud.com/daftpunkofficialmusic/sets/x",
            "user": { "username": "Daft Punk" }
        }"#;

        let as_playlist: Entry = serde_json::from_str(ENTRY).unwrap();
        assert_eq!(
            as_playlist
                .into_collection(SearchKind::Playlist)
                .unwrap()
                .uploader
                .as_deref(),
            Some("Daft Punk"),
            "a playlist says whose it is"
        );

        let as_artist: Entry = serde_json::from_str(ENTRY).unwrap();
        assert_eq!(
            as_artist.into_collection(SearchKind::Artist).unwrap().uploader,
            None,
            "an artist row would otherwise read as \"Daft Punk, by Daft Punk\""
        );
    }

    #[test]
    fn an_entry_pointing_somewhere_else_is_dropped() {
        // The URL becomes a subprocess argument, so a host that is not
        // SoundCloud's never gets that far.
        let entry: Entry = serde_json::from_str(
            r#"{ "title": "Evil", "permalink_url": "https://example.com/x" }"#,
        )
        .unwrap();

        assert!(entry.into_collection(SearchKind::Playlist).is_none());
    }

    #[test]
    fn an_entry_with_no_url_is_dropped() {
        let entry: Entry = serde_json::from_str(r#"{ "title": "Nowhere" }"#).unwrap();
        assert!(entry.into_collection(SearchKind::Playlist).is_none());
    }
}

/// Against the real API, because every risk here belongs to SoundCloud rather
/// than to this code: whether a key can still be found in the web player's
/// JavaScript, and whether the endpoints still answer.
#[cfg(test)]
mod network_tests {
    use super::*;

    #[tokio::test]
    async fn a_client_id_can_still_be_found() {
        let client = client().expect("an HTTPS client");

        let id = discover_client_id(&client)
            .await
            .expect("SoundCloud's web player should still carry a key");

        assert_eq!(id.len(), 32, "got: {id}");
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()), "got: {id}");
        eprintln!("found a {}-character key", id.len());
    }

    #[tokio::test]
    async fn playlists_can_be_searched() {
        let found = search(SearchKind::Playlist, "daft punk", 5)
            .await
            .expect("the playlist search should answer");

        assert!(!found.is_empty(), "no playlists at all");

        for collection in &found {
            assert!(
                collection.url.contains("/sets/"),
                "not a set URL: {}",
                collection.url
            );
            assert_eq!(collection.kind, SearchKind::Playlist);
        }

        eprintln!(
            "{} playlists, first: {} ({:?} tracks)",
            found.len(),
            found[0].title,
            found[0].item_count
        );
    }

    #[tokio::test]
    async fn artists_can_be_searched() {
        let found = search(SearchKind::Artist, "daft punk", 5)
            .await
            .expect("the user search should answer");

        assert!(!found.is_empty(), "no artists at all");
        assert!(
            found[0].uploader.is_none(),
            "an artist row must not say it is by itself"
        );

        eprintln!("{} artists, first: {}", found.len(), found[0].title);
    }

    /// The whole point of the split: what search returns has to be something
    /// the app can then open, or discovery leads nowhere.
    #[tokio::test]
    async fn a_searched_playlist_opens_to_its_full_contents() {
        let found = search(SearchKind::Playlist, "daft punk discovery", 3)
            .await
            .expect("the playlist search should answer");

        let first = found.first().expect("at least one playlist");

        let tracks = expand_set(&first.url, 200)
            .await
            .unwrap_or_else(|e| panic!("could not open {}: {e}", first.url));

        assert!(!tracks.is_empty(), "{} opened to nothing", first.url);

        // The count the search advertised is the count the page should hold.
        // Getting five of thirty-three back was the reported bug, and it is a
        // search result's own number that gives it away.
        if let Some(promised) = first.item_count {
            assert_eq!(
                tracks.len() as u64,
                promised,
                "{} promised {promised} tracks and opened to {}",
                first.title,
                tracks.len()
            );
        }

        eprintln!(
            "{} expanded to {} tracks (promised {:?})",
            first.title,
            tracks.len(),
            first.item_count
        );
    }

    /// The bug this exists for: a SoundCloud set lists bare ids, so every row
    /// arrived as "(untitled)" with no picture.
    #[tokio::test]
    async fn a_soundcloud_set_gets_titles_and_artwork() {
        let response = match crate::youtube::flat_playlist_at(
            crate::collections::network_tests::yt_dlp(),
            "https://soundcloud.com/0o8/sets/t_t",
            Some(20),
        )
        .await
        {
            Ok(response) => response,
            Err(e) => {
                eprintln!("SKIP: the fixture set is gone ({e})");
                return;
            }
        };

        let mut tracks: Vec<SearchResult> = response
            .entries
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.normalize(Provider::SoundCloud))
            .collect();

        assert!(!tracks.is_empty(), "nothing to hydrate");

        // What yt-dlp alone produces, so the improvement is measured rather
        // than asserted.
        let untitled_before = tracks.iter().filter(|t| t.title == "(untitled)").count();
        let with_art_before = tracks.iter().filter(|t| t.thumbnail_url.is_some()).count();

        hydrate(&mut tracks).await;

        let untitled_after = tracks.iter().filter(|t| t.title == "(untitled)").count();
        let with_art_after = tracks.iter().filter(|t| t.thumbnail_url.is_some()).count();

        eprintln!(
            "{} tracks | untitled {untitled_before} -> {untitled_after} | with art {with_art_before} -> {with_art_after}",
            tracks.len()
        );

        assert_eq!(untitled_after, 0, "every row should have a real title");
        assert!(
            with_art_after > with_art_before,
            "hydration added no artwork at all"
        );
    }

    /// The reported bug: a SoundCloud playlist always came back with five
    /// songs. yt-dlp emits only the tracks SoundCloud pre-filled.
    #[tokio::test]
    async fn a_set_expands_past_what_yt_dlp_returns() {
        const SET: &str = "https://soundcloud.com/0o8/sets/t_t";

        // What the old path produced: yt-dlp emits every track, but most are
        // `{id}` stubs with no page URL, and a row with nowhere to play is
        // dropped. That -- not a limit in yt-dlp -- is where "always five"
        // came from.
        let via_yt_dlp = crate::youtube::flat_playlist_at(
            crate::collections::network_tests::yt_dlp(),
            SET,
            Some(200),
        )
        .await
        .map(|response| {
            response
                .entries
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.normalize(Provider::SoundCloud))
                .count()
        })
        .unwrap_or(0);

        let tracks = match expand_set(SET, 200).await {
            Ok(tracks) => tracks,
            Err(e) => {
                eprintln!("SKIP: the fixture set is gone ({e})");
                return;
            }
        };

        eprintln!("yt-dlp: {via_yt_dlp} tracks | api: {} tracks", tracks.len());

        assert!(
            tracks.len() > via_yt_dlp,
            "expansion found no more than yt-dlp did ({} vs {via_yt_dlp})",
            tracks.len()
        );
        assert!(
            tracks.iter().all(|t| !t.remote_url.is_empty()),
            "a row with no page cannot be played"
        );
        assert_eq!(
            tracks.iter().filter(|t| t.title == "(untitled)").count(),
            0,
            "every row should have a real title"
        );
    }

    /// The reported bug: an artist page filled with music they did not upload,
    /// because a bare user URL is the "All" tab and All includes reposts.
    #[tokio::test]
    async fn an_artist_page_holds_only_their_own_uploads() {
        // An account that reposts. Chosen deliberately: on one that never
        // does, both tabs look identical and the test would pass whatever the
        // code did.
        const ARTIST: &str = "https://soundcloud.com/0o8";

        let all = crate::youtube::flat_playlist_at(
            crate::collections::network_tests::yt_dlp(),
            ARTIST,
            Some(200),
        )
        .await
        .expect("the All tab should list something");

        let own = crate::youtube::flat_playlist_at(
            crate::collections::network_tests::yt_dlp(),
            &artist_tracks_url(ARTIST),
            Some(200),
        )
        .await
        .expect("the tracks tab should list something");

        fn tally(response: crate::youtube::SearchResponse) -> (usize, usize) {
            let tracks: Vec<_> = response
                .entries
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.normalize(Provider::SoundCloud))
                .collect();

            let foreign = tracks
                .iter()
                .filter(|track| !track.remote_url.starts_with("https://soundcloud.com/0o8/"))
                .count();

            (tracks.len(), foreign)
        }

        let (all_total, all_foreign) = tally(all);
        let (own_total, own_foreign) = tally(own);

        eprintln!(
            "All: {all_total} entries ({all_foreign} by someone else) | \n             Tracks: {own_total} entries ({own_foreign} by someone else)"
        );

        assert_eq!(
            own_foreign, 0,
            "an artist page must not list other people's uploads"
        );
    }

    /// The reported bug: the artist's picture was empty and their name showed
    /// as "all" -- both taken from yt-dlp's envelope, which titles the page
    /// after the tab and carries no avatar at all.
    #[tokio::test]
    async fn an_artist_is_described_by_name_and_picture() {
        let described = describe("https://soundcloud.com/daftpunkofficialmusic", SearchKind::Artist)
        .await
        .expect("the artist should describe itself");

        eprintln!(
            "{} | {:?} followers | {:?}",
            described.title, described.follower_count, described.thumbnail_url
        );

        assert!(
            !described.title.to_lowercase().contains("(all)")
                && !described.title.eq_ignore_ascii_case("all"),
            "the tab's name is not the artist's: {}",
            described.title
        );
        assert!(
            described.thumbnail_url.is_some(),
            "an artist page with no picture is the bug this fixes"
        );
    }

    #[test]
    fn only_a_bare_user_url_gets_the_tracks_tab() {
        assert_eq!(
            artist_tracks_url("https://soundcloud.com/daftpunk"),
            "https://soundcloud.com/daftpunk/tracks"
        );
        assert_eq!(
            artist_tracks_url("https://soundcloud.com/daftpunk/"),
            "https://soundcloud.com/daftpunk/tracks"
        );
        // A tab the caller chose on purpose is left alone.
        assert_eq!(
            artist_tracks_url("https://soundcloud.com/daftpunk/likes"),
            "https://soundcloud.com/daftpunk/likes"
        );
        assert_eq!(
            artist_tracks_url("https://soundcloud.com/daftpunk/tracks"),
            "https://soundcloud.com/daftpunk/tracks"
        );
    }
}
