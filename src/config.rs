use std::{env, time::Duration};

use anyhow::{Context, Result};
use serenity::all::GuildId;

#[derive(Clone, Debug)]
pub struct Config {
    pub discord_token: String,
    pub development_guild: Option<GuildId>,
    pub database_url: String,
    pub idle_disconnect: Duration,
    pub max_playlist_tracks: usize,
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

        Ok(Self {
            discord_token,
            development_guild,
            database_url,
            idle_disconnect,
            max_playlist_tracks,
        })
    }
}
