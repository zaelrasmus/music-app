//! The remote sources tracks can come from.
//!
//! Deliberately an enum rather than a trait. Every provider here is "yt-dlp
//! with a different search prefix and a different id shape", so a trait object
//! would be ceremony around what is really a small data table -- adding
//! Bandcamp or Mixcloud is one variant plus one row of each `match`.
//!
//! That stops paying the moment a provider is *not* yt-dlp-backed (a Subsonic
//! server, a real API). The seam for that already exists one level up in
//! `playable.rs`, which is what absorbed YouTube without the player noticing,
//! so this enum is not a trap -- it is the right size for the providers that
//! actually share a mechanism.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    YouTube,
    SoundCloud,
}

impl Provider {
    /// Every provider yt-dlp can search, in the order the UI should offer them.
    pub const ALL: [Provider; 2] = [Provider::YouTube, Provider::SoundCloud];

    /// The value stored in `tracks.source`, and what the schema's CHECK allows.
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::YouTube => "youtube",
            Provider::SoundCloud => "soundcloud",
        }
    }

    /// Parses a `tracks.source` value. `"local"` is deliberately not a
    /// provider: a local file has no remote identity to resolve.
    pub fn from_source(source: &str) -> Option<Self> {
        match source {
            "youtube" => Some(Provider::YouTube),
            "soundcloud" => Some(Provider::SoundCloud),
            _ => None,
        }
    }

    /// What to show a user.
    pub fn display_name(self) -> &'static str {
        match self {
            Provider::YouTube => "YouTube",
            Provider::SoundCloud => "SoundCloud",
        }
    }

    /// yt-dlp's search-prefix scheme: `<prefix><n>:<query>`.
    pub fn search_prefix(self) -> &'static str {
        match self {
            Provider::YouTube => "ytsearch",
            Provider::SoundCloud => "scsearch",
        }
    }

    /// Builds a page URL from an id, where that is possible at all.
    ///
    /// Only a fallback for when yt-dlp omits `webpage_url`. SoundCloud returns
    /// `None` because its URLs embed the uploader's handle
    /// (`soundcloud.com/<user>/<slug>`), which the numeric id does not contain
    /// -- the reason `remote_url` is a stored column rather than a derived one.
    pub fn page_url(self, id: &str) -> Option<String> {
        if !self.accepts_id(id) {
            return None;
        }

        match self {
            Provider::YouTube => Some(format!("https://www.youtube.com/watch?v={id}")),
            Provider::SoundCloud => None,
        }
    }

    /// The host a `remote_url` must be on for this provider.
    ///
    /// Checked before handing a stored URL to yt-dlp. The URL comes from a
    /// database row rather than straight from the user, but the row was
    /// written from network data, and this is the last point where a URL
    /// pointing somewhere unexpected can be caught cheaply.
    fn url_hosts(self) -> &'static [&'static str] {
        match self {
            Provider::YouTube => &["youtube.com", "www.youtube.com", "m.youtube.com", "youtu.be"],
            Provider::SoundCloud => &["soundcloud.com", "www.soundcloud.com", "on.soundcloud.com"],
        }
    }

    /// Whether `id` has this provider's shape.
    ///
    /// The point is not validation for its own sake: these ids are
    /// concatenated into yt-dlp arguments, so anything that could pass for a
    /// flag has to be rejected.
    pub fn accepts_id(self, id: &str) -> bool {
        match self {
            // Exactly 11 characters of [A-Za-z0-9_-].
            Provider::YouTube => {
                id.len() == 11
                    && id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            }
            // A plain positive integer, e.g. "199428706".
            Provider::SoundCloud => {
                !id.is_empty() && id.len() <= 24 && id.chars().all(|c| c.is_ascii_digit())
            }
        }
    }

    /// Whether `url` is a plausible page URL for this provider.
    ///
    /// Requires https and a known host. A stored URL that fails this is a
    /// corrupted row, not a user error.
    pub fn accepts_url(self, url: &str) -> bool {
        let Some(rest) = url.strip_prefix("https://") else {
            return false;
        };

        // Host runs up to the first '/', '?' or '#'.
        let host = rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        // Reject userinfo (`user@host`) and explicit ports, which could make a
        // lookalike host pass a naive comparison.
        if host.contains('@') || host.contains(':') {
            return false;
        }

        self.url_hosts().contains(&host.as_str())
    }
}

/// One entry in the provider picker.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: Provider,
    pub name: &'static str,
}

/// The providers the app can search.
///
/// A command rather than a constant in the frontend: the enum is the single
/// source of truth, and a hardcoded TypeScript list would drift the first time
/// one is added.
#[tauri::command]
pub async fn list_providers() -> Result<Vec<ProviderInfo>, String> {
    Ok(Provider::ALL
        .into_iter()
        .map(|id| ProviderInfo {
            id,
            name: id.display_name(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_strings_round_trip() {
        for provider in Provider::ALL {
            assert_eq!(Provider::from_source(provider.as_str()), Some(provider));
        }
    }

    /// A local track has no remote identity, so it must never resolve to a
    /// provider that would try to fetch it over the network.
    #[test]
    fn local_is_not_a_provider() {
        assert_eq!(Provider::from_source("local"), None);
        assert_eq!(Provider::from_source(""), None);
        assert_eq!(Provider::from_source("bandcamp"), None);
    }

    #[test]
    fn youtube_ids_are_eleven_characters() {
        assert!(Provider::YouTube.accepts_id("dQw4w9WgXcQ"));
        assert!(Provider::YouTube.accepts_id("_-aB3cD4eF5"));
        assert!(!Provider::YouTube.accepts_id("dQw4w9WgXc"), "too short");
        assert!(!Provider::YouTube.accepts_id("dQw4w9WgXcQQ"), "too long");
    }

    #[test]
    fn soundcloud_ids_are_integers() {
        assert!(Provider::SoundCloud.accepts_id("199428706"));
        assert!(Provider::SoundCloud.accepts_id("2366118086"));
        assert!(!Provider::SoundCloud.accepts_id(""));
        assert!(!Provider::SoundCloud.accepts_id("199-428"));
        assert!(!Provider::SoundCloud.accepts_id("dQw4w9WgXcQ"));
    }

    /// These ids become yt-dlp arguments, so a leading dash must never survive.
    #[test]
    fn nothing_that_could_smuggle_a_flag_is_accepted() {
        for provider in Provider::ALL {
            assert!(!provider.accepts_id("--version"));
            assert!(!provider.accepts_id("-f"));
            assert!(!provider.accepts_id("../../etc/pw"));
            assert!(!provider.accepts_id("id with space"));
        }
    }

    /// The two id shapes are disjoint, which is what makes `(source,
    /// remote_id)` uniqueness meaningful rather than decorative.
    #[test]
    fn a_youtube_id_is_not_a_soundcloud_id() {
        assert!(!Provider::SoundCloud.accepts_id("dQw4w9WgXcQ"));
        assert!(!Provider::YouTube.accepts_id("199428706"));
    }

    #[test]
    fn real_page_urls_are_accepted() {
        assert!(Provider::YouTube.accepts_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        assert!(Provider::YouTube.accepts_url("https://youtu.be/dQw4w9WgXcQ"));
        assert!(Provider::SoundCloud
            .accepts_url("https://soundcloud.com/daft-punk-id/daft-punk-one-more-time"));
    }

    #[test]
    fn a_url_for_the_wrong_provider_is_rejected() {
        assert!(!Provider::YouTube.accepts_url("https://soundcloud.com/a/b"));
        assert!(!Provider::SoundCloud.accepts_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
    }

    /// Lookalike hosts are the whole reason this checks the host rather than
    /// doing a substring match.
    #[test]
    fn lookalike_hosts_are_rejected() {
        assert!(!Provider::YouTube.accepts_url("https://youtube.com.evil.test/watch?v=x"));
        assert!(!Provider::YouTube.accepts_url("https://evil.test/https://youtube.com/x"));
        assert!(!Provider::YouTube.accepts_url("https://youtube.com@evil.test/x"));
        assert!(!Provider::SoundCloud.accepts_url("https://notsoundcloud.com/a/b"));
    }

    #[test]
    fn plain_http_is_rejected() {
        assert!(!Provider::YouTube.accepts_url("http://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        assert!(!Provider::YouTube.accepts_url("file:///etc/passwd"));
    }
}
