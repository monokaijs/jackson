use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context as _, Result, bail};
use jackson::{
    commands, config::Config, database::Database, player::MusicService, resolver::Resolver,
};
use serenity::all::{Context, EventHandler, GatewayIntents, Interaction, Ready, VoiceState};
use songbird::{SerenityInit, Songbird};
use tokio::process::Command;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

fn build_version() -> &'static str {
    match option_env!("JACKSON_RELEASE_VERSION") {
        Some(version) if !version.is_empty() => version,
        _ => env!("CARGO_PKG_VERSION"),
    }
}

struct Handler {
    music: Arc<MusicService>,
    development_guild: Option<serenity::all::GuildId>,
    initialized: AtomicBool,
}

#[serenity::async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!(user = %ready.user.name, guilds = ready.guilds.len(), "Discord gateway ready");
        if self.initialized.swap(true, Ordering::SeqCst) {
            return;
        }

        if let Err(error) = commands::register(&ctx, self.development_guild).await {
            error!(%error, "failed to register application commands");
        }
        if let Err(error) = self.music.restore_always_on().await {
            error!(%error, "failed to restore 24/7 sessions");
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(command) => {
                commands::handle_command(&ctx, &command, &self.music).await;
            }
            Interaction::Component(component) if component.data.custom_id.starts_with("music:") => {
                commands::handle_component(&ctx, &component, &self.music).await;
            }
            _ => {}
        }
    }

    async fn voice_state_update(&self, ctx: Context, old: Option<VoiceState>, new: VoiceState) {
        if new.user_id == ctx.cache.current_user().id
            && old.as_ref().and_then(|state| state.channel_id).is_some()
            && new.channel_id.is_none()
            && let Some(guild_id) = new.guild_id
        {
            let music = Arc::clone(&self.music);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                if let Err(error) = music.restore_guild_if_enabled(guild_id).await {
                    warn!(%guild_id, %error, "could not restore interrupted 24/7 session");
                }
            });
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("jackson=info,songbird=warn,serenity=warn")),
        )
        .compact()
        .init();

    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--version")) {
        println!("jackson {}", build_version());
        return Ok(());
    }

    let config = Config::from_env()?;
    verify_ytdlp().await?;
    let database = Database::connect(&config.database_url).await?;
    let http_client = reqwest::Client::builder()
        .user_agent(format!("jackson/{}", build_version()))
        .pool_max_idle_per_host(16)
        .build()
        .context("failed to create media HTTP client")?;

    let voice = Songbird::serenity();
    let music = MusicService::new(
        Arc::clone(&voice),
        database,
        Resolver::new(http_client, config.max_playlist_tracks),
        config.idle_disconnect,
    );
    let handler = Handler {
        music,
        development_guild: config.development_guild,
        initialized: AtomicBool::new(false),
    };
    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES;
    let mut client = serenity::Client::builder(&config.discord_token, intents)
        .event_handler(handler)
        .register_songbird_with(voice)
        .await
        .context("failed to create Discord client")?;

    client
        .start_autosharded()
        .await
        .context("Discord client stopped")
}

async fn verify_ytdlp() -> Result<()> {
    let output = Command::new("yt-dlp").arg("--version").output().await;
    match output {
        Ok(output) if output.status.success() => {
            info!(version = %String::from_utf8_lossy(&output.stdout).trim(), "yt-dlp available");
            Ok(())
        }
        Ok(output) => bail!(
            "yt-dlp failed its startup check: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => Err(error).context("yt-dlp is required; install it and ensure it is on PATH"),
    }
}
