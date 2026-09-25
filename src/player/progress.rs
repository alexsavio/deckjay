//! When an item played with `progress: true` reports how far it got
//! ([`PlayerEvent::Progress`]). The UI saves every report to disk (an SD card
//! on the Pi), so a playing item reports at most every [`INTERVAL`]. A track
//! change, a pause, a stop, a failure and the next item report at once.
//! Items that did not ask for progress report nothing.

use std::time::{Duration, Instant};

use super::{PlayerEvent, Start};
use crate::library::ItemId;

/// The shortest time between two reports while the place moves on by itself.
pub(super) const INTERVAL: Duration = Duration::from_secs(5);
/// A last track that stops this close to its length has ended; earlier, it
/// was stopped. Speakers tell the place every few seconds, so the last place
/// seen can be that far from the end.
pub(super) const END_MARGIN: Duration = Duration::from_secs(10);

/// A place in an item: `track` indexes its tracks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Place {
    pub(super) track: usize,
    pub(super) position: Duration,
    /// The track's length, when the speaker or the file tells it.
    pub(super) duration: Option<Duration>,
}

impl Place {
    /// The start of `track`, length unknown.
    pub(super) fn start_of(track: usize, position: Duration) -> Place {
        Place {
            track,
            position,
            duration: None,
        }
    }

    fn same_spot(self, other: Place) -> bool {
        self.track == other.track && self.position == other.position
    }
}

pub(super) struct Policy {
    pub(super) interval: Duration,
    /// The item that asked for progress; `None` while the item did not.
    item: Option<ItemId>,
    /// The latest place the backend saw.
    latest: Option<Place>,
    /// The place reported last, and when.
    sent: Option<(Place, Instant)>,
}

impl Policy {
    pub(super) fn new() -> Policy {
        Policy {
            interval: INTERVAL,
            item: None,
            latest: None,
            sent: None,
        }
    }

    /// `item` replaces the item playing; returns the old item's latest place
    /// if it was not reported yet. Only a `wanted` item gets reports.
    pub(super) fn begin(
        &mut self,
        item: ItemId,
        wanted: bool,
        now: Instant,
    ) -> Option<PlayerEvent> {
        let old = self.flush(now);
        self.item = wanted.then_some(item);
        self.latest = None;
        self.sent = None;
        old
    }

    /// The item is at `at`. A `jump` (a track started, restarted or moved to
    /// another spot) and a new track report at once; otherwise a new spot
    /// waits for [`INTERVAL`] after the last report.
    pub(super) fn offer(&mut self, at: Place, jump: bool, now: Instant) -> Option<PlayerEvent> {
        self.item?;
        self.latest = Some(at);
        let due = match self.sent {
            None => true,
            Some((sent, _)) if sent.same_spot(at) => false,
            Some((sent, when)) => {
                jump || sent.track != at.track || now.duration_since(when) >= self.interval
            }
        };
        if due { self.send(at, now) } else { None }
    }

    /// The latest place, unless it was reported already: for a pause, a stop
    /// or a failure.
    pub(super) fn flush(&mut self, now: Instant) -> Option<PlayerEvent> {
        let latest = self.latest?;
        if self.sent.is_some_and(|(sent, _)| sent.same_spot(latest)) {
            return None;
        }
        self.send(latest, now)
    }

    /// The item's last track played to its end: there is nothing to resume,
    /// so the `Stopped` that follows reports no place.
    pub(super) fn finish(&mut self) -> Option<PlayerEvent> {
        let item = self.item.take()?;
        self.latest = None;
        self.sent = None;
        Some(PlayerEvent::Finished(item))
    }

    /// Where the item would go on: its latest place, if it asked for progress.
    pub(super) fn resume_point(&self) -> Option<Start> {
        self.item?;
        self.latest.map(|at| Start {
            track: at.track,
            position: at.position,
        })
    }

    pub(super) fn wanted(&self) -> bool {
        self.item.is_some()
    }

    fn send(&mut self, at: Place, now: Instant) -> Option<PlayerEvent> {
        let item = self.item?;
        self.sent = Some((at, now));
        Some(PlayerEvent::Progress {
            item,
            track: at.track,
            position: at.position,
            duration: at.duration,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOK: ItemId = ItemId(7);
    const SONGS: ItemId = ItemId(8);

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    fn at(track: usize, position: f64) -> Place {
        Place {
            track,
            position: secs(position),
            duration: Some(secs(600.0)),
        }
    }

    fn report(item: ItemId, place: Place) -> PlayerEvent {
        PlayerEvent::Progress {
            item,
            track: place.track,
            position: place.position,
            duration: place.duration,
        }
    }

    fn book(t0: Instant) -> Policy {
        let mut policy = Policy::new();
        assert_eq!(policy.begin(BOOK, true, t0), None);
        policy
    }

    #[test]
    fn an_item_without_progress_reports_nothing() {
        let t0 = Instant::now();
        let mut policy = Policy::new();
        policy.begin(SONGS, false, t0);
        assert_eq!(policy.offer(at(0, 0.0), true, t0), None);
        assert_eq!(policy.offer(at(1, 0.0), false, t0 + secs(60.0)), None);
        assert_eq!(policy.flush(t0 + secs(61.0)), None);
        assert_eq!(policy.finish(), None);
        assert_eq!(policy.resume_point(), None);
    }

    #[test]
    fn the_first_place_and_a_new_track_report_at_once() {
        let t0 = Instant::now();
        let mut policy = book(t0);
        assert_eq!(
            policy.offer(at(2, 30.0), false, t0),
            Some(report(BOOK, at(2, 30.0)))
        );
        let later = t0 + secs(0.1);
        assert_eq!(
            policy.offer(at(3, 0.0), false, later),
            Some(report(BOOK, at(3, 0.0)))
        );
    }

    #[test]
    fn a_playing_item_reports_at_most_every_interval() {
        let t0 = Instant::now();
        let mut policy = book(t0);
        policy.offer(at(0, 10.0), false, t0);
        assert_eq!(policy.offer(at(0, 11.0), false, t0 + secs(1.0)), None);
        assert_eq!(policy.offer(at(0, 14.9), false, t0 + secs(4.9)), None);
        assert_eq!(
            policy.offer(at(0, 15.0), false, t0 + secs(5.0)),
            Some(report(BOOK, at(0, 15.0)))
        );
        assert_eq!(policy.offer(at(0, 19.0), false, t0 + secs(9.0)), None);
    }

    #[test]
    fn a_place_that_does_not_move_is_reported_once() {
        let t0 = Instant::now();
        let mut policy = book(t0);
        policy.offer(at(0, 10.0), false, t0);
        assert_eq!(policy.offer(at(0, 10.0), false, t0 + secs(60.0)), None);
        assert_eq!(policy.offer(at(0, 10.0), true, t0 + secs(61.0)), None);
        assert_eq!(policy.flush(t0 + secs(62.0)), None);
    }

    #[test]
    fn a_jump_inside_the_track_reports_at_once() {
        let t0 = Instant::now();
        let mut policy = book(t0);
        policy.offer(at(1, 40.0), false, t0);
        let restart = t0 + secs(1.0);
        assert_eq!(
            policy.offer(at(1, 0.0), true, restart),
            Some(report(BOOK, at(1, 0.0)))
        );
    }

    #[test]
    fn pause_stop_and_failure_report_the_latest_place_at_once() {
        let t0 = Instant::now();
        let mut policy = book(t0);
        policy.offer(at(0, 10.0), false, t0);
        policy.offer(at(0, 12.0), false, t0 + secs(2.0));
        assert_eq!(
            policy.flush(t0 + secs(2.1)),
            Some(report(BOOK, at(0, 12.0)))
        );
        assert_eq!(policy.flush(t0 + secs(2.2)), None, "reported already");
        assert_eq!(
            policy.resume_point(),
            Some(Start {
                track: 0,
                position: secs(12.0)
            })
        );
    }

    #[test]
    fn the_next_item_reports_the_old_items_latest_place_first() {
        let t0 = Instant::now();
        let mut policy = book(t0);
        policy.offer(at(4, 100.0), false, t0);
        policy.offer(at(4, 103.0), false, t0 + secs(3.0));
        assert_eq!(
            policy.begin(SONGS, true, t0 + secs(3.5)),
            Some(report(BOOK, at(4, 103.0)))
        );
        assert_eq!(policy.resume_point(), None);
        assert_eq!(
            policy.offer(at(0, 0.0), true, t0 + secs(3.6)),
            Some(report(SONGS, at(0, 0.0)))
        );

        let mut policy = book(t0);
        policy.offer(at(4, 100.0), false, t0);
        assert_eq!(
            policy.begin(SONGS, false, t0 + secs(1.0)),
            None,
            "sent already"
        );
    }

    #[test]
    fn finished_comes_only_from_finish_and_leaves_nothing_to_report() {
        let t0 = Instant::now();
        let mut policy = book(t0);
        policy.offer(at(2, 590.0), false, t0);
        policy.offer(at(2, 593.0), false, t0 + secs(3.0));
        assert_eq!(policy.finish(), Some(PlayerEvent::Finished(BOOK)));
        assert_eq!(policy.flush(t0 + secs(4.0)), None);
        assert_eq!(policy.offer(at(0, 0.0), true, t0 + secs(5.0)), None);
        assert_eq!(policy.finish(), None);
        assert_eq!(policy.resume_point(), None);
    }
}
