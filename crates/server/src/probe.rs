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
use crate::store::{ChannelFilter, ProbeIdentity, Store};

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

/// A channel to probe, as it was when the probe was requested.
struct Target {
    channel_id: i64,
    created_at: chrono::DateTime<chrono::Utc>,
    ace: String,
}

enum Attempt {
    Done(ProbeResult),
    /// The guard stopped the run at this channel.
    Stop(ProbeResult, String),
}

fn skipped(channel_id: i64, why: &str) -> ProbeResult {
    ProbeResult {
        channel_id,
        outcome: ProbeOutcome::Skipped,
        detail: Some(json!({ "reason": why, "recorded": false })),
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
    pub async fn probe(self: &Arc<Self>, ids: &[i64], force: bool) -> Result<Vec<ProbeResult>> {
        let mut targets = Vec::new();
        for id in ids {
            let c = self.store.get_channel(*id).await?;
            match c.source {
                Source::Acestream { id: ace } => targets.push(Target {
                    channel_id: c.id,
                    created_at: c.created_at,
                    ace,
                }),
                _ => return Err(invalid(format!("channel {} is not an acestream channel", c.id))),
            }
        }
        Ok(self.run_owned(targets, force).await.0)
    }

    /// All enabled acestream channels. Returns whether the guard stopped the run early.
    pub async fn probe_all(self: &Arc<Self>, force: bool) -> Result<(Vec<ProbeResult>, bool)> {
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
                Source::Acestream { id } => Some(Target {
                    channel_id: c.id,
                    created_at: c.created_at,
                    ace: id,
                }),
                _ => None,
            })
            .collect();
        Ok(self.run_owned(targets, force).await)
    }

    /// Run in a task of its own: once started, a run finishes (stopping its sessions and recording
    /// results) and keeps holding the run lock even if the caller goes away, so runs never overlap.
    async fn run_owned(self: &Arc<Self>, targets: Vec<Target>, force: bool) -> (Vec<ProbeResult>, bool) {
        let me = self.clone();
        match tokio::spawn(async move { me.run(targets, force).await }).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("probe run failed: {e}");
                (vec![], true)
            }
        }
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

    async fn run(self: &Arc<Self>, targets: Vec<Target>, force: bool) -> (Vec<ProbeResult>, bool) {
        let _lock = self.run_lock.lock().await;
        let mut results = Vec::new();
        let mut stopped: Option<String> = None;
        tracing::info!(
            "probe run starting: {} channel(s){}",
            targets.len(),
            if force { ", forced" } else { "" }
        );
        for t in targets {
            let not_recorded = |reason: &str| ProbeResult {
                channel_id: t.channel_id,
                outcome: ProbeOutcome::Skipped,
                detail: Some(json!({ "reason": reason, "recorded": false })),
            };
            if let Some(reason) = &stopped {
                results.push(not_recorded(reason));
                continue;
            }
            // The target may have waited for the lock: probe only the channel and source it named.
            if !self.still_current(&t).await {
                results.push(not_recorded("the channel or its source changed"));
                continue;
            }
            match self.attempt(t, force).await {
                Attempt::Done(r) => results.push(r),
                Attempt::Stop(r, why) => {
                    results.push(r);
                    stopped = Some(why);
                }
            }
        }
        let n = |o: ProbeOutcome| results.iter().filter(|r| r.outcome == o).count();
        tracing::info!(
            "probe run done: {} ok, {} dead, {} unknown, {} skipped{}",
            n(ProbeOutcome::Ok),
            n(ProbeOutcome::Dead),
            n(ProbeOutcome::Unknown),
            n(ProbeOutcome::Skipped),
            stopped
                .as_deref()
                .map(|w| format!(" (stopped: {w})"))
                .unwrap_or_default()
        );
        (results, stopped.is_some())
    }

    async fn still_current(&self, t: &Target) -> bool {
        match self.store.get_channel(t.channel_id).await {
            Ok(c) => c.created_at == t.created_at && matches!(&c.source, Source::Acestream { id } if *id == t.ace),
            Err(_) => false,
        }
    }

    /// Guard, probe and record one target.
    async fn attempt(&self, t: Target, force: bool) -> Attempt {
        let settings = match self.store.settings().await {
            Ok(s) => s,
            Err(e) => {
                let why = format!("reading settings: {e}");
                return Attempt::Stop(skipped(t.channel_id, &why), why);
            }
        };
        let (outcome, detail) = if force {
            self.probe_one(&settings.engine_base, &t.ace).await
        } else {
            match self.guard(&settings.acexy_base, &t.ace).await {
                Guard::Go => self.probe_one(&settings.engine_base, &t.ace).await,
                Guard::Skip(why) => (ProbeOutcome::Skipped, json!({ "reason": why })),
                Guard::Stop(why) => return Attempt::Stop(skipped(t.channel_id, &why), why),
            }
        };
        let identity = ProbeIdentity {
            channel_id: t.channel_id,
            created_at: t.created_at,
            ace: &t.ace,
        };
        tracing::info!("probe of channel {} ({}): {outcome:?} {detail}", t.channel_id, t.ace);
        let mut detail = detail;
        match self.store.record_probe(identity, outcome, Some(detail.clone())).await {
            Ok(true) => {}
            Ok(false) => detail["recorded"] = json!(false),
            Err(e) => tracing::warn!("recording probe of channel {}: {e}", t.channel_id),
        }
        Attempt::Done(ProbeResult {
            channel_id: t.channel_id,
            outcome,
            detail: Some(detail),
        })
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
        // Without a command URL the session cannot be stopped; do not keep it open to measure it.
        let Some(command_url) = command_url else {
            return unknown("engine response has no command_url".into());
        };
        let Some(stat_url) = stat_url else {
            self.stop(&command_url).await;
            return unknown("engine response has no stat_url".into());
        };

        let deadline = tokio::time::Instant::now() + self.timing.give_up_after;
        let mut samples = 0u32;
        // Every sample so far showed prebuffering with zero peers.
        let mut all_prebuf = true;
        let mut failures: Vec<String> = Vec::new();
        let mut last: Value = Value::Null;
        let mut healthy = false;
        loop {
            match self.stat(&stat_url).await {
                Ok(s) => {
                    samples += 1;
                    let peers = s.get("peers").and_then(Value::as_u64).unwrap_or(0);
                    let downloading = s.get("status").and_then(Value::as_str) == Some("dl");
                    let prebuf_zero = s.get("status").and_then(Value::as_str) == Some("prebuf")
                        && s.get("peers").and_then(Value::as_u64) == Some(0);
                    all_prebuf &= prebuf_zero;
                    last = s;
                    if peers > 0 || downloading {
                        healthy = true;
                        break;
                    }
                }
                Err(e) => failures.push(e),
            }
            if tokio::time::Instant::now() + self.timing.poll_every > deadline {
                break;
            }
            tokio::time::sleep(self.timing.poll_every).await;
        }
        self.stop(&command_url).await;
        let outcome = if healthy {
            ProbeOutcome::Ok
        } else {
            // Dead only if every sample came back and showed prebuffering with no peers; any failed
            // or unexpected sample leaves the window incomplete.
            if samples > 0 && failures.is_empty() && all_prebuf {
                ProbeOutcome::Dead
            } else {
                ProbeOutcome::Unknown
            }
        };
        let mut detail = json!({ "stat": last, "samples": samples });
        if !failures.is_empty() {
            detail["errors"] = json!(failures);
        }
        (outcome, detail)
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
