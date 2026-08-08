use std::{sync::Arc, time::Duration};

use anyhow::{Result, anyhow, bail};
use serenity::all::{
    Command, CommandDataOptionValue, CommandInteraction, CommandOptionType, ComponentInteraction,
    Context, CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseMessage, EditInteractionResponse, GuildId, Permissions, UserId,
};
use tracing::{error, info};

use crate::{
    player::{LoopMode, MusicService},
    ui,
};

pub async fn register(ctx: &Context, development_guild: Option<GuildId>) -> Result<()> {
    let commands = definitions();
    if let Some(guild_id) = development_guild {
        guild_id.set_commands(&ctx.http, commands).await?;
        info!(%guild_id, "registered development guild commands");
    } else {
        Command::set_global_commands(&ctx.http, commands).await?;
        info!("registered global commands");
    }
    Ok(())
}

pub async fn handle_command(
    ctx: &Context,
    command: &CommandInteraction,
    music: &Arc<MusicService>,
) {
    if let Err(error) = command
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new()),
        )
        .await
    {
        error!(%error, command = %command.data.name, "failed to defer interaction");
        return;
    }

    let response = match execute(ctx, command, music).await {
        Ok(response) => response,
        Err(error) => {
            error!(%error, command = %command.data.name, guild_id = ?command.guild_id, "command failed");
            ui::error_response(&error)
        }
    };

    if let Err(error) = command.edit_response(&ctx.http, response).await {
        error!(%error, command = %command.data.name, "failed to edit interaction response");
    }
}

async fn execute(
    ctx: &Context,
    command: &CommandInteraction,
    music: &Arc<MusicService>,
) -> Result<EditInteractionResponse> {
    let guild_id = command
        .guild_id
        .ok_or_else(|| anyhow!("Music commands only work in a server."))?;

    match command.data.name.as_str() {
        "play" => {
            let channel_id = require_user_voice(ctx, guild_id, command.user.id)?;
            reject_other_bot_channel(ctx, guild_id, channel_id)?;
            let tracks = music
                .enqueue(
                    guild_id,
                    channel_id,
                    required_string(command, "query")?,
                    command.user.id,
                )
                .await?;
            let snapshot = music.snapshot(guild_id).await?;
            Ok(ui::queued_response(&tracks, &snapshot))
        }
        "pause" => {
            require_same_voice(ctx, guild_id, command.user.id)?;
            music.pause(guild_id).await?;
            Ok(ui::text_response("⏸️ Paused."))
        }
        "resume" => {
            require_same_voice(ctx, guild_id, command.user.id)?;
            music.resume(guild_id).await?;
            Ok(ui::text_response("▶️ Resumed."))
        }
        "skip" => {
            require_same_voice(ctx, guild_id, command.user.id)?;
            music.skip(guild_id).await?;
            Ok(ui::text_response("⏭️ Skipped."))
        }
        "stop" => {
            require_same_voice(ctx, guild_id, command.user.id)?;
            music.stop(guild_id).await?;
            Ok(ui::text_response(
                "⏹️ Stopped playback and cleared the queue.",
            ))
        }
        "leave" => {
            require_same_voice(ctx, guild_id, command.user.id)?;
            music.leave(guild_id).await?;
            Ok(ui::text_response("👋 Disconnected. 24/7 mode is off."))
        }
        "queue" => {
            let page = optional_integer(command, "page").unwrap_or(1).max(1) as usize;
            Ok(ui::queue_response(&music.snapshot(guild_id).await?, page))
        }
        "nowplaying" => Ok(ui::now_playing_response(&music.snapshot(guild_id).await?)),
        "seek" => {
            require_same_voice(ctx, guild_id, command.user.id)?;
            let position = parse_duration(required_string(command, "position")?)?;
            music.seek(guild_id, position).await?;
            Ok(ui::text_response(format!(
                "⏩ Seeked to {}.",
                ui::format_clock(position)
            )))
        }
        "volume" => {
            require_same_voice(ctx, guild_id, command.user.id)?;
            let percent = required_integer(command, "percent")?.clamp(0, 200);
            music.set_volume(guild_id, percent as f32 / 100.0).await?;
            let note = if percent == 100 {
                " Opus passthrough is enabled for compatible sources."
            } else {
                " Custom volume requires audio mixing; use 100% for the lightest pipeline."
            };
            Ok(ui::text_response(format!(
                "🔊 Volume set to {percent}%.{note}"
            )))
        }
        "loop" => {
            require_same_voice(ctx, guild_id, command.user.id)?;
            let mode = match required_string(command, "mode")? {
                "off" => LoopMode::Off,
                "track" => LoopMode::Track,
                "queue" => LoopMode::Queue,
                _ => bail!("Unknown loop mode."),
            };
            music.set_loop(guild_id, mode).await?;
            Ok(ui::text_response(format!(
                "🔁 Loop mode: **{}**.",
                mode.label()
            )))
        }
        "shuffle" => {
            require_same_voice(ctx, guild_id, command.user.id)?;
            let count = music.shuffle(guild_id).await?;
            Ok(ui::text_response(format!(
                "🔀 Shuffled {count} queued tracks."
            )))
        }
        "remove" => {
            require_same_voice(ctx, guild_id, command.user.id)?;
            let position = required_integer(command, "position")? as usize;
            let removed = music.remove(guild_id, position).await?;
            Ok(ui::text_response(format!(
                "🗑️ Removed **{}**.",
                removed.title
            )))
        }
        "move" => {
            require_same_voice(ctx, guild_id, command.user.id)?;
            let from = required_integer(command, "from")? as usize;
            let to = required_integer(command, "to")? as usize;
            music.move_track(guild_id, from, to).await?;
            Ok(ui::text_response(format!(
                "↕️ Moved queue item {from} to {to}."
            )))
        }
        "clear" => {
            require_same_voice(ctx, guild_id, command.user.id)?;
            let count = music.clear_queue(guild_id).await?;
            Ok(ui::text_response(format!(
                "🧹 Cleared {count} upcoming tracks."
            )))
        }
        "247" => execute_always_on(ctx, command, music, guild_id).await,
        _ => bail!("Unknown command."),
    }
}

async fn execute_always_on(
    ctx: &Context,
    command: &CommandInteraction,
    music: &Arc<MusicService>,
    guild_id: GuildId,
) -> Result<EditInteractionResponse> {
    let action = required_string(command, "action")?;
    if action == "status" {
        let enabled = music.snapshot(guild_id).await?.always_on;
        return Ok(ui::text_response(format!(
            "24/7 mode is **{}**.",
            if enabled { "on" } else { "off" }
        )));
    }

    let can_manage = command
        .member
        .as_ref()
        .and_then(|member| member.permissions)
        .is_some_and(|permissions| permissions.contains(Permissions::MANAGE_GUILD));
    if !can_manage {
        bail!("You need the Manage Server permission to change 24/7 mode.");
    }

    match action {
        "on" => {
            let channel_id = require_user_voice(ctx, guild_id, command.user.id)?;
            reject_other_bot_channel(ctx, guild_id, channel_id)?;
            music.set_always_on(guild_id, channel_id, true).await?;
            Ok(ui::text_response(
                "♾️ 24/7 mode is **on**. I will stay connected and restore this channel after restarts.",
            ))
        }
        "off" => {
            music.disable_always_on(guild_id).await?;
            Ok(ui::text_response(
                "♾️ 24/7 mode is **off**. Normal idle disconnects are active.",
            ))
        }
        _ => bail!("Unknown 24/7 action."),
    }
}

pub async fn handle_component(
    ctx: &Context,
    component: &ComponentInteraction,
    music: &Arc<MusicService>,
) {
    let result = async {
        let guild_id = component
            .guild_id
            .ok_or_else(|| anyhow!("This control only works in a server."))?;
        require_same_voice(ctx, guild_id, component.user.id)?;
        match component.data.custom_id.as_str() {
            "music:pause" => {
                let paused = music.toggle_pause(guild_id).await?;
                Ok(if paused {
                    "⏸️ Paused.".to_owned()
                } else {
                    "▶️ Resumed.".to_owned()
                })
            }
            "music:skip" => {
                music.skip(guild_id).await?;
                Ok("⏭️ Skipped.".to_owned())
            }
            "music:loop" => {
                let mode = music.cycle_loop(guild_id).await?;
                Ok(format!("🔁 Loop mode: **{}**.", mode.label()))
            }
            "music:shuffle" => {
                let count = music.shuffle(guild_id).await?;
                Ok(format!("🔀 Shuffled {count} queued tracks."))
            }
            "music:stop" => {
                music.stop(guild_id).await?;
                Ok("⏹️ Stopped and cleared the queue.".to_owned())
            }
            _ => bail!("This player control has expired."),
        }
    }
    .await;

    let message = match result {
        Ok(message) => message,
        Err(error) => error.root_cause().to_string(),
    };
    if let Err(error) = component
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(ui::component_text(message)),
        )
        .await
    {
        error!(%error, "failed to respond to player component");
    }
}

fn require_user_voice(
    ctx: &Context,
    guild_id: GuildId,
    user_id: UserId,
) -> Result<serenity::all::ChannelId> {
    voice_channel(ctx, guild_id, user_id).ok_or_else(|| anyhow!("Join a voice channel first."))
}

fn require_same_voice(
    ctx: &Context,
    guild_id: GuildId,
    user_id: UserId,
) -> Result<serenity::all::ChannelId> {
    let user_channel = require_user_voice(ctx, guild_id, user_id)?;
    let bot_channel = voice_channel(ctx, guild_id, ctx.cache.current_user().id)
        .ok_or_else(|| anyhow!("I'm not in a voice channel."))?;
    if user_channel != bot_channel {
        bail!("Join my voice channel to control playback.");
    }
    Ok(user_channel)
}

fn reject_other_bot_channel(
    ctx: &Context,
    guild_id: GuildId,
    user_channel: serenity::all::ChannelId,
) -> Result<()> {
    if let Some(bot_channel) = voice_channel(ctx, guild_id, ctx.cache.current_user().id)
        && bot_channel != user_channel
    {
        bail!("I'm already playing in another voice channel.");
    }
    Ok(())
}

fn voice_channel(
    ctx: &Context,
    guild_id: GuildId,
    user_id: UserId,
) -> Option<serenity::all::ChannelId> {
    ctx.cache.guild(guild_id).and_then(|guild| {
        guild
            .voice_states
            .get(&user_id)
            .and_then(|state| state.channel_id)
    })
}

fn required_string<'a>(command: &'a CommandInteraction, name: &str) -> Result<&'a str> {
    command
        .data
        .options
        .iter()
        .find(|option| option.name == name)
        .and_then(|option| match &option.value {
            CommandDataOptionValue::String(value) => Some(value.as_str()),
            _ => None,
        })
        .ok_or_else(|| anyhow!("Missing `{name}`."))
}

fn optional_integer(command: &CommandInteraction, name: &str) -> Option<i64> {
    command
        .data
        .options
        .iter()
        .find(|option| option.name == name)
        .and_then(|option| match option.value {
            CommandDataOptionValue::Integer(value) => Some(value),
            _ => None,
        })
}

fn required_integer(command: &CommandInteraction, name: &str) -> Result<i64> {
    optional_integer(command, name).ok_or_else(|| anyhow!("Missing `{name}`."))
}

fn parse_duration(value: &str) -> Result<Duration> {
    if value.contains(':') {
        let pieces = value
            .split(':')
            .map(str::parse::<u64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let seconds = match pieces.as_slice() {
            [minutes, seconds] if *seconds < 60 => minutes * 60 + seconds,
            [hours, minutes, seconds] if *minutes < 60 && *seconds < 60 => {
                hours * 3_600 + minutes * 60 + seconds
            }
            _ => bail!("Use a time like `1:30`, `1:02:30`, or `90s`."),
        };
        return Ok(Duration::from_secs(seconds));
    }
    humantime::parse_duration(value).map_err(Into::into)
}

fn definitions() -> Vec<CreateCommand> {
    let required_string = |name, description| {
        CreateCommandOption::new(CommandOptionType::String, name, description).required(true)
    };
    let required_integer = |name, description| {
        CreateCommandOption::new(CommandOptionType::Integer, name, description)
            .required(true)
            .min_int_value(1)
    };

    vec![
        CreateCommand::new("play")
            .description("Play a song, URL, or playlist")
            .add_option(required_string("query", "Song name or media URL")),
        CreateCommand::new("pause").description("Pause the current track"),
        CreateCommand::new("resume").description("Resume the current track"),
        CreateCommand::new("skip").description("Skip the current track"),
        CreateCommand::new("stop").description("Stop playback and clear the queue"),
        CreateCommand::new("leave").description("Stop playback, disable 24/7, and disconnect"),
        CreateCommand::new("queue")
            .description("Show the music queue")
            .add_option(
                CreateCommandOption::new(CommandOptionType::Integer, "page", "Queue page")
                    .min_int_value(1),
            ),
        CreateCommand::new("nowplaying").description("Show the current track and player controls"),
        CreateCommand::new("seek")
            .description("Seek within the current track")
            .add_option(required_string("position", "Time such as 1:30 or 90s")),
        CreateCommand::new("volume")
            .description("Set playback volume (100% keeps the zero-transcode path)")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Integer,
                    "percent",
                    "Volume from 0 to 200 percent",
                )
                .required(true)
                .min_int_value(0)
                .max_int_value(200),
            ),
        CreateCommand::new("loop")
            .description("Set the loop mode")
            .add_option(
                required_string("mode", "What to repeat")
                    .add_string_choice("Off", "off")
                    .add_string_choice("Current track", "track")
                    .add_string_choice("Whole queue", "queue"),
            ),
        CreateCommand::new("shuffle").description("Shuffle upcoming tracks"),
        CreateCommand::new("remove")
            .description("Remove an upcoming track")
            .add_option(required_integer("position", "Queue position to remove")),
        CreateCommand::new("move")
            .description("Move an upcoming track")
            .add_option(required_integer("from", "Current queue position"))
            .add_option(required_integer("to", "New queue position")),
        CreateCommand::new("clear")
            .description("Clear upcoming tracks without stopping the current song"),
        CreateCommand::new("247")
            .description("Keep the bot in voice continuously")
            .add_option(
                required_string("action", "Turn 24/7 mode on or off")
                    .add_string_choice("Status", "status")
                    .add_string_choice("On", "on")
                    .add_string_choice("Off", "off"),
            ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_seek_positions() {
        assert_eq!(parse_duration("1:30").unwrap(), Duration::from_secs(90));
        assert_eq!(
            parse_duration("1:02:03").unwrap(),
            Duration::from_secs(3_723)
        );
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert!(parse_duration("1:99").is_err());
    }
}
