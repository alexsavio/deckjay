//! Decides per feed which episodes to download, show and delete. No IO: the
//! refresh calls [`plan`] before the downloads (for `download`) and again
//! after them (for `publish` and `drop`), so a failed download simply leaves
//! an older episode in its place.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::feed::RemoteEpisode;

/// An episode file in the cache, as the manifest records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    pub id: String,
    pub title: String,
    /// Unix seconds.
    pub published: Option<i64>,
    /// File name inside the feed's folder.
    pub file: String,
    pub content_type: String,
    pub bytes: u64,
    /// Picture file name inside the feed's folder.
    #[serde(default)]
    pub picture: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Newest first.
    pub download: Vec<RemoteEpisode>,
    /// Newest first.
    pub publish: Vec<Stored>,
    pub drop: Vec<Stored>,
}

/// `feed` is `None` when the fetch failed; an empty feed counts as failed
/// too, so a broken feed never empties the shelf. `pinned` is the id of the
/// episode that is playing: it is never dropped.
pub fn plan(
    feed: Option<&[RemoteEpisode]>,
    on_disk: &[Stored],
    keep: usize,
    pinned: Option<&str>,
) -> Plan {
    let Some(feed) = feed.filter(|feed| !feed.is_empty()) else {
        let publish = ranked(&[], on_disk)
            .into_iter()
            .filter_map(|c| c.stored.cloned())
            .collect();
        return Plan {
            publish,
            ..Plan::default()
        };
    };
    let ranked = ranked(feed, on_disk);
    let download = ranked
        .iter()
        .take(keep)
        .filter(|c| c.stored.is_none())
        .filter_map(|c| c.remote.cloned())
        .collect();
    let publish: Vec<Stored> = ranked
        .iter()
        .filter_map(|c| c.stored)
        .take(keep)
        .cloned()
        .collect();
    let published: HashSet<&str> = publish.iter().map(|s| s.id.as_str()).collect();
    let drop = on_disk
        .iter()
        .filter(|s| !published.contains(s.id.as_str()) && pinned != Some(s.id.as_str()))
        .cloned()
        .collect();
    Plan {
        download,
        publish,
        drop,
    }
}

/// Sizes of the newest `keep` episodes of feed and disk, newest first: the
/// file size when on disk, else the enclosure's `length`. Many feeds leave
/// that out; the biggest episode on disk stands in for it, so an episode the
/// budget refuses after downloading it is not fetched again on every refresh.
/// After a failed fetch this is the disk.
pub fn wanted_sizes(feed: Option<&[RemoteEpisode]>, on_disk: &[Stored], keep: usize) -> Vec<u64> {
    let feed = feed.filter(|feed| !feed.is_empty());
    let keep = if feed.is_some() { keep } else { usize::MAX };
    let guess = on_disk.iter().map(|s| s.bytes).max().unwrap_or(0);
    ranked(feed.unwrap_or_default(), on_disk)
        .iter()
        .take(keep)
        .map(|c| match (c.stored, c.remote) {
            (Some(stored), _) => stored.bytes,
            (None, Some(remote)) => remote.length.unwrap_or(guess),
            (None, None) => 0,
        })
        .collect()
}

/// How many of each feed's episodes (sizes newest first) fit in `max_bytes`:
/// every feed's newest, then every feed's second newest, and so on, so one
/// big feed cannot starve the others. A feed stops at its first episode that
/// does not fit, so it always keeps its newest ones.
pub fn fit_budget(feeds: &[Vec<u64>], max_bytes: u64) -> Vec<usize> {
    let mut counts = vec![0; feeds.len()];
    let mut stopped = vec![false; feeds.len()];
    let mut total: u64 = 0;
    let deepest = feeds.iter().map(Vec::len).max().unwrap_or(0);
    for rank in 0..deepest {
        for (feed, sizes) in feeds.iter().enumerate() {
            let Some(&size) = sizes.get(rank) else {
                continue;
            };
            if stopped[feed] {
                continue;
            }
            match total.checked_add(size).filter(|&sum| sum <= max_bytes) {
                Some(sum) => {
                    total = sum;
                    counts[feed] += 1;
                }
                None => stopped[feed] = true,
            }
        }
    }
    counts
}

struct Candidate<'a> {
    published: Option<i64>,
    remote: Option<&'a RemoteEpisode>,
    stored: Option<&'a Stored>,
}

/// Feed and disk together, newest first. Episodes that left the feed go after
/// the feed's own, unless every date is known.
fn ranked<'a>(feed: &'a [RemoteEpisode], on_disk: &'a [Stored]) -> Vec<Candidate<'a>> {
    let stored: HashMap<&str, &Stored> = on_disk.iter().map(|s| (s.id.as_str(), s)).collect();
    let in_feed: HashSet<&str> = feed.iter().map(|r| r.id.as_str()).collect();
    let mut list: Vec<Candidate> = feed
        .iter()
        .map(|remote| Candidate {
            published: remote.published,
            remote: Some(remote),
            stored: stored.get(remote.id.as_str()).copied(),
        })
        .collect();
    let mut gone: Vec<Candidate> = on_disk
        .iter()
        .filter(|s| !in_feed.contains(s.id.as_str()))
        .map(|s| Candidate {
            published: s.published,
            remote: None,
            stored: Some(s),
        })
        .collect();
    gone.sort_by_key(|c| Reverse(c.published));
    list.extend(gone);
    if list.iter().all(|c| c.published.is_some()) {
        list.sort_by_key(|c| Reverse(c.published));
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(n: i64) -> RemoteEpisode {
        RemoteEpisode {
            id: format!("id{n}"),
            title: format!("Episode {n}"),
            published: Some(n * 1000),
            audio_url: format!("https://example.org/{n}.mp3"),
            length: Some(10),
            mime: "audio/mpeg",
            ext: "mp3",
            picture_url: None,
        }
    }

    fn stored(n: i64) -> Stored {
        Stored {
            id: format!("id{n}"),
            title: format!("Episode {n}"),
            published: Some(n * 1000),
            file: format!("{n}.mp3"),
            content_type: "audio/mpeg".into(),
            bytes: 100,
            picture: None,
        }
    }

    fn feed(ns: &[i64]) -> Vec<RemoteEpisode> {
        ns.iter().copied().map(remote).collect()
    }

    fn disk(ns: &[i64]) -> Vec<Stored> {
        ns.iter().copied().map(stored).collect()
    }

    fn ids<'a, T: 'a>(items: impl IntoIterator<Item = &'a T>, id: fn(&T) -> &str) -> Vec<String> {
        items.into_iter().map(|t| id(t).to_string()).collect()
    }

    fn summary(plan: &Plan) -> (Vec<String>, Vec<String>, Vec<String>) {
        (
            ids(&plan.download, |r| &r.id),
            ids(&plan.publish, |s| &s.id),
            ids(&plan.drop, |s| &s.id),
        )
    }

    fn names(ns: &[i64]) -> Vec<String> {
        ns.iter().map(|n| format!("id{n}")).collect()
    }

    #[test]
    fn keeps_the_newest_n() {
        let feed = feed(&[5, 4, 3, 2, 1]);
        let before = plan(Some(&feed), &disk(&[3, 2, 1]), 2, None);
        assert_eq!(names(&[5, 4]), summary(&before).0);

        let after = plan(Some(&feed), &disk(&[3, 2, 1, 5, 4]), 2, None);
        assert_eq!(summary(&after), (vec![], names(&[5, 4]), names(&[3, 2, 1])));
    }

    #[test]
    fn ranks_by_date_even_when_the_feed_is_unsorted() {
        let feed = feed(&[1, 3, 2]);
        let plan = plan(Some(&feed), &[], 2, None);
        assert_eq!(summary(&plan).0, names(&[3, 2]));
    }

    #[test]
    fn a_failed_feed_keeps_every_episode() {
        let plan = plan(None, &disk(&[1, 3, 2]), 1, None);
        assert_eq!(summary(&plan), (vec![], names(&[3, 2, 1]), vec![]));
    }

    #[test]
    fn an_empty_feed_counts_as_failed() {
        let plan = plan(Some(&[]), &disk(&[2, 1]), 1, None);
        assert_eq!(summary(&plan), (vec![], names(&[2, 1]), vec![]));
    }

    #[test]
    fn a_failed_newest_download_keeps_the_older_episode() {
        let feed = feed(&[3, 2, 1]);
        let before = plan(Some(&feed), &disk(&[2, 1]), 2, None);
        assert_eq!(summary(&before).0, names(&[3]));

        // Episode 3 did not arrive: 2 and 1 stay, nothing is dropped.
        let after = plan(Some(&feed), &disk(&[2, 1]), 2, None);
        assert_eq!(summary(&after).1, names(&[2, 1]));
        assert_eq!(after.drop, []);
    }

    #[test]
    fn the_pinned_episode_is_never_dropped() {
        let feed = feed(&[3, 2, 1]);
        let plan = plan(Some(&feed), &disk(&[3, 2, 1]), 1, Some("id2"));
        assert_eq!(summary(&plan), (vec![], names(&[3]), names(&[1])));
    }

    #[test]
    fn episodes_that_left_the_feed_still_rank_by_date() {
        let feed = feed(&[5, 3]);
        let after = plan(Some(&feed), &disk(&[5, 4, 3]), 2, None);
        assert_eq!(summary(&after).1, names(&[5, 4]));
        assert_eq!(summary(&after).2, names(&[3]));
    }

    #[test]
    fn without_dates_episodes_that_left_the_feed_come_last() {
        let mut feed = feed(&[1, 2]);
        feed[0].published = None;
        let after = plan(Some(&feed), &disk(&[9, 1]), 2, None);
        assert_eq!(summary(&after).1, names(&[1, 9]));
        assert_eq!(summary(&after).0, names(&[2]));
    }

    #[test]
    fn sizes_use_disk_then_enclosure_length() {
        let mut feed = feed(&[3, 2, 1]);
        feed[0].length = None;
        assert_eq!(wanted_sizes(Some(&feed), &[], 2), [0, 10]);
        let mut on_disk = disk(&[2, 1]);
        on_disk[1].bytes = 300;
        assert_eq!(wanted_sizes(Some(&feed), &on_disk, 2), [300, 100]);
        assert_eq!(wanted_sizes(None, &on_disk, 1), [100, 300]);
    }

    #[test]
    fn budget_goes_round_robin_by_rank() {
        let feeds = [vec![5, 5, 5], vec![5, 5], vec![20]];
        assert_eq!(fit_budget(&feeds, 17), [2, 1, 0]);
        assert_eq!(fit_budget(&feeds, 100), [3, 2, 1]);
        assert_eq!(fit_budget(&feeds, 0), [0, 0, 0]);
    }

    #[test]
    fn a_feed_stops_at_its_first_episode_that_does_not_fit() {
        let feeds = [vec![1, 50, 1], vec![1, 1, 1]];
        assert_eq!(fit_budget(&feeds, 10), [1, 3]);
    }

    #[test]
    fn budget_survives_absurd_sizes() {
        assert_eq!(fit_budget(&[vec![u64::MAX, 1], vec![1]], u64::MAX), [1, 0]);
    }
}
