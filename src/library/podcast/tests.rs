use std::time::Duration;

use super::*;
use crate::config::{Podcast, PodcastFeed, SourceKind};
use crate::library::{ItemId, Library, Refilled};
use crate::podcasts::{Episode, FeedState};

fn source(feeds: &[(&str, FeedOrder)]) -> Source {
    let mut source = Source::plain("maus", SourceKind::Podcast, Path::new("/cache/maus"));
    source.podcast = Some(Podcast {
        keep: 5,
        refresh: Duration::from_hours(6),
        max_cache_bytes: 1000,
        max_episode_bytes: 100,
        feeds: feeds
            .iter()
            .map(|&(name, order)| PodcastFeed {
                name: name.into(),
                url: "https://example.org/feed".into(),
                order,
                keep: Some(2),
            })
            .collect(),
    });
    source
}

fn episode(slug: &str, id: &str, ext: &str) -> Episode {
    Episode {
        id: id.into(),
        title: format!("Episode {id}"),
        published: Some(0),
        file: format!("/cache/maus/{slug}/{id}.{ext}").into(),
        rel: format!("{slug}/{id}.{ext}").into(),
        content_type: "audio/mpeg".into(),
        bytes: 10,
        picture: None,
    }
}

fn snapshot(episodes: &[(&str, &str)]) -> Snapshot {
    Snapshot {
        feeds: vec![FeedState {
            slug: "die-maus".into(),
            title: "Die Maus".into(),
            picture: Some("/cache/maus/die-maus/feed.jpg".into()),
            episodes: episodes
                .iter()
                .map(|&(id, ext)| episode("die-maus", id, ext))
                .collect(),
        }],
    }
}

#[test]
fn feed_folders_are_slugs_of_the_names_and_unique() {
    let settings = settings(&source(&[
        ("Die Maus!", FeedOrder::Newest),
        ("die maus", FeedOrder::Oldest),
        ("Kinderhörspiel", FeedOrder::Newest),
        ("???", FeedOrder::Newest),
    ]))
    .unwrap();
    let slugs: Vec<&str> = settings.feeds.iter().map(|f| f.slug.as_str()).collect();
    assert_eq!(slugs, ["die-maus", "die-maus-2", "kinderh-rspiel", "feed"]);
    assert_eq!(settings.feeds[1].order, Order::OldestFirst);
    assert_eq!(settings.cache_dir, Path::new("/cache/maus"));
    assert_eq!(settings.feeds[0].keep, Some(2));
}

#[test]
fn a_long_name_gives_a_slug_the_cache_accepts() {
    let long = "a".repeat(100);
    let mut taken = HashSet::new();
    let first = unique(&slug(&long), &mut taken);
    let second = unique(&slug(&long), &mut taken);
    assert_eq!(first.len(), MAX_SLUG);
    assert_eq!(second.len(), MAX_SLUG);
    assert!(second.ends_with("-2"));
}

#[test]
fn only_podcast_sources_have_settings() {
    let music = Source::plain("music", SourceKind::Music, Path::new("/m"));
    assert!(settings(&music).is_none());
}

#[test]
fn episodes_become_items_served_below_the_source() {
    let items = items(
        &source(&[("Die Maus", FeedOrder::Newest)]),
        &snapshot(&[("a1", "mp3"), ("b2", "xyz"), ("c3", "m4a")]),
    );
    assert_eq!(items.len(), 2, "b2 has no playable extension");
    let first = &items[0];
    assert_eq!(first.kind, Kind::Podcast);
    assert_eq!(first.key.0, "maus/die-maus/a1");
    assert_eq!(first.name, "Episode a1");
    assert_eq!(
        first.tracks()[0].rel_path,
        Path::new("maus/die-maus/a1.mp3")
    );
    assert_eq!(first.tracks()[0].content_type, "audio/mpeg");
    assert_eq!(
        first.cover_rel.as_deref(),
        Some(Path::new("maus/die-maus/feed.jpg"))
    );
    assert_eq!(items[1].tracks()[0].content_type, "audio/mp4");
}

#[test]
fn a_refill_keeps_the_ids_of_known_episodes() {
    let source = source(&[("Die Maus", FeedOrder::Newest)]);
    let mut library = Library::with_shelves(vec![
        ("music", Kind::Music, Vec::new()),
        (
            "maus",
            Kind::Podcast,
            items(&source, &snapshot(&[("a1", "mp3")])),
        ),
    ]);
    assert_eq!(library.shelves()[1].items, [ItemId(0)]);

    let refilled = library.refill(
        1,
        items(&source, &snapshot(&[("n9", "mp3"), ("a1", "mp3")])),
    );

    assert!(refilled.moved);
    assert_eq!(library.shelves()[1].items, [ItemId(1), ItemId(0)]);
    assert_eq!(library.item(ItemId(1)).key.0, "maus/die-maus/n9");
    let again = library.refill(
        1,
        items(&source, &snapshot(&[("n9", "mp3"), ("a1", "mp3")])),
    );
    assert_eq!(again, Refilled::default());
    assert_eq!(library.items().len(), 2);

    let mut snapshot = snapshot(&[("n9", "mp3"), ("a1", "mp3")]);
    snapshot.feeds[0].episodes[1].picture = Some("/cache/maus/die-maus/a1.jpg".into());
    let restyled = library.refill(1, items(&source, &snapshot));
    assert_eq!(
        restyled,
        Refilled {
            moved: false,
            restyled: vec![ItemId(0)],
        }
    );
}
