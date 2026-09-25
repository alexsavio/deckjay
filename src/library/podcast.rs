//! Podcast sources as shelves: the settings of their `podcasts` thread, and
//! their cached episodes as items.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::{Item, ItemKey, Kind, Media, Track, scan};
use crate::config::{FeedOrder, Source};
use crate::podcasts::{FeedSettings, Order, Settings, Snapshot};

/// The longest cache folder name the podcasts thread accepts.
const MAX_SLUG: usize = 64;

/// `None` unless `source` is a podcast source.
pub fn settings(source: &Source) -> Option<Settings> {
    let podcast = source.podcast.as_ref()?;
    let mut slugs = HashSet::new();
    let feeds = podcast
        .feeds
        .iter()
        .map(|feed| FeedSettings {
            slug: unique(&slug(&feed.name), &mut slugs),
            name: feed.name.clone(),
            url: feed.url.clone(),
            keep: feed.keep,
            order: match feed.order {
                FeedOrder::Newest => Order::NewestFirst,
                FeedOrder::Oldest => Order::OldestFirst,
            },
        })
        .collect();
    Some(Settings {
        cache_dir: source.path.clone(),
        keep: podcast.keep,
        refresh: podcast.refresh,
        max_cache_bytes: podcast.max_cache_bytes,
        max_episode_bytes: podcast.max_episode_bytes,
        feeds,
    })
}

/// One item per episode, feed after feed. Keys are
/// `<source>/<feed folder>/<episode id>`, so progress follows an episode
/// across refreshes.
pub fn items(source: &Source, snapshot: &Snapshot) -> Vec<Item> {
    let served = |path: &Path| -> Option<PathBuf> {
        Some(Path::new(&source.name).join(path.strip_prefix(&source.path).ok()?))
    };
    snapshot
        .feeds
        .iter()
        .flat_map(|feed| {
            feed.episodes.iter().filter_map(move |episode| {
                let track = Track {
                    content_type: scan::content_type(&episode.file)?,
                    path: episode.file.clone(),
                    rel_path: Path::new(&source.name).join(&episode.rel),
                    title: episode.title.clone(),
                };
                let cover = episode.picture.clone().or_else(|| feed.picture.clone());
                Some(Item {
                    kind: Kind::Podcast,
                    key: ItemKey(format!("{}/{}/{}", source.name, feed.slug, episode.id)),
                    name: episode.title.clone(),
                    media: Media::Tracks(vec![track]),
                    cover_rel: cover.as_deref().and_then(served),
                    cover,
                    picture: None,
                    color: None,
                })
            })
        })
        .collect()
}

/// A cache folder name: lower-case letters, digits and `-`.
fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(MAX_SLUG).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "feed".into()
    } else {
        slug.into()
    }
}

/// `slug`, or `slug-2`, `slug-3` ... when an earlier feed has it.
fn unique(slug: &str, taken: &mut HashSet<String>) -> String {
    let mut candidate = slug.to_string();
    let mut n = 2;
    while !taken.insert(candidate.clone()) {
        let suffix = format!("-{n}");
        let stem: String = slug.chars().take(MAX_SLUG - suffix.len()).collect();
        candidate = format!("{stem}{suffix}");
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests;
