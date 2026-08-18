use std::{env, time::Duration};

use anyhow::{Context, Result};
use serenity::all::GuildId;

#[derive(Clone)]
pub struct Config {
    pub discord_token: String,
    pub development_guild: Option<GuildId>,
    pub database_url: String,
    pub idle_disconnect: Duration,
    pub max_playlist_tracks: usize,
    pub ytdlp_cookies: Option<String>,
    pub ytdlp_cookies_file: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let discord_token = env::var("DISCORD_TOKEN").context("DISCORD_TOKEN is required")?;
        let development_guild = env::var("DISCORD_GUILD_ID")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| value.parse::<u64>().map(GuildId::new))
            .transpose()
            .context("DISCORD_GUILD_ID must be a Discord snowflake")?;
        let database_url = env::var("DATABASE_URL")
            .unwrap_or_else(|_| "sqlite://data/jackson.db?mode=rwc".to_owned());
        let idle_disconnect = Duration::from_secs(
            env::var("IDLE_DISCONNECT_SECS")
                .unwrap_or_else(|_| "300".to_owned())
                .parse()
                .context("IDLE_DISCONNECT_SECS must be an integer")?,
        );
        let max_playlist_tracks = env::var("MAX_PLAYLIST_TRACKS")
            .unwrap_or_else(|_| "100".to_owned())
            .parse::<usize>()
            .context("MAX_PLAYLIST_TRACKS must be an integer")?
            .clamp(1, 500);
        let ytdlp_cookies_file = env::var("YTDLP_COOKIES_FILE")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let ytdlp_cookies = env::var("YTDLP_COOKIES")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if ytdlp_cookies.is_some() && ytdlp_cookies_file.is_some() {
            anyhow::bail!("set only one of YTDLP_COOKIES or YTDLP_COOKIES_FILE");
        }
        if ytdlp_cookies
            .as_ref()
            .is_some_and(|cookies| cookies.contains(['\r', '\n']))
        {
            anyhow::bail!("YTDLP_COOKIES must be a single-line HTTP Cookie header value");
        }

        Ok(Self {
            discord_token,
            development_guild,
            database_url,
            idle_disconnect,
            max_playlist_tracks,
            ytdlp_cookies,
            ytdlp_cookies_file,
        })
    }
}
