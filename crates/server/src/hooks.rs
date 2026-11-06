//! Hook dispatcher (§8): one task per enabled hook, fed by the store's events.
//!
//! Each worker collects matching events until none has arrived for `debounce_secs` (or the window
//! has been open for [`MAX_WAIT_FACTOR`] times that), then delivers them in one envelope, retrying
//! with backoff. Jellyfin hooks also schedule one follow-up refresh `followup_minutes` after the
//! latest delivery. Workers are rebuilt whenever hooks change.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use api_types::{DeliveryTrigger, Hook, HookConfig, HookDelivery};
use chrono::{DateTime, Utc};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::error::{Error, Result};
use crate::store::{Event, NewDelivery, Store};

/// A debounce window never stays open longer than this many debounce periods.
const MAX_WAIT_FACTOR: u32 = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Time units, shortened in tests.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Length of one `debounce_secs` unit.
    pub second: Duration,
    /// Length of one `followup_minutes` unit.
    pub minute: Duration,
    /// Waits before the retries (3 retries after the first attempt).
    pub backoff: [Duration; 3],
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            second: Duration::from_secs(1),
            minute: Duration::from_secs(60),
            backoff: [Duration::from_secs(2), Duration::from_secs(10), Duration::from_secs(30)],
        }
    }
}

/// What the supervisor sends a worker.
enum Msg {
    Event(Stamped),
    /// The hook was edited (and is still enabled).
    Config(Box<Hook>),
}

struct Worker {
    /// The configuration last sent to the worker; used for routing.
    hook: Hook,
    tx: mpsc::UnboundedSender<Msg>,
    task: tokio::task::JoinHandle<()>,
}

type Workers = HashMap<i64, Worker>;

#[derive(Debug, Clone)]
struct Stamped {
    event: Event,
    at: DateTime<Utc>,
}

pub struct Hooks {
    store: Arc<Store>,
    client: reqwest::Client,
    timing: Timing,
    /// The hooks version the running workers reflect.
    applied: tokio::sync::watch::Sender<Option<u64>>,
}

/// Result of one HTTP attempt.
struct Attempt {
    ok: bool,
    http_code: Option<u16>,
    error: Option<String>,
}

impl Hooks {
    pub fn new(store: Arc<Store>, client: reqwest::Client, timing: Timing) -> Arc<Self> {
        Arc::new(Hooks {
            store,
            client,
            timing,
            applied: tokio::sync::watch::Sender::new(None),
        })
    }

    /// Start the dispatcher: it runs until the store's event channel closes.
    pub fn spawn(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        // Subscribe before returning so no event emitted after this call is missed.
        let events = me.store.subscribe();
        let watch = me.store.watch_hooks();
        tokio::spawn(async move { me.supervise(events, watch).await })
    }

    /// Wait until the dispatcher runs the current hook configuration (tests, diagnostics).
    pub async fn synced(&self) {
        let want = *self.store.watch_hooks().borrow();
        let mut rx = self.applied.subscribe();
        let _ = rx.wait_for(|v| v.is_some_and(|v| v >= want)).await;
    }

    async fn supervise(
        self: Arc<Self>,
        mut events: tokio::sync::broadcast::Receiver<Event>,
        mut watch: tokio::sync::watch::Receiver<u64>,
    ) {
        let mut workers: Workers = HashMap::new();
        watch.borrow_and_update();
        self.reload(&mut workers).await;
        loop {
            tokio::select! {
                // Configuration first: a change committed before an event was emitted is applied
                // before that event is routed.
                biased;
                r = watch.changed() => {
                    if r.is_err() {
                        break;
                    }
                    watch.borrow_and_update();
                    self.reload(&mut workers).await;
                }
                r = events.recv() => match r {
                    Ok(event) => {
                        // A hook change and an event can become ready together; never route with a
                        // configuration older than the event.
                        if watch.has_changed().unwrap_or(false) {
                            watch.borrow_and_update();
                            self.reload(&mut workers).await;
                        }
                        let stamped = Stamped { event, at: Utc::now() };
                        for w in workers.values() {
                            if w.hook.events.iter().any(|e| e == stamped.event.kind()) {
                                let _ = w.tx.send(Msg::Event(stamped.clone()));
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("hook dispatcher missed {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
            }
        }
        for w in workers.into_values() {
            w.task.abort();
        }
    }

    /// Bring workers in line with the stored hooks. Changed hooks get their new configuration
    /// without losing pending events or a scheduled follow-up; deleted or disabled hooks' workers
    /// are stopped (dropping what they had pending), new ones started.
    async fn reload(self: &Arc<Self>, workers: &mut Workers) {
        let version = *self.store.watch_hooks().borrow();
        let hooks = match self.store.hooks_with_secrets().await {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("loading hooks: {e}");
                return;
            }
        };
        let wanted: HashMap<i64, Hook> = hooks.into_iter().filter(|h| h.enabled).map(|h| (h.id, h)).collect();
        workers.retain(|id, w| {
            let keep = wanted.contains_key(id);
            if !keep {
                w.task.abort();
            }
            keep
        });
        for (id, hook) in wanted {
            match workers.get_mut(&id) {
                Some(w) => {
                    if w.hook != hook {
                        let _ = w.tx.send(Msg::Config(Box::new(hook.clone())));
                        w.hook = hook;
                    }
                }
                None => {
                    let (tx, rx) = mpsc::unbounded_channel();
                    let me = self.clone();
                    let h = hook.clone();
                    let task = tokio::spawn(async move { me.worker(h, rx).await });
                    workers.insert(id, Worker { hook, tx, task });
                }
            }
        }
        self.applied.send_replace(Some(version));
    }

    async fn worker(self: Arc<Self>, mut hook: Hook, mut rx: mpsc::UnboundedReceiver<Msg>) {
        let mut pending: Vec<Stamped> = Vec::new();
        let mut window_start: Option<Instant> = None;
        let mut last_event: Option<Instant> = None;
        let mut followup_at: Option<Instant> = None;
        let mut task_id: Option<String> = None;
        loop {
            let debounce = self.timing.second * hook.debounce_secs;
            let followup = match &hook.config {
                HookConfig::Jellyfin { followup_minutes, .. } if *followup_minutes > 0 => {
                    Some(self.timing.minute * *followup_minutes)
                }
                _ => None,
            };
            if followup.is_none() {
                followup_at = None;
            }
            // Recomputed every turn so configuration changes apply to an open window.
            let fire_at = match (window_start, last_event) {
                (Some(start), Some(last)) => Some((last + debounce).min(start + debounce * MAX_WAIT_FACTOR)),
                _ => None,
            };
            let far = Instant::now() + Duration::from_secs(365 * 24 * 3600);
            tokio::select! {
                msg = rx.recv() => match msg {
                    None => return,
                    Some(Msg::Config(h)) => {
                        if h.config != hook.config {
                            task_id = None;
                        }
                        hook = *h;
                    }
                    Some(Msg::Event(ev)) => {
                        let now = Instant::now();
                        if pending.is_empty() {
                            window_start = Some(now);
                        }
                        last_event = Some(now);
                        pending.push(ev);
                    }
                },
                _ = tokio::time::sleep_until(fire_at.unwrap_or(far)), if fire_at.is_some() => {
                    window_start = None;
                    last_event = None;
                    let batch = std::mem::take(&mut pending);
                    let body = envelope(&batch);
                    let types = event_types(&batch);
                    self.deliver(&hook, &mut task_id, &body, &types, DeliveryTrigger::Events).await;
                    // Later deliveries move the single follow-up later.
                    if let Some(f) = followup {
                        followup_at = Some(Instant::now() + f);
                    }
                }
                _ = tokio::time::sleep_until(followup_at.unwrap_or(far)), if followup_at.is_some() => {
                    followup_at = None;
                    let body = envelope(&[]);
                    self.deliver(&hook, &mut task_id, &body, &[], DeliveryTrigger::Followup).await;
                }
            }
        }
    }

    /// Deliver with retries, logging every attempt.
    async fn deliver(
        &self,
        hook: &Hook,
        task_id: &mut Option<String>,
        body: &serde_json::Value,
        types: &[String],
        trigger: DeliveryTrigger,
    ) {
        for attempt in 1..=(self.timing.backoff.len() as u32 + 1) {
            let a = self.send(hook, task_id, body).await;
            let ok = a.ok;
            let what = if types.is_empty() {
                format!("{trigger:?}").to_lowercase()
            } else {
                types.join(", ")
            };
            if ok {
                tracing::info!(
                    "hook {} ({}): delivered {what} (HTTP {}, attempt {attempt})",
                    hook.id,
                    hook.name,
                    a.http_code.unwrap_or(0)
                );
            } else {
                tracing::warn!(
                    "hook {} ({}) attempt {attempt} for {what}: {}",
                    hook.id,
                    hook.name,
                    a.error.as_deref().unwrap_or("?")
                );
            }
            self.log(hook.id, types, trigger, attempt, a).await;
            if ok {
                return;
            }
            match self.timing.backoff.get(attempt as usize - 1) {
                Some(wait) => tokio::time::sleep(*wait).await,
                None => tracing::warn!("hook {} ({}): giving up on {what}", hook.id, hook.name),
            }
        }
    }

    async fn log(
        &self,
        hook_id: i64,
        types: &[String],
        trigger: DeliveryTrigger,
        attempt: u32,
        a: Attempt,
    ) -> Option<HookDelivery> {
        let d = NewDelivery {
            hook_id,
            events: types,
            trigger,
            attempt,
            ok: a.ok,
            http_code: a.http_code,
            error: a.error,
        };
        match self.store.record_delivery(d).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("recording hook delivery: {e}");
                None
            }
        }
    }

    /// The test action: one attempt with a synthetic `hook.test` event, logged and returned.
    pub async fn test(&self, id: i64) -> Result<HookDelivery> {
        let hook = self.store.hook_with_secrets(id).await?;
        let body = json!({
            "events": [{ "type": "hook.test", "at": Utc::now(), "payload": {} }],
            "window_start": Utc::now(),
            "window_end": Utc::now(),
        });
        let types = vec!["hook.test".to_string()];
        let a = self.send(&hook, &mut None, &body).await;
        // The row this attempt inserted, not merely the latest one.
        self.log(id, &types, DeliveryTrigger::Test, 1, a)
            .await
            .ok_or_else(|| Error::NotFound(format!("hook {id} was deleted during the test")))
    }

    async fn send(&self, hook: &Hook, task_id: &mut Option<String>, body: &serde_json::Value) -> Attempt {
        let res = match &hook.config {
            HookConfig::Webhook { method, url, headers } => {
                let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::POST);
                let mut req = self.client.request(method, url).json(body).timeout(REQUEST_TIMEOUT);
                for (k, v) in headers {
                    req = req.header(k, v);
                }
                req.send().await.map_err(|e| e.to_string())
            }
            HookConfig::Jellyfin {
                base_url,
                api_key,
                task_id: configured,
                ..
            } => {
                let id = match configured.clone().or_else(|| task_id.clone()) {
                    Some(id) => id,
                    None => match self.find_refresh_task(base_url, api_key).await {
                        Ok(id) => {
                            *task_id = Some(id.clone());
                            id
                        }
                        Err(e) => {
                            return Attempt {
                                ok: false,
                                http_code: None,
                                error: Some(e),
                            };
                        }
                    },
                };
                self.client
                    .post(format!("{base_url}/ScheduledTasks/Running/{id}"))
                    .header(reqwest::header::AUTHORIZATION, jellyfin_auth(api_key))
                    .timeout(REQUEST_TIMEOUT)
                    .send()
                    .await
                    .map_err(|e| e.to_string())
            }
        };
        match res {
            Ok(r) => {
                let code = r.status().as_u16();
                let ok = r.status().is_success();
                // A stale cached task id (e.g. Jellyfin reinstalled) is looked up again next time.
                if r.status() == reqwest::StatusCode::NOT_FOUND {
                    *task_id = None;
                }
                Attempt {
                    ok,
                    http_code: Some(code),
                    error: (!ok).then(|| format!("HTTP {code}")),
                }
            }
            Err(e) => Attempt {
                ok: false,
                http_code: None,
                error: Some(e),
            },
        }
    }

    /// `GET {base}/ScheduledTasks` and pick the task with key `RefreshGuide`.
    async fn find_refresh_task(&self, base_url: &str, api_key: &str) -> std::result::Result<String, String> {
        let res = self
            .client
            .get(format!("{base_url}/ScheduledTasks"))
            .header(reqwest::header::AUTHORIZATION, jellyfin_auth(api_key))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("listing scheduled tasks: {e}"))?;
        if !res.status().is_success() {
            return Err(format!("listing scheduled tasks: HTTP {}", res.status()));
        }
        let tasks: Vec<serde_json::Value> = res.json().await.map_err(|e| format!("listing scheduled tasks: {e}"))?;
        tasks
            .iter()
            .find(|t| t.get("Key").and_then(|k| k.as_str()) == Some("RefreshGuide"))
            .and_then(|t| t.get("Id").and_then(|i| i.as_str()))
            .map(str::to_string)
            .ok_or_else(|| "Jellyfin has no scheduled task with key RefreshGuide".into())
    }
}

fn jellyfin_auth(api_key: &str) -> String {
    format!("MediaBrowser Token=\"{api_key}\"")
}

fn event_types(batch: &[Stamped]) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for s in batch {
        let k = s.event.kind().to_string();
        if !v.contains(&k) {
            v.push(k);
        }
    }
    v
}

/// `{events: [{type, at, payload}], window_start, window_end}`.
fn envelope(batch: &[Stamped]) -> serde_json::Value {
    let now = Utc::now();
    json!({
        "events": batch
            .iter()
            .map(|s| json!({ "type": s.event.kind(), "at": s.at, "payload": s.event.payload() }))
            .collect::<Vec<_>>(),
        "window_start": batch.first().map_or(now, |s| s.at),
        "window_end": batch.last().map_or(now, |s| s.at),
    })
}
