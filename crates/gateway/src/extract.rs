//! Running the configured extractors as subprocesses, in the order a rule lists them, and caching
//! what they find as descriptors.
//!
//! Subprocess hygiene: argument arrays (no shell), null stdin, all traffic through the egress
//! proxy (the `{proxy}` argument), bounded output, a deadline, and kill on drop (the process group
//! is killed so helpers die too). At most `limits.extractions` run at once; background work
//! (observe, check) gets at most one of those slots, so playback always has room.
//!
//! Lookups are keyed by the rule's [`RelayPolicy`] and the page: two rules with different
//! extractors or expiry settings never share a descriptor, a failure or its backoff. Lookups of the
//! same key are coalesced.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::AsyncReadExt;

pub use crate::config::OutputFormat;
use tokio::sync::Semaphore;

/// What an extraction found: the HLS manifest to play and how to request it.
#[derive(Debug, Clone, PartialEq)]
pub struct Descriptor {
    /// The extractor that found it (its configured name).
    pub tool: String,
    /// HLS master (or media) playlist.
    pub manifest_url: String,
    /// Request headers the CDN expects (User-Agent etc.). Never cookies or credentials.
    pub headers: Vec<(String, String)>,
    pub is_live: Option<bool>,
    pub title: Option<String>,
    pub duration: Option<f64>,
    /// Unix time the signed manifest URL expires, when it says.
    pub expires: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ExtractError {
    /// The page has no video (removed, private, never existed).
    #[error("{0}")]
    NotFound(String),
    /// The page has a video but no playable HLS stream (yet), e.g. a recording being processed.
    #[error("{0}")]
    NoStream(String),
    /// The tool or the network failed; says nothing about the page.
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Playback,
    Background,
}

/// A configured extractor.
#[derive(Debug, Clone, Hash)]
pub struct Tool {
    pub name: String,
    pub command: PathBuf,
    /// Argument templates: `{url}` and `{proxy}` are replaced as they are (never encoded).
    pub args: Vec<String>,
    pub output: OutputFormat,
    pub timeout: Duration,
    pub phrases: Phrases,
}

/// Extra phrases in a tool's error output, on top of the format's defaults.
#[derive(Debug, Clone, Default, Hash)]
pub struct Phrases {
    pub not_found: Vec<String>,
    pub no_stream: Vec<String>,
}

impl Tool {
    pub fn from_config(name: &str, c: &crate::config::ExtractorConfig, default_timeout: Duration) -> Self {
        Tool {
            name: name.to_string(),
            command: c.command.clone(),
            args: c.args.clone(),
            output: c.output,
            timeout: c.timeout_secs.map_or(default_timeout, Duration::from_secs),
            phrases: Phrases {
                not_found: c.not_found.clone(),
                no_stream: c.no_stream.clone(),
            },
        }
    }
}

/// How one relay rule looks pages up and treats its upstream: immutable, shared by its lookups
/// and sessions.
#[derive(Debug)]
pub struct RelayPolicy {
    pub tools: Vec<Arc<Tool>>,
    pub expired_status: Vec<u16>,
    pub expires_param: String,
    /// A hash of everything above: lookups of rules with the same policy may share results.
    pub fingerprint: String,
}

impl RelayPolicy {
    pub fn new(tools: Vec<Arc<Tool>>, expired_status: Vec<u16>, expires_param: String) -> Self {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for t in &tools {
            t.hash(&mut h);
        }
        expired_status.hash(&mut h);
        expires_param.hash(&mut h);
        RelayPolicy {
            fingerprint: format!("{:016x}", h.finish()),
            tools,
            expired_status,
            expires_param,
        }
    }

    pub fn expired(&self, status: u16) -> bool {
        self.expired_status.contains(&status)
    }
}

/// One page under one rule's policy.
#[derive(Debug, Clone)]
pub struct Lookup {
    pub page: String,
    pub policy: Arc<RelayPolicy>,
}

impl Lookup {
    pub fn new(page: impl Into<String>, policy: Arc<RelayPolicy>) -> Self {
        Lookup {
            page: page.into(),
            policy,
        }
    }

    /// The cache and coalescing key.
    pub fn key(&self) -> String {
        format!("{} {}", self.policy.fingerprint, self.page)
    }
}

struct Entry {
    result: Result<Descriptor, ExtractError>,
    until: Instant,
    /// Consecutive failures, for backoff.
    failures: u32,
}

struct Inner {
    /// The egress proxy URL every tool must use.
    proxy: String,
    all: Arc<Semaphore>,
    background: Arc<Semaphore>,
    cache: Mutex<HashMap<String, Entry>>,
    /// Per-page locks for coalescing; removed by the lookup that created them.
    inflight: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Lookups admitted (running or waiting for a slot), per priority: background work never
    /// takes playback's places.
    admitted_playback: std::sync::atomic::AtomicUsize,
    admitted_background: std::sync::atomic::AtomicUsize,
}

#[derive(Clone)]
pub struct Extractor(Arc<Inner>);

const MAX_OUTPUT: usize = 16 * 1024 * 1024;
const RECORDING_TTL: Duration = Duration::from_secs(3 * 3600);
/// Live, or not known to be a finished recording yet: look again soon.
const SHORT_TTL: Duration = Duration::from_secs(60);
/// Backoff after failures: 1, 2, 5, 10 minutes.
const BACKOFF: [u64; 4] = [60, 120, 300, 600];
const MAX_CACHED: usize = 1000;
/// Playback lookups admitted at once (running plus waiting); more are refused.
const MAX_ADMITTED: usize = 16;
/// Background lookups admitted at once.
const MAX_ADMITTED_BACKGROUND: usize = 8;
/// How long an admitted lookup may wait for a slot.
const QUEUE_DEADLINE: Duration = Duration::from_secs(60);

impl Extractor {
    pub fn new(proxy: String, concurrency: usize) -> Self {
        Extractor(Arc::new(Inner {
            proxy,
            all: Arc::new(Semaphore::new(concurrency.max(1))),
            background: Arc::new(Semaphore::new(1)),
            cache: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
            admitted_playback: std::sync::atomic::AtomicUsize::new(0),
            admitted_background: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    /// The descriptor of a lookup, from the cache or a new extraction. The extraction runs in a
    /// task of its own: a caller that goes away does not stop work other callers wait for, and the
    /// tool's process group is still killed on its deadline.
    pub async fn descriptor(&self, l: &Lookup, priority: Priority) -> Result<Descriptor, ExtractError> {
        if let Some(r) = self.0.cached(&l.key()) {
            return r;
        }
        let me = self.0.clone();
        let l = l.clone();
        tokio::spawn(async move { me.lookup(&l, priority).await })
            .await
            .unwrap_or_else(|e| Err(ExtractError::Failed(format!("lookup task failed: {e}"))))
    }

    /// Forget a lookup's descriptor (its URLs stopped working).
    pub fn invalidate(&self, l: &Lookup) {
        self.0.cache.lock().unwrap().remove(&l.key());
    }

    /// A media playlist of the lookup's descriptor has ENDLIST: unless the lookup said live, the
    /// item is a finished recording and its descriptor may be kept longer.
    pub fn observed_ended(&self, l: &Lookup, d: &Descriptor) {
        if d.is_live == Some(true) {
            return;
        }
        let mut cache = self.0.cache.lock().unwrap();
        if let Some(e) = cache.get_mut(&l.key())
            && matches!(&e.result, Ok(x) if x == d)
        {
            e.until = Instant::now() + expiry_cap(d, RECORDING_TTL);
        }
    }
}

impl Inner {
    fn cached(&self, key: &str) -> Option<Result<Descriptor, ExtractError>> {
        let cache = self.cache.lock().unwrap();
        cache
            .get(key)
            .filter(|e| e.until > Instant::now())
            .map(|e| e.result.clone())
    }

    async fn lookup(self: Arc<Self>, l: &Lookup, priority: Priority) -> Result<Descriptor, ExtractError> {
        let key = l.key();
        let page = key.as_str();
        use std::sync::atomic::Ordering;
        let (counter, max) = match priority {
            Priority::Playback => (&self.admitted_playback, MAX_ADMITTED),
            Priority::Background => (&self.admitted_background, MAX_ADMITTED_BACKGROUND),
        };
        if counter.fetch_add(1, Ordering::SeqCst) >= max {
            counter.fetch_sub(1, Ordering::SeqCst);
            return Err(ExtractError::Failed("too many lookups are waiting; try again".into()));
        }
        struct Admitted<'a>(&'a std::sync::atomic::AtomicUsize);
        impl Drop for Admitted<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let _admitted = Admitted(counter);
        // Background work takes its slot before the page lock, so it never holds a page that a
        // playback request waits for while it is itself still queued.
        let _bg = match priority {
            Priority::Background => Some(
                tokio::time::timeout(QUEUE_DEADLINE, self.background.clone().acquire_owned())
                    .await
                    .map_err(|_| ExtractError::Failed("the lookup queue is full; try again".into()))?
                    .expect("semaphore"),
            ),
            Priority::Playback => None,
        };
        let lock = self
            .inflight
            .lock()
            .unwrap()
            .entry(page.to_string())
            .or_default()
            .clone();
        let _page = tokio::time::timeout(QUEUE_DEADLINE, lock.lock())
            .await
            .map_err(|_| ExtractError::Failed("the lookup queue is full; try again".into()))?;
        // Whatever happens next, this page's entry leaves the in-flight table.
        struct Done<'a>(&'a Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>, &'a str);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.lock().unwrap().remove(self.1);
            }
        }
        let _done = Done(&self.inflight, page);
        if let Some(r) = self.cached(page) {
            return r;
        }
        let _slot = tokio::time::timeout(QUEUE_DEADLINE, self.all.clone().acquire_owned())
            .await
            .map_err(|_| ExtractError::Failed("the lookup queue is full; try again".into()))?
            .expect("semaphore");
        let result = self.extract(l).await;
        self.store(page, &result);
        result
    }

    fn store(&self, page: &str, result: &Result<Descriptor, ExtractError>) {
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap();
        let failures = match result {
            Ok(_) => 0,
            Err(_) => cache.get(page).map_or(0, |e| e.failures) + 1,
        };
        let ttl = match result {
            // Not known to be a finished recording yet: look again soon (a relay that sees ENDLIST
            // extends it, see `observed_ended`).
            Ok(d) => expiry_cap(d, SHORT_TTL),
            Err(ExtractError::Failed(_)) => Duration::from_secs(15),
            Err(_) => Duration::from_secs(BACKOFF[(failures as usize - 1).min(BACKOFF.len() - 1)]),
        };
        cache.retain(|_, e| e.until > now);
        while cache.len() >= MAX_CACHED {
            let Some(k) = cache.iter().min_by_key(|(_, e)| e.until).map(|(k, _)| k.clone()) else {
                break;
            };
            cache.remove(&k);
        }
        cache.insert(
            page.to_string(),
            Entry {
                result: result.clone(),
                until: now + ttl,
                failures,
            },
        );
    }

    async fn extract(&self, l: &Lookup) -> Result<Descriptor, ExtractError> {
        let mut errors = Vec::new();
        for tool in &l.policy.tools {
            match self.run_tool(tool, &l.page, &l.policy.expires_param).await {
                Ok(d) => return Ok(d),
                Err(e) => errors.push(e),
            }
        }
        // The most telling failure: a page without a video, then no stream yet, then a tool failure.
        let pick = |f: fn(&ExtractError) -> bool| errors.iter().find(|e| f(e)).cloned();
        Err(pick(|e| matches!(e, ExtractError::NotFound(_)))
            .or_else(|| pick(|e| matches!(e, ExtractError::NoStream(_))))
            .or_else(|| errors.first().cloned())
            .unwrap_or_else(|| ExtractError::Failed("no extractor is configured".into())))
    }

    async fn run_tool(&self, tool: &Tool, page: &str, expires_param: &str) -> Result<Descriptor, ExtractError> {
        let args: Vec<String> = tool
            .args
            .iter()
            .map(|a| a.replace("{url}", page).replace("{proxy}", &self.proxy))
            .collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(tool, &args).await?;
        let v: Value = serde_json::from_slice(&out)
            .map_err(|e| ExtractError::Failed(format!("{} printed something that is not JSON: {e}", tool.name)))?;
        let d = match tool.output {
            OutputFormat::YtDlpJson => descriptor_from_yt_dlp(&v, expires_param),
            OutputFormat::StreamlinkJson => descriptor_from_streamlink(&v, expires_param, &tool.phrases),
        };
        d.map(|d| Descriptor {
            tool: tool.name.clone(),
            ..d
        })
        .map_err(|e| rename_error(e, &tool.name, tool.output))
    }
}

/// Parser messages name the format's usual tool; name the configured extractor instead.
fn rename_error(e: ExtractError, name: &str, output: OutputFormat) -> ExtractError {
    let default = match output {
        OutputFormat::YtDlpJson => "yt-dlp",
        OutputFormat::StreamlinkJson => "streamlink",
    };
    let fix = |m: String| match m.strip_prefix(default) {
        Some(rest) if name != default => format!("{name}{rest}"),
        _ => m,
    };
    match e {
        ExtractError::NotFound(m) => ExtractError::NotFound(fix(m)),
        ExtractError::NoStream(m) => ExtractError::NoStream(fix(m)),
        ExtractError::Failed(m) => ExtractError::Failed(fix(m)),
    }
}

/// `ttl`, but never past the signed URL's expiry (minus ten minutes).
fn expiry_cap(d: &Descriptor, ttl: Duration) -> Duration {
    match d.expires {
        Some(exp) => {
            let left = exp - chrono::Utc::now().timestamp() - 600;
            ttl.min(Duration::from_secs(left.max(0) as u64))
        }
        None => ttl,
    }
}

/// Kills a tool's whole process group when dropped, unless the tool finished: a timeout, an error
/// or a cancelled task all end its helpers too.
struct GroupGuard {
    pid: Option<u32>,
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            kill_group(pid);
        }
    }
}

/// Run a tool; its stdout on success. Errors carry the tail of its stderr.
async fn run(tool: &Tool, args: &[&str]) -> Result<Vec<u8>, ExtractError> {
    let timeout = tool.timeout;
    let mut cmd = tokio::process::Command::new(&tool.command);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let name = tool.name.clone();
    let mut child = cmd
        .spawn()
        .map_err(|e| ExtractError::Failed(format!("starting {name}: {e}")))?;
    let mut guard = GroupGuard { pid: child.id() };
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let work = async {
        // Drain both pipes concurrently, bounded.
        let out = async {
            let mut buf = Vec::new();
            let mut chunk = vec![0u8; 65536];
            loop {
                let n = stdout.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                if buf.len() + n > MAX_OUTPUT {
                    return Err(std::io::Error::other("output too large"));
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Ok::<_, std::io::Error>(buf)
        };
        let err = async {
            let mut tail = Vec::new();
            let mut chunk = vec![0u8; 8192];
            loop {
                let n = stderr.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > 16384 {
                    tail.drain(..tail.len() - 16384);
                }
            }
            Ok::<_, std::io::Error>(tail)
        };
        let (out, err) = tokio::join!(out, err);
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, out?, err.unwrap_or_default()))
    };
    match tokio::time::timeout(timeout, work).await {
        Err(_) => Err(ExtractError::Failed(format!(
            "{name} took longer than {}s",
            timeout.as_secs()
        ))),
        Ok(Err(e)) => Err(ExtractError::Failed(format!("{name}: {e}"))),
        Ok(Ok((status, out, err))) => {
            // Helpers a finished tool left behind are killed too.
            drop(std::mem::replace(&mut guard, GroupGuard { pid: None }));
            if status.success() {
                return Ok(out);
            }
            let msg = String::from_utf8_lossy(&err);
            let line = msg
                .lines()
                .rev()
                .find(|l| l.to_ascii_lowercase().contains("error"))
                .or_else(|| msg.lines().last())
                .unwrap_or("")
                .trim()
                .to_string();
            Err(classify(&name, &line, &tool.phrases))
        }
    }
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    // `kill -KILL -<pgid>` without libc bindings; the child process reaps itself via tokio.
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// What a tool's error line says about the page. Only explicit statements that the video is gone
/// count as "not found"; parser, schema and network errors stay inconclusive.
fn classify(tool: &str, line: &str, extra: &Phrases) -> ExtractError {
    let l = line.to_ascii_lowercase();
    let msg = format!("{tool}: {line}");
    let gone = [
        "video not found",
        "this video has been removed",
        "video has been deleted",
        "this video is private",
        "this video is not available",
        "access to this video is restricted",
        "video does not exist",
        "http error 404",
    ];
    let has = |extra: &[String]| {
        extra
            .iter()
            .any(|k| !k.is_empty() && l.contains(&k.to_ascii_lowercase()))
    };
    let schema = l.contains("unable to validate") || l.contains("key '") || l.contains("unable to extract");
    if !schema && (gone.iter().any(|k| l.contains(k)) || has(&extra.not_found)) {
        ExtractError::NotFound(msg)
    } else if [
        "no video formats",
        "no playable streams",
        "no streams found",
        "is not available yet",
        "requested format is not available",
    ]
    .iter()
    .any(|k| l.contains(k))
        || has(&extra.no_stream)
    {
        ExtractError::NoStream(msg)
    } else {
        ExtractError::Failed(msg)
    }
}

/// Headers worth passing on to the CDN; never cookies or authorization.
fn keep_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "user-agent" | "accept" | "accept-language" | "referer" | "origin"
    )
}

fn headers(v: Option<&Value>) -> Vec<(String, String)> {
    v.and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|(k, _)| keep_header(k))
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn expires_of(url: &str, param: &str) -> Option<i64> {
    let u = url::Url::parse(url).ok()?;
    u.query_pairs()
        .find(|(k, _)| k == param)
        .and_then(|(_, v)| v.parse().ok())
        .filter(|t: &i64| *t > 1_000_000_000)
}

/// The HLS master of a yt-dlp info dict: any HLS format's `manifest_url`, else the best HLS
/// format's own URL.
pub fn descriptor_from_yt_dlp(v: &Value, expires_param: &str) -> Result<Descriptor, ExtractError> {
    let formats = v.get("formats").and_then(Value::as_array).cloned().unwrap_or_default();
    let is_hls = |f: &Value| {
        f.get("protocol")
            .and_then(Value::as_str)
            .is_some_and(|p| p.starts_with("m3u8"))
    };
    let hls: Vec<&Value> = formats.iter().filter(|f| is_hls(f)).collect();
    let chosen = hls
        .iter()
        .find(|f| f.get("manifest_url").and_then(Value::as_str).is_some())
        .or_else(|| hls.last());
    let Some(f) = chosen else {
        return Err(ExtractError::NoStream(format!(
            "yt-dlp: no HLS stream among {} formats",
            formats.len()
        )));
    };
    let manifest = f
        .get("manifest_url")
        .and_then(Value::as_str)
        .or_else(|| f.get("url").and_then(Value::as_str))
        .ok_or_else(|| ExtractError::NoStream("yt-dlp: the HLS format has no URL".into()))?
        .to_string();
    let headers = {
        let h = headers(f.get("http_headers"));
        if h.is_empty() {
            headers(v.get("http_headers"))
        } else {
            h
        }
    };
    Ok(Descriptor {
        tool: "yt-dlp".into(),
        expires: expires_of(&manifest, expires_param),
        manifest_url: manifest,
        headers,
        is_live: v.get("is_live").and_then(Value::as_bool),
        title: v.get("title").and_then(Value::as_str).map(str::to_string),
        duration: v.get("duration").and_then(Value::as_f64),
    })
}

/// The selected stream of `streamlink --json URL best`.
pub fn descriptor_from_streamlink(v: &Value, expires_param: &str, extra: &Phrases) -> Result<Descriptor, ExtractError> {
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        return Err(classify("streamlink", e, extra));
    }
    let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
    if kind != "hls" {
        return Err(ExtractError::NoStream(format!(
            "streamlink: the best stream is {kind}, not HLS"
        )));
    }
    let manifest = v
        .get("master")
        .and_then(Value::as_str)
        .or_else(|| v.get("url").and_then(Value::as_str))
        .ok_or_else(|| ExtractError::NoStream("streamlink: the stream has no URL".into()))?
        .to_string();
    Ok(Descriptor {
        tool: "streamlink".into(),
        expires: expires_of(&manifest, expires_param),
        manifest_url: manifest,
        headers: headers(v.get("headers")),
        is_live: None,
        title: v
            .get("metadata")
            .and_then(|m| m.get("title"))
            .and_then(Value::as_str)
            .map(str::to_string),
        duration: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn yt_dlp(script: Option<PathBuf>, timeout: Duration) -> Arc<RelayPolicy> {
        let tools = script
            .map(|command| {
                Arc::new(Tool {
                    name: "yt-dlp".into(),
                    command,
                    args: ["--ignore-config", "--proxy", "{proxy}", "--", "{url}"]
                        .map(String::from)
                        .to_vec(),
                    output: OutputFormat::YtDlpJson,
                    timeout,
                    phrases: Phrases::default(),
                })
            })
            .into_iter()
            .collect();
        Arc::new(RelayPolicy::new(tools, vec![400, 403], "expires".into()))
    }

    fn page(policy: &Arc<RelayPolicy>, page: &str) -> Lookup {
        Lookup::new(page, policy.clone())
    }

    #[test]
    fn reads_yt_dlp() {
        let v = json!({
            "title": "Match", "is_live": false, "duration": 8163.0,
            "http_headers": {"User-Agent": "Mozilla/5.0 Chrome", "Cookie": "x=1"},
            "formats": [
                {"format_id": "url1080", "protocol": "https", "url": "https://cdn/x.mp4"},
                {"format_id": "hls-720", "protocol": "m3u8_native", "url": "https://cdn/720.m3u8",
                 "manifest_url": "https://cdn/master.m3u8?expires=1999999999&sig=a",
                 "http_headers": {"User-Agent": "Mozilla/5.0 Chrome", "Cookie": "secret", "Accept": "*/*"}}
            ]
        });
        let d = descriptor_from_yt_dlp(&v, "expires").unwrap();
        assert_eq!(d.manifest_url, "https://cdn/master.m3u8?expires=1999999999&sig=a");
        assert_eq!(d.expires, Some(1999999999));
        assert_eq!(d.is_live, Some(false));
        assert!(d.headers.iter().all(|(k, _)| k != "Cookie"), "{:?}", d.headers);
        assert!(d.headers.iter().any(|(k, _)| k == "User-Agent"));
        let none = json!({"formats": [{"protocol": "https", "url": "https://cdn/x.mp4"}]});
        assert!(matches!(
            descriptor_from_yt_dlp(&none, "expires"),
            Err(ExtractError::NoStream(_))
        ));
    }

    #[test]
    fn reads_streamlink() {
        let v = json!({"type": "hls", "url": "https://cdn/720.m3u8", "master": "https://cdn/master.m3u8",
                       "headers": {"User-Agent": "Mozilla/5.0 Firefox"}});
        let d = descriptor_from_streamlink(&v, "expires", &Phrases::default()).unwrap();
        assert_eq!(d.manifest_url, "https://cdn/master.m3u8");
        assert!(matches!(
            descriptor_from_streamlink(&json!({"type": "http", "url": "x"}), "expires", &Phrases::default()),
            Err(ExtractError::NoStream(_))
        ));
        assert!(matches!(
            descriptor_from_streamlink(
                &json!({"error": "No plugin can handle URL: x"}),
                "expires",
                &Phrases::default()
            ),
            Err(ExtractError::Failed(_))
        ));
    }

    #[test]
    fn classifies_errors() {
        assert!(matches!(
            classify("yt-dlp", "ERROR: [vk] 1_2: Video not found", &Phrases::default()),
            ExtractError::NotFound(_)
        ));
        assert!(matches!(
            classify("yt-dlp", "ERROR: No video formats found!", &Phrases::default()),
            ExtractError::NoStream(_)
        ));
        assert!(matches!(
            classify(
                "yt-dlp",
                "ERROR: Unable to download webpage: timed out",
                &Phrases::default()
            ),
            ExtractError::Failed(_)
        ));
    }

    #[tokio::test]
    async fn runs_tools_with_hygiene_and_coalesces() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("calls");
        // A fake yt-dlp: records its arguments, sleeps a little, prints a descriptor.
        let script = dir.path().join("yt-dlp");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$@\" >> {}\nsleep 0.2\necho '{{\"is_live\": true, \"formats\": [{{\"protocol\": \"m3u8_native\", \"url\": \"https://cdn/a.m3u8\"}}]}}'\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let policy = yt_dlp(Some(script), Duration::from_secs(5));
        let x = Extractor::new("http://127.0.0.1:9".into(), 2);
        let l = page(&policy, "https://vk.com/video1_2");
        let (a, b) = tokio::join!(
            x.descriptor(&l, Priority::Playback),
            x.descriptor(&l, Priority::Background)
        );
        assert_eq!(a.unwrap().manifest_url, "https://cdn/a.m3u8");
        assert_eq!(b.unwrap().is_live, Some(true));
        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(calls.lines().count(), 1, "coalesced: {calls}");
        assert!(
            calls.contains("--ignore-config")
                && calls.contains("--proxy http://127.0.0.1:9 -- https://vk.com/video1_2"),
            "{calls}"
        );
    }

    #[test]
    fn unknown_liveness_is_looked_up_again_soon() {
        let d = Descriptor {
            tool: "streamlink".into(),
            manifest_url: "https://cdn/m.m3u8".into(),
            headers: vec![],
            is_live: None,
            title: None,
            duration: None,
            expires: None,
        };
        let policy = yt_dlp(None, Duration::from_secs(1));
        let x = Extractor::new("http://127.0.0.1:9".into(), 1);
        let p = page(&policy, "p");
        x.0.store(&p.key(), &Ok(d.clone()));
        let until = x.0.cache.lock().unwrap()[&p.key()].until;
        assert!(until <= Instant::now() + SHORT_TTL);
        // An observed ENDLIST promotes it to a recording.
        x.observed_ended(&p, &d);
        let until = x.0.cache.lock().unwrap()[&p.key()].until;
        assert!(until > Instant::now() + Duration::from_secs(3600));
        // A live lookup is never promoted.
        let live = Descriptor {
            is_live: Some(true),
            ..d
        };
        let q = page(&policy, "q");
        x.0.store(&q.key(), &Ok(live.clone()));
        x.observed_ended(&q, &live);
        assert!(x.0.cache.lock().unwrap()[&q.key()].until <= Instant::now() + SHORT_TTL);
    }

    #[tokio::test]
    async fn background_work_never_refuses_playback() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("yt-dlp");
        std::fs::write(
            &script,
            "#!/bin/sh\nsleep 0.3\necho '{\"formats\": [{\"protocol\": \"m3u8_native\", \"url\": \"https://cdn/a.m3u8\"}]}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let policy = yt_dlp(Some(script), Duration::from_secs(5));
        let x = Extractor::new("http://127.0.0.1:9".into(), 2);
        // Fill background admission with lookups of distinct pages.
        let bg: Vec<_> = (0..20)
            .map(|i| {
                let (x, policy) = (x.clone(), policy.clone());
                tokio::spawn(async move {
                    x.descriptor(
                        &page(&policy, &format!("https://vk.com/video1_{i}")),
                        Priority::Background,
                    )
                    .await
                })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let r = x
            .descriptor(&page(&policy, "https://vk.com/video9_9"), Priority::Playback)
            .await;
        assert!(r.is_ok(), "{r:?}");
        let refused = futures_util::future::join_all(bg)
            .await
            .into_iter()
            .filter(|r| matches!(r, Ok(Err(ExtractError::Failed(m))) if m.contains("too many")))
            .count();
        assert!(refused > 0, "background admission is bounded");
    }

    #[tokio::test]
    async fn helpers_die_with_their_tool() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("helper.pid");
        let script = dir.path().join("yt-dlp");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh
sleep 30 &
echo $! > {}
sleep 30
",
                pidfile.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let policy = yt_dlp(Some(script), Duration::from_millis(500));
        let x = Extractor::new("http://127.0.0.1:9".into(), 1);
        let _ = x
            .descriptor(&page(&policy, "https://vk.com/video1_2"), Priority::Playback)
            .await;
        let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_string();
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid])
            .status()
            .unwrap()
            .success();
        assert!(!alive, "the helper {pid} outlived its tool");
    }

    #[tokio::test]
    async fn kills_slow_tools() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("yt-dlp");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let policy = yt_dlp(Some(script), Duration::from_millis(300));
        let x = Extractor::new("http://127.0.0.1:9".into(), 1);
        let started = Instant::now();
        let r = x
            .descriptor(&page(&policy, "https://vk.com/video1_2"), Priority::Playback)
            .await;
        assert!(matches!(r, Err(ExtractError::Failed(m)) if m.contains("longer than")));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
