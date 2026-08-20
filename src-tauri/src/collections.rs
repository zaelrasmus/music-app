//! Playlists and artists on a provider, as things to browse rather than play.
//!
//! A track is playable and belongs in the database. A remote playlist or an
//! artist is neither: it is a *place to look*, it changes upstream without
//! telling anyone, and it stops existing the moment the user navigates away.
//! So nothing here is stored. These are view state, fetched on demand and
//! discarded, and the only thing that ever reaches the database is the tracks
//! a user chose to keep out of one.
//!
//! Two operations, and the split matters. **Searching** finds collections and
//! is the fragile half -- on YouTube it rides an undocumented filter
//! parameter, and on SoundCloud yt-dlp offers no route to it at all.
//! **Expanding** turns a collection's URL into tracks and is the solid half:
//! it is the same `--flat-playlist` extraction the track search already uses,
//! maintained by yt-dlp, and it works for every provider by URL. Anything
//! reached by a link therefore works even where searching does not.

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::providers::{Provider, SearchKind};
use crate::youtube::{self, SearchResult};

/// A playlist or an artist, as a row in a result list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Collection {
    pub provider: Provider,
    pub kind: SearchKind,
    /// The provider's page, and the only field that has to be right: it is
    /// what expanding, playing and importing all go through.
    pub url: String,
    pub title: String,
    /// Who published it. `None` for an artist, where it would repeat the title.
    pub uploader: Option<String>,
    /// How many tracks, when the provider says. Never guessed -- an unknown
    /// count shows nothing rather than a wrong number.
    pub item_count: Option<u64>,
    /// Followers or subscribers, for an artist. `None` when unknown, and
    /// never shown as zero -- an artist with no number is not an artist with
    /// no listeners.
    pub follower_count: Option<u64>,
    pub thumbnail_url: Option<String>,
}

/// Upper bound on collection results.
///
/// Lower than the track search's 25 because a page of playlists is scanned by
/// eye rather than read -- the right one is usually in the first handful, and
/// each extra row is another thumbnail.
const MAX_COLLECTIONS: u32 = 12;

/// How many tracks a collection is expanded to at most.
///
/// A YouTube playlist can hold thousands, and a channel more. Measured on a
/// 50-item playlist: 4.0s to expand. Nothing about that scales in a direction
/// worth discovering on a user's machine, so the list is capped and the UI
/// says so.
const MAX_EXPANDED: u32 = 200;

/// Finds playlists or artists matching `query`.
///
/// Returns `Ok(vec![])` rather than an error when the provider cannot be
/// searched this way, because the UI does not offer the tab in the first place
/// -- reaching here at all would be a bug, not something to explain to a user.
#[tauri::command]
pub async fn search_collections(
    app: AppHandle,
    provider: Provider,
    kind: SearchKind,
    query: String,
) -> Result<Vec<Collection>, String> {
    let query = query.trim().to_string();
    if query.is_empty() || kind == SearchKind::Track {
        return Ok(Vec::new());
    }

    // Two routes, and which one a provider takes is a property of the
    // provider, not a branch on its name: a `search_target` is what yt-dlp can
    // be pointed at, and its absence is what says the provider needs its own
    // client. Adding Bandcamp tomorrow answers the same question the same way.
    let Some(target) = provider.search_target(kind, &query, MAX_COLLECTIONS) else {
        return crate::soundcloud::search(kind, &query, MAX_COLLECTIONS).await;
    };

    let response = youtube::flat_playlist(&app, &target, Some(MAX_COLLECTIONS)).await?;

    let found: Vec<Collection> = response
        .entries
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.into_collection(provider, kind))
        .collect();

    // The filter is undocumented and applied by YouTube, not by us. If it ever
    // stops being recognised the page returns ordinary videos, every one of
    // which fails the check above -- so an empty list here, from a search that
    // itself succeeded, is the signature of exactly that.
    //
    // Said plainly rather than shown as "no results": the user's query is
    // fine, and telling them otherwise would send them off rewording it.
    if found.is_empty() {
        return Err(format!(
            "{} did not return any {}s for that. If this keeps happening for \
             every search, the app's {} filter may no longer be supported.",
            provider.display_name(),
            kind.as_str(),
            provider.display_name()
        ));
    }

    Ok(found)
}

/// Turns a playlist, album or artist page into the tracks inside it.
///
/// Takes a URL rather than an id because that is what every route to here
/// already has -- a search result, a track's uploader, or something the user
/// pasted -- and because only the URL survives the differences between
/// providers.
#[tauri::command]
pub async fn expand_collection(
    app: AppHandle,
    provider: Provider,
    kind: SearchKind,
    url: String,
) -> Result<Expansion, String> {
    // The URL reaches a subprocess, and it can arrive from the frontend.
    if !provider.accepts_url(&url) {
        return Err(format!(
            "That does not look like a {} link.",
            provider.display_name()
        ));
    }

    if provider == Provider::SoundCloud {
        return expand_soundcloud(&app, kind, url).await;
    }

    let (collection, tracks) = expand_youtube(&app, kind, &url).await?;
    Ok(Expansion { collection, tracks })
}

/// Opens a YouTube playlist or channel.
///
/// Retries once at the uploads tab when the page as given holds no songs. A
/// channel's landing tab is whatever its owner arranged, and a curated one can
/// be entirely shelves of *playlists* -- every entry a collection rather than a
/// video, so every entry is dropped and the page looks empty. `/videos` is the
/// grid that always means uploads.
///
/// Only ever a second attempt, never the first: auto-generated "- Topic"
/// channels have no videos tab at all and fail outright on it, and those are
/// precisely the music channels this feature is most used on.
async fn expand_youtube(
    app: &AppHandle,
    kind: SearchKind,
    url: &str,
) -> Result<(Collection, Vec<SearchResult>), String> {
    let response = youtube::flat_playlist(app, url, Some(MAX_EXPANDED)).await?;

    // Read before the entries are consumed: the envelope is the page talking
    // about itself, and it is the only place an artist's own name and picture
    // appear. A row that linked here knew a name and a URL and nothing else.
    let collection = response.collection(Provider::YouTube, kind, url);

    let tracks: Vec<SearchResult> = response
        .entries
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.normalize(Provider::YouTube))
        .collect();

    if !tracks.is_empty() {
        return Ok((collection, tracks));
    }

    let uploads = format!("{}/videos", url.trim_end_matches('/'));
    if kind != SearchKind::Artist || url.ends_with("/videos") {
        return Err(EMPTY.to_string());
    }

    let retry = youtube::flat_playlist(app, &uploads, Some(MAX_EXPANDED))
        .await
        // The channel has no uploads tab either. Report what the page itself
        // said rather than the retry's complaint about a tab the user never
        // asked for.
        .map_err(|_| EMPTY.to_string())?;

    let collection = retry.collection(Provider::YouTube, kind, url);
    let tracks: Vec<SearchResult> = retry
        .entries
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.normalize(Provider::YouTube))
        .collect();

    if tracks.is_empty() {
        return Err(EMPTY.to_string());
    }

    Ok((collection, tracks))
}

/// Opens a SoundCloud set or artist, through SoundCloud's own API.
///
/// Neither half can come from yt-dlp here. It drops every track a set did not
/// hydrate, which turns a 33-track playlist into 5; and a bare user URL is the
/// "All" tab, which mixes in everything they have reposted. Both were reported
/// as bugs before they were understood as one cause -- the flat listing is
/// simply not what these pages contain.
async fn expand_soundcloud(
    app: &AppHandle,
    kind: SearchKind,
    url: String,
) -> Result<Expansion, String> {
    let collection = crate::soundcloud::describe(&url, kind).await?;

    if kind == SearchKind::Playlist {
        let tracks = crate::soundcloud::expand_set(&url, MAX_EXPANDED).await?;
        return Ok(Expansion { collection, tracks });
    }

    // An artist's own uploads, not their reposts.
    let uploads = crate::soundcloud::artist_tracks_url(&url);
    let response = youtube::flat_playlist(app, &uploads, Some(MAX_EXPANDED)).await?;

    let mut tracks: Vec<SearchResult> = response
        .entries
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.normalize(Provider::SoundCloud))
        .collect();

    if tracks.is_empty() {
        return Err(EMPTY.to_string());
    }

    // The flat entries carry an id, a URL and a title, and nothing else --
    // no artwork and no uploader, so every row would read as unknown.
    crate::soundcloud::hydrate(&mut tracks).await;

    Ok(Expansion { collection, tracks })
}

const EMPTY: &str = "There is nothing playable in there.";

/// A collection and what is inside it.
///
/// The collection comes back too, rather than the caller keeping the row it
/// clicked: that row is whatever the search happened to say, and for an artist
/// reached from a track it is a bare name and a URL. The page itself knows its
/// real name, its picture and how much it holds.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Expansion {
    pub collection: Collection,
    pub tracks: Vec<SearchResult>,
}

/// The cap, so the UI can say "the first 200" without hardcoding it twice.
#[tauri::command]
pub fn max_expanded_tracks() -> u32 {
    MAX_EXPANDED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soundcloud_collections_do_not_go_through_yt_dlp() {
        // The dispatch this whole module turns on. yt-dlp exposes no playlist
        // or user search for SoundCloud, so it has no target for either --
        // and the absence is the signal to use SoundCloud's own API, not a
        // reason to show the user an empty tab.
        assert!(Provider::SoundCloud
            .search_target(SearchKind::Playlist, "daft punk", 10)
            .is_none());
        assert!(Provider::SoundCloud
            .search_target(SearchKind::Artist, "daft punk", 10)
            .is_none());

        // Tracks still do, on both providers.
        assert!(Provider::SoundCloud
            .search_target(SearchKind::Track, "daft punk", 10)
            .is_some());
    }

    #[test]
    fn both_providers_answer_every_kind() {
        // What the UI builds its tabs from. Two routes, one capability table.
        for provider in Provider::ALL {
            for kind in [SearchKind::Track, SearchKind::Playlist, SearchKind::Artist] {
                assert!(
                    provider.searchable_kinds().contains(&kind),
                    "{} cannot be searched for {}s",
                    provider.display_name(),
                    kind.as_str()
                );
            }
        }
    }

    #[test]
    fn a_youtube_playlist_search_is_a_filtered_results_page() {
        let target = Provider::YouTube
            .search_target(SearchKind::Playlist, "daft punk", 12)
            .expect("YouTube can search playlists");

        assert_eq!(
            target,
            "https://www.youtube.com/results?search_query=daft+punk&sp=EgIQAw%3D%3D"
        );
    }

    #[test]
    fn artists_and_playlists_use_different_filters() {
        let playlists = Provider::YouTube
            .search_target(SearchKind::Playlist, "x", 12)
            .unwrap();
        let artists = Provider::YouTube
            .search_target(SearchKind::Artist, "x", 12)
            .unwrap();

        assert_ne!(playlists, artists, "one filter cannot serve both");
    }

    #[test]
    fn a_track_search_still_uses_the_search_prefix() {
        assert_eq!(
            Provider::YouTube
                .search_target(SearchKind::Track, "daft punk", 10)
                .unwrap(),
            "ytsearch10:daft punk"
        );
        assert_eq!(
            Provider::SoundCloud
                .search_target(SearchKind::Track, "aphex twin", 5)
                .unwrap(),
            "scsearch5:aphex twin"
        );
    }

    #[test]
    fn a_query_cannot_escape_the_results_url() {
        // The whole point of encoding it. Without this, a query could close
        // the search parameter and set `sp` itself -- or any other parameter
        // the results page understands.
        let target = Provider::YouTube
            .search_target(SearchKind::Playlist, "rock&sp=evil#x", 12)
            .unwrap();

        assert!(
            target.ends_with("&sp=EgIQAw%3D%3D"),
            "the filter must stay last and intact: {target}"
        );
        assert!(!target.contains("&sp=evil"), "got: {target}");
        assert!(!target.contains('#'), "a fragment would truncate it: {target}");
    }

    #[test]
    fn a_unicode_query_survives_encoding() {
        let target = Provider::YouTube
            .search_target(SearchKind::Playlist, "canción", 12)
            .unwrap();

        // UTF-8 bytes, percent-encoded one at a time.
        assert!(target.contains("canci%C3%B3n"), "got: {target}");
    }
}

/// Against the real services, because the two things being checked here are
/// not properties of this code: whether YouTube still honours an undocumented
/// filter parameter, and whether yt-dlp still returns what it did.
///
/// Both are exactly the kind of thing that passes in a fixture forever after
/// it has stopped being true.
#[cfg(test)]
pub(crate) mod network_tests {
    use super::*;
    use crate::youtube::flat_playlist_at;
    use std::path::PathBuf;

    pub(crate) fn yt_dlp() -> PathBuf {
        // The staged copy first, which is the binary the app itself runs.
        // Falling back to PATH keeps this working on a machine that has never
        // launched the app.
        let staged = dirs_app_data()
            .map(|dir| dir.join("bin").join(if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" }));

        match staged {
            Some(path) if path.is_file() => path,
            _ => PathBuf::from("yt-dlp"),
        }
    }

    fn dirs_app_data() -> Option<PathBuf> {
        let base = std::env::var_os(if cfg!(windows) { "APPDATA" } else { "HOME" })?;
        Some(PathBuf::from(base).join("com.kiza2.music-app"))
    }

    #[tokio::test]
    async fn youtube_still_returns_playlists_for_the_playlist_filter() {
        let target = Provider::YouTube
            .search_target(SearchKind::Playlist, "daft punk", MAX_COLLECTIONS)
            .expect("YouTube can search playlists");

        let response = match flat_playlist_at(yt_dlp(), &target, Some(MAX_COLLECTIONS)).await {
            Ok(response) => response,
            Err(e) => panic!("the filtered search itself failed: {e}"),
        };

        let found: Vec<Collection> = response
            .entries
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.into_collection(Provider::YouTube, SearchKind::Playlist))
            .collect();

        // The assertion that matters. An `sp` value YouTube no longer
        // recognises is ignored rather than refused, so the search succeeds
        // and returns videos -- none of which survive `into_collection`.
        assert!(
            !found.is_empty(),
            "the playlist filter returned nothing that is a playlist -- \
             YouTube may have retired it"
        );

        for collection in &found {
            assert!(
                collection.url.contains("/playlist?list="),
                "not a playlist URL: {}",
                collection.url
            );
        }

        eprintln!("{} playlists, first: {}", found.len(), found[0].title);
    }

    #[tokio::test]
    async fn youtube_still_returns_channels_for_the_artist_filter() {
        let target = Provider::YouTube
            .search_target(SearchKind::Artist, "daft punk", MAX_COLLECTIONS)
            .expect("YouTube can search artists");

        let response = flat_playlist_at(yt_dlp(), &target, Some(MAX_COLLECTIONS))
            .await
            .expect("the filtered search should run");

        let found: Vec<Collection> = response
            .entries
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.into_collection(Provider::YouTube, SearchKind::Artist))
            .collect();

        assert!(
            !found.is_empty(),
            "the artist filter returned nothing that is a channel -- \
             YouTube may have retired it"
        );

        // The artist page is a picture and a name; a channel row with no
        // avatar is the bug that sent this looking in the first place. The
        // host matters as much as the presence: channel avatars live on
        // googleusercontent, which the CSP has to allow by name.
        let avatar = found[0]
            .thumbnail_url
            .as_deref()
            .expect("a channel should carry an avatar");
        assert!(
            avatar.starts_with("https://yt3.googleusercontent.com/")
                || avatar.contains(".ggpht.com/"),
            "an avatar host the CSP does not allow: {avatar}"
        );

        eprintln!("{} channels, first: {} ({avatar})", found.len(), found[0].title);
    }

    /// The other half: a collection URL has to become playable tracks.
    ///
    /// Deliberately a playlist that has existed for years, so a failure here
    /// means the mechanism broke rather than the fixture going away.
    #[tokio::test]
    async fn a_youtube_playlist_expands_into_tracks() {
        let url = "https://www.youtube.com/playlist?list=PLSdoVPM5WnndLX6Ngmb8wktMF61dJirKl";

        let response = flat_playlist_at(yt_dlp(), url, Some(MAX_EXPANDED))
            .await
            .expect("the playlist should expand");

        let tracks: Vec<SearchResult> = response
            .entries
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.normalize(Provider::YouTube))
            .collect();

        assert!(tracks.len() > 3, "expanded to only {} tracks", tracks.len());

        for track in &tracks {
            assert!(
                Provider::YouTube.accepts_url(&track.remote_url),
                "a track that could never be played: {}",
                track.remote_url
            );
        }

        eprintln!("{} tracks, first: {}", tracks.len(), tracks[0].title);
    }

    /// SoundCloud has no playlist search, but its sets expand by URL like any
    /// other -- which is the whole basis for reaching them another way.
    #[tokio::test]
    async fn a_soundcloud_set_expands_by_url() {
        let url = "https://soundcloud.com/0o8/sets/t_t";

        let response = match flat_playlist_at(yt_dlp(), url, Some(MAX_EXPANDED)).await {
            Ok(response) => response,
            Err(e) => {
                // A user's set can be deleted; that is not this code failing.
                eprintln!("SKIP: the fixture set is gone ({e})");
                return;
            }
        };

        let tracks: Vec<SearchResult> = response
            .entries
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.normalize(Provider::SoundCloud))
            .collect();

        assert!(!tracks.is_empty(), "a set that expanded to nothing");
        eprintln!("{} SoundCloud tracks", tracks.len());
    }

    /// Against the real channel, because the shape that caused this -- banners
    /// first, avatar last -- is YouTube's choice and can change again.
    #[tokio::test]
    async fn a_real_channel_page_yields_a_square_avatar() {
        let response = crate::youtube::flat_playlist_at(
            crate::collections::network_tests::yt_dlp(),
            "https://www.youtube.com/channel/UCdI8MAC5HoPJSJ4zrgDDI-Q",
            Some(1),
        )
        .await
        .expect("the channel should open");

        let collection = response.collection(
            Provider::YouTube,
            SearchKind::Artist,
            "https://www.youtube.com/channel/UCdI8MAC5HoPJSJ4zrgDDI-Q",
        );

        let picture = collection
            .thumbnail_url
            .as_deref()
            .expect("an artist page needs a picture");

        eprintln!("{} -> {picture}", collection.title);

        // A banner crop carries `fcrop64`; an avatar is served square with an
        // `=sNNN` size. Asserted on the URL because the alternative is
        // downloading it to measure.
        assert!(
            !picture.contains("fcrop64"),
            "that is the channel banner, not the artist: {picture}"
        );
    }
}
