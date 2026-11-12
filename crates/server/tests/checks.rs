//! Channel checks through gateway routes, and the revision poller, against the stub gateway.

use std::sync::Arc;

use api_types::{ChannelCreate, CheckOutcome, ContentState, RouteConfig};
use gateway_contract::{CheckOutcome as GwOutcome, ContentState as GwState};
use lineup_server::http::App;
use lineup_server::testing::{Failure, StubGateway};
use lineup_server::{Event, Store};

const ACE1: &str = "41169a3f280a5537b0611b775e926852d372743d";
const ACE2: &str = "816b61ec91d5d4da0bc477a42cd6ee407fc07d44";

struct Fixture {
    gw: StubGateway,
    app: App,
    store: Arc<Store>,
    route: i64,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let gw = StubGateway::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_memory().await.unwrap());
    let app = App::new(store.clone(), dir.path());
    let route = store
        .create_route(
            "Acestream",
            RouteConfig::Gateway {
                base_url: gw.url.clone(),
                rule: "acestream".into(),
            },
            true,
        )
        .await
        .unwrap()
        .id;
    Fixture {
        gw,
        app,
        store,
        route,
        _dir: dir,
    }
}

impl Fixture {
    async fn channel(&self, name: &str, route_id: i64, input: &str) -> i64 {
        self.app
            .catalog
            .create_channel(ChannelCreate {
                name: name.into(),
                group_title: None,
                tvg_id: None,
                tvg_logo: None,
                route_id,
                input: input.into(),
                is_event: false,
                kickoff_at: None,
                enabled: true,
                playlists: vec![],
            })
            .await
            .unwrap()
            .id
    }
}

#[tokio::test]
async fn results_are_recorded_with_transitions() {
    let f = fixture().await;
    let a = f.channel("A", f.route, ACE1).await;
    let mut rx = f.store.subscribe();

    f.gw.set_check(GwOutcome::Dead, None);
    let r = f.app.checker.check(&[a], false).await.unwrap();
    assert_eq!((r[0].outcome, r[0].recorded), (CheckOutcome::Dead, true));
    assert!(matches!(rx.recv().await.unwrap(), Event::ChannelUnhealthy { channel_id, .. } if channel_id == a));

    // Unknown and skipped never replace the conclusive result.
    for outcome in [GwOutcome::Unknown, GwOutcome::Skipped] {
        f.gw.set_check(outcome, None);
        f.app.checker.check(&[a], false).await.unwrap();
        assert!(matches!(rx.recv().await.unwrap(), Event::ChannelChecked { .. }));
        let h = f.store.get_channel(a).await.unwrap().health.unwrap();
        assert_eq!(h.conclusive, Some(CheckOutcome::Dead));
    }
    let h = f.store.get_channel(a).await.unwrap().health.unwrap();
    assert_eq!(
        (h.last_attempt, h.reason.as_deref()),
        (CheckOutcome::Skipped, Some("busy"))
    );

    // Forcing goes through what the gateway would skip.
    let r = f.app.checker.check(&[a], true).await.unwrap();
    assert_eq!(r[0].outcome, CheckOutcome::Ok);
    assert!(matches!(rx.recv().await.unwrap(), Event::ChannelRecovered { channel_id, .. } if channel_id == a));

    // An ended broadcast with a recording is healthy, with its content state.
    f.gw.set_check(GwOutcome::Ok, Some(GwState::Recording));
    f.app.checker.check(&[a], false).await.unwrap();
    let h = f.store.get_channel(a).await.unwrap().health.unwrap();
    assert_eq!(
        (h.conclusive, h.content_state),
        (Some(CheckOutcome::Ok), Some(ContentState::Recording))
    );
}

#[tokio::test]
async fn a_failing_gateway_stops_its_route_and_records_unknown() {
    let f = fixture().await;
    let a = f.channel("A", f.route, ACE1).await;
    let b = f.channel("B", f.route, ACE2).await;
    f.gw.set_check(GwOutcome::Ok, None);
    f.app.checker.check(&[a], false).await.unwrap();

    f.gw.fail(Failure::ServerError);
    // Requested checks fail up front: the rule can't be read.
    assert_eq!(
        f.app.checker.check(&[a], false).await.unwrap_err().code(),
        "gateway_error"
    );
    // A run over everything: the route is skipped as a whole.
    let (results, incomplete) = f.app.checker.check_all(false).await.unwrap();
    assert!(results.is_empty());
    assert!(incomplete);
    let h = f.store.get_channel(a).await.unwrap().health.unwrap();
    assert_eq!(
        h.conclusive,
        Some(CheckOutcome::Ok),
        "a gateway outage never marks channels dead"
    );
    assert!(f.store.get_channel(b).await.unwrap().health.is_none());
}

#[tokio::test]
async fn a_check_answer_for_another_ref_is_refused() {
    let f = fixture().await;
    let a = f.channel("A", f.route, ACE1).await;
    f.gw.fail(Failure::WrongCheckRef);
    let r = f.app.checker.check(&[a], false).await.unwrap();
    assert_eq!(r[0].outcome, CheckOutcome::Unknown);
    let h = f.store.get_channel(a).await.unwrap().health.unwrap();
    assert_eq!((h.conclusive, h.reason.as_deref()), (None, Some("gateway_error")));
}

#[tokio::test]
async fn check_all_covers_enabled_channels_of_checkable_routes() {
    let f = fixture().await;
    let vk = f
        .store
        .create_route(
            "VK",
            RouteConfig::Gateway {
                base_url: f.gw.url.clone(),
                rule: "vk".into(),
            },
            true,
        )
        .await
        .unwrap()
        .id;
    let a = f.channel("A", f.route, ACE1).await;
    let b = f.channel("B", f.route, ACE2).await;
    f.channel("V", vk, "https://vk.com/video-1_2").await; // the vk rule has no checks
    f.channel("D", 1, "http://x/y.m3u8").await; // direct routes have no checks
    f.app
        .catalog
        .patch_channel(
            b,
            api_types::ChannelPatch {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (results, incomplete) = f.app.checker.check_all(false).await.unwrap();
    assert_eq!(results.iter().map(|r| r.channel_id).collect::<Vec<_>>(), [a]);
    assert!(!incomplete);

    // Skipped channels make the run incomplete (the scheduler then retries sooner).
    f.gw.set_check(GwOutcome::Skipped, None);
    let (_, incomplete) = f.app.checker.check_all(false).await.unwrap();
    assert!(incomplete);

    // A disabled route is not checked.
    f.store.update_route(f.route, None, Some(false)).await.unwrap();
    let (results, _) = f.app.checker.check_all(false).await.unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn revision_changes_resync_routes() {
    let f = fixture().await;
    let a = f.channel("A", f.route, ACE1).await;
    // A new route has never synced: the first poll records the revisions.
    f.app.checker.poll_revisions().await;
    let r = f.store.get_route(f.route).await.unwrap();
    assert_eq!(r.synced_play_revision.as_deref(), Some("1"));
    assert_eq!(r.last_resync.as_ref().map(|x| x.updated), Some(0));
    let matches = f.gw.calls("match");

    // Nothing changed: no resync.
    f.app.checker.poll_revisions().await;
    assert_eq!(f.gw.calls("match"), matches);

    // The gateway moved acexy: the next poll updates the play URL.
    f.gw.set_acexy("http://acexy-new:8080", "2");
    f.app.checker.poll_revisions().await;
    let c = f.store.get_channel(a).await.unwrap();
    assert_eq!(c.play_url, format!("http://acexy-new:8080/ace/getstream?id={ACE1}"));
    let r = f.store.get_route(f.route).await.unwrap();
    assert_eq!(r.synced_play_revision.as_deref(), Some("2"));
    let st = f.app.catalog.route_status(f.route).await.unwrap();
    assert_eq!(st.pending_channels, Some(0));
}
