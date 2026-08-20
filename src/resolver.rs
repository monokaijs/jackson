use std::{
    borrow::Cow,
    io::Write,
    process::{Command as ProcessCommand, Stdio},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serenity::all::UserId;
use songbird::input::{ChildContainer, Input, YoutubeDl};
use tokio::process::Command as TokioCommand;
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
    ytdlp_args: Vec<String>,
    _cookie_file: Option<Arc<tempfile::TempPath>>,
}

impl Resolver {
    pub fn new(
        client: reqwest::Client,
        max_playlist_tracks: usize,
        cookies: Option<String>,
        cookies_file: Option<String>,
    ) -> Result<Self> {
        let mut generated_cookie_file = None;
        let cookie_path = if let Some(cookies) = cookies {
            let mut file = tempfile::NamedTempFile::new()
                .context("failed to create temporary yt-dlp cookie file")?;
            file.write_all(cookie_header_to_netscape(&cookies)?.as_bytes())
                .context("failed to write temporary yt-dlp cookie file")?;
            let path = file.path().to_string_lossy().into_owned();
            generated_cookie_file = Some(Arc::new(file.into_temp_path()));
            Some(path)
        } else {
            cookies_file
        };

        let mut ytdlp_args = Vec::new();
        if let Some(path) = cookie_path {
            ytdlp_args.extend(["--cookies".to_owned(), path]);
            ytdlp_args.extend([
                "--extractor-args".to_owned(),
                "youtube:player_client=default,web_embedded".to_owned(),
            ]);
        }

        Ok(Self {
            client,
            max_playlist_tracks,
            ytdlp_args,
            _cookie_file: generated_cookie_file,
        })
    }

    fn configure_ytdlp(&self, source: YoutubeDl<'static>) -> YoutubeDl<'static> {
        if self.ytdlp_args.is_empty() {
            source
        } else {
            source.user_args(self.ytdlp_args.clone())
        }
    }

    pub fn input(&self, track: &QueuedTrack) -> Result<Input> {
        let child = ProcessCommand::new("yt-dlp")
            .args(&self.ytdlp_args)
            .args([
                "--no-playlist",
                "--no-progress",
                "-f",
                "ba[abr>0][vcodec=none]/best",
                "-o",
                "-",
                "--",
                &track.source_url,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("failed to start yt-dlp audio stream")?;

        Ok(ChildContainer::from(child).into())
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
        let source = if search {
            YoutubeDl::new_search(self.client.clone(), query)
        } else {
            YoutubeDl::new(self.client.clone(), query)
        };
        let mut source = self.configure_ytdlp(source);
        let output = source
            .query(1)
            .await
            .context("No playable audio was found for that query")?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No playable audio was found for that query"))?;
        let metadata = output.as_aux_metadata();
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
        let output = TokioCommand::new("yt-dlp")
            .args(&self.ytdlp_args)
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

fn cookie_header_to_netscape(header: &str) -> Result<String> {
    let mut output = String::from("# Netscape HTTP Cookie File\n");
    let mut found_cookie = false;

    for part in header.split(';') {
        let (name, value) = part
            .trim()
            .split_once('=')
            .ok_or_else(|| anyhow!("YTDLP_COOKIES contains an invalid cookie"))?;
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || name.contains('\t') || value.contains('\t') {
            bail!("YTDLP_COOKIES contains an invalid cookie");
        }
        output.push_str(&format!(
            ".youtube.com\tTRUE\t/\tTRUE\t0\t{name}\t{value}\n"
        ));
        found_cookie = true;
    }

    if !found_cookie {
        bail!("YTDLP_COOKIES contains no cookies");
    }
    Ok(output)
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

    #[test]
    fn converts_cookie_header_to_netscape_jar() {
        assert_eq!(
            cookie_header_to_netscape("SID=abc; TOKEN=value=with=equals").unwrap(),
            concat!(
                "# Netscape HTTP Cookie File\n",
                ".youtube.com\tTRUE\t/\tTRUE\t0\tSID\tabc\n",
                ".youtube.com\tTRUE\t/\tTRUE\t0\tTOKEN\tvalue=with=equals\n",
            )
        );
    }
}
