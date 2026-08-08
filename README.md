# Jackson

Jackson is a low-overhead Discord music bot written in Rust. Its interaction model is inspired by Rythm: short slash commands, useful queue messages, and player buttons that work for everyone in the active voice channel.

It has two operating modes:

- **Normal:** leaves after the queue has been idle for a configurable period (five minutes by default).
- **24/7:** stays in a server's selected voice channel and restores that connection after a process or gateway restart. The setting is persisted in SQLite.

## Audio path

```text
query/URL -> yt-dlp resolves audio-only URL -> Songbird demuxes -> Discord voice
                                            |               |
                                            +-- Opus/WebM ---+  frame passthrough
                                            +-- other codec -> decode/resample/Opus fallback
```

There is no Lavalink process and no FFmpeg transcoding process. Songbird asks `yt-dlp` for an audio-only source and prefers WebM/Opus. A single Opus track at exactly 100% volume is sent using direct frame passthrough. Seeking, pausing, and queue operations stay outside the real-time audio task. Non-Opus inputs, overlapping audio, or custom volume require the normal in-process decode/mix/encode fallback.

## Commands

| Command | Purpose |
| --- | --- |
| `/play <name-or-url>` | Search or enqueue a track or playlist (up to the configured limit) |
| `/pause`, `/resume`, `/skip` | Playback controls |
| `/stop`, `/leave` | Stop and clear; or also disconnect and disable 24/7 |
| `/queue [page]`, `/nowplaying` | Queue and player views |
| `/seek <1:30>` | Seek in a track |
| `/volume <0..200>` | Change volume; 100% preserves passthrough eligibility |
| `/loop <off|track|queue>` | Select repeat mode |
| `/shuffle`, `/remove`, `/move`, `/clear` | Queue editing |
| `/247 <status|on|off>` | Persistent always-connected mode; changes require Manage Server |

Pause/resume, skip, loop, shuffle, and stop are also available as buttons. Playback controls require the user to be in the bot's voice channel, which prevents remote queue hijacking.

## Run locally

Requirements:

- Rust 1.85 or newer
- A current [`yt-dlp`](https://github.com/yt-dlp/yt-dlp#installation) executable on `PATH`
- A Discord application bot token

Create a bot in the Discord developer portal. Enable no privileged intents; Jackson only requests `GUILDS` and `GUILD_VOICE_STATES`. Invite it with the `bot` and `applications.commands` scopes and these channel permissions:

- View Channels
- Send Messages
- Embed Links
- Connect
- Speak
- Use Voice Activity

Then run:

```bash
cp .env.example .env
# edit DISCORD_TOKEN; set DISCORD_GUILD_ID while developing for instant command updates
cargo run --release
```

Global slash-command changes can take time to propagate. A development guild uses guild commands, which update immediately.

## Docker

```bash
cp .env.example .env
docker compose up --build -d
docker compose logs -f jackson
```

The SQLite database is stored in the `jackson-data` volume. Update the image regularly because media sites change and an old `yt-dlp` build will eventually stop resolving sources.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `DISCORD_TOKEN` | required | Bot token; never commit this |
| `DISCORD_GUILD_ID` | unset | Optional development server for command registration |
| `DATABASE_URL` | `sqlite://data/jackson.db?mode=rwc` | SQLite connection URL |
| `IDLE_DISCONNECT_SECS` | `300` | Empty-queue delay in normal mode |
| `MAX_PLAYLIST_TRACKS` | `100` | Playlist cap, clamped from 1 to 500 |
| `RUST_LOG` | `jackson=info,songbird=warn,serenity=warn` | Structured log filter |

## Design notes

- Each guild owns a small mutex-protected state machine; guilds never share a queue lock.
- Track completion uses generation IDs, so delayed end events cannot skip a newly started track.
- A shared HTTP connection pool is reused across all sources.
- Only the current item is handed to Songbird. Queue metadata does not allocate decoder or voice resources.
- The bot uses no message-content intent and does not parse legacy prefix commands.
- SQLite uses a five-connection pool only for small settings writes; it is not touched by the audio loop.

Run the quality checks with:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

## Source policy

Only play media you are authorized to access and rebroadcast. Operators are responsible for complying with Discord's terms and each media provider's terms, copyright rules, and local law. The resolver is deliberately isolated in `src/resolver.rs` so a licensed catalog or first-party media API can replace `yt-dlp` without changing the player.
