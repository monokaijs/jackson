use std::{borrow::Cow, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serenity::all::UserId;
use songbird::input::{Compose, YoutubeDl};
use tokio::process::Command;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedTrack {
    pub title: String,
    pub artist: Option<String>,
    pub source_url: String,
    pub thumbnail: Option<String>,
    pub duration: Option<Duration>,
    pub requester: UserId,
}

impl QueuedTrack {
    pub fn display_artist(&self) -> &str {
        self.artist.as_deref().unwrap_or("Unknown artist")
    }
}

#[derive(Clone)]
pub struct Resolver {
    client: reqwest::Client,
    max_playlist_tracks: usize,
}

impl Resolver {
    pub fn new(client: reqwest::Client, max_playlist_tracks: usize) -> Self {
        Self {
            client,
            max_playlist_tracks,
        }
    }

    pub fn input(&self, track: &QueuedTrack) -> YoutubeDl<'static> {
        YoutubeDl::new(self.client.clone(), track.source_url.clone())
    }

    pub async fn resolve(&self, query: &str, requester: UserId) -> Result<Vec<QueuedTrack>> {
        let query = query.trim();
        if query.is_empty() {
            bail!("Give me a song name or URL.");
        }

        if let Ok(url) = Url::parse(query) {
            if looks_like_playlist(&url) {
                let tracks = self.resolve_playlist(query, requester).await?;
                if !tracks.is_empty() {
                    return Ok(tracks);
                }
            }

            return self
                .resolve_one(Cow::Owned(query.to_owned()), false, requester)
                .await;
        }

        self.resolve_one(Cow::Owned(query.to_owned()), true, requester)
            .await
    }

    async fn resolve_one(
        &self,
        query: Cow<'static, str>,
        search: bool,
        requester: UserId,
    ) -> Result<Vec<QueuedTrack>> {
        let mut source = if search {
            YoutubeDl::new_search(self.client.clone(), query)
        } else {
            YoutubeDl::new(self.client.clone(), query)
        };
        let metadata = source
            .aux_metadata()
            .await
            .context("No playable audio was found for that query")?;
        let source_url = metadata
            .source_url
            .clone()
            .ok_or_else(|| anyhow!("The source did not return a reusable track URL"))?;

        Ok(vec![QueuedTrack {
            title: metadata
                .title
                .or(metadata.track)
                .unwrap_or_else(|| "Unknown title".to_owned()),
            artist: metadata.artist.or(metadata.channel),
            source_url,
            thumbnail: metadata.thumbnail,
            duration: metadata.duration,
            requester,
        }])
    }

    async fn resolve_playlist(&self, url: &str, requester: UserId) -> Result<Vec<QueuedTrack>> {
        let output = Command::new("yt-dlp")
            .args([
                "--dump-single-json",
                "--flat-playlist",
                "--yes-playlist",
                "--playlist-end",
                &self.max_playlist_tracks.to_string(),
                "--",
                url,
            ])
            .output()
            .await
            .context("yt-dlp is required and was not found on PATH")?;

        if !output.status.success() {
            bail!(
                "Playlist lookup failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        let playlist: FlatPlaylist = serde_json::from_slice(&output.stdout)
            .context("yt-dlp returned invalid playlist metadata")?;
        let entries = playlist.entries.unwrap_or_default();

        Ok(entries
            .into_iter()
            .filter_map(|entry| {
                let source_url = entry
                    .webpage_url
                    .or_else(|| normalize_flat_url(entry.url?))?;
                Some(QueuedTrack {
                    title: entry.title.unwrap_or_else(|| "Unknown title".to_owned()),
                    artist: entry.artist.or(entry.uploader).or(entry.channel),
                    source_url,
                    thumbnail: entry.thumbnail,
                    duration: entry
                        .duration
                        .filter(|v| v.is_finite() && *v >= 0.0)
                        .map(Duration::from_secs_f64),
                    requester,
                })
            })
            .take(self.max_playlist_tracks)
            .collect())
    }
}

fn looks_like_playlist(url: &Url) -> bool {
    url.query_pairs().any(|(key, _)| key == "list")
        || url.path().contains("playlist")
        || url.path().contains("sets/")
        || url.path().contains("album")
}

fn normalize_flat_url(url: String) -> Option<String> {
    if Url::parse(&url).is_ok() {
        Some(url)
    } else if url.len() == 11 {
        Some(format!("https://www.youtube.com/watch?v={url}"))
    } else {
        None
    }
}

#[derive(Debug, Deserialize)]
struct FlatPlaylist {
    entries: Option<Vec<FlatEntry>>,
}

#[derive(Debug, Deserialize)]
struct FlatEntry {
    title: Option<String>,
    artist: Option<String>,
    uploader: Option<String>,
    channel: Option<String>,
    url: Option<String>,
    webpage_url: Option<String>,
    thumbnail: Option<String>,
    duration: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_common_playlist_urls() {
        assert!(looks_like_playlist(
            &Url::parse("https://youtube.com/watch?v=abc&list=xyz").unwrap()
        ));
        assert!(looks_like_playlist(
            &Url::parse("https://soundcloud.com/user/sets/mix").unwrap()
        ));
        assert!(!looks_like_playlist(
            &Url::parse("https://youtube.com/watch?v=abc").unwrap()
        ));
    }
}
