//! Spotify playlists, played by remote-controlling a Spotify Connect device
//! through the Web API ([`crate::spotify::api`]). The audio never passes
//! through deckjay: Spotify streams it to the device.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use super::{Content, Emitter, PlayerCmd, PlayerEvent, Playlist, Speaker, clamp_volume};
use crate::library::ItemId;
use crate::spotify::api::{ApiError, Client, Endpoints};
use crate::spotify::pick_device;

/// Spotify limits requests per app, so polls are slow.
const POLL_PLAYING: Duration = Duration::from_secs(5);
const POLL_PAUSED: Duration = Duration::from_secs(15);

/// Where Spotify plays: the saved login and part of the device's name.
#[derive(Debug, Clone)]
pub struct Connect {
    pub state_dir: PathBuf,
    pub device: String,
}

/// The playlist that plays, and where.
struct Now {
    item: ItemId,
    uri: String,
    device_id: String,
    playing: bool,
}

pub(super) struct SpotifyPlayer {
    connect: Connect,
    endpoints: Endpoints,
    /// Loaded at the first command, so a sign-in after the start counts.
    client: Option<Client>,
    now: Option<Now>,
}

impl SpotifyPlayer {
    pub(super) fn new(connect: Connect, endpoints: Endpoints) -> SpotifyPlayer {
        SpotifyPlayer {
            connect,
            endpoints,
            client: None,
            now: None,
        }
    }

    fn client(&mut self) -> Result<&mut Client> {
        if self.client.is_none() {
            self.client = Some(Client::load(
                &self.connect.state_dir,
                self.endpoints.clone(),
            )?);
        }
        self.client.as_mut().context("no Spotify client")
    }

    fn play(
        &mut self,
        item: ItemId,
        playlist: Playlist,
        volume: f32,
        events: &mut Emitter,
    ) -> Result<()> {
        let Playlist { uri, name } = playlist;
        events.begin(item, false);
        let wanted = self.connect.device.clone();
        let client = self.client()?;
        let devices = client.devices()?;
        let device = pick_device(&devices, &wanted)?.clone();
        let id = device.id.clone().context("the Spotify device has no id")?;
        if !device.is_active {
            client.transfer(&id, false)?;
        }
        if device.supports_volume
            && let Err(err) = client.set_volume(&id, percent(volume))
        {
            warn!("cannot set the Spotify volume: {err:#}");
        }
        if let Err(err) = client.play(&id, Some(&uri)) {
            // The device went idle between the transfer and the play.
            if status(&err) != Some(404) {
                return Err(err);
            }
            client.transfer(&id, false)?;
            client.play(&id, Some(&uri))?;
        }
        info!(playlist = %name, device = %device.name, "playing on Spotify");
        self.now = Some(Now {
            item,
            uri,
            device_id: id,
            playing: true,
        });
        events.emit(PlayerEvent::Playing(item));
        Ok(())
    }

    fn toggle(&mut self, events: &mut Emitter) -> Result<()> {
        let Some(now) = &self.now else {
            return Ok(());
        };
        let (item, id, playing) = (now.item, now.device_id.clone(), now.playing);
        let client = self.client()?;
        if playing {
            client.pause(&id)?;
        } else {
            client.play(&id, None)?;
        }
        if let Some(now) = &mut self.now {
            now.playing = !playing;
        }
        events.emit(if playing {
            PlayerEvent::Paused(item)
        } else {
            PlayerEvent::Playing(item)
        });
        Ok(())
    }

    fn device_id(&self) -> Option<String> {
        self.now.as_ref().map(|now| now.device_id.clone())
    }
}

impl Speaker for SpotifyPlayer {
    fn handle(&mut self, cmd: PlayerCmd, events: &mut Emitter) -> Result<()> {
        match cmd {
            PlayerCmd::Play {
                item,
                content: Content::Spotify(playlist),
                volume,
            } => self.play(item, playlist, volume, events),
            PlayerCmd::Play { .. } => bail!("Spotify plays playlists only"),
            PlayerCmd::TogglePause => self.toggle(events),
            PlayerCmd::Off => Speaker::stop(self, events),
            PlayerCmd::Next => match self.device_id() {
                Some(id) => self.client()?.next(&id),
                None => Ok(()),
            },
            PlayerCmd::Prev => match self.device_id() {
                Some(id) => self.client()?.previous(&id),
                None => Ok(()),
            },
            PlayerCmd::Seek(_) => Ok(()),
            PlayerCmd::SetVolume(volume) => {
                if let Some(id) = self.device_id()
                    && let Err(err) = self.client()?.set_volume(&id, percent(volume))
                {
                    // Some devices refuse remote volume; playing on is better.
                    warn!("cannot set the Spotify volume: {err:#}");
                }
                Ok(())
            }
        }
    }

    /// Our playlist on our device is ours; anything else means someone took
    /// over in the Spotify app.
    fn poll(&mut self, events: &mut Emitter) -> Result<()> {
        let Some(now) = &self.now else {
            return Ok(());
        };
        let (item, uri, id) = (now.item, now.uri.clone(), now.device_id.clone());
        let state = self.client()?.player()?;
        match state {
            Some(state)
                if state.device_id.as_deref() == Some(&id)
                    && state.context_uri.as_deref() == Some(&uri) =>
            {
                if let Some(now) = &mut self.now {
                    now.playing = state.is_playing;
                }
                events.emit(if state.is_playing {
                    PlayerEvent::Playing(item)
                } else {
                    PlayerEvent::Paused(item)
                });
            }
            _ => {
                info!("Spotify plays something else now");
                self.now = None;
                events.emit(PlayerEvent::Stopped);
            }
        }
        Ok(())
    }

    fn poll_interval(&self) -> Option<Duration> {
        self.now.as_ref().map(|now| {
            if now.playing {
                POLL_PLAYING
            } else {
                POLL_PAUSED
            }
        })
    }

    fn reset(&mut self) {
        self.now = None;
    }

    /// Pauses the configured device whatever it plays, e.g. a playlist that
    /// an earlier run of deckjay started.
    fn stop_everything(&mut self, events: &mut Emitter) -> Result<()> {
        self.stop(events)?;
        let wanted = self.connect.device.clone();
        let client = self.client()?;
        let Some(state) = client.player()? else {
            return Ok(());
        };
        if !state.is_playing {
            return Ok(());
        }
        let devices = client.devices()?;
        if let Ok(device) = pick_device(&devices, &wanted)
            && device.id.is_some()
            && device.id == state.device_id
            && let Some(id) = &device.id
        {
            client.pause(id)?;
        }
        Ok(())
    }

    fn stop(&mut self, _events: &mut Emitter) -> Result<()> {
        if let Some(now) = self.now.take()
            && now.playing
            && let Err(err) = self.client()?.pause(&now.device_id)
        {
            warn!("cannot pause Spotify: {err:#}");
        }
        Ok(())
    }
}

/// The Web API takes whole percents.
fn percent(volume: f32) -> u8 {
    (clamp_volume(volume) * 100.0).round() as u8
}

fn status(err: &anyhow::Error) -> Option<u16> {
    err.downcast_ref::<ApiError>().map(|err| err.status)
}

#[cfg(test)]
mod tests;
