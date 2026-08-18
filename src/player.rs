use std::{
    collections::VecDeque,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use dashmap::{DashMap, mapref::entry::Entry};
use rand::seq::SliceRandom;
use serenity::all::{ChannelId, GuildId};
use songbird::{
    Event, EventContext, EventHandler, Songbird, TrackEvent,
    input::cached::Memory,
    tracks::{PlayMode, TrackHandle},
};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::{
    database::Database,
    resolver::{QueuedTrack, Resolver},
};

const MAX_QUEUE_LENGTH: usize = 1_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopMode {
    #[default]
    Off,
    Track,
    Queue,
}

impl LoopMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Track => "track",
            Self::Queue => "queue",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Track,
            Self::Track => Self::Queue,
            Self::Queue => Self::Off,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PlayerSnapshot {
    pub current: Option<QueuedTrack>,
    pub upcoming: Vec<QueuedTrack>,
    pub loop_mode: LoopMode,
    pub volume: f32,
    pub paused: bool,
    pub always_on: bool,
    pub position: Duration,
}

struct CurrentTrack {
    item: QueuedTrack,
    handle: Option<TrackHandle>,
    generation: u64,
    started_at: Instant,
}

struct PlayerState {
    queue: VecDeque<QueuedTrack>,
    current: Option<CurrentTrack>,
    loop_mode: LoopMode,
    volume: f32,
    paused: bool,
    always_on: bool,
    voice_channel: Option<ChannelId>,
    generation: u64,
    idle_generation: u64,
}

impl PlayerState {
    fn new(volume: f32, always_on: bool, voice_channel: Option<ChannelId>) -> Self {
        Self {
            queue: VecDeque::new(),
            current: None,
            loop_mode: LoopMode::Off,
            volume,
            paused: false,
            always_on,
            voice_channel,
            generation: 0,
            idle_generation: 0,
        }
    }

    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }
}

pub struct MusicService {
    manager: Arc<Songbird>,
    database: Database,
    resolver: Resolver,
    players: DashMap<GuildId, Arc<Mutex<PlayerState>>>,
    idle_disconnect: Duration,
}

impl MusicService {
    pub fn new(
        manager: Arc<Songbird>,
        database: Database,
        resolver: Resolver,
        idle_disconnect: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            manager,
            database,
            resolver,
            players: DashMap::new(),
            idle_disconnect,
        })
    }

    async fn player(&self, guild_id: GuildId) -> Result<Arc<Mutex<PlayerState>>> {
        if let Some(player) = self.players.get(&guild_id) {
            return Ok(Arc::clone(player.value()));
        }

        let settings = self.database.get(guild_id).await?;
        let candidate = Arc::new(Mutex::new(PlayerState::new(
            settings.volume,
            settings.always_on,
            settings.voice_channel_id,
        )));

        Ok(match self.players.entry(guild_id) {
            Entry::Occupied(entry) => Arc::clone(entry.get()),
            Entry::Vacant(entry) => Arc::clone(&entry.insert(candidate)),
        })
    }

    pub async fn join(&self, guild_id: GuildId, channel_id: ChannelId) -> Result<()> {
        self.manager
            .join(guild_id, channel_id)
            .await
            .context("I couldn't join that voice channel")?;
        let player = self.player(guild_id).await?;
        let mut state = player.lock().await;
        state.voice_channel = Some(channel_id);
        state.idle_generation = state.idle_generation.wrapping_add(1);
        Ok(())
    }

    pub async fn enqueue(
        self: &Arc<Self>,
        guild_id: GuildId,
        channel_id: ChannelId,
        query: &str,
        requester: serenity::all::UserId,
    ) -> Result<Vec<QueuedTrack>> {
        let tracks = self.resolver.resolve(query, requester).await?;
        if tracks.is_empty() {
            bail!("That playlist did not contain any playable tracks.");
        }
        self.join(guild_id, channel_id).await?;

        let player = self.player(guild_id).await?;
        let should_start;
        {
            let mut state = player.lock().await;
            if state.queue.len() + tracks.len() > MAX_QUEUE_LENGTH {
                bail!("The queue is full ({MAX_QUEUE_LENGTH} tracks maximum).");
            }
            state.idle_generation = state.idle_generation.wrapping_add(1);
            state.queue.extend(tracks.iter().cloned());
            should_start = state.current.is_none();
        }

        if should_start {
            self.advance(guild_id, None, false).await?;
        }
        Ok(tracks)
    }

    async fn advance(
        self: &Arc<Self>,
        guild_id: GuildId,
        ended_generation: Option<u64>,
        force_skip: bool,
    ) -> Result<()> {
        let player = self.player(guild_id).await?;
        let (next, generation, volume, idle_generation) = {
            let mut state = player.lock().await;

            if let Some(expected) = ended_generation
                && state.current.as_ref().map(|c| c.generation) != Some(expected)
            {
                return Ok(());
            }
            if ended_generation.is_none() && state.current.is_some() {
                return Ok(());
            }

            let previous = state.current.take().map(|current| current.item);
            if !force_skip && let Some(previous) = previous {
                match state.loop_mode {
                    // Track looping is handled by Songbird on the current, cached input.
                    LoopMode::Track => {}
                    LoopMode::Queue => state.queue.push_back(previous),
                    LoopMode::Off => {}
                }
            }

            let next = state.queue.pop_front();
            state.paused = false;
            let generation = state.next_generation();
            state.idle_generation = state.idle_generation.wrapping_add(1);
            let idle_generation = state.idle_generation;
            let volume = state.volume;

            if let Some(item) = next.clone() {
                state.current = Some(CurrentTrack {
                    item,
                    handle: None,
                    generation,
                    started_at: Instant::now(),
                });
            }
            (next, generation, volume, idle_generation)
        };

        let Some(track) = next else {
            self.schedule_idle_disconnect(guild_id, idle_generation);
            return Ok(());
        };

        let call = self
            .manager
            .get(guild_id)
            .ok_or_else(|| anyhow!("I'm not connected to a voice channel."))?;
        // YouTube's media URL is resolved once, then the bytes read during playback are
        // retained in a seekable cache. Native looping can rewind this input without
        // launching yt-dlp again or downloading the track again.
        let source = self.resolver.input(&track)?;
        let input = match Memory::new(source).await {
            Ok(input) => input,
            Err(error) => {
                let mut state = player.lock().await;
                if state.current.as_ref().map(|current| current.generation) == Some(generation) {
                    state.current = None;
                }
                return Err(error).context("failed to prepare cached audio input");
            }
        };
        let mut loader = input.new_handle();
        tokio::task::spawn_blocking(move || loader.raw.load_all())
            .await
            .context("audio cache loader panicked")?;
        if input.raw.is_empty() {
            let mut state = player.lock().await;
            if state.current.as_ref().map(|current| current.generation) == Some(generation) {
                state.current = None;
            }
            bail!("yt-dlp returned an empty audio stream");
        }
        let handle = call.lock().await.play_only_input(input.into());

        // Keeping this exactly at 1.0 preserves Opus frame passthrough.
        if (volume - 1.0).abs() > f32::EPSILON {
            handle
                .set_volume(volume)
                .context("failed to set track volume")?;
        }
        handle
            .add_event(
                Event::Track(TrackEvent::End),
                TrackEnded {
                    service: Arc::downgrade(self),
                    guild_id,
                    generation,
                },
            )
            .context("failed to attach track completion handler")?;
        handle
            .add_event(
                Event::Track(TrackEvent::Error),
                TrackFailed {
                    guild_id,
                    title: track.title.clone(),
                },
            )
            .context("failed to attach track error handler")?;

        let mut state = player.lock().await;
        let should_loop_track = state.loop_mode == LoopMode::Track;
        if let Some(current) = state.current.as_mut()
            && current.generation == generation
        {
            if should_loop_track {
                handle
                    .enable_loop()
                    .context("failed to enable native track loop")?;
            }
            current.handle = Some(handle);
            return Ok(());
        }
        let _ = handle.stop();
        Ok(())
    }

    pub async fn pause(&self, guild_id: GuildId) -> Result<()> {
        let player = self.player(guild_id).await?;
        let mut state = player.lock().await;
        let current = state
            .current
            .as_ref()
            .ok_or_else(|| anyhow!("Nothing is playing."))?;
        current
            .handle
            .as_ref()
            .ok_or_else(|| anyhow!("The track is still loading."))?
            .pause()?;
        state.paused = true;
        Ok(())
    }

    pub async fn resume(&self, guild_id: GuildId) -> Result<()> {
        let player = self.player(guild_id).await?;
        let mut state = player.lock().await;
        let current = state
            .current
            .as_ref()
            .ok_or_else(|| anyhow!("Nothing is playing."))?;
        current
            .handle
            .as_ref()
            .ok_or_else(|| anyhow!("The track is still loading."))?
            .play()?;
        state.paused = false;
        Ok(())
    }

    pub async fn toggle_pause(&self, guild_id: GuildId) -> Result<bool> {
        let paused = self.snapshot(guild_id).await?.paused;
        if paused {
            self.resume(guild_id).await?;
        } else {
            self.pause(guild_id).await?;
        }
        Ok(!paused)
    }

    pub async fn skip(self: &Arc<Self>, guild_id: GuildId) -> Result<()> {
        let player = self.player(guild_id).await?;
        let (generation, handle) = {
            let state = player.lock().await;
            let current = state
                .current
                .as_ref()
                .ok_or_else(|| anyhow!("Nothing is playing."))?;
            (current.generation, current.handle.clone())
        };
        self.advance(guild_id, Some(generation), true).await?;
        if let Some(handle) = handle {
            let _ = handle.stop();
        }
        Ok(())
    }

    pub async fn stop(self: &Arc<Self>, guild_id: GuildId) -> Result<()> {
        let player = self.player(guild_id).await?;
        let (handle, idle_generation) = {
            let mut state = player.lock().await;
            state.queue.clear();
            let handle = state.current.take().and_then(|current| current.handle);
            state.paused = false;
            state.next_generation();
            state.idle_generation = state.idle_generation.wrapping_add(1);
            (handle, state.idle_generation)
        };
        if let Some(handle) = handle {
            let _ = handle.stop();
        }
        self.schedule_idle_disconnect(guild_id, idle_generation);
        Ok(())
    }

    pub async fn leave(self: &Arc<Self>, guild_id: GuildId) -> Result<()> {
        self.stop(guild_id).await?;
        self.database.set_always_on(guild_id, None, false).await?;
        if let Some(player) = self.players.get(&guild_id) {
            let mut state = player.lock().await;
            state.always_on = false;
            state.voice_channel = None;
            state.idle_generation = state.idle_generation.wrapping_add(1);
        }
        self.manager.remove(guild_id).await?;
        Ok(())
    }

    pub async fn seek(&self, guild_id: GuildId, position: Duration) -> Result<()> {
        let player = self.player(guild_id).await?;
        let handle = player
            .lock()
            .await
            .current
            .as_ref()
            .and_then(|current| current.handle.clone())
            .ok_or_else(|| anyhow!("Nothing is playing."))?;
        handle.seek_async(position).await?;
        Ok(())
    }

    pub async fn set_volume(&self, guild_id: GuildId, volume: f32) -> Result<()> {
        let volume = volume.clamp(0.0, 2.0);
        let player = self.player(guild_id).await?;
        {
            let mut state = player.lock().await;
            state.volume = volume;
            if let Some(handle) = state
                .current
                .as_ref()
                .and_then(|current| current.handle.as_ref())
            {
                handle.set_volume(volume)?;
            }
        }
        self.database.set_volume(guild_id, volume).await?;
        Ok(())
    }

    pub async fn set_loop(&self, guild_id: GuildId, loop_mode: LoopMode) -> Result<()> {
        let player = self.player(guild_id).await?;
        let mut state = player.lock().await;
        state.loop_mode = loop_mode;
        if let Some(handle) = state
            .current
            .as_ref()
            .and_then(|current| current.handle.as_ref())
        {
            set_native_track_loop(handle, loop_mode)?;
        }
        Ok(())
    }

    pub async fn cycle_loop(&self, guild_id: GuildId) -> Result<LoopMode> {
        let player = self.player(guild_id).await?;
        let mut state = player.lock().await;
        state.loop_mode = state.loop_mode.next();
        if let Some(handle) = state
            .current
            .as_ref()
            .and_then(|current| current.handle.as_ref())
        {
            set_native_track_loop(handle, state.loop_mode)?;
        }
        Ok(state.loop_mode)
    }

    pub async fn shuffle(&self, guild_id: GuildId) -> Result<usize> {
        let player = self.player(guild_id).await?;
        let mut state = player.lock().await;
        let mut items: Vec<_> = state.queue.drain(..).collect();
        items.shuffle(&mut rand::rng());
        let len = items.len();
        state.queue.extend(items);
        Ok(len)
    }

    pub async fn remove(&self, guild_id: GuildId, position: usize) -> Result<QueuedTrack> {
        if position == 0 {
            bail!("Queue positions start at 1.");
        }
        self.player(guild_id)
            .await?
            .lock()
            .await
            .queue
            .remove(position - 1)
            .ok_or_else(|| anyhow!("There is no track at queue position {position}."))
    }

    pub async fn clear_queue(&self, guild_id: GuildId) -> Result<usize> {
        let player = self.player(guild_id).await?;
        let mut state = player.lock().await;
        let len = state.queue.len();
        state.queue.clear();
        Ok(len)
    }

    pub async fn move_track(&self, guild_id: GuildId, from: usize, to: usize) -> Result<()> {
        if from == 0 || to == 0 {
            bail!("Queue positions start at 1.");
        }
        let player = self.player(guild_id).await?;
        let mut state = player.lock().await;
        if from > state.queue.len() || to > state.queue.len() {
            bail!(
                "Both positions must be within the {} queued tracks.",
                state.queue.len()
            );
        }
        let item = state.queue.remove(from - 1).expect("position checked");
        state.queue.insert(to - 1, item);
        Ok(())
    }

    pub async fn snapshot(&self, guild_id: GuildId) -> Result<PlayerSnapshot> {
        let player = self.player(guild_id).await?;
        let state = player.lock().await;
        let handle = state
            .current
            .as_ref()
            .and_then(|current| current.handle.clone());
        let fallback_position = state
            .current
            .as_ref()
            .map(|current| current.started_at.elapsed())
            .unwrap_or_default();
        let mut snapshot = PlayerSnapshot {
            current: state.current.as_ref().map(|current| current.item.clone()),
            upcoming: state.queue.iter().cloned().collect(),
            loop_mode: state.loop_mode,
            volume: state.volume,
            paused: state.paused,
            always_on: state.always_on,
            position: fallback_position,
        };
        drop(state);
        if let Some(handle) = handle
            && let Ok(info) = handle.get_info().await
        {
            snapshot.position = info.play_time;
        }
        Ok(snapshot)
    }

    pub async fn set_always_on(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
        enabled: bool,
    ) -> Result<()> {
        if enabled {
            self.join(guild_id, channel_id).await?;
        }
        self.database
            .set_always_on(guild_id, Some(channel_id), enabled)
            .await?;
        let player = self.player(guild_id).await?;
        let mut state = player.lock().await;
        state.always_on = enabled;
        state.voice_channel = Some(channel_id);
        state.idle_generation = state.idle_generation.wrapping_add(1);
        Ok(())
    }

    pub async fn disable_always_on(self: &Arc<Self>, guild_id: GuildId) -> Result<()> {
        self.database.set_always_on(guild_id, None, false).await?;
        let player = self.player(guild_id).await?;
        let mut state = player.lock().await;
        state.always_on = false;
        state.idle_generation = state.idle_generation.wrapping_add(1);
        let should_schedule = state.current.is_none();
        let idle_generation = state.idle_generation;
        drop(state);
        if should_schedule {
            self.schedule_idle_disconnect(guild_id, idle_generation);
        }
        Ok(())
    }

    pub async fn restore_always_on(&self) -> Result<()> {
        for settings in self.database.enabled_guilds().await? {
            let Some(channel_id) = settings.voice_channel_id else {
                continue;
            };
            match self.join(settings.guild_id, channel_id).await {
                Ok(()) => {
                    info!(guild_id = %settings.guild_id, channel_id = %channel_id, "restored 24/7 voice connection")
                }
                Err(error) => {
                    warn!(guild_id = %settings.guild_id, %error, "failed to restore 24/7 voice connection")
                }
            }
        }
        Ok(())
    }

    pub async fn restore_guild_if_enabled(&self, guild_id: GuildId) -> Result<()> {
        let settings = self.database.get(guild_id).await?;
        if settings.always_on
            && let Some(channel_id) = settings.voice_channel_id
        {
            self.join(guild_id, channel_id).await?;
        }
        Ok(())
    }

    fn schedule_idle_disconnect(self: &Arc<Self>, guild_id: GuildId, idle_generation: u64) {
        let service = Arc::downgrade(self);
        let delay = self.idle_disconnect;
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let Some(service) = service.upgrade() else {
                return;
            };
            let Ok(player) = service.player(guild_id).await else {
                return;
            };
            let should_leave = {
                let state = player.lock().await;
                state.idle_generation == idle_generation
                    && state.current.is_none()
                    && !state.always_on
            };
            if should_leave {
                if let Err(error) = service.manager.remove(guild_id).await {
                    warn!(guild_id = %guild_id, %error, "idle disconnect failed");
                } else {
                    info!(guild_id = %guild_id, "left idle voice connection");
                }
            }
        });
    }
}

fn set_native_track_loop(handle: &TrackHandle, loop_mode: LoopMode) -> Result<()> {
    if loop_mode == LoopMode::Track {
        handle.enable_loop()?;
    } else {
        handle.disable_loop()?;
    }
    Ok(())
}

struct TrackFailed {
    guild_id: GuildId,
    title: String,
}

#[async_trait]
impl EventHandler for TrackFailed {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(tracks) = ctx
            && let Some((state, _)) = tracks.first()
            && let PlayMode::Errored(error) = &state.playing
        {
            warn!(
                guild_id = %self.guild_id,
                title = %self.title,
                ?error,
                "audio track failed"
            );
        }
        Some(Event::Cancel)
    }
}

struct TrackEnded {
    service: Weak<MusicService>,
    guild_id: GuildId,
    generation: u64,
}

#[async_trait]
impl EventHandler for TrackEnded {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        if let Some(service) = self.service.upgrade()
            && let Err(error) = service
                .advance(self.guild_id, Some(self.generation), false)
                .await
        {
            warn!(guild_id = %self.guild_id, %error, "failed to advance music queue");
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_modes_cycle_in_expected_order() {
        assert_eq!(LoopMode::Off.next(), LoopMode::Track);
        assert_eq!(LoopMode::Track.next(), LoopMode::Queue);
        assert_eq!(LoopMode::Queue.next(), LoopMode::Off);
    }
}
