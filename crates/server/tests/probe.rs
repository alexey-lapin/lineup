//! Health probes against a fake engine and a fake acexy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api_types::{ChannelCreate, IntegrationPatch, ProbeOutcome, SourceKind};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use lineup_server::probe::{ProbeTiming, Prober};
use lineup_server::{ChannelFilter, Event, Store};
use serde_json::json;

const OK: &str = "1111111111111111111111111111111111111111";
const DEAD: &str = "2222222222222222222222222222222222222222";
const WATCHED: &str = "3333333333333333333333333333333333333333";
const BROKEN: &str = "4444444444444444444444444444444444444444";
/// The first stat request fails, later ones report prebuffering.
const FLAKY: &str = "5555555555555555555555555555555555555555";
/// The first stat sample is neither healthy nor prebuffering; later ones prebuffer.
const WOBBLY: &str = "7777777777777777777777777777777777777777";
/// The engine returns no command_url.
const NOCMD: &str = "6666666666666666666666666666666666666666";

#[derive(Default)]
struct Fake {
    /// Content id → (status, peers) reported by the stat URL.
    swarm: HashMap<String, (String, u64)>,
    stops: Vec<String>,
    /// acexy: None = per-id status unsupported (400); Some(ids) = ids being watched.
    watched: Option<Vec<String>>,
    global_streams: u64,
    stat_calls: HashMap<String, u32>,
    /// Sessions started and not yet stopped, and the most seen at once.
    open: i32,
    max_open: i32,
    /// Every getstream request (sessions started).
    starts: u32,
}

type Shared = Arc<Mutex<Fake>>;

async fn getstream(State(f): State<Shared>, Query(q): Query<HashMap<String, String>>) -> axum::response::Response {
    assert_eq!(q["format"], "json");
    assert!(q["pid"].starts_with("lineup-probe-"));
    f.lock().unwrap().starts += 1;
    let id = &q["id"];
    if id == BROKEN {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if id != NOCMD {
        let mut f = f.lock().unwrap();
        f.open += 1;
        f.max_open = f.max_open.max(f.open);
    }
    if id == NOCMD {
        return axum::Json(json!({
            "response": { "stat_url": format!("http://127.0.0.1:1/ace/stat/{id}/s") },
            "error": null
        }))
        .into_response();
    }
    // The engine reports its own loopback address and port; lineup rebases these.
    axum::Json(json!({
        "response": {
            "stat_url": format!("http://127.0.0.1:1/ace/stat/{id}/s?x=1"),
            "command_url": format!("http://127.0.0.1:1/ace/cmd/{id}/s"),
        },
        "error": null
    }))
    .into_response()
}

async fn stat(State(f): State<Shared>, Path(id): Path<String>) -> axum::response::Response {
    let mut f = f.lock().unwrap();
    let n = {
        let c = f.stat_calls.entry(id.clone()).or_default();
        *c += 1;
        *c
    };
    if id == FLAKY && n == 1 {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    if id == WOBBLY && n == 1 {
        return axum::Json(json!({ "response": { "status": "check", "peers": 0 }, "error": null })).into_response();
    }
    let (status, peers) = f.swarm.get(&id).cloned().unwrap_or(("prebuf".into(), 0));
    axum::Json(json!({ "response": { "status": status, "peers": peers }, "error": null })).into_response()
}

async fn cmd(
    State(f): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> axum::response::Response {
    assert_eq!(q["method"], "stop");
    let mut f = f.lock().unwrap();
    f.stops.push(id);
    f.open -= 1;
    axum::Json(json!({ "response": "ok", "error": null })).into_response()
}

async fn acexy_status(State(f): State<Shared>, Query(q): Query<HashMap<String, String>>) -> axum::response::Response {
    let f = f.lock().unwrap();
    match (q.get("id"), &f.watched) {
        // acexy reports the stat URL of its own engine session (on the engine's loopback address).
        (Some(id), Some(w)) if w.contains(id) => axum::Json(json!({
            "clients": 1, "streams": 1, "stream_id": id,
            "stat_url": format!("http://127.0.0.1:1/ace/stat/{id}/s")
        }))
        .into_response(),
        (Some(_), Some(_)) => StatusCode::NOT_FOUND.into_response(),
        (Some(_), None) => StatusCode::BAD_REQUEST.into_response(),
        (None, _) => axum::Json(json!({ "streams": f.global_streams })).into_response(),
    }
}

async fn fake() -> (String, Shared) {
    let shared: Shared = Arc::new(Mutex::new(Fake {
        swarm: [
            (OK.to_string(), ("dl".to_string(), 7)),
            (DEAD.to_string(), ("prebuf".to_string(), 0)),
        ]
        .into(),
        watched: Some(vec![WATCHED.to_string()]),
        ..Default::default()
    }));
    let app = axum::Router::new()
        .route("/ace/getstream", get(getstream))
        .route("/ace/stat/{id}/s", get(stat))
        .route("/ace/cmd/{id}/s", get(cmd))
        .route("/ace/status", get(acexy_status))
        .route(
            "/server/api",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(q["method"], "get_media_files");
                axum::Json(json!({ "result": { "0": "Arena 1 HD.mkv" }, "error": null }))
            }),
        )
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), shared)
}

fn ace_config(engine: &str, acexy: Option<&str>) -> IntegrationPatch {
    IntegrationPatch::Acestream {
        engine_url: Some(engine.into()),
        acexy_url: Some(acexy.map(Into::into)),
        probe_interval_hours: None,
    }
}

const TIMING: ProbeTiming = ProbeTiming {
    poll_every: Duration::from_millis(10),
    give_up_after: Duration::from_millis(60),
    request_timeout: Duration::from_secs(2),
    first_run: Duration::from_millis(10),
    busy_retry: Duration::from_millis(50),
    tick: Duration::from_millis(10),
};

#[tokio::test]
async fn probes_record_health_and_emit_transitions() {
    let (base, fake) = fake().await;
    let store = Arc::new(Store::open_memory().await.unwrap());
    store
        .patch_integration(SourceKind::Acestream, ace_config(&base, Some(&base)))
        .await
        .unwrap();
    let mut ids = HashMap::new();
    for (name, ace) in [("ok", OK), ("dead", DEAD), ("watched", WATCHED), ("broken", BROKEN)] {
        let c = store
            .create_channel(ChannelCreate {
                name: name.into(),
                input: Some(ace.into()),
                ..Default::default()
            })
            .await
            .unwrap();
        ids.insert(name, c.id);
    }
    let vk = store
        .create_channel(ChannelCreate {
            name: "vk".into(),
            input: Some("https://vk.com/video-1_2".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut events = store.subscribe();
    let prober = Prober::new(store.clone(), reqwest::Client::new(), TIMING);

    let (results, stopped) = prober.probe_all(false).await.unwrap();
    assert!(!stopped);
    let by: HashMap<i64, ProbeOutcome> = results.iter().map(|r| (r.channel_id, r.outcome)).collect();
    assert_eq!(by[&ids["ok"]], ProbeOutcome::Ok);
    assert_eq!(by[&ids["dead"]], ProbeOutcome::Dead);
    assert_eq!(by[&ids["watched"]], ProbeOutcome::Skipped);
    assert_eq!(by[&ids["broken"]], ProbeOutcome::Unknown);
    assert!(!by.contains_key(&vk.id), "only acestream channels are probed");
    let mut stops = fake.lock().unwrap().stops.clone();
    stops.sort();
    assert_eq!(stops, [OK, DEAD], "every started session is stopped, nothing else");

    let mut got = Vec::new();
    while let Ok(e) = events.try_recv() {
        got.push(e.kind());
    }
    assert_eq!(got.iter().filter(|k| **k == "channel.unhealthy").count(), 1, "{got:?}");
    assert_eq!(got.iter().filter(|k| **k == "channel.probed").count(), 3, "{got:?}");

    let dead = store.get_channel(ids["dead"]).await.unwrap().health.unwrap();
    assert_eq!(
        (dead.conclusive, dead.last_attempt),
        (Some(ProbeOutcome::Dead), ProbeOutcome::Dead)
    );
    let watched = store.get_channel(ids["watched"]).await.unwrap().health.unwrap();
    assert_eq!(
        (watched.conclusive, watched.last_attempt),
        (None, ProbeOutcome::Skipped)
    );
    let filter = ChannelFilter {
        health: Some(ProbeOutcome::Dead),
        ..Default::default()
    };
    let dead_list = store.list_channels(&filter).await.unwrap();
    assert_eq!(dead_list.len(), 1);

    // Inconclusive attempts keep the conclusive result and emit no transition; recovery emits one.
    fake.lock().unwrap().swarm.remove(DEAD);
    let r = prober.probe(&[ids["dead"]], false).await.unwrap();
    assert_eq!(r[0].outcome, ProbeOutcome::Dead, "still prebuffering with no peers");
    fake.lock().unwrap().swarm.insert(DEAD.into(), ("dl".into(), 1));
    while events.try_recv().is_ok() {}
    let r = prober.probe(&[ids["dead"]], false).await.unwrap();
    assert_eq!(r[0].outcome, ProbeOutcome::Ok);
    match events.try_recv().unwrap() {
        Event::ChannelRecovered { channel_id, name } => assert_eq!((channel_id, name.as_str()), (ids["dead"], "dead")),
        e => panic!("unexpected {e:?}"),
    }

    // force skips the guard.
    let r = prober.probe(&[ids["watched"]], true).await.unwrap();
    assert_eq!(r[0].outcome, ProbeOutcome::Dead);

    // Non-acestream channels are refused.
    assert!(prober.probe(&[vk.id], false).await.is_err());

    // Without per-id status, active streams stop the run; nothing further is probed or recorded.
    {
        let mut f = fake.lock().unwrap();
        f.watched = None;
        f.global_streams = 1;
    }
    let before = store.get_channel(ids["ok"]).await.unwrap().health.unwrap();
    let (results, stopped) = prober.probe_all(false).await.unwrap();
    assert!(stopped);
    assert!(results.iter().all(|r| r.outcome == ProbeOutcome::Skipped));
    let after = store.get_channel(ids["ok"]).await.unwrap().health.unwrap();
    assert_eq!(before, after, "a stopped run records nothing");
    // And with none active, it runs.
    fake.lock().unwrap().global_streams = 0;
    let (_, stopped) = prober.probe_all(false).await.unwrap();
    assert!(!stopped);
}

#[tokio::test]
async fn probes_are_bound_to_their_channel_and_session() {
    let (base, fake) = fake().await;
    let store = Arc::new(Store::open_memory().await.unwrap());
    store
        .patch_integration(SourceKind::Acestream, ace_config(&base, Some(&base)))
        .await
        .unwrap();
    let add = |name: &str, ace: &str| {
        let store = store.clone();
        let (name, ace) = (name.to_string(), ace.to_string());
        async move {
            store
                .create_channel(ChannelCreate {
                    name,
                    input: Some(ace),
                    ..Default::default()
                })
                .await
                .unwrap()
        }
    };
    let prober = Prober::new(store.clone(), reqwest::Client::new(), TIMING);

    // A failed sample makes the window incomplete: unknown, not dead, and the error is kept.
    let flaky = add("flaky", FLAKY).await;
    let r = prober.probe(&[flaky.id], true).await.unwrap();
    assert_eq!(r[0].outcome, ProbeOutcome::Unknown, "{:?}", r[0].detail);
    assert!(r[0].detail.as_ref().unwrap()["errors"].is_array());

    // No command_url: the session cannot be stopped, so it is not measured.
    let nocmd = add("nocmd", NOCMD).await;
    let r = prober.probe(&[nocmd.id], true).await.unwrap();
    assert_eq!(r[0].outcome, ProbeOutcome::Unknown);
    assert_eq!(fake.lock().unwrap().stat_calls.get(NOCMD), None);

    // Changing the source drops health measured for the old one.
    let dead = add("dead", DEAD).await;
    prober.probe(&[dead.id], true).await.unwrap();
    assert!(store.get_channel(dead.id).await.unwrap().health.is_some());
    store
        .patch_channel(
            dead.id,
            api_types::ChannelPatch {
                input: Some("http://example.org/stream".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(store.get_channel(dead.id).await.unwrap().health.is_none());

    // A result for an older incarnation of the id is not recorded.
    let ok = add("ok", OK).await;
    let stale = lineup_server::store::ProbeIdentity {
        channel_id: ok.id,
        created_at: ok.created_at - chrono::Duration::seconds(1),
        ace: OK,
        engine_url: &base,
    };
    assert!(!store.record_probe(stale, ProbeOutcome::Dead, None).await.unwrap());
    assert!(store.get_channel(ok.id).await.unwrap().health.is_none());

    // One unexpected sample is enough to make the window inconclusive.
    let wobbly = add("wobbly", WOBBLY).await;
    let r = prober.probe(&[wobbly.id], true).await.unwrap();
    assert_eq!(r[0].outcome, ProbeOutcome::Unknown, "{:?}", r[0].detail);

    // A cancelled request still stops the session it started.
    let again = add("again", DEAD).await;
    fake.lock().unwrap().stops.clear();
    let _ = tokio::time::timeout(Duration::from_millis(25), prober.probe(&[again.id], true)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.lock().unwrap().stops, [DEAD]);
    assert!(
        store.get_channel(again.id).await.unwrap().health.is_some(),
        "and records its result"
    );

    // A run started after a cancelled one waits for it: sessions never overlap.
    fake.lock().unwrap().max_open = 0;
    let _ = tokio::time::timeout(Duration::from_millis(25), prober.probe(&[again.id], true)).await;
    prober.probe(&[ok.id, wobbly.id], true).await.unwrap();
    assert_eq!(fake.lock().unwrap().max_open, 1);
}

#[tokio::test]
async fn channel_info_reads_live_numbers_without_starting_sessions() {
    use api_types::LiveState;
    let (base, fake) = fake().await;
    let store = Arc::new(Store::open_memory().await.unwrap());
    store
        .patch_integration(SourceKind::Acestream, ace_config(&base, None))
        .await
        .unwrap();
    let add = |name: &str, ace: &str| {
        let store = store.clone();
        let (name, ace) = (name.to_string(), ace.to_string());
        async move {
            store
                .create_channel(ChannelCreate {
                    name,
                    input: Some(ace),
                    ..Default::default()
                })
                .await
                .unwrap()
                .id
        }
    };
    let watched = add("watched", WATCHED).await;
    let idle = add("idle", OK).await;
    let prober = Prober::new(store.clone(), reqwest::Client::new(), TIMING);
    let info = |id: i64, name: bool| {
        let store = store.clone();
        async move { lineup_server::live::channel_info(&store, id, name).await.unwrap() }
    };

    // Engine only: the name, but no live numbers and no checks (not even forced).
    let i = info(watched, true).await;
    let a = i.acestream.clone().unwrap();
    assert_eq!(a.engine_name.as_deref(), Some("Arena 1 HD"));
    assert_eq!(a.live.state, LiveState::Unavailable);
    assert!(!i.checks.available && i.checks.reason.unwrap().contains("acexy"));
    for force in [false, true] {
        let e = prober.probe(&[watched], force).await.unwrap_err();
        assert_eq!(e.code(), "conflict", "{e}");
    }
    assert_eq!(prober.probe_all(false).await.unwrap_err().code(), "conflict");

    // With acexy: live numbers of acexy's session while it is watched, rebased onto the engine.
    store
        .patch_integration(SourceKind::Acestream, ace_config(&base, Some(&base)))
        .await
        .unwrap();
    fake.lock().unwrap().swarm.insert(WATCHED.into(), ("dl".into(), 5));
    let i = info(watched, false).await;
    assert!(i.checks.available);
    let a = i.acestream.unwrap();
    assert_eq!(a.engine_name, None, "the name only when asked for");
    assert_eq!(a.live.state, LiveState::Active, "{:?}", a.live);
    assert_eq!((a.live.clients, a.live.peers), (Some(1), Some(5)));
    assert_eq!(a.live.status.as_deref(), Some("dl"));
    assert_eq!(
        info(idle, false).await.acestream.unwrap().live.state,
        LiveState::Inactive
    );

    // An acexy without per-stream status: unknown, not inferred from the global count.
    fake.lock().unwrap().watched = None;
    fake.lock().unwrap().global_streams = 1;
    let live = info(watched, false).await.acestream.unwrap().live;
    assert_eq!(live.state, LiveState::Unavailable);
    assert!(live.reason.unwrap().contains("per-stream"));

    // Nothing was started or stopped, and nothing was recorded as health.
    let (starts, stops) = {
        let f = fake.lock().unwrap();
        (f.starts, f.stops.len())
    };
    assert_eq!((starts, stops), (0, 0));
    assert!(store.get_channel(watched).await.unwrap().health.is_none());

    // Not an acestream channel: no acestream section.
    let d = add("direct", "http://example.org/x.m3u8").await;
    let i = info(d, true).await;
    assert!(i.acestream.is_none() && !i.checks.available);
}

#[tokio::test]
async fn results_through_a_replaced_engine_are_dropped() {
    let (base, _fake) = fake().await;
    let store = Arc::new(Store::open_memory().await.unwrap());
    store
        .patch_integration(SourceKind::Acestream, ace_config(&base, Some(&base)))
        .await
        .unwrap();
    let c = store
        .create_channel(ChannelCreate {
            name: "ok".into(),
            input: Some(OK.into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let measured = lineup_server::store::ProbeIdentity {
        channel_id: c.id,
        created_at: c.created_at,
        ace: OK,
        engine_url: "http://old-engine:6878",
    };
    assert!(!store.record_probe(measured, ProbeOutcome::Ok, None).await.unwrap());
    assert!(store.get_channel(c.id).await.unwrap().health.is_none());
}
