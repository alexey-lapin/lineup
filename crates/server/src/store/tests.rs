use api_types::{CheckOutcome, ResyncTrigger, RouteConfig};

use super::*;
use crate::routes::{self, Validated};

const ACE1: &str = "41169a3f280a5537b0611b775e926852d372743d";
const ACE2: &str = "816b61ec91d5d4da0bc477a42cd6ee407fc07d44";
const ACE3: &str = "914c3344a2cb94beeb26c187ca10e5c862090dba";
const DIRECT: i64 = 1;

/// A store with the seeded Direct route (id 1) and an Acestream gateway route (id 2).
async fn store() -> Store {
    let s = Store::open_memory().await.unwrap();
    let r = s
        .create_route(
            "Acestream",
            RouteConfig::Gateway {
                base_url: "http://10.0.0.5:8099".into(),
                rule: "acestream".into(),
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(r.id, 2);
    s
}

/// What the acestream rule of a gateway returns for an id: an absolute acexy play URL.
fn ace(id: &str) -> Validated {
    Validated {
        source: format!("acestream://{id}"),
        identity: format!("ace:{id}"),
        play: Some(format!("http://10.0.0.5:8080/ace/getstream?id={id}")),
        play_revision: Some("1".into()),
        hints: Default::default(),
        warnings: vec![],
    }
}

/// A page rule: a play path relative to the gateway.
fn page(id: &str, list: &str) -> Validated {
    let source = format!("https://vk.com/video{id}?list={list}");
    Validated {
        play: Some(format!(
            "play/vk.m3u8?ref={}",
            url::form_urlencoded::byte_serialize(source.as_bytes()).collect::<String>()
        )),
        identity: format!("vk:{id}"),
        source,
        play_revision: Some("1".into()),
        hints: Default::default(),
        warnings: vec![],
    }
}

fn direct(url: &str) -> Validated {
    routes::direct(url).unwrap()
}

async fn ch(s: &Store, name: &str, route_id: i64, v: Validated, playlists: &[&str]) -> Result<Channel> {
    let rev = s.route_row(route_id).await?.revision;
    s.create_channel(NewChannelRequest {
        name: name.into(),
        group_title: None,
        tvg_id: None,
        tvg_logo: None,
        route_id,
        route_revision: rev,
        validated: v,
        is_event: false,
        kickoff_at: None,
        enabled: true,
        playlists: playlists.iter().map(|s| s.to_string()).collect(),
    })
    .await
}

async fn pl(s: &Store, slug: &str) {
    s.create_playlist(PlaylistCreate {
        slug: slug.into(),
        name: None,
    })
    .await
    .unwrap();
}

async fn order(s: &Store, slug: &str) -> Vec<(i64, String)> {
    s.get_playlist(slug)
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|i| (i.position, i.channel.name))
        .collect()
}

fn rename(name: &str) -> ChannelUpdate {
    ChannelUpdate {
        name: Some(name.into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn create_dedup_and_render() {
    let s = store().await;
    pl(&s, "jf").await;
    let a = ch(&s, "Alpha", 2, ace(ACE1), &["jf"]).await.unwrap();
    assert_eq!(a.play_url, format!("http://10.0.0.5:8080/ace/getstream?id={ACE1}"));
    assert!(a.route_enabled);
    assert_eq!((a.route_id, a.route_name.as_str()), (2, "Acestream"));
    assert_eq!(a.playlists, vec!["jf"]);

    // Duplicate identity on the same route.
    let err = ch(&s, "Dup", 2, ace(ACE1), &[]).await.unwrap_err();
    assert_eq!(err.code(), "conflict");
    assert_eq!(
        s.find_duplicate(2, &format!("ace:{ACE1}")).await.unwrap().map(|d| d.0),
        Some(a.id)
    );
    // The same identity on another route is another channel.
    ch(&s, "Alpha direct", DIRECT, direct(&format!("http://h/{ACE1}")), &[])
        .await
        .unwrap();

    let mut c = NewChannelRequest {
        name: "Arena 1".into(),
        group_title: Some("Sports".into()),
        tvg_id: Some("Arena.1.de".into()),
        tvg_logo: None,
        route_id: 2,
        route_revision: s.route_row(2).await.unwrap().revision,
        validated: ace(ACE2),
        is_event: false,
        kickoff_at: None,
        enabled: true,
        playlists: vec!["jf".into()],
    };
    c.group_title = Some("Sports".into());
    s.create_channel(c).await.unwrap();

    let out = s.render_m3u("jf").await.unwrap();
    assert_eq!(
        out,
        format!(
            "#EXTM3U\n\n#EXTINF:-1 tvg-id=\"lineup.{}\" tvg-chno=\"1\",Alpha\nhttp://10.0.0.5:8080/ace/getstream?id={ACE1}\n\n\
             #EXTINF:-1 tvg-id=\"Arena.1.de\" tvg-chno=\"2\" group-title=\"Sports\",Arena 1\nhttp://10.0.0.5:8080/ace/getstream?id={ACE2}\n",
            a.id
        )
    );
}

#[tokio::test]
async fn relative_play_paths_join_below_the_base_path() {
    let s = Store::open_memory().await.unwrap();
    let r = s
        .create_route(
            "VK",
            RouteConfig::Gateway {
                base_url: "http://proxy/gateway/".into(),
                rule: "vk".into(),
            },
            true,
        )
        .await
        .unwrap();
    let c = ch(&s, "Match", r.id, page("-1_2", "abc"), &[]).await.unwrap();
    assert_eq!(
        c.play_url,
        "http://proxy/gateway/play/vk.m3u8?ref=https%3A%2F%2Fvk.com%2Fvideo-1_2%3Flist%3Dabc"
    );
    assert_eq!(c.source, "https://vk.com/video-1_2?list=abc");
    assert_eq!(c.identity, "vk:-1_2");
}

#[tokio::test]
async fn disabled_keeps_slot() {
    let s = store().await;
    pl(&s, "jf").await;
    let a = ch(&s, "A", 2, ace(ACE1), &["jf"]).await.unwrap();
    ch(&s, "B", 2, ace(ACE2), &["jf"]).await.unwrap();
    s.patch_channel(
        a.id,
        ChannelUpdate {
            enabled: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let out = s.render_m3u("jf").await.unwrap();
    assert!(!out.contains(",A\n"));
    assert!(out.contains("tvg-chno=\"2\""), "{out}");
    assert!(!out.contains("tvg-chno=\"1\""), "{out}");
}

#[tokio::test]
async fn disabled_routes_leave_output_and_keep_positions() {
    let s = store().await;
    pl(&s, "jf").await;
    let a = ch(&s, "Ace", 2, ace(ACE1), &["jf"]).await.unwrap();
    ch(&s, "Direct", DIRECT, direct("http://x/live.m3u8"), &["jf"])
        .await
        .unwrap();
    let mut rx = s.subscribe();
    let r = s.update_route(2, None, Some(false)).await.unwrap();
    assert!(!r.enabled);
    assert_eq!(r.channel_count, 1);
    assert_eq!(
        rx.try_recv().unwrap(),
        Event::PlaylistChanged {
            slugs: vec!["jf".into()]
        }
    );
    assert!(matches!(
        rx.try_recv().unwrap(),
        Event::RouteChanged { route_id: 2, .. }
    ));
    let out = s.render_m3u("jf").await.unwrap();
    assert!(!out.contains("Ace"), "{out}");
    assert!(out.contains("tvg-chno=\"2\""), "positions are kept: {out}");
    let a2 = s.get_channel(a.id).await.unwrap();
    assert!(!a2.route_enabled);
    assert_eq!(a2.playlists, ["jf"], "membership is kept");

    // Renaming does not change output.
    s.update_route(2, Some("Ace".into()), None).await.unwrap();
    assert!(matches!(
        rx.try_recv().unwrap(),
        Event::RouteChanged { route_id: 2, .. }
    ));
    assert!(rx.try_recv().is_err());

    // A used route can't be deleted; an unused one can.
    assert_eq!(s.delete_route(2).await.unwrap_err().code(), "conflict");
    let extra = s.create_route("Spare", RouteConfig::Direct {}, true).await.unwrap();
    s.delete_route(extra.id).await.unwrap();
    assert_eq!(s.get_route(extra.id).await.unwrap_err().code(), "not_found");
    // Names are unique (case-insensitively as typed).
    assert_eq!(
        s.create_route("Direct", RouteConfig::Direct {}, true)
            .await
            .unwrap_err()
            .code(),
        "conflict"
    );
}

#[tokio::test]
async fn ordering_ops_keep_positions_dense() {
    let s = store().await;
    pl(&s, "jf").await;
    let a = ch(&s, "A", 2, ace(ACE1), &["jf"]).await.unwrap();
    let b = ch(&s, "B", 2, ace(ACE2), &["jf"]).await.unwrap();
    let c = ch(&s, "C", 2, ace(ACE3), &["jf"]).await.unwrap();

    s.move_item("jf", c.id, 1).await.unwrap();
    assert_eq!(
        order(&s, "jf").await,
        vec![(1, "C".into()), (2, "A".into()), (3, "B".into())]
    );

    s.replace_items("jf", vec![b.id, c.id, a.id]).await.unwrap();
    assert_eq!(
        order(&s, "jf").await,
        vec![(1, "B".into()), (2, "C".into()), (3, "A".into())]
    );

    s.remove_item("jf", c.id).await.unwrap();
    assert_eq!(order(&s, "jf").await, vec![(1, "B".into()), (2, "A".into())]);
    s.add_item("jf", c.id, Some(1)).await.unwrap();
    assert_eq!(
        order(&s, "jf").await,
        vec![(1, "C".into()), (2, "B".into()), (3, "A".into())]
    );
    assert_eq!(s.add_item("jf", c.id, None).await.unwrap_err().code(), "conflict");

    // deleting a channel compacts every playlist it was in
    s.delete_channel(b.id).await.unwrap();
    assert_eq!(order(&s, "jf").await, vec![(1, "C".into()), (2, "A".into())]);

    assert_eq!(
        s.replace_items("jf", vec![a.id, a.id]).await.unwrap_err().code(),
        "invalid"
    );
    assert_eq!(
        s.replace_items("jf", vec![a.id, 999]).await.unwrap_err().code(),
        "not_found"
    );
    // failed replace left order untouched
    assert_eq!(order(&s, "jf").await, vec![(1, "C".into()), (2, "A".into())]);
}

#[tokio::test]
async fn events_follow_the_mutation_table() {
    let s = store().await;
    pl(&s, "jf").await;
    let a = ch(&s, "A", 2, ace(ACE1), &["jf"]).await.unwrap();
    let mut rx = s.subscribe();

    s.patch_channel(a.id, rename("A2")).await.unwrap();
    assert_eq!(
        rx.try_recv().unwrap(),
        Event::PlaylistChanged {
            slugs: vec!["jf".into()]
        }
    );

    // no-op patch emits nothing
    s.patch_channel(a.id, rename("A2")).await.unwrap();
    assert!(rx.try_recv().is_err());

    // Settings: the UI notice only.
    s.patch_settings(SettingsPatch {
        epg_refresh_hours: Some(12),
        check_interval_hours: None,
    })
    .await
    .unwrap();
    assert!(matches!(rx.try_recv().unwrap(), Event::SettingsChanged { keys, .. } if keys == ["epg_refresh_hours"]));
    s.patch_settings(SettingsPatch {
        epg_refresh_hours: None,
        check_interval_hours: Some(6),
    })
    .await
    .unwrap();
    assert!(matches!(rx.try_recv().unwrap(), Event::SettingsChanged { keys, .. } if keys == ["check_interval_hours"]));
    assert_eq!(s.settings().await.unwrap().check_interval_hours, 6);
    assert!(rx.try_recv().is_err());

    s.delete_channel(a.id).await.unwrap();
    assert_eq!(
        rx.try_recv().unwrap(),
        Event::PlaylistChanged {
            slugs: vec!["jf".into()]
        }
    );
}

#[tokio::test]
async fn writes_refuse_a_route_that_changed_since_validation() {
    let s = store().await;
    let stale = s.route_row(2).await.unwrap().revision;
    s.update_route(2, Some("Renamed".into()), None).await.unwrap();
    let err = s
        .create_channel(NewChannelRequest {
            name: "A".into(),
            group_title: None,
            tvg_id: None,
            tvg_logo: None,
            route_id: 2,
            route_revision: stale,
            validated: ace(ACE1),
            is_event: false,
            kickoff_at: None,
            enabled: true,
            playlists: vec![],
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "conflict");
    assert!(s.list_channels(&ChannelFilter::default()).await.unwrap().is_empty());
}

#[tokio::test]
async fn source_changes_bump_the_revision_and_clear_health() {
    let s = store().await;
    let a = ch(&s, "A", 2, ace(ACE1), &[]).await.unwrap();
    let before = s.channel_source(a.id).await.unwrap();
    let who = CheckIdentity {
        channel_id: a.id,
        created_at: a.created_at,
        source_revision: before.source_revision,
        route_id: 2,
        route_revision: s.route_row(2).await.unwrap().revision,
    };
    let rec = || CheckRecord {
        outcome: CheckOutcome::Ok,
        content_state: None,
        reason: None,
        detail: None,
    };
    assert!(s.record_check(who, rec()).await.unwrap());
    assert!(s.get_channel(a.id).await.unwrap().health.is_some());

    let change = |v: Validated, based_on: i64| ChannelUpdate {
        source: Some(SourceChange {
            route_id: 2,
            route_revision: 0,
            validated: v,
            based_on_source_revision: based_on,
        }),
        ..Default::default()
    };
    let rev = s.route_row(2).await.unwrap().revision;
    let mut update = change(ace(ACE2), before.source_revision);
    update.source.as_mut().unwrap().route_revision = rev;
    let a2 = s.patch_channel(a.id, update).await.unwrap();
    assert_eq!(a2.identity, format!("ace:{ACE2}"));
    assert!(a2.health.is_none(), "health described the old source");
    let after = s.channel_source(a.id).await.unwrap();
    assert_eq!(after.source_revision, before.source_revision + 1);

    // A result measured on the old source is not recorded.
    assert!(!s.record_check(who, rec()).await.unwrap());

    // An update computed from the old source revision is refused.
    let mut stale = change(ace(ACE3), before.source_revision);
    stale.source.as_mut().unwrap().route_revision = rev;
    assert_eq!(s.patch_channel(a.id, stale).await.unwrap_err().code(), "conflict");

    // Moving onto an identity another channel has is refused and changes nothing.
    ch(&s, "B", 2, ace(ACE3), &[]).await.unwrap();
    let mut clash = change(ace(ACE3), after.source_revision);
    clash.source.as_mut().unwrap().route_revision = rev;
    clash.name = Some("A3".into());
    assert_eq!(s.patch_channel(a.id, clash).await.unwrap_err().code(), "conflict");
    let a3 = s.get_channel(a.id).await.unwrap();
    assert_eq!((a3.name.as_str(), a3.identity.as_str()), ("A", a2.identity.as_str()));
}

fn entry(index: usize, name: &str, v: Validated, tvg_id: Option<&str>) -> ImportEntryIn {
    ImportEntryIn {
        index,
        name: name.into(),
        group_title: Some("Sports".into()),
        tvg_id: tvg_id.map(Into::into),
        tvg_logo: None,
        validated: v,
    }
}

async fn import(s: &Store, slug: &str, dry_run: bool, entries: Vec<ImportEntryIn>) -> ImportReport {
    s.import_validated(ImportBatch {
        slug: slug.into(),
        dry_run,
        route_id: 2,
        route_revision: s.route_row(2).await.unwrap().revision,
        entries,
        skipped_lines: vec!["entry 5 (Broken): the route does not accept this input".into()],
    })
    .await
    .unwrap()
}

fn sample() -> Vec<ImportEntryIn> {
    vec![
        entry(1, "Sports One", ace(ACE1), Some("SportsOne.uk")),
        entry(2, "Arena 1", ace(ACE2), Some("Arena.1.de")),
        entry(3, "Alpha again", ace(ACE1), Some("dupe")),
    ]
}

#[tokio::test]
async fn import_dry_run_is_read_only() {
    let s = store().await;
    let r = import(&s, "jellyfin", true, sample()).await;
    assert!(r.dry_run && r.playlist_created);
    let actions: Vec<_> = r.entries.iter().map(|e| e.action).collect();
    assert_eq!(
        actions,
        vec![ImportAction::Create, ImportAction::Create, ImportAction::SkipInPlaylist]
    );
    assert_eq!(r.skipped_lines.len(), 1);
    assert!(s.list_playlists().await.unwrap().is_empty());
    assert!(s.list_channels(&ChannelFilter::default()).await.unwrap().is_empty());
}

#[tokio::test]
async fn import_commit_and_reimport() {
    let s = store().await;
    let r = import(&s, "jellyfin", false, sample()).await;
    assert_eq!(r.entries.iter().filter(|e| e.action == ImportAction::Create).count(), 2);
    assert_eq!(r.entries[0].identity, format!("ace:{ACE1}"));
    let out = s.render_m3u("jellyfin").await.unwrap();
    assert!(
        out.contains(&format!("http://10.0.0.5:8080/ace/getstream?id={ACE2}")),
        "{out}"
    );
    assert_eq!(order(&s, "jellyfin").await.len(), 2);

    // Edit existing metadata, then re-import into a second playlist: reuse, no overwrite, diff
    // reported (including a changed source for the same identity).
    let one = s
        .list_channels(&ChannelFilter {
            q: Some("sports one".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    s.patch_channel(one[0].id, rename("Sports One HD")).await.unwrap();
    let mut again = sample();
    again[0].validated.source = format!("acestream://{ACE1}?x=1");
    let r = import(&s, "other", false, again).await;
    assert!(r.entries.iter().all(|e| e.action != ImportAction::Create));
    let e0 = &r.entries[0];
    assert_eq!(e0.action, ImportAction::Reuse);
    let fields: Vec<&str> = e0.diff.iter().map(|d| d.field.as_str()).collect();
    assert_eq!(fields, ["name", "source"]);
    let kept = s.get_channel(one[0].id).await.unwrap();
    assert_eq!(kept.name, "Sports One HD");
    assert_eq!(kept.source, format!("acestream://{ACE1}"), "the stored source is kept");
    assert_eq!(order(&s, "other").await.len(), 2);

    // re-import into the same playlist is a no-op
    let r = import(&s, "jellyfin", false, sample()).await;
    assert!(r.entries.iter().all(|e| e.action == ImportAction::SkipInPlaylist));
}

#[tokio::test]
async fn import_diffs_repeated_entries() {
    let s = store().await;
    let two = |a: &str, b: &str| vec![entry(1, "A", ace(ACE1), Some(a)), entry(2, "A", ace(ACE1), Some(b))];
    for dry_run in [true, false] {
        let r = import(&s, "jf", dry_run, two("first.id", "second.id")).await;
        assert_eq!(r.entries[1].action, ImportAction::SkipInPlaylist);
        assert_eq!(r.entries[1].diff.len(), 1, "dry_run={dry_run}: {:?}", r.entries[1].diff);
        assert_eq!(r.entries[1].diff[0].existing.as_deref(), Some("first.id"));
    }
    // now persisted: both entries diff against the stored channel
    let r = import(&s, "jf", true, two("x", "y")).await;
    assert_eq!(r.entries[0].diff[0].existing.as_deref(), Some("first.id"));
    assert_eq!(r.entries[1].diff[0].existing.as_deref(), Some("first.id"));
}

#[tokio::test]
async fn failed_mutations_roll_back() {
    let s = store().await;
    pl(&s, "jf").await;
    // create into a missing playlist: nothing is created
    let err = ch(&s, "A", 2, ace(ACE1), &["jf", "missing"]).await.unwrap_err();
    assert_eq!(err.code(), "not_found");
    assert!(s.list_channels(&ChannelFilter::default()).await.unwrap().is_empty());
    assert!(order(&s, "jf").await.is_empty());
}

async fn srev(s: &Store, id: i64) -> i64 {
    s.channel_source(id).await.unwrap().source_revision
}

fn resync_rows(s: &[(i64, i64, Validated)]) -> Vec<ResyncRow> {
    s.iter()
        .map(|(id, based_on, v)| ResyncRow {
            channel_id: *id,
            based_on: *based_on,
            validated: v.clone(),
        })
        .collect()
}

#[tokio::test]
async fn resync_publishes_consistently() {
    let s = store().await;
    pl(&s, "jf").await;
    let a = ch(&s, "A", 2, ace(ACE1), &["jf"]).await.unwrap();
    let b = ch(&s, "B", 2, ace(ACE2), &["jf"]).await.unwrap();
    let c = ch(&s, "C", 2, ace(ACE3), &[]).await.unwrap();
    let (ra, rb, rc) = (srev(&s, a.id).await, srev(&s, b.id).await, srev(&s, c.id).await);

    // The gateway now plays A from another address (play revision 2); B is unchanged except for the
    // revision; C changed in the meantime (its row is skipped).
    let mut a_new = ace(ACE1);
    a_new.play = Some(format!("http://acexy-new:8080/ace/getstream?id={ACE1}"));
    a_new.play_revision = Some("2".into());
    let mut b_new = ace(ACE2);
    b_new.play_revision = Some("2".into());
    let mut c_new = ace(ACE3);
    c_new.play_revision = Some("2".into());
    s.patch_channel(c.id, rename("C2")).await.unwrap(); // metadata only: source revision unchanged
    let route_rev = s.route_row(2).await.unwrap().revision;
    let mut rx = s.subscribe();
    let (route, report) = s
        .publish_resync(ResyncPublish {
            route_id: 2,
            expected_revision: route_rev,
            new_base: None,
            rows: resync_rows(&[(a.id, ra, a_new), (b.id, rb, b_new), (c.id, rc + 1, c_new)]),
            report: new_report(ResyncTrigger::Manual, 3, Some("1".into()), Some("2".into())),
            all_or_nothing: false,
            name: None,
            enabled: None,
        })
        .await
        .unwrap();
    assert_eq!((report.updated, report.unchanged), (1, 1));
    assert_eq!(report.skipped.len(), 1, "{report:?}");
    assert!(!report.complete);
    assert_eq!(
        route.synced_play_revision, None,
        "incomplete: the synced revision stays"
    );
    assert_eq!(route.last_resync.as_ref().map(|r| r.updated), Some(1));
    assert_eq!(
        rx.try_recv().unwrap(),
        Event::PlaylistChanged {
            slugs: vec!["jf".into()]
        }
    );
    assert!(s.render_m3u("jf").await.unwrap().contains("acexy-new"));
    assert_eq!(
        s.route_pending(2, "2").await.unwrap(),
        1,
        "only C is still on revision 1"
    );

    // A stale route revision is refused.
    let err = s
        .publish_resync(ResyncPublish {
            route_id: 2,
            expected_revision: route_rev,
            new_base: None,
            rows: vec![],
            report: new_report(ResyncTrigger::Manual, 0, None, None),
            all_or_nothing: false,
            name: None,
            enabled: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "conflict");

    // A complete resync (every member validated) advances the synced revisions.
    let route_rev = s.route_row(2).await.unwrap().revision;
    let mut c_new = ace(ACE3);
    c_new.play_revision = Some("2".into());
    let mut a2 = ace(ACE1);
    a2.play = Some(format!("http://acexy-new:8080/ace/getstream?id={ACE1}"));
    a2.play_revision = Some("2".into());
    let mut b2 = ace(ACE2);
    b2.play_revision = Some("2".into());
    let (route, report) = s
        .publish_resync(ResyncPublish {
            route_id: 2,
            expected_revision: route_rev,
            new_base: None,
            rows: resync_rows(&[
                (a.id, srev(&s, a.id).await, a2),
                (b.id, srev(&s, b.id).await, b2),
                (c.id, srev(&s, c.id).await, c_new),
            ]),
            report: new_report(ResyncTrigger::RevisionChange, 3, Some("1".into()), Some("2".into())),
            all_or_nothing: false,
            name: None,
            enabled: None,
        })
        .await
        .unwrap();
    assert!(report.complete);
    assert_eq!(route.synced_play_revision.as_deref(), Some("2"));
    assert_eq!(s.route_pending(2, "2").await.unwrap(), 0);
}

#[tokio::test]
async fn resync_never_merges_and_base_changes_are_all_or_nothing() {
    let s = store().await;
    pl(&s, "jf").await;
    let a = ch(&s, "A", 2, ace(ACE1), &["jf"]).await.unwrap();
    let b = ch(&s, "B", 2, ace(ACE2), &["jf"]).await.unwrap();
    let ra = s.channel_source(a.id).await.unwrap().source_revision;
    let rb = s.channel_source(b.id).await.unwrap().source_revision;
    let route_rev = s.route_row(2).await.unwrap().revision;

    // The gateway now gives A the identity B has: A fails, B is fine.
    let (_, report) = s
        .publish_resync(ResyncPublish {
            route_id: 2,
            expected_revision: route_rev,
            new_base: None,
            rows: resync_rows(&[(a.id, ra, ace(ACE2)), (b.id, rb, ace(ACE2))]),
            report: new_report(ResyncTrigger::Manual, 2, None, None),
            all_or_nothing: false,
            name: None,
            enabled: None,
        })
        .await
        .unwrap();
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].channel_id, a.id);
    assert_eq!(s.get_channel(a.id).await.unwrap().identity, format!("ace:{ACE1}"));

    // With a new base URL, any failure refuses everything, including the base change.
    let route_rev = s.route_row(2).await.unwrap().revision;
    let mut failed = new_report(ResyncTrigger::BaseUrlChange, 2, None, None);
    failed.failed.push(api_types::ResyncItem {
        channel_id: b.id,
        name: "B".into(),
        reason: "the route does not accept this input".into(),
    });
    let err = s
        .publish_resync(ResyncPublish {
            route_id: 2,
            expected_revision: route_rev,
            new_base: Some("http://new:8099".into()),
            rows: resync_rows(&[(a.id, ra, ace(ACE1))]),
            report: failed,
            all_or_nothing: true,
            name: None,
            enabled: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "conflict");
    assert!(err.to_string().contains("B"), "{err}");
    let row = s.route_row(2).await.unwrap();
    assert_eq!(
        row.config,
        RouteConfig::Gateway {
            base_url: "http://10.0.0.5:8099".into(),
            rule: "acestream".into()
        }
    );
    assert_eq!(row.revision, route_rev);

    // Without failures the base change and the rows publish together.
    let mut rx = s.subscribe();
    let (route, _) = s
        .publish_resync(ResyncPublish {
            route_id: 2,
            expected_revision: route_rev,
            new_base: Some("http://new:8099".into()),
            rows: resync_rows(&[(a.id, ra, ace(ACE1)), (b.id, rb, ace(ACE2))]),
            report: new_report(ResyncTrigger::BaseUrlChange, 2, Some("1".into()), Some("1".into())),
            all_or_nothing: true,
            name: None,
            enabled: None,
        })
        .await
        .unwrap();
    assert_eq!(
        route.config,
        RouteConfig::Gateway {
            base_url: "http://new:8099".into(),
            rule: "acestream".into()
        }
    );
    assert_eq!(
        rx.try_recv().unwrap(),
        Event::PlaylistChanged {
            slugs: vec!["jf".into()]
        },
        "a base change changes output"
    );
}

/// File-backed pool (several connections): reads must see one committed state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_are_consistent_under_concurrent_writes() {
    let dir = tempfile::tempdir().unwrap();
    let s = std::sync::Arc::new(Store::open(&dir.path().join("t.db")).await.unwrap());
    pl(&s, "jf").await;
    let keep = ch(&s, "Persistent", DIRECT, direct("http://x/1"), &["jf"])
        .await
        .unwrap();

    let writer = {
        let s = s.clone();
        tokio::spawn(async move {
            for _ in 0..150 {
                let t = ch(&s, "Transient", DIRECT, direct("http://x/2"), &[]).await.unwrap();
                s.add_item("jf", t.id, Some(1)).await.unwrap();
                s.delete_channel(t.id).await.unwrap();
            }
        })
    };
    let reader = {
        let s = s.clone();
        tokio::spawn(async move {
            let mut checks = 0;
            while checks < 400 {
                let d = s.get_playlist("jf").await.unwrap();
                assert_eq!(d.playlist.channel_count as usize, d.items.len(), "count/items disagree");
                let pos: Vec<i64> = d.items.iter().map(|i| i.position).collect();
                assert_eq!(
                    pos,
                    (1..=d.items.len() as i64).collect::<Vec<_>>(),
                    "positions not dense"
                );

                let m = s.render_m3u("jf").await.unwrap();
                let valid_alone = m.contains("tvg-chno=\"1\",Persistent");
                let valid_pair = m.contains("tvg-chno=\"1\",Transient") && m.contains("tvg-chno=\"2\",Persistent");
                assert!(valid_alone || valid_pair, "rendered a state that never existed:\n{m}");
                checks += 1;
            }
        })
    };
    writer.await.unwrap();
    reader.await.unwrap();
    assert_eq!(order(&s, "jf").await, vec![(1, keep.name)]);
}

#[tokio::test]
async fn backup_to_writes_a_database() {
    let dir = tempfile::tempdir().unwrap();
    // File-backed only: VACUUM INTO uses the source database's VFS, so an in-memory source writes in memory.
    let s = Store::open(&dir.path().join("src.db")).await.unwrap();
    pl(&s, "jf").await;
    ch(&s, "A", DIRECT, direct("http://x/a"), &["jf"]).await.unwrap();
    let out = dir.path().join("backup.db");
    s.backup_to(&out).await.unwrap();
    assert!(out.exists(), "{out:?} missing");
    let r = Store::open(&out).await.unwrap();
    assert_eq!(order(&r, "jf").await.len(), 1);
}

#[tokio::test]
async fn a_new_database_has_one_direct_route() {
    let s = Store::open_memory().await.unwrap();
    let routes = s.list_routes().await.unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!((routes[0].id, routes[0].name.as_str()), (DIRECT, "Direct"));
    assert_eq!(routes[0].config, RouteConfig::Direct {});
    assert_eq!(s.find_route("direct").await.unwrap().id, DIRECT);
    assert_eq!(s.find_route("1").await.unwrap().id, DIRECT);
}

#[tokio::test]
async fn channel_ids_are_never_reused() {
    let s = store().await;
    let a = ch(&s, "A", 2, ace(ACE1), &[]).await.unwrap();
    let staged = s.channel_source(a.id).await.unwrap();
    s.delete_channel(a.id).await.unwrap();
    let b = ch(&s, "B", 2, ace(ACE2), &[]).await.unwrap();
    assert_ne!(a.id, b.id, "a deleted channel's id is not given to a new one");
    // A resync staged for the deleted channel does not touch the new one.
    let route_rev = s.route_row(2).await.unwrap().revision;
    let (_, report) = s
        .publish_resync(ResyncPublish {
            route_id: 2,
            expected_revision: route_rev,
            new_base: None,
            rows: resync_rows(&[(a.id, staged.source_revision, ace(ACE3))]),
            report: new_report(ResyncTrigger::Manual, 1, None, None),
            all_or_nothing: false,
            name: None,
            enabled: None,
        })
        .await
        .unwrap();
    assert_eq!(report.updated, 0);
    assert_eq!(s.get_channel(b.id).await.unwrap().identity, format!("ace:{ACE2}"));
}

#[tokio::test]
async fn channels_joining_during_a_resync_keep_it_incomplete() {
    let s = store().await;
    let a = ch(&s, "A", 2, ace(ACE1), &[]).await.unwrap();
    let ra = srev(&s, a.id).await;
    let route_rev = s.route_row(2).await.unwrap().revision;
    // B joins after the snapshot was validated.
    let b = ch(&s, "B", 2, ace(ACE2), &[]).await.unwrap();
    let publish = |base: Option<&str>, all: bool| ResyncPublish {
        route_id: 2,
        expected_revision: route_rev,
        new_base: base.map(Into::into),
        rows: resync_rows(&[(a.id, ra, ace(ACE1))]),
        report: new_report(ResyncTrigger::Manual, 1, Some("1".into()), Some("2".into())),
        all_or_nothing: all,
        name: None,
        enabled: None,
    };
    // A base change is refused as a whole.
    let err = s
        .publish_resync(publish(Some("http://new:8099"), true))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "conflict");
    assert!(err.to_string().contains("joined the route"), "{err}");
    // A resync publishes what it validated but is not complete.
    let (route, report) = s.publish_resync(publish(None, false)).await.unwrap();
    assert!(!report.complete);
    assert_eq!(report.skipped[0].channel_id, b.id);
    assert_eq!(route.synced_play_revision, None);
}

#[tokio::test]
async fn a_rejected_combined_route_edit_changes_nothing() {
    let s = store().await;
    s.create_route("Other", RouteConfig::Direct {}, true).await.unwrap();
    let a = ch(&s, "A", 2, ace(ACE1), &[]).await.unwrap();
    let ra = srev(&s, a.id).await;
    let before = s.route_row(2).await.unwrap();
    let mut rx = s.subscribe();
    let err = s
        .publish_resync(ResyncPublish {
            route_id: 2,
            expected_revision: before.revision,
            new_base: Some("http://new:8099".into()),
            rows: resync_rows(&[(a.id, ra, ace(ACE1))]),
            report: new_report(ResyncTrigger::BaseUrlChange, 1, None, None),
            all_or_nothing: true,
            name: Some("Other".into()),
            enabled: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "conflict");
    let after = s.route_row(2).await.unwrap();
    assert_eq!(
        (after.config, after.name, after.revision),
        (before.config, before.name, before.revision)
    );
    assert!(rx.try_recv().is_err(), "no events");
}
