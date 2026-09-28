//! Tells systemd how the service is doing (`sd_notify`): `READY=1` when the
//! player runs, `WATCHDOG=1` heartbeats while it runs, `STOPPING=1` on the
//! way out. Outside systemd (no `NOTIFY_SOCKET`) every call does nothing.

use std::time::{Duration, Instant};

use sd_notify::NotifyState;
use tracing::{info, warn};

pub fn ready() {
    send(NotifyState::Ready);
}

pub fn stopping() {
    send(NotifyState::Stopping);
}

fn send(state: NotifyState) {
    if let Err(err) = sd_notify::notify(&[state]) {
        warn!("cannot notify systemd: {err}");
    }
}

/// Sends `WATCHDOG=1` twice per `WatchdogSec=`, as systemd asks; when a whole
/// period passes without one, systemd kills and restarts the service. Off
/// (`beat` does nothing) when systemd set no watchdog.
pub struct Watchdog {
    every: Option<Duration>,
    last: Instant,
}

impl Watchdog {
    pub fn from_env() -> Self {
        let every = sd_notify::watchdog_enabled().map(|period| period / 2);
        if let Some(every) = every {
            info!("systemd watchdog on: heartbeat every {every:?}");
        }
        Self::new(every)
    }

    pub fn off() -> Self {
        Self::new(None)
    }

    fn new(every: Option<Duration>) -> Self {
        Self {
            every,
            last: Instant::now(),
        }
    }

    pub fn beat(&mut self) {
        let now = Instant::now();
        if self.due(now) {
            self.last = now;
            send(NotifyState::Watchdog);
        }
    }

    fn due(&self, now: Instant) -> bool {
        self.every
            .is_some_and(|every| now.duration_since(self.last) >= every)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_is_never_due() {
        let watchdog = Watchdog::off();
        assert!(!watchdog.due(Instant::now() + Duration::from_secs(3600)));
    }

    #[test]
    fn due_after_half_a_period() {
        let watchdog = Watchdog::new(Some(Duration::from_secs(30)));
        assert!(!watchdog.due(watchdog.last + Duration::from_secs(29)));
        assert!(watchdog.due(watchdog.last + Duration::from_secs(30)));
    }
}
