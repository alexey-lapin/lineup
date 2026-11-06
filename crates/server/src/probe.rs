//! Health probes for acestream channels (§9). They measure swarm health through the engine, not
//! whether playback works.
//!
//! A probe starts a session with `format=json` and a unique pid, polls its stat URL (rebased onto
//! `engine_base`, since the engine reports its own loopback address) and always stops the session.
//! Before each probe an activity guard asks acexy whether the stream is being watched.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use api_types::{ProbeOutcome, ProbeResult, Source, SourceKind};
use serde_json::{Value, json};

use crate::error::{Result, invalid};
use crate::store::{ChannelFilter, Store};

/// Timing, shortened in tests.
#[derive(Debug, Clone, Copy)]
pub struct ProbeTiming {
    pub poll_every: Duration,
    pub give_up_after: Duration,
    pub request_timeout: Duration,
    /// Delay before the first scheduled run.
    pub first_run: Duration,
    /// Retry delay after a run stopped because streams were active.
    pub busy_retry: Duration,
    /// How often the scheduler checks whether a run is due.
    pub tick: Duration,
}

impl Default for ProbeTiming {
    fn default() -> Self {
        ProbeTiming {
            poll_every: Duration::from_secs(2),
            give_up_after: Duration::from_secs(20),
            request_timeout: Duration::from_secs(5),
            first_run: Duration::from_secs(600),
            busy_retry: Duration::from_secs(1800),
            tick: Duration::from_secs(60),
        }
    }
}

/// What the activity guard decided for one channel.
#[derive(Debug, PartialEq)]
enum Guard {
    Go,
    /// This stream is being watched: record `skipped`.
    Skip(String),
    /// Stop the whole run (streams active, or status unreadable).
    Stop(String),
}

pub struct Prober {
    store: Arc<Store>,
    client: reqwest::Client,
    timing: ProbeTiming,
    run_lock: tokio::sync::Mutex<()>,
}

impl Prober {
    pub fn new(store: Arc<Store>, client: reqwest::Client, timing: ProbeTiming) -> Arc<Self> {
        Arc::new(Prober {
            store,
            client,
            timing,
            run_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Probe the given channels (they must be acestream) now, one at a time, and return the results.
    /// Runs never overlap. Unless `force`, the activity guard applies; if it stops the run, the
    /// remaining channels are reported (not recorded) as skipped.
    pub async fn probe(&self, ids: &[i64], force: bool) -> Result<Vec<ProbeResult>> {
        let mut targets = Vec::new();
        for id in ids {
            let c = self.store.get_channel(*id).await?;
            match c.source {
                Source::Acestream { id: ace } => targets.push((c.id, ace)),
                _ => return Err(invalid(format!("channel {} is not an acestream channel", c.id))),
            }
        }
        Ok(self.run(targets, force).await.0)
    }

    /// All enabled acestream channels. Returns whether the guard stopped the run early.
    pub async fn probe_all(&self, force: bool) -> Result<(Vec<ProbeResult>, bool)> {
        let channels = self
            .store
            .list_channels(&ChannelFilter {
                kind: Some(SourceKind::Acestream),
                ..Default::default()
            })
            .await?;
        let targets = channels
            .into_iter()
            .filter(|c| c.enabled)
            .filter_map(|c| match c.source {
                Source::Acestream { id } => Some((c.id, id)),
                _ => None,
            })
            .collect();
        Ok(self.run(targets, force).await)
    }

    pub fn spawn_all(self: &Arc<Self>, force: bool) {
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.probe_all(force).await {
                tracing::warn!("probe run: {e}");
            }
        });
    }

    /// Boot-relative schedule over all channels; off while `probe_interval_hours` is 0.
    pub fn spawn_scheduler(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(me.timing.first_run).await;
            let mut next_due: Option<SystemTime> = None; // None: due now
            loop {
                let hours = me.store.settings().await.map(|s| s.probe_interval_hours).unwrap_or(0);
                let due = hours > 0 && next_due.is_none_or(|t| SystemTime::now() >= t);
                if due {
                    match me.probe_all(false).await {
                        Ok((_, stopped)) if stopped => {
                            tracing::info!("probe run stopped by the activity guard; retrying later");
                            next_due = Some(SystemTime::now() + me.timing.busy_retry);
                        }
                        Ok(_) => next_due = Some(SystemTime::now() + Duration::from_secs(u64::from(hours) * 3600)),
                        Err(e) => {
                            tracing::warn!("scheduled probe run: {e}");
                            next_due = Some(SystemTime::now() + me.timing.busy_retry);
                        }
                    }
                }
                tokio::time::sleep(me.timing.tick).await;
            }
        });
    }

    async fn run(&self, targets: Vec<(i64, String)>, force: bool) -> (Vec<ProbeResult>, bool) {
        let _lock = self.run_lock.lock().await;
        let mut results = Vec::new();
        let mut stopped: Option<String> = None;
        for (channel_id, ace) in targets {
            if let Some(reason) = &stopped {
                results.push(ProbeResult {
                    channel_id,
                    outcome: ProbeOutcome::Skipped,
                    detail: Some(json!({ "reason": reason, "recorded": false })),
                });
                continue;
            }
            let settings = match self.store.settings().await {
                Ok(s) => s,
                Err(e) => {
                    stopped = Some(format!("reading settings: {e}"));
                    continue;
                }
            };
            let (outcome, detail) = if force {
                self.probe_one(&settings.engine_base, &ace).await
            } else {
                match self.guard(&settings.acexy_base, &ace).await {
                    Guard::Go => self.probe_one(&settings.engine_base, &ace).await,
                    Guard::Skip(why) => (ProbeOutcome::Skipped, json!({ "reason": why })),
                    Guard::Stop(why) => {
                        results.push(ProbeResult {
                            channel_id,
                            outcome: ProbeOutcome::Skipped,
                            detail: Some(json!({ "reason": why, "recorded": false })),
                        });
                        stopped = Some(why);
                        continue;
                    }
                }
            };
            if let Err(e) = self.store.record_probe(channel_id, outcome, Some(detail.clone())).await {
                tracing::warn!("recording probe of channel {channel_id}: {e}");
            }
            results.push(ProbeResult {
                channel_id,
                outcome,
                detail: Some(detail),
            });
        }
        (results, stopped.is_some())
    }

    /// Ask acexy whether `id` is being watched. A per-id status (404 = not active) decides for that
    /// id; when it is unavailable, any active stream stops the run; when nothing can be read, the
    /// run stops too (fail closed).
    async fn guard(&self, acexy_base: &str, id: &str) -> Guard {
        let base = acexy_base.trim_end_matches('/');
        let res = self
            .client
            .get(format!("{base}/ace/status"))
            .query(&[("id", id)])
            .timeout(self.timing.request_timeout)
            .send()
            .await;
        if let Ok(r) = res {
            let status = r.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                return Guard::Go;
            }
            if status.is_success() {
                let body: Value = r.json().await.unwrap_or(Value::Null);
                return match active_count(&body) {
                    Some(0) => Guard::Go,
                    Some(n) => Guard::Skip(format!("acexy reports {n} client(s) on this stream")),
                    None => Guard::Skip("acexy status for this stream is not understood".into()),
                };
            }
            // Other 4xx: no per-id status in this acexy version; 5xx: unreadable. Both fall back.
        }
        let res = self
            .client
            .get(format!("{base}/ace/status"))
            .timeout(self.timing.request_timeout)
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => {
                let body: Value = r.json().await.unwrap_or(Value::Null);
                match body.get("streams").and_then(Value::as_u64) {
                    Some(0) => Guard::Go,
                    Some(n) => Guard::Stop(format!("acexy reports {n} active stream(s)")),
                    None => Guard::Stop("acexy status has no stream count".into()),
                }
            }
            Ok(r) => Guard::Stop(format!("acexy status: HTTP {}", r.status())),
            Err(e) => Guard::Stop(format!("acexy status unreadable: {e}")),
        }
    }

    /// Start a session, poll its stats, always stop it.
    async fn probe_one(&self, engine_base: &str, id: &str) -> (ProbeOutcome, Value) {
        let base = engine_base.trim_end_matches('/');
        let pid = format!("lineup-probe-{}", unique());
        let started = self
            .client
            .get(format!("{base}/ace/getstream"))
            .query(&[("id", id), ("format", "json"), ("pid", pid.as_str())])
            .timeout(self.timing.request_timeout)
            .send()
            .await;
        let body: Value = match started {
            Ok(r) if r.status().is_success() => match r.json().await {
                Ok(v) => v,
                Err(e) => return unknown(format!("engine response: {e}")),
            },
            Ok(r) => return unknown(format!("engine: HTTP {}", r.status())),
            Err(e) => return unknown(format!("engine unreachable: {e}")),
        };
        if let Some(err) = body.get("error").filter(|e| !e.is_null()) {
            return unknown(format!("engine error: {err}"));
        }
        let resp = body.get("response").cloned().unwrap_or(Value::Null);
        let stat_url = resp
            .get("stat_url")
            .and_then(Value::as_str)
            .and_then(|u| rebase(u, base));
        let command_url = resp
            .get("command_url")
            .and_then(Value::as_str)
            .and_then(|u| rebase(u, base));
        let Some(stat_url) = stat_url else {
            if let Some(c) = &command_url {
                self.stop(c).await;
            }
            return unknown("engine response has no stat_url".into());
        };

        let deadline = tokio::time::Instant::now() + self.timing.give_up_after;
        let mut last: Option<Value>;
        let mut outcome = None;
        loop {
            match self.stat(&stat_url).await {
                Ok(s) => {
                    let peers = s.get("peers").and_then(Value::as_u64).unwrap_or(0);
                    let status = s.get("status").and_then(Value::as_str).unwrap_or("");
                    if peers > 0 || status == "dl" {
                        outcome = Some(ProbeOutcome::Ok);
                        last = Some(s);
                        break;
                    }
                    last = Some(s);
                }
                Err(e) => last = Some(json!({ "error": e })),
            }
            if tokio::time::Instant::now() + self.timing.poll_every > deadline {
                break;
            }
            tokio::time::sleep(self.timing.poll_every).await;
        }
        if let Some(c) = &command_url {
            self.stop(c).await;
        }
        let last = last.unwrap_or(Value::Null);
        let outcome = outcome.unwrap_or_else(|| {
            // Dead only if the session really started and sat in prebuffering without peers.
            let status = last.get("status").and_then(Value::as_str).unwrap_or("");
            let peers = last.get("peers").and_then(Value::as_u64);
            if status == "prebuf" && peers == Some(0) {
                ProbeOutcome::Dead
            } else {
                ProbeOutcome::Unknown
            }
        });
        (outcome, json!({ "stat": last }))
    }

    async fn stat(&self, url: &str) -> std::result::Result<Value, String> {
        let r = self
            .client
            .get(url)
            .timeout(self.timing.request_timeout)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(format!("HTTP {}", r.status()));
        }
        let v: Value = r.json().await.map_err(|e| e.to_string())?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            return Err(format!("engine error: {err}"));
        }
        Ok(v.get("response").cloned().unwrap_or(Value::Null))
    }

    /// Stop the session this probe created (only that one).
    async fn stop(&self, command_url: &str) {
        let Ok(mut url) = url::Url::parse(command_url) else {
            return;
        };
        url.query_pairs_mut().append_pair("method", "stop");
        if let Err(e) = self.client.get(url).timeout(self.timing.request_timeout).send().await {
            tracing::warn!("stopping probe session: {e}");
        }
    }
}

fn unknown(msg: String) -> (ProbeOutcome, Value) {
    (ProbeOutcome::Unknown, json!({ "error": msg }))
}

/// Number of clients/streams in an acexy per-id status body, if it has one.
fn active_count(body: &Value) -> Option<u64> {
    if let Some(b) = body.get("active").and_then(Value::as_bool) {
        return Some(u64::from(b));
    }
    ["clients", "streams", "viewers"]
        .iter()
        .find_map(|k| match body.get(*k) {
            Some(Value::Number(n)) => n.as_u64(),
            Some(Value::Array(a)) => Some(a.len() as u64),
            _ => None,
        })
}

/// Keep the path and query of `url`, but on `base`'s scheme, host and port.
fn rebase(url: &str, base: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let mut b = url::Url::parse(base).ok()?;
    b.set_path(u.path());
    b.set_query(u.query());
    Some(b.to_string())
}

fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    format!(
        "{nanos:x}-{:x}-{:x}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebases_engine_urls() {
        assert_eq!(
            rebase("http://127.0.0.1:6878/ace/stat/abc/def?x=1", "http://media-host:6878").as_deref(),
            Some("http://media-host:6878/ace/stat/abc/def?x=1")
        );
    }

    #[test]
    fn reads_activity() {
        assert_eq!(active_count(&json!({"clients": 2})), Some(2));
        assert_eq!(active_count(&json!({"clients": []})), Some(0));
        assert_eq!(active_count(&json!({"active": false})), Some(0));
        assert_eq!(active_count(&json!({"id": "x"})), None);
    }
}
