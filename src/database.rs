use std::{str::FromStr, time::Duration};

use anyhow::{Context, Result};
use serenity::all::{ChannelId, GuildId};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

#[derive(Clone, Debug)]
pub struct GuildSettings {
    pub guild_id: GuildId,
    pub voice_channel_id: Option<ChannelId>,
    pub always_on: bool,
    pub volume: f32,
}

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn connect(url: &str) -> Result<Self> {
        if let Some(path) = url
            .strip_prefix("sqlite://")
            .and_then(|path| path.split('?').next())
            .and_then(|path| std::path::Path::new(path).parent())
        {
            tokio::fs::create_dir_all(path).await.with_context(|| {
                format!("failed to create database directory {}", path.display())
            })?;
        }

        let options = SqliteConnectOptions::from_str(url)
            .context("invalid SQLite database URL")?
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .context("failed to open SQLite database")?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS guild_settings (
                guild_id        TEXT PRIMARY KEY NOT NULL,
                voice_channel_id TEXT,
                always_on       INTEGER NOT NULL DEFAULT 0,
                volume          REAL NOT NULL DEFAULT 1.0,
                updated_at      TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&pool)
        .await
        .context("failed to migrate guild_settings")?;

        Ok(Self { pool })
    }

    pub async fn get(&self, guild_id: GuildId) -> Result<GuildSettings> {
        let row = sqlx::query(
            "SELECT voice_channel_id, always_on, volume FROM guild_settings WHERE guild_id = ?",
        )
        .bind(guild_id.get().to_string())
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(GuildSettings {
                guild_id,
                voice_channel_id: None,
                always_on: false,
                volume: 1.0,
            });
        };

        let channel = row
            .try_get::<Option<String>, _>("voice_channel_id")?
            .map(|value| value.parse::<u64>().map(ChannelId::new))
            .transpose()
            .context("invalid voice channel ID in database")?;

        Ok(GuildSettings {
            guild_id,
            voice_channel_id: channel,
            always_on: row.try_get::<i64, _>("always_on")? != 0,
            volume: row.try_get::<f64, _>("volume")? as f32,
        })
    }

    pub async fn set_always_on(
        &self,
        guild_id: GuildId,
        channel_id: Option<ChannelId>,
        enabled: bool,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO guild_settings (guild_id, voice_channel_id, always_on)
            VALUES (?, ?, ?)
            ON CONFLICT(guild_id) DO UPDATE SET
                voice_channel_id = excluded.voice_channel_id,
                always_on = excluded.always_on,
                updated_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(guild_id.get().to_string())
        .bind(channel_id.map(|id| id.get().to_string()))
        .bind(i64::from(enabled))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_volume(&self, guild_id: GuildId, volume: f32) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO guild_settings (guild_id, volume)
            VALUES (?, ?)
            ON CONFLICT(guild_id) DO UPDATE SET
                volume = excluded.volume,
                updated_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(guild_id.get().to_string())
        .bind(volume as f64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn enabled_guilds(&self) -> Result<Vec<GuildSettings>> {
        let rows = sqlx::query(
            "SELECT guild_id, voice_channel_id, always_on, volume FROM guild_settings WHERE always_on = 1",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let guild_id = row
                    .try_get::<String, _>("guild_id")?
                    .parse::<u64>()
                    .map(GuildId::new)?;
                let voice_channel_id = row
                    .try_get::<Option<String>, _>("voice_channel_id")?
                    .map(|value| value.parse::<u64>().map(ChannelId::new))
                    .transpose()?;
                Ok(GuildSettings {
                    guild_id,
                    voice_channel_id,
                    always_on: row.try_get::<i64, _>("always_on")? != 0,
                    volume: row.try_get::<f64, _>("volume")? as f32,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persists_always_on_settings() {
        let directory = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("settings.db").display()
        );
        let database = Database::connect(&url).await.unwrap();
        let guild_id = GuildId::new(42);
        let channel_id = ChannelId::new(84);

        database
            .set_always_on(guild_id, Some(channel_id), true)
            .await
            .unwrap();
        database.set_volume(guild_id, 0.75).await.unwrap();

        let settings = database.get(guild_id).await.unwrap();
        assert!(settings.always_on);
        assert_eq!(settings.voice_channel_id, Some(channel_id));
        assert_eq!(settings.volume, 0.75);
        assert_eq!(database.enabled_guilds().await.unwrap().len(), 1);
    }
}
