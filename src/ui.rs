use serenity::all::{
    ButtonStyle, CreateActionRow, CreateButton, CreateEmbed, CreateEmbedFooter,
    CreateInteractionResponseMessage, EditInteractionResponse, ReactionType,
};

use crate::{player::PlayerSnapshot, resolver::QueuedTrack};

pub const ACCENT: u32 = 0x5B_8D_FF;
pub const ERROR: u32 = 0xED_42_45;

pub fn error_response(error: &anyhow::Error) -> EditInteractionResponse {
    EditInteractionResponse::new().embed(
        CreateEmbed::new()
            .color(ERROR)
            .title("Couldn't do that")
            .description(error.root_cause().to_string()),
    )
}

pub fn text_response(message: impl Into<String>) -> EditInteractionResponse {
    EditInteractionResponse::new().content(message)
}

pub fn component_text(message: impl Into<String>) -> CreateInteractionResponseMessage {
    CreateInteractionResponseMessage::new()
        .content(message)
        .ephemeral(true)
}

pub fn queued_response(
    tracks: &[QueuedTrack],
    snapshot: &PlayerSnapshot,
) -> EditInteractionResponse {
    let first = &tracks[0];
    let title = if tracks.len() == 1 {
        "Added to queue".to_owned()
    } else {
        format!("Added {} tracks", tracks.len())
    };
    let description = if tracks.len() == 1 {
        format!(
            "[{}]({})\n{} • requested by <@{}>",
            escape_markdown(&first.title),
            first.source_url,
            format_duration(first.duration),
            first.requester.get()
        )
    } else {
        format!(
            "Starting with [{}]({})\nQueue now contains {} upcoming tracks.",
            escape_markdown(&first.title),
            first.source_url,
            snapshot.upcoming.len()
        )
    };

    let mut embed = CreateEmbed::new()
        .color(ACCENT)
        .title(title)
        .description(description);
    if let Some(thumbnail) = &first.thumbnail {
        embed = embed.thumbnail(thumbnail);
    }
    EditInteractionResponse::new()
        .embed(embed)
        .components(controller(snapshot.paused))
}

pub fn now_playing_response(snapshot: &PlayerSnapshot) -> EditInteractionResponse {
    let Some(track) = &snapshot.current else {
        return text_response("Nothing is playing right now.");
    };
    let mut embed = CreateEmbed::new()
        .color(ACCENT)
        .title("Now playing")
        .description(format!(
            "[{}]({})\n{} • {} • requested by <@{}>",
            escape_markdown(&track.title),
            track.source_url,
            track.display_artist(),
            format_duration(track.duration),
            track.requester.get(),
        ))
        .field(
            "Player",
            format!(
                "{} • loop {} • volume {}%{}",
                if snapshot.paused { "paused" } else { "playing" },
                snapshot.loop_mode.label(),
                (snapshot.volume * 100.0).round() as u16,
                if snapshot.always_on { " • 24/7" } else { "" },
            ),
            false,
        )
        .footer(CreateEmbedFooter::new(format!(
            "Position {}",
            format_clock(snapshot.position)
        )));
    if let Some(thumbnail) = &track.thumbnail {
        embed = embed.thumbnail(thumbnail);
    }
    EditInteractionResponse::new()
        .embed(embed)
        .components(controller(snapshot.paused))
}

pub fn queue_response(snapshot: &PlayerSnapshot, page: usize) -> EditInteractionResponse {
    const PAGE_SIZE: usize = 10;
    let pages = snapshot.upcoming.len().max(1).div_ceil(PAGE_SIZE);
    let page = page.clamp(1, pages);
    let start = (page - 1) * PAGE_SIZE;
    let lines = snapshot
        .upcoming
        .iter()
        .enumerate()
        .skip(start)
        .take(PAGE_SIZE)
        .map(|(index, track)| {
            format!(
                "`{:>2}.` [{}]({}) `({})` • <@{}>",
                index + 1,
                escape_markdown(&track.title),
                track.source_url,
                format_duration(track.duration),
                track.requester.get()
            )
        })
        .collect::<Vec<_>>();

    let current = snapshot
        .current
        .as_ref()
        .map(|track| {
            format!(
                "**Now:** [{}]({})\n\n",
                escape_markdown(&track.title),
                track.source_url
            )
        })
        .unwrap_or_default();
    let body = if lines.is_empty() {
        "The queue is empty. Use `/play` to add something.".to_owned()
    } else {
        lines.join("\n")
    };

    EditInteractionResponse::new().embed(
        CreateEmbed::new()
            .color(ACCENT)
            .title(format!("Queue • {} track(s)", snapshot.upcoming.len()))
            .description(format!("{current}{body}"))
            .footer(CreateEmbedFooter::new(format!(
                "Page {page}/{pages} • loop {}{}",
                snapshot.loop_mode.label(),
                if snapshot.always_on { " • 24/7" } else { "" }
            ))),
    )
}

pub fn controller(paused: bool) -> Vec<CreateActionRow> {
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new("music:pause")
            .style(ButtonStyle::Secondary)
            .emoji(ReactionType::Unicode(
                if paused { "▶️" } else { "⏯️" }.to_owned(),
            )),
        CreateButton::new("music:skip")
            .style(ButtonStyle::Primary)
            .emoji(ReactionType::Unicode("⏭️".to_owned())),
        CreateButton::new("music:loop")
            .style(ButtonStyle::Secondary)
            .emoji(ReactionType::Unicode("🔁".to_owned())),
        CreateButton::new("music:shuffle")
            .style(ButtonStyle::Secondary)
            .emoji(ReactionType::Unicode("🔀".to_owned())),
        CreateButton::new("music:stop")
            .style(ButtonStyle::Danger)
            .emoji(ReactionType::Unicode("⏹️".to_owned())),
    ])]
}

pub fn format_duration(duration: Option<std::time::Duration>) -> String {
    duration
        .map(format_clock)
        .unwrap_or_else(|| "live".to_owned())
}

pub fn format_clock(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn escape_markdown(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('*', "\\*")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_track_lengths() {
        assert_eq!(format_clock(std::time::Duration::from_secs(65)), "1:05");
        assert_eq!(
            format_clock(std::time::Duration::from_secs(3_661)),
            "1:01:01"
        );
        assert_eq!(format_duration(None), "live");
    }
}
