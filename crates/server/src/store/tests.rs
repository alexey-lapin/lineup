use super::*;

const ACE1: &str = "41169a3f280a5537b0611b775e926852d372743d";
const ACE2: &str = "816b61ec91d5d4da0bc477a42cd6ee407fc07d44";
const ACE3: &str = "914c3344a2cb94beeb26c187ca10e5c862090dba";

async fn store() -> Store {
    let s = Store::open_memory().await.unwrap();
    s.patch_settings(SettingsPatch {
        acexy_base: Some("http://10.0.0.5:8080".into()),
        vk_base: Some("http://10.0.0.5:8099".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    s
}

fn ch(name: &str, input: &str, playlists: &[&str]) -> ChannelCreate {
    ChannelCreate {
        name: name.into(),
        input: Some(input.into()),
        enabled: true,
        playlists: playlists.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
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

#[tokio::test]
async fn create_dedup_and_render() {
    let s = store().await;
    pl(&s, "jf").await;
    let a = s
        .create_channel(ch("Alpha", &format!("acestream://{ACE1}"), &["jf"]))
        .await
        .unwrap();
    assert_eq!(a.stream_url, format!("http://10.0.0.5:8080/ace/getstream?id={ACE1}"));
    assert_eq!(a.playlists, vec!["jf"]);

    // duplicate by source key, regardless of input form
    let err = s
        .create_channel(ch("Dup", &ACE1.to_uppercase(), &[]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "conflict");

    let mut c = ch("Arena 1", ACE2, &["jf"]);
    c.tvg_id = Some("Arena.1.de".into());
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
async fn disabled_keeps_slot() {
    let s = store().await;
    pl(&s, "jf").await;
    let a = s.create_channel(ch("A", ACE1, &["jf"])).await.unwrap();
    s.create_channel(ch("B", ACE2, &["jf"])).await.unwrap();
    s.patch_channel(
        a.id,
        ChannelPatch {
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
async fn ordering_ops_keep_positions_dense() {
    let s = store().await;
    pl(&s, "jf").await;
    let a = s.create_channel(ch("A", ACE1, &["jf"])).await.unwrap();
    let b = s.create_channel(ch("B", ACE2, &["jf"])).await.unwrap();
    let c = s.create_channel(ch("C", ACE3, &["jf"])).await.unwrap();

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
    pl(&s, "vk").await;
    let a = s.create_channel(ch("A", ACE1, &["jf"])).await.unwrap();
    s.create_channel(ch("V", "https://vk.com/video-1_2", &["vk"]))
        .await
        .unwrap();
    let mut rx = s.subscribe();

    s.patch_channel(
        a.id,
        ChannelPatch {
            name: Some("A2".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        rx.try_recv().unwrap(),
        Event::PlaylistChanged {
            slugs: vec!["jf".into()]
        }
    );

    // no-op patch emits nothing
    s.patch_channel(
        a.id,
        ChannelPatch {
            name: Some("A2".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(rx.try_recv().is_err());

    s.patch_settings(SettingsPatch {
        vk_base: Some("http://10.0.0.1:8099".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(
        rx.try_recv().unwrap(),
        Event::PlaylistChanged {
            slugs: vec!["vk".into()]
        }
    );
    assert_eq!(
        rx.try_recv().unwrap(),
        Event::SettingsChanged {
            keys: vec!["vk_base".into()]
        }
    );

    // engine_base does not affect output: only the UI notice
    s.patch_settings(SettingsPatch {
        engine_base: Some("http://10.0.0.1:6878".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(
        rx.try_recv().unwrap(),
        Event::SettingsChanged {
            keys: vec!["engine_base".into()]
        }
    );
    assert!(rx.try_recv().is_err());

    s.delete_channel(a.id).await.unwrap();
    assert_eq!(
        rx.try_recv().unwrap(),
        Event::PlaylistChanged {
            slugs: vec!["jf".into()]
        }
    );
}

const SAMPLE_M3U: &str = "#EXTM3U\n\
#EXTINF:-1 tvg-id=\"SportsOne.uk\" group-title=\"Sports\",Sports One\n\
http://10.0.0.5:8080/ace/getstream?id=41169a3f280a5537b0611b775e926852d372743d\n\
\n\
#EXTINF:-1 tvg-id=\"Arena.1.de\" group-title=\"Sports\",Arena 1\n\
http://10.0.0.9:6878/ace/getstream?id=816b61ec91d5d4da0bc477a42cd6ee407fc07d44\n\
\n\
#EXTINF:-1 tvg-id=\"dupe\" group-title=\"Sports\",Alpha again\n\
http://10.0.0.5:8080/ace/getstream?id=41169A3F280A5537B0611B775E926852D372743D\n\
\n\
#EXTINF:-1 tvg-id=\"vk.live.test\" group-title=\"VK\",VK Live\n\
http://10.0.0.5:8099/vk/-100200300_456000001?list=4d2c9e1a7f\n\
#EXTINF:-1,Broken\n\
notaurl\n";

#[tokio::test]
async fn import_dry_run_is_read_only() {
    let s = store().await;
    let r = s.import(SAMPLE_M3U, "jellyfin", true).await.unwrap();
    assert!(r.dry_run && r.playlist_created);
    let actions: Vec<_> = r.entries.iter().map(|e| e.action).collect();
    assert_eq!(
        actions,
        vec![
            ImportAction::Create,
            ImportAction::Create,
            ImportAction::SkipInPlaylist,
            ImportAction::Create
        ]
    );
    assert_eq!(r.skipped_lines.len(), 1);
    assert!(s.list_playlists().await.unwrap().is_empty());
    assert!(s.list_channels(&ChannelFilter::default()).await.unwrap().is_empty());
}

#[tokio::test]
async fn import_commit_and_reimport() {
    let s = store().await;
    let r = s.import(SAMPLE_M3U, "jellyfin", false).await.unwrap();
    assert_eq!(r.entries.iter().filter(|e| e.action == ImportAction::Create).count(), 3);
    let out = s.render_m3u("jellyfin").await.unwrap();
    // the wrong-subnet engine URL was normalised onto acexy_base
    assert!(
        out.contains(&format!("http://10.0.0.5:8080/ace/getstream?id={ACE2}")),
        "{out}"
    );
    assert!(out.contains("http://10.0.0.5:8099/vk/-100200300_456000001?list=4d2c9e1a7f"));
    assert_eq!(order(&s, "jellyfin").await.len(), 3);

    // edit existing metadata, then re-import into a second playlist: reuse, no overwrite, diff reported
    let one = s
        .list_channels(&ChannelFilter {
            q: Some("sports one".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    s.patch_channel(
        one[0].id,
        ChannelPatch {
            name: Some("Sports One HD".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r = s.import(SAMPLE_M3U, "other", false).await.unwrap();
    assert!(r.entries.iter().all(|e| e.action != ImportAction::Create));
    let e0 = &r.entries[0];
    assert_eq!(e0.action, ImportAction::Reuse);
    assert_eq!(e0.diff[0].field, "name");
    assert_eq!(s.get_channel(one[0].id).await.unwrap().name, "Sports One HD");
    assert_eq!(order(&s, "other").await.len(), 3);

    // re-import into the same playlist is a no-op
    let r = s.import(SAMPLE_M3U, "jellyfin", false).await.unwrap();
    assert!(r.entries.iter().all(|e| e.action == ImportAction::SkipInPlaylist));
}

#[tokio::test]
async fn resolve_reports_duplicates() {
    let s = store().await;
    let a = s.create_channel(ch("A", ACE1, &[])).await.unwrap();
    let r = s.resolve(&format!("acestream://{ACE1}")).await.unwrap();
    assert_eq!(r.duplicate_of, Some(a.id));
    let r = s.resolve(ACE2).await.unwrap();
    assert_eq!(r.duplicate_of, None);
}

#[tokio::test]
async fn import_diffs_repeated_entries() {
    let s = store().await;
    let text = "#EXTM3U\n#EXTINF:-1 tvg-id=\"first.id\",A\nhttp://a\n#EXTINF:-1 tvg-id=\"second.id\",A\nhttp://a\n";
    for dry_run in [true, false] {
        let r = s.import(text, "jf", dry_run).await.unwrap();
        assert_eq!(r.entries[1].action, ImportAction::SkipInPlaylist);
        assert_eq!(r.entries[1].diff.len(), 1, "dry_run={dry_run}: {:?}", r.entries[1].diff);
        assert_eq!(r.entries[1].diff[0].existing.as_deref(), Some("first.id"));
    }
    // now persisted: both entries diff against the stored channel
    let text2 = "#EXTM3U\n#EXTINF:-1 tvg-id=\"x\",A\nhttp://a\n#EXTINF:-1 tvg-id=\"y\",A\nhttp://a\n";
    let r = s.import(text2, "jf", true).await.unwrap();
    assert_eq!(r.entries[0].diff[0].existing.as_deref(), Some("first.id"));
    assert_eq!(r.entries[1].diff[0].existing.as_deref(), Some("first.id"));
}

#[tokio::test]
async fn failed_mutations_roll_back() {
    let s = store().await;
    pl(&s, "jf").await;
    // create into a missing playlist: nothing is created
    let err = s.create_channel(ch("A", ACE1, &["jf", "missing"])).await.unwrap_err();
    assert_eq!(err.code(), "not_found");
    assert!(s.list_channels(&ChannelFilter::default()).await.unwrap().is_empty());
    assert!(order(&s, "jf").await.is_empty());

    // source conflict on patch leaves the channel unchanged
    let a = s.create_channel(ch("A", ACE1, &[])).await.unwrap();
    s.create_channel(ch("B", ACE2, &[])).await.unwrap();
    let err = s
        .patch_channel(
            a.id,
            ChannelPatch {
                name: Some("A2".into()),
                input: Some(ACE2.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "conflict");
    let a2 = s.get_channel(a.id).await.unwrap();
    assert_eq!((a2.name.as_str(), &a2.source), ("A", &a.source));
}

/// File-backed pool (several connections): reads must see one committed state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_are_consistent_under_concurrent_writes() {
    let dir = tempfile::tempdir().unwrap();
    let s = std::sync::Arc::new(Store::open(&dir.path().join("t.db")).await.unwrap());
    pl(&s, "jf").await;
    let keep = s.create_channel(ch("Persistent", ACE1, &["jf"])).await.unwrap();

    let writer = {
        let s = s.clone();
        tokio::spawn(async move {
            for _ in 0..150 {
                let t = s.create_channel(ch("Transient", ACE2, &[])).await.unwrap();
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
    for s in [Store::open(&dir.path().join("src.db")).await.unwrap()] {
        pl(&s, "jf").await;
        s.create_channel(ch("A", ACE1, &["jf"])).await.unwrap();
        let out = dir.path().join(format!("b{}.db", rand_suffix()));
        s.backup_to(&out).await.unwrap();
        assert!(out.exists(), "{out:?} missing");
        let r = Store::open(&out).await.unwrap();
        assert_eq!(order(&r, "jf").await.len(), 1);
    }
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}
