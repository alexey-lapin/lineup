//! Health probes against a fake engine and a fake acexy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api_types::{ChannelCreate, ProbeOutcome, SettingsPatch};
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

#[derive(Default)]
struct Fake {
    /// Content id → (status, peers) reported by the stat URL.
    swarm: HashMap<String, (String, u64)>,
    stops: Vec<String>,
    /// acexy: None = per-id status unsupported (400); Some(ids) = ids being watched.
    watched: Option<Vec<String>>,
    global_streams: u64,
}

type Shared = Arc<Mutex<Fake>>;

async fn getstream(State(f): State<Shared>, Query(q): Query<HashMap<String, String>>) -> axum::response::Response {
    assert_eq!(q["format"], "json");
    assert!(q["pid"].starts_with("lineup-probe-"));
    let id = &q["id"];
    if id == BROKEN {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let _ = f;
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
    let f = f.lock().unwrap();
    let (status, peers) = f.swarm.get(&id).cloned().unwrap_or(("prebuf".into(), 0));
    axum::Json(json!({ "response": { "status": status, "peers": peers }, "error": null })).into_response()
}

async fn cmd(
    State(f): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> axum::response::Response {
    assert_eq!(q["method"], "stop");
    f.lock().unwrap().stops.push(id);
    axum::Json(json!({ "response": "ok", "error": null })).into_response()
}

async fn acexy_status(State(f): State<Shared>, Query(q): Query<HashMap<String, String>>) -> axum::response::Response {
    let f = f.lock().unwrap();
    match (q.get("id"), &f.watched) {
        (Some(id), Some(w)) if w.contains(id) => axum::Json(json!({ "clients": 1 })).into_response(),
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
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), shared)
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
        .patch_settings(SettingsPatch {
            engine_base: Some(base.clone()),
            acexy_base: Some(base.clone()),
            ..Default::default()
        })
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
