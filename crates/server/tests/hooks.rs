//! Hook dispatch against local receivers, with shortened time units.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use api_types::{ChannelCreate, DeliveryTrigger, HookConfig, HookCreate, HookPatch, PlaylistCreate, SECRET_MASK};
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::IntoResponse;
use lineup_server::Store;
use lineup_server::hooks::{Hooks, Timing};

#[derive(Debug, Clone)]
struct Req {
    method: Method,
    path: String,
    headers: HeaderMap,
    body: serde_json::Value,
}

#[derive(Default)]
struct Receiver {
    requests: Vec<Req>,
    /// Status codes to answer with, in order; 200 (204 for Jellyfin runs) once exhausted.
    script: Vec<u16>,
}

type Shared = Arc<Mutex<Receiver>>;

async fn handle(
    State(r): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> axum::response::Response {
    let mut r = r.lock().unwrap();
    r.requests.push(Req {
        method: method.clone(),
        path: uri.path().to_string(),
        headers,
        body: serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
    });
    if uri.path() == "/ScheduledTasks" && method == Method::GET {
        return axum::Json(serde_json::json!([
            {"Key": "RefreshLibrary", "Id": "lib"},
            {"Key": "RefreshGuide", "Id": "guide-task"}
        ]))
        .into_response();
    }
    let code = if r.script.is_empty() { 204 } else { r.script.remove(0) };
    StatusCode::from_u16(code).unwrap().into_response()
}

async fn receiver(script: Vec<u16>) -> (String, Shared) {
    let shared: Shared = Arc::new(Mutex::new(Receiver {
        requests: vec![],
        script,
    }));
    let app = axum::Router::new().fallback(handle).with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), shared)
}

const TIMING: Timing = Timing {
    second: Duration::from_millis(20),
    backoff: [Duration::from_millis(20); 3],
};

async fn setup() -> (Arc<Store>, Arc<Hooks>) {
    let store = Arc::new(Store::open_memory().await.unwrap());
    let hooks = Hooks::new(store.clone(), reqwest::Client::new(), TIMING);
    hooks.spawn();
    (store, hooks)
}

async fn wait_for(shared: &Shared, n: usize) -> Vec<Req> {
    for _ in 0..200 {
        let reqs = shared.lock().unwrap().requests.clone();
        if reqs.len() >= n {
            return reqs;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {n} requests, got {:?}", shared.lock().unwrap().requests);
}

async fn make_changes(store: &Arc<Store>) {
    store
        .create_playlist(PlaylistCreate {
            slug: "jf".into(),
            name: None,
        })
        .await
        .unwrap();
    // A direct route never calls a gateway.
    let catalog = lineup_server::catalog::Catalog::new(store.clone(), lineup_server::gateway::Gateway::default());
    catalog
        .create_channel(ChannelCreate {
            name: "A".into(),
            group_title: None,
            tvg_id: None,
            tvg_logo: None,
            route_id: 1,
            input: "http://h/a.m3u8".into(),
            enabled: true,
            playlists: vec!["jf".into()],
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn webhook_debounce_envelope_and_secrets() {
    let (url, rx) = receiver(vec![]).await;
    let (store, hooks) = setup().await;
    let hook = store
        .create_hook(HookCreate {
            name: "notify".into(),
            config: HookConfig::Webhook {
                method: "post".into(),
                url: format!("{url}/notify"),
                headers: [("X-Token".to_string(), "s3cret".to_string())].into(),
            },
            events: None,
            debounce_secs: Some(5),
            enabled: true,
        })
        .await
        .unwrap();
    let HookConfig::Webhook { headers, method, .. } = &hook.config;
    assert_eq!(headers["X-Token"], SECRET_MASK, "secrets are masked");
    assert_eq!(method, "POST");
    assert_eq!(hook.events.len(), 4, "webhooks get every event by default");

    // Sending the masked value back keeps the secret.
    store
        .patch_hook(
            hook.id,
            HookPatch {
                name: Some("notify me".into()),
                config: Some(hook.config.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    hooks.synced().await;

    make_changes(&store).await;
    let reqs = wait_for(&rx, 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rx.lock().unwrap().requests.len(),
        1,
        "both changes go out in one delivery"
    );
    let r = &reqs[0];
    assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/notify"));
    assert_eq!(r.headers["x-token"], "s3cret");
    let events = r.body["events"].as_array().unwrap();
    assert_eq!(events.len(), 2, "{}", r.body);
    assert_eq!(events[0]["type"], "playlist.changed");
    assert_eq!(events[0]["payload"]["slugs"], serde_json::json!(["jf"]));
    assert!(r.body["window_start"].is_string() && r.body["window_end"].is_string());

    let log = store.hook_deliveries(hook.id, 10).await.unwrap();
    assert_eq!(log.len(), 1);
    assert!(log[0].ok);
    assert_eq!(log[0].trigger, DeliveryTrigger::Events);
    assert_eq!(log[0].events, ["playlist.changed"]);

    // Validation.
    assert!(
        store
            .create_hook(HookCreate {
                name: "x".into(),
                config: HookConfig::Webhook {
                    method: "GET".into(),
                    url: url.clone(),
                    headers: Default::default()
                },
                events: None,
                debounce_secs: None,
                enabled: true,
            })
            .await
            .is_err()
    );
    assert!(
        store
            .patch_hook(
                hook.id,
                HookPatch {
                    events: Some(vec!["nope".into()]),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn retries_until_accepted() {
    let (url, rx) = receiver(vec![500, 502, 200]).await;
    let (store, hooks) = setup().await;
    let hook = store
        .create_hook(HookCreate {
            name: "flaky".into(),
            config: HookConfig::Webhook {
                method: "POST".into(),
                url,
                headers: Default::default(),
            },
            events: Some(vec!["playlist.changed".into()]),
            debounce_secs: Some(0),
            enabled: true,
        })
        .await
        .unwrap();
    hooks.synced().await;
    make_changes(&store).await;
    wait_for(&rx, 3).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let log = store.hook_deliveries(hook.id, 10).await.unwrap();
    let attempts: Vec<_> = log.iter().rev().map(|d| (d.attempt, d.ok, d.http_code)).collect();
    // Two quick changes may be one or two deliveries; the first delivery took three attempts.
    assert_eq!(
        &attempts[..3],
        [(1, false, Some(500)), (2, false, Some(502)), (3, true, Some(200))]
    );
}

#[tokio::test]
async fn jellyfin_recipe_is_a_plain_webhook() {
    let (url, rx) = receiver(vec![]).await;
    let (store, hooks) = setup().await;
    let hook = store
        .create_hook(HookCreate {
            name: "jellyfin".into(),
            config: HookConfig::Webhook {
                method: "POST".into(),
                url: format!("{url}/ScheduledTasks/Running/guide-task"),
                headers: [("Authorization".to_string(), "MediaBrowser Token=\"key123\"".to_string())].into(),
            },
            events: Some(vec!["playlist.changed".into(), "epg.updated".into()]),
            // 10 units (200 ms here): both changes land in one window even on a slow machine.
            debounce_secs: Some(10),
            enabled: true,
        })
        .await
        .unwrap();
    hooks.synced().await;
    make_changes(&store).await;

    // One POST to the task URL with the token, and nothing after it (no follow-up resend).
    let reqs = wait_for(&rx, 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rx.lock().unwrap().requests.len(), 1);
    assert_eq!(
        (reqs[0].method.as_str(), reqs[0].path.as_str()),
        ("POST", "/ScheduledTasks/Running/guide-task")
    );
    assert_eq!(reqs[0].headers["authorization"], "MediaBrowser Token=\"key123\"");
    let log = store.hook_deliveries(hook.id, 10).await.unwrap();
    let triggers: Vec<_> = log.iter().map(|d| d.trigger).collect();
    assert_eq!(triggers, [DeliveryTrigger::Events]);

    // The test action: one attempt, logged.
    let d = hooks.test(hook.id).await.unwrap();
    assert!(d.ok);
    assert_eq!((d.trigger, d.http_code), (DeliveryTrigger::Test, Some(204)));

    // Disabled hooks receive nothing.
    store
        .patch_hook(
            hook.id,
            HookPatch {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    hooks.synced().await;
    let before = rx.lock().unwrap().requests.len();
    store.refresh(vec![]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rx.lock().unwrap().requests.len(), before);
}

#[tokio::test]
async fn events_right_after_creation_are_delivered() {
    let (url, rx) = receiver(vec![]).await;
    let (store, hooks) = setup().await;
    hooks.synced().await;
    // No synced() between creating the hook and the change: the change must still be delivered.
    store
        .create_hook(HookCreate {
            name: "w".into(),
            config: HookConfig::Webhook {
                method: "POST".into(),
                url,
                headers: Default::default(),
            },
            events: None,
            debounce_secs: Some(0),
            enabled: true,
        })
        .await
        .unwrap();
    make_changes(&store).await;
    wait_for(&rx, 1).await;
}

#[tokio::test]
async fn edits_keep_pending_events() {
    let (url, rx) = receiver(vec![]).await;
    let (store, hooks) = setup().await;
    let hook = store
        .create_hook(HookCreate {
            name: "w".into(),
            config: HookConfig::Webhook {
                method: "POST".into(),
                url,
                headers: Default::default(),
            },
            events: None,
            debounce_secs: Some(5),
            enabled: true,
        })
        .await
        .unwrap();
    hooks.synced().await;
    make_changes(&store).await;
    // Renamed while the window is open: the events are still delivered, once.
    let rename = HookPatch {
        name: Some("a".into()),
        ..Default::default()
    };
    store.patch_hook(hook.id, rename).await.unwrap();
    hooks.synced().await;
    wait_for(&rx, 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let log = store.hook_deliveries(hook.id, 10).await.unwrap();
    let triggers: Vec<_> = log.iter().map(|d| d.trigger).collect();
    assert_eq!(triggers, [DeliveryTrigger::Events]);
}

#[tokio::test]
async fn concurrent_delivery_logs_are_all_kept() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("db")).await.unwrap());
    let hook = store
        .create_hook(HookCreate {
            name: "w".into(),
            config: HookConfig::Webhook {
                method: "POST".into(),
                url: "http://127.0.0.1:1/".into(),
                headers: Default::default(),
            },
            events: None,
            debounce_secs: None,
            enabled: true,
        })
        .await
        .unwrap();
    let rec = |attempt| {
        let store = store.clone();
        async move {
            store
                .record_delivery(lineup_server::store::NewDelivery {
                    hook_id: hook.id,
                    events: &[],
                    trigger: DeliveryTrigger::Events,
                    attempt,
                    ok: true,
                    http_code: Some(200),
                    error: None,
                })
                .await
        }
    };
    let results = futures_util::future::join_all((1..=8).map(rec)).await;
    let ids: Vec<i64> = results.into_iter().map(|r| r.unwrap().unwrap().id).collect();
    assert_eq!(ids.len(), 8);
    assert_eq!(store.hook_deliveries(hook.id, 50).await.unwrap().len(), 8);
}
