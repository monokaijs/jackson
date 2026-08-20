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

- Rust 1.93 or newer
- A current [`yt-dlp`](https://github.com/yt-dlp/yt-dlp#installation) executable on `PATH`, installed with its default extras
- [Deno](https://docs.deno.com/runtime/getting_started/installation/) 2.3 or newer on `PATH` for YouTube's JavaScript challenges
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
# Set DISCORD_TOKEN and keep JACKSON_IMAGE_TAG pinned to a released version.
chmod 600 .env
docker compose pull
docker compose up -d
docker compose ps
docker compose logs -f jackson
```

Compose pulls the multi-architecture `ghcr.io/monokaijs/jackson:0.1.2` release by default and Docker automatically selects amd64 or arm64. Set `JACKSON_IMAGE_TAG` to another released version when upgrading; architecture-specific tags such as `0.1.2-amd64` are also available. The service runs with a read-only root filesystem, no Linux capabilities, bounded resources and logs, a health check, and a persistent `jackson-data` volume for SQLite. No inbound ports are required. Update the image regularly because media sites change and an old `yt-dlp` build will eventually stop resolving sources.

Create a consistent database backup during a brief graceful stop:

```bash
docker compose stop jackson
docker compose cp jackson:/app/data/jackson.db ./jackson.db.backup
docker compose start jackson
```

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `DISCORD_TOKEN` | required | Bot token; never commit this |
| `DISCORD_GUILD_ID` | unset | Optional development server for command registration |
| `DATABASE_URL` | `sqlite://data/jackson.db?mode=rwc` | SQLite connection URL |
| `IDLE_DISCONNECT_SECS` | `300` | Empty-queue delay in normal mode |
| `MAX_PLAYLIST_TRACKS` | `100` | Playlist cap, clamped from 1 to 500 |
| `YTDLP_COOKIES` | unset | Optional single-line YouTube Cookie header value converted to a private temporary Netscape cookie jar |
| `YTDLP_COOKIES_FILE` | unset | Optional Netscape-format cookies file path passed to every yt-dlp invocation |
| `RUST_LOG` | `jackson=info,songbird=warn,serenity=warn` | Structured log filter |
| `JACKSON_IMAGE_TAG` | `0.1.2` | Compose image version; pin this in production |
| `JACKSON_CPUS` | `2.0` | Compose CPU limit |
| `JACKSON_MEMORY_LIMIT` | `2g` | Compose memory limit |
| `JACKSON_MEMORY_RESERVATION` | `256m` | Compose soft memory reservation |

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

## Releases

Releases are created manually from GitHub's **Actions → Release → Run workflow** screen on the `main` branch. The default `current` strategy releases the version declared in `Cargo.toml`. Alternatively, select `patch`, `minor`, or `major` to derive the next version from the latest stable `vX.Y.Z` tag, or provide an exact SemVer such as `1.0.0-rc.1`.

The workflow rejects invalid or duplicate versions, runs formatting, Clippy, and tests, then builds Linux x86-64, macOS Apple Silicon, and Windows x86-64 archives. Native amd64 and arm64 runners independently publish architecture-specific GHCR tags as soon as each image finishes; a final manifest publishes the version, `v`-prefixed, and stable `latest` multi-architecture tags. The GitHub Release publishes independently once its binary archives are ready. Each release includes a `SHA256SUMS` file. The optional artifact run ID can recover a release using archives from a prior successful build. Runtime binary installations still require `yt-dlp` on `PATH`; the container includes it.

## Source policy

Only play media you are authorized to access and rebroadcast. Operators are responsible for complying with Discord's terms and each media provider's terms, copyright rules, and local law. The resolver is deliberately isolated in `src/resolver.rs` so a licensed catalog or first-party media API can replace `yt-dlp` without changing the player.
