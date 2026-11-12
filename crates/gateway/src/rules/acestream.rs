//! The `acestream` rule: Acestream content ids, played through acexy.
//!
//! - match: `acestream://<id>`, a bare 40-hex id, or an acexy/engine URL with `id=` or
//!   `content_id=` under an `/ace/` path segment (also behind a path prefix, and this gateway's
//!   own play URLs). Identity `ace:<id>`, ref `acestream://<id>`, play
//!   `<acexy_public>/ace/getstream?id=<id>`. The engine is asked for the content's name (cleaned:
//!   quality words dropped, a `[DE]` tag becomes the region, sports-looking names get "Sports").
//! - observe: acexy's per-stream status and, while acexy serves the stream, the statistics of its
//!   engine session. Never starts, controls or stops a session.
//! - check: a short engine session of its own (unique pid), its statistics polled, then stopped.
//!   Before each check acexy is asked whether the stream is watched; a watched stream is skipped,
//!   and an unreadable guard skips too (fail closed). One check per engine at a time.
//!
//! All requests go to the configured control origins only, never follow redirects, and URLs the
//! engine reports are validated and rebased onto the configured engine.

use std::time::{Duration, SystemTime};

use futures_util::StreamExt;
use gateway_contract::{CheckOutcome, CheckResponse, Fact, Hints, MatchResult, Matched, Observation};
use serde_json::{Value, json};

use crate::config::AcestreamConfig;

const TIMEOUT: Duration = Duration::from_secs(5);

/// Probe timing, shortened in tests.
#[derive(Debug, Clone, Copy)]
pub struct ProbeTiming {
    pub poll_every: Duration,
    pub give_up_after: Duration,
}

impl Default for ProbeTiming {
    fn default() -> Self {
        ProbeTiming {
            poll_every: Duration::from_secs(2),
            give_up_after: Duration::from_secs(20),
        }
    }
}

pub struct Acestream {
    cfg: AcestreamConfig,
    client: reqwest::Client,
    timing: ProbeTiming,
    /// One probe per engine at a time. Held by the task that owns the probe, so a caller that goes
    /// away does not release it while the probe still runs.
    probe_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    /// Engine names already looked up (raw), for observe.
    names: std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, Option<String>)>>,
}

/// Total time a match batch spends on optional engine name lookups.
const NAME_BUDGET: Duration = Duration::from_secs(20);
const NAME_CACHE: Duration = Duration::from_secs(600);

pub const MATCH_REVISION: &str = "1";

impl Acestream {
    pub fn new(cfg: AcestreamConfig, timing: ProbeTiming) -> Self {
        let client = reqwest::Client::builder()
            .no_proxy() // only the configured origins, directly
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .build()
            .expect("building the control client");
        Acestream {
            cfg,
            client,
            timing,
            probe_lock: Default::default(),
            names: Default::default(),
        }
    }

    /// Changes when play URLs change (the public acexy address).
    pub fn play_revision(&self) -> String {
        format!("1:{}", short_hash(&self.cfg.acexy_public))
    }

    /// Checks need acexy: only it can tell whether a stream is being watched.
    pub fn can_check(&self) -> bool {
        self.cfg.acexy_control.is_some()
    }

    // ---------------------------------------------------------------- match

    pub async fn match_inputs(&self, inputs: &[String]) -> Vec<MatchResult> {
        let deadline = tokio::time::Instant::now() + NAME_BUDGET;
        futures_util::stream::iter(inputs.iter().cloned())
            .map(|input| async move { self.match_one(input, deadline).await })
            .buffered(8)
            .collect()
            .await
    }

    /// The engine's raw name for `id`, cached; bounded by `deadline`.
    async fn name_until(&self, id: &str, deadline: tokio::time::Instant) -> Result<Option<String>, String> {
        if let Some((at, n)) = self.names.lock().unwrap().get(id)
            && at.elapsed() < NAME_CACHE
        {
            return Ok(n.clone());
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err("skipped: the batch's time for name lookups is used up".into());
        }
        let n = tokio::time::timeout(left, self.engine_name(id))
            .await
            .map_err(|_| "skipped: the batch's time for name lookups is used up".to_string())??;
        let mut names = self.names.lock().unwrap();
        names.retain(|_, (at, _)| at.elapsed() < NAME_CACHE);
        if names.len() < 10_000 {
            names.insert(id.to_string(), (std::time::Instant::now(), n.clone()));
        }
        Ok(n)
    }

    async fn match_one(&self, input: String, deadline: tokio::time::Instant) -> MatchResult {
        let Some(id) = parse_id(&input) else {
            return MatchResult::NoMatch {
                input,
                reason: Some("not an Acestream id, acestream:// link or acexy/engine URL".into()),
            };
        };
        let mut warnings = vec![];
        let mut hints = Hints::default();
        match self.name_until(&id, deadline).await {
            Ok(Some(raw)) => {
                let (name, region) = clean_name(&raw);
                if looks_like_sports(&name) {
                    hints.group = Some("Sports".into());
                }
                if !name.is_empty() {
                    hints.title = Some(name);
                }
                hints.region = region;
            }
            Ok(None) => warnings.push("the engine has no name for this id".into()),
            Err(e) => warnings.push(format!("engine name lookup failed: {e}")),
        }
        MatchResult::Matched(Matched {
            input,
            identity: format!("ace:{id}"),
            reference: format!("acestream://{id}"),
            play: format!("{}/ace/getstream?id={id}", self.cfg.acexy_public),
            hints,
            warnings,
        })
    }

    async fn engine_name(&self, id: &str) -> Result<Option<String>, String> {
        let url = format!("{}/server/api", self.cfg.engine_control);
        let res = self
            .client
            .get(url)
            .query(&[("method", "get_media_files"), ("content_id", id)])
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        if !res.status().is_success() {
            return Err(format!("HTTP {}", res.status()));
        }
        let body: Value = res.json().await.map_err(|e| e.without_url().to_string())?;
        if let Some(e) = body.get("error").filter(|e| !e.is_null()) {
            return Err(format!("engine error: {e}"));
        }
        Ok(first_string(body.get("result")).map(|s| strip_extension(&s)))
    }

    // ---------------------------------------------------------------- observe

    pub async fn observe(&self, reference: &str) -> Observation {
        let mut o = Observation {
            reference: reference.to_string(),
            available: false,
            reason: None,
            content_state: None,
            watching: None,
            facts: vec![],
            title: None,
            raw: None,
        };
        let Some(id) = parse_id(reference) else {
            o.reason = Some("not an Acestream reference".into());
            return o;
        };
        // The engine's own name, independently of acexy (the channel may have been renamed).
        match self.name_until(&id, tokio::time::Instant::now() + TIMEOUT).await {
            Ok(n) => o.title = n,
            Err(e) => o.facts.push(fact("Engine name", format!("unavailable ({e})"))),
        }
        let Some(acexy) = &self.cfg.acexy_control else {
            o.available = o.title.is_some();
            o.reason = Some("live numbers come from acexy, which is not configured".into());
            return o;
        };
        let url = format!("{acexy}/ace/status?id={id}");
        let (status, body) = match self.get(&url).await {
            Ok(r) => r,
            Err(e) => {
                o.reason = Some(format!("acexy did not answer: {e}"));
                return o;
            }
        };
        if status == 404 {
            o.available = true;
            o.watching = Some(false);
            return o;
        }
        if !(200..300).contains(&status) {
            o.reason = Some(format!("this acexy has no per-stream status (HTTP {status})"));
            return o;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&body) else {
            o.reason = Some("acexy status is not JSON".into());
            return o;
        };
        let clients = v.get("clients").and_then(Value::as_u64);
        let stat_url = v.get("stat_url").and_then(Value::as_str);
        if clients.is_none() && stat_url.is_none() {
            o.reason = Some("this acexy has no per-stream status".into());
            return o;
        }
        o.available = true;
        if clients == Some(0) {
            o.watching = Some(false);
            return o;
        }
        o.watching = Some(true);
        if let Some(c) = clients {
            o.facts.push(fact("Players", c.to_string()));
        }
        let Some(stat_url) = stat_url else {
            o.reason = Some("acexy reports no session statistics for this stream".into());
            return o;
        };
        match self.read_stat(stat_url).await {
            Ok(Some(s)) => {
                let n = |k: &str| s.get(k).and_then(Value::as_u64);
                if let Some(st) = s.get("status").and_then(Value::as_str) {
                    o.facts.push(fact("Status", st.to_string()));
                }
                for (label, key, unit) in [
                    ("Peers", "peers", ""),
                    ("Down", "speed_down", " KB/s"),
                    ("Up", "speed_up", " KB/s"),
                ] {
                    if let Some(v) = n(key) {
                        o.facts.push(fact(label, format!("{v}{unit}")));
                    }
                }
                for (label, key) in [("Downloaded", "downloaded"), ("Uploaded", "uploaded")] {
                    if let Some(v) = n(key) {
                        o.facts.push(fact(label, bytes(v)));
                    }
                }
                o.raw = Some(s);
            }
            Ok(None) => o.watching = Some(false),
            Err(e) => o.reason = Some(format!("session statistics unavailable: {e}")),
        }
        o
    }

    /// The `response` of a stat URL reported by acexy, moved onto the engine. Ok(None): gone.
    async fn read_stat(&self, stat_url: &str) -> Result<Option<Value>, String> {
        let reported = url::Url::parse(stat_url).map_err(|_| "acexy reported an unusable statistics URL")?;
        if !is_stat_path(reported.path()) {
            return Err("acexy reported an unexpected statistics URL".into());
        }
        let url = rebase(stat_url, &self.cfg.engine_control).ok_or("acexy reported an unusable statistics URL")?;
        let (status, body) = self.get(&url).await?;
        if status == 404 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(format!("engine: HTTP {status}"));
        }
        let v: Value = serde_json::from_slice(&body).map_err(|_| "the engine's answer is not JSON".to_string())?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            let text = err.as_str().map_or_else(|| err.to_string(), str::to_string);
            if text.to_ascii_lowercase().contains("unknown") {
                return Ok(None);
            }
            return Err(format!("engine error: {text}"));
        }
        Ok(Some(v.get("response").cloned().unwrap_or(Value::Null)))
    }

    async fn get(&self, url: &str) -> Result<(u16, Vec<u8>), String> {
        get(&self.client, url).await
    }

    // ---------------------------------------------------------------- check

    pub async fn check(&self, reference: &str, force: bool) -> CheckResponse {
        let done = |outcome, reason: Option<&str>, detail: Value| CheckResponse {
            reference: reference.to_string(),
            outcome,
            reason: reason.map(str::to_string),
            content_state: None,
            detail: Some(detail),
            checked_at: chrono::Utc::now().to_rfc3339(),
        };
        let Some(id) = parse_id(reference) else {
            return done(
                CheckOutcome::Unknown,
                Some("bad_ref"),
                json!({"error": "not an Acestream reference"}),
            );
        };
        // Without acexy there is no guard, so there are no checks, forced or not.
        if self.cfg.acexy_control.is_none() {
            return done(
                CheckOutcome::Skipped,
                Some("guard_unavailable"),
                json!({ "reason": "checks need acexy, which tells whether a stream is watched" }),
            );
        }
        // The lock, the guard and the probe all live in one task of its own: once started, a check
        // finishes (stopping its session) and keeps the lock even if the caller goes away, so two
        // probes never overlap.
        let lock = self.probe_lock.clone();
        let me = Checker {
            client: self.client.clone(),
            acexy: self.cfg.acexy_control.clone(),
            run: ProbeRun {
                client: self.client.clone(),
                engine: self.cfg.engine_control.clone(),
                timing: self.timing,
            },
        };
        let owned = tokio::spawn(async move {
            let _lock = lock.lock_owned().await;
            if !force {
                match me.guard(&id).await {
                    Guard::Go => {}
                    Guard::Busy(why) => return (CheckOutcome::Skipped, Some("busy"), json!({ "reason": why })),
                    Guard::Unavailable(why) => {
                        return (
                            CheckOutcome::Skipped,
                            Some("guard_unavailable"),
                            json!({ "reason": why }),
                        );
                    }
                }
            }
            let (outcome, detail) = me.run.probe(&id).await;
            let reason = (outcome == CheckOutcome::Unknown).then_some("engine_error");
            (outcome, reason, detail)
        });
        match owned.await {
            Ok((outcome, reason, detail)) => done(outcome, reason, detail),
            Err(e) => done(
                CheckOutcome::Unknown,
                Some("engine_error"),
                json!({ "error": e.to_string() }),
            ),
        }
    }
}

/// What a check needs, owned by its task.
struct Checker {
    client: reqwest::Client,
    acexy: Option<String>,
    run: ProbeRun,
}

impl Checker {
    async fn get(&self, url: &str) -> Result<(u16, Vec<u8>), String> {
        get(&self.client, url).await
    }

    /// Ask acexy whether `id` is being watched. A per-id status decides for that id; without it,
    /// any active stream makes this check wait; anything unreadable skips it (fail closed).
    async fn guard(&self, id: &str) -> Guard {
        let Some(acexy) = &self.acexy else {
            return Guard::Unavailable("checks need acexy, which tells whether a stream is watched".into());
        };
        match self.get(&format!("{acexy}/ace/status?id={id}")).await {
            Ok((404, _)) => return Guard::Go,
            Ok((s, body)) if (200..300).contains(&s) => {
                let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                return match active_count(&v) {
                    Some(0) => Guard::Go,
                    Some(n) => Guard::Busy(format!("acexy reports {n} client(s) on this stream")),
                    None => Guard::Unavailable("acexy status for this stream is not understood".into()),
                };
            }
            Ok((s, _)) if (500..600).contains(&s) => {
                return Guard::Unavailable(format!("acexy status: HTTP {s}"));
            }
            Ok(_) => {} // another 4xx: no per-id status in this acexy; fall back to the global count
            Err(e) => return Guard::Unavailable(format!("acexy status unreadable: {e}")),
        }
        match self.get(&format!("{acexy}/ace/status")).await {
            Ok((s, body)) if (200..300).contains(&s) => {
                let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                match v.get("streams").and_then(Value::as_u64) {
                    Some(0) => Guard::Go,
                    Some(n) => Guard::Busy(format!("acexy reports {n} active stream(s)")),
                    None => Guard::Unavailable("acexy status has no stream count".into()),
                }
            }
            Ok((s, _)) => Guard::Unavailable(format!("acexy status: HTTP {s}")),
            Err(e) => Guard::Unavailable(format!("acexy status unreadable: {e}")),
        }
    }
}

#[derive(Debug, PartialEq)]
enum Guard {
    Go,
    Busy(String),
    Unavailable(String),
}

struct ProbeRun {
    client: reqwest::Client,
    engine: String,
    timing: ProbeTiming,
}

impl ProbeRun {
    /// Start a session, poll its stats, always stop it.
    async fn probe(&self, id: &str) -> (CheckOutcome, Value) {
        let pid = format!("lineup-gateway-probe-{}", unique());
        let started = self
            .client
            .get(format!("{}/ace/getstream", self.engine))
            .query(&[("id", id), ("format", "json"), ("pid", pid.as_str())])
            .send()
            .await;
        let body: Value = match started {
            Ok(r) if r.status().is_success() => match r.json().await {
                Ok(v) => v,
                Err(e) => return unknown(format!("engine response: {}", e.without_url())),
            },
            Ok(r) => return unknown(format!("engine: HTTP {}", r.status())),
            Err(e) => return unknown(format!("engine unreachable: {}", e.without_url())),
        };
        if let Some(err) = body.get("error").filter(|e| !e.is_null()) {
            return unknown(format!("engine error: {err}"));
        }
        let resp = body.get("response").cloned().unwrap_or(Value::Null);
        let stat_url = resp
            .get("stat_url")
            .and_then(Value::as_str)
            .filter(|u| url::Url::parse(u).is_ok_and(|x| is_stat_path(x.path())))
            .and_then(|u| rebase(u, &self.engine));
        let command_url = resp
            .get("command_url")
            .and_then(Value::as_str)
            .filter(|u| url::Url::parse(u).is_ok_and(|x| x.path().starts_with("/ace/cmd/")))
            .and_then(|u| rebase(u, &self.engine));
        let Some(command_url) = command_url else {
            return unknown("engine response has no usable command_url".into());
        };
        let Some(stat_url) = stat_url else {
            self.stop(&command_url).await;
            return unknown("engine response has no usable stat_url".into());
        };
        let deadline = tokio::time::Instant::now() + self.timing.give_up_after;
        let mut samples = 0u32;
        let mut all_prebuf = true;
        let mut failures: Vec<String> = Vec::new();
        let mut last = Value::Null;
        let mut healthy = false;
        loop {
            match self.stat(&stat_url).await {
                Ok(s) => {
                    samples += 1;
                    let peers = s.get("peers").and_then(Value::as_u64).unwrap_or(0);
                    let st = s.get("status").and_then(Value::as_str).map(str::to_string);
                    all_prebuf &= st.as_deref() == Some("prebuf") && s.get("peers").and_then(Value::as_u64) == Some(0);
                    last = s;
                    if peers > 0 || st.as_deref() == Some("dl") {
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
            CheckOutcome::Ok
        } else if samples > 0 && failures.is_empty() && all_prebuf {
            CheckOutcome::Dead
        } else {
            CheckOutcome::Unknown
        };
        let mut detail = json!({ "stat": last, "samples": samples });
        if !failures.is_empty() {
            detail["errors"] = json!(failures);
        }
        (outcome, detail)
    }

    async fn stat(&self, url: &str) -> Result<Value, String> {
        let r = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        if !r.status().is_success() {
            return Err(format!("HTTP {}", r.status()));
        }
        let v: Value = r.json().await.map_err(|e| e.without_url().to_string())?;
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
        if let Err(e) = self.client.get(url).send().await {
            tracing::warn!("stopping probe session: {}", e.without_url());
        }
    }
}

/// GET with a capped body; the client never follows redirects.
async fn get(client: &reqwest::Client, url: &str) -> Result<(u16, Vec<u8>), String> {
    let res = client.get(url).send().await.map_err(|e| e.without_url().to_string())?;
    let status = res.status().as_u16();
    let body = crate::net::read_capped(res, 64 * 1024)
        .await
        .map_err(|e| e.to_string())?;
    Ok((status, body))
}

fn unknown(msg: String) -> (CheckOutcome, Value) {
    (CheckOutcome::Unknown, json!({ "error": msg }))
}

fn fact(label: &str, value: String) -> Fact {
    Fact {
        label: label.into(),
        value,
    }
}

fn bytes(v: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut n = v as f64;
    let mut i = 0;
    while n >= 1024.0 && i < units.len() - 1 {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v} B")
    } else {
        format!("{n:.1} {}", units[i])
    }
}

/// The 40-hex content id in an input, lowercased.
pub fn parse_id(input: &str) -> Option<String> {
    let s = input.trim();
    if let Some(rest) = strip_prefix_ci(s, "acestream://") {
        return is_ace_id(rest.trim_end_matches('/')).then(|| rest.trim_end_matches('/').to_ascii_lowercase());
    }
    if is_ace_id(s) {
        return Some(s.to_ascii_lowercase());
    }
    let u = url::Url::parse(s).ok()?;
    if !matches!(u.scheme(), "http" | "https") || !u.path().contains("/ace/") {
        return None;
    }
    let q = |k: &str| u.query_pairs().find(|(key, _)| key == k).map(|(_, v)| v.into_owned());
    let id = q("id").or_else(|| q("content_id"))?;
    is_ace_id(&id).then(|| id.to_ascii_lowercase())
}

fn is_ace_id(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &s[prefix.len()..])
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

/// Move an engine-reported URL onto the configured engine `base`, keeping its query.
pub fn rebase(url: &str, base: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let mut b = url::Url::parse(base).ok()?;
    let prefix = b.path().trim_end_matches('/').to_string();
    b.set_path(&format!("{prefix}{}", u.path()));
    b.set_query(u.query());
    Some(b.to_string())
}

/// `/ace/stat/<segment>[/<segment>...]` with plain segments only.
fn is_stat_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/ace/stat/") else {
        return false;
    };
    !rest.is_empty()
        && rest
            .split('/')
            .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}

fn first_string(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => a.iter().find_map(|x| first_string(Some(x))),
        Value::Object(o) => {
            // Keys are file indexes ("0", "1", ...): take the lowest.
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort_by_key(|k| k.parse::<u64>().unwrap_or(u64::MAX));
            keys.into_iter().find_map(|k| first_string(o.get(k)))
        }
        _ => None,
    }
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

fn strip_extension(s: &str) -> String {
    for ext in [".mp4", ".mkv", ".ts", ".avi", ".m2ts"] {
        if s.len() > ext.len() && s.to_ascii_lowercase().ends_with(ext) {
            return s[..s.len() - ext.len()].to_string();
        }
    }
    s.to_string()
}

const NOISE: &[&str] = &[
    "hd", "fhd", "uhd", "sd", "hq", "lq", "4k", "8k", "bar", "hevc", "h264", "h265", "x264", "x265", "1080p", "1080i",
    "720p", "576p", "50fps", "60fps", "25fps", "multi", "backup",
];

/// Clean an engine name: drop `[XX]`/`(XX)` tags (a two-letter one becomes the region) and quality
/// words. "Arena 1 Bar HD [DE]" → ("Arena 1", Some("de")).
pub fn clean_name(raw: &str) -> (String, Option<String>) {
    let mut region = None;
    let mut text = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        let close = match c {
            '[' => ']',
            '(' => ')',
            _ => {
                text.push(c);
                continue;
            }
        };
        let mut tag = String::new();
        for d in chars.by_ref() {
            if d == close {
                break;
            }
            tag.push(d);
        }
        let tag = tag.trim();
        if region.is_none() && tag.len() == 2 && tag.chars().all(|c| c.is_ascii_alphabetic()) {
            region = Some(tag.to_ascii_lowercase());
        }
        text.push(' ');
    }
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == '|' || c == '_')
        .filter(|w| !w.is_empty() && *w != "-")
        .filter(|w| !NOISE.contains(&w.to_ascii_lowercase().as_str()))
        .collect();
    (words.join(" "), region)
}

const SPORTS_WORDS: &[&str] = &[
    "sport",
    "sports",
    "football",
    "soccer",
    "arena",
    "match",
    "league",
    "liga",
    "cup",
    "f1",
    "racing",
    "golf",
    "tennis",
    "fight",
    "boxing",
    "ufc",
    "nba",
    "nfl",
    "nhl",
    "mlb",
    "rugby",
    "cricket",
    "motor",
    "bein",
    "eurosport",
    "espn",
];

fn looks_like_sports(name: &str) -> bool {
    let t = tokens(name);
    let joined = t.join(" ");
    joined.contains("sky sports")
        || t.iter()
            .any(|w| SPORTS_WORDS.contains(&w.as_str()) || w.starts_with("sport"))
}

/// Lowercased alphanumeric runs, split at camelCase and letter/digit boundaries.
fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev = None;
            continue;
        }
        if let Some(p) = prev {
            let boundary = (p.is_lowercase() && c.is_uppercase())
                || (p.is_alphabetic() && c.is_numeric())
                || (p.is_numeric() && c.is_alphabetic());
            if boundary && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.extend(c.to_lowercase());
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
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

/// A short stable hash (FNV-1a) for revision strings.
pub fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACE: &str = "41169a3f280a5537b0611b775e926852d372743d";

    #[test]
    fn input_forms() {
        for input in [
            format!("acestream://{ACE}"),
            format!("ACESTREAM://{}", ACE.to_uppercase()),
            format!("  {ACE}  "),
            format!("http://10.0.0.9:6878/ace/getstream?id={ACE}"),
            format!("http://x:8080/ace/manifest.m3u8?content_id={ACE}"),
            format!("http://proxy/acexy/ace/getstream?id={ACE}"),
        ] {
            assert_eq!(parse_id(&input).as_deref(), Some(ACE), "{input}");
        }
        for bad in [
            "acestream://abc",
            "http://x/ace/live.m3u8?id=1",
            "https://vk.com/video1_2",
            "ккк",
        ] {
            assert_eq!(parse_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn cleans_engine_names() {
        assert_eq!(clean_name("Arena 1 Bar HD [DE]"), ("Arena 1".into(), Some("de".into())));
        assert_eq!(
            clean_name("Sports One FHD (UK) [backup]"),
            ("Sports One".into(), Some("uk".into()))
        );
        assert_eq!(clean_name("  Movie  Max | 1080p  "), ("Movie Max".into(), None));
        assert!(looks_like_sports("Arena 1"));
        assert!(looks_like_sports("SportsOne"));
        assert!(!looks_like_sports("Movie Max"));
        let v = json!({"1": "b.mp4", "0": "Arena 1 HD.mkv"});
        assert_eq!(
            first_string(Some(&v)).map(|s| strip_extension(&s)).as_deref(),
            Some("Arena 1 HD")
        );
    }

    #[test]
    fn engine_urls() {
        assert_eq!(
            rebase("http://127.0.0.1:6878/ace/stat/abc/def?x=1", "http://media-host:6878").as_deref(),
            Some("http://media-host:6878/ace/stat/abc/def?x=1")
        );
        assert_eq!(
            rebase("http://127.0.0.1:6878/ace/stat/abc", "https://proxy/engine").as_deref(),
            Some("https://proxy/engine/ace/stat/abc")
        );
        assert!(is_stat_path("/ace/stat/abc/def"));
        for bad in [
            "/ace/stat/",
            "/ace/stat//x",
            "/ace/stat/a/../cmd",
            "/ace/cmd/abc",
            "/proxy/ace/stat/a",
        ] {
            assert!(!is_stat_path(bad), "{bad}");
        }
        assert_eq!(active_count(&json!({"clients": 2})), Some(2));
        assert_eq!(active_count(&json!({"clients": []})), Some(0));
        assert_eq!(active_count(&json!({"active": false})), Some(0));
        assert_eq!(active_count(&json!({"id": "x"})), None);
    }
}
