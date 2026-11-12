//! Running the extractors (yt-dlp first, streamlink as the fallback) as subprocesses, and caching
//! what they find as descriptors.
//!
//! Subprocess hygiene: argument arrays (no shell), null stdin, their own config files ignored,
//! all traffic through the egress proxy, bounded output, a deadline, and kill on drop (the process
//! group is killed so helpers die too). At most `limits.extractions` run at once; background work
//! (observe, check) gets at most one of those slots, so playback always has room. Lookups of the
//! same page are coalesced.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;

/// What an extraction found: the HLS manifest to play and how to request it.
#[derive(Debug, Clone, PartialEq)]
pub struct Descriptor {
    pub tool: &'static str,
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

#[derive(Debug, Clone)]
pub struct Tools {
    pub yt_dlp: Option<PathBuf>,
    pub streamlink: Option<PathBuf>,
    /// The egress proxy URL every tool must use.
    pub proxy: String,
    pub timeout: Duration,
}

struct Entry {
    result: Result<Descriptor, ExtractError>,
    until: Instant,
    /// Consecutive failures, for backoff.
    failures: u32,
}

struct Inner {
    tools: Tools,
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
    pub fn new(tools: Tools, concurrency: usize) -> Self {
        Extractor(Arc::new(Inner {
            tools,
            all: Arc::new(Semaphore::new(concurrency.max(1))),
            background: Arc::new(Semaphore::new(1)),
            cache: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
            admitted_playback: std::sync::atomic::AtomicUsize::new(0),
            admitted_background: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    /// The descriptor of `page`, from the cache or a new extraction. The extraction runs in a task
    /// of its own: a caller that goes away does not stop work other callers wait for, and the
    /// tool's process group is still killed on its deadline.
    pub async fn descriptor(&self, page: &str, priority: Priority) -> Result<Descriptor, ExtractError> {
        if let Some(r) = self.0.cached(page) {
            return r;
        }
        let me = self.0.clone();
        let page = page.to_string();
        tokio::spawn(async move { me.lookup(&page, priority).await })
            .await
            .unwrap_or_else(|e| Err(ExtractError::Failed(format!("lookup task failed: {e}"))))
    }

    /// Forget a page's descriptor (its URLs stopped working).
    pub fn invalidate(&self, page: &str) {
        self.0.cache.lock().unwrap().remove(page);
    }

    /// A media playlist of `page`'s descriptor has ENDLIST: unless the lookup said live, the item
    /// is a finished recording and its descriptor may be kept longer.
    pub fn observed_ended(&self, page: &str, d: &Descriptor) {
        if d.is_live == Some(true) {
            return;
        }
        let mut cache = self.0.cache.lock().unwrap();
        if let Some(e) = cache.get_mut(page)
            && matches!(&e.result, Ok(x) if x == d)
        {
            e.until = Instant::now() + expiry_cap(d, RECORDING_TTL);
        }
    }
}

impl Inner {
    fn cached(&self, page: &str) -> Option<Result<Descriptor, ExtractError>> {
        let cache = self.cache.lock().unwrap();
        cache
            .get(page)
            .filter(|e| e.until > Instant::now())
            .map(|e| e.result.clone())
    }

    async fn lookup(self: Arc<Self>, page: &str, priority: Priority) -> Result<Descriptor, ExtractError> {
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
        let result = self.extract(page).await;
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

    async fn extract(&self, page: &str) -> Result<Descriptor, ExtractError> {
        let mut errors = Vec::new();
        if let Some(y) = &self.tools.yt_dlp {
            match self.yt_dlp(y, page).await {
                Ok(d) => return Ok(d),
                Err(e) => errors.push(e),
            }
        }
        if let Some(s) = &self.tools.streamlink {
            match self.streamlink(s, page).await {
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

    async fn yt_dlp(&self, bin: &PathBuf, page: &str) -> Result<Descriptor, ExtractError> {
        let args = [
            "--ignore-config",
            "--no-playlist",
            "--no-warnings",
            "--dump-single-json",
            "--socket-timeout",
            "15",
            "--proxy",
            &self.tools.proxy,
            "--",
            page,
        ];
        let out = run(bin, &args, self.tools.timeout).await?;
        let v: Value = serde_json::from_slice(&out)
            .map_err(|e| ExtractError::Failed(format!("yt-dlp printed something that is not JSON: {e}")))?;
        descriptor_from_yt_dlp(&v)
    }

    async fn streamlink(&self, bin: &PathBuf, page: &str) -> Result<Descriptor, ExtractError> {
        let args = [
            "--no-config",
            "--json",
            "--http-proxy",
            &self.tools.proxy,
            "--",
            page,
            "best",
        ];
        let out = run(bin, &args, self.tools.timeout).await?;
        let v: Value = serde_json::from_slice(&out)
            .map_err(|e| ExtractError::Failed(format!("streamlink printed something that is not JSON: {e}")))?;
        descriptor_from_streamlink(&v)
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
async fn run(bin: &PathBuf, args: &[&str], timeout: Duration) -> Result<Vec<u8>, ExtractError> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let name = bin
        .file_name()
        .map_or_else(|| "tool".into(), |n| n.to_string_lossy().to_string());
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
            Err(classify(&name, &line))
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
fn classify(tool: &str, line: &str) -> ExtractError {
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
    let schema = l.contains("unable to validate") || l.contains("key '") || l.contains("unable to extract");
    if !schema && gone.iter().any(|k| l.contains(k)) {
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

fn expires_of(url: &str) -> Option<i64> {
    let u = url::Url::parse(url).ok()?;
    u.query_pairs()
        .find(|(k, _)| k == "expires")
        .and_then(|(_, v)| v.parse().ok())
        .filter(|t: &i64| *t > 1_000_000_000)
}

/// The HLS master of a yt-dlp info dict: any HLS format's `manifest_url`, else the best HLS
/// format's own URL.
pub fn descriptor_from_yt_dlp(v: &Value) -> Result<Descriptor, ExtractError> {
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
        tool: "yt-dlp",
        expires: expires_of(&manifest),
        manifest_url: manifest,
        headers,
        is_live: v.get("is_live").and_then(Value::as_bool),
        title: v.get("title").and_then(Value::as_str).map(str::to_string),
        duration: v.get("duration").and_then(Value::as_f64),
    })
}

/// The selected stream of `streamlink --json URL best`.
pub fn descriptor_from_streamlink(v: &Value) -> Result<Descriptor, ExtractError> {
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        return Err(classify("streamlink", e));
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
        tool: "streamlink",
        expires: expires_of(&manifest),
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
        let d = descriptor_from_yt_dlp(&v).unwrap();
        assert_eq!(d.manifest_url, "https://cdn/master.m3u8?expires=1999999999&sig=a");
        assert_eq!(d.expires, Some(1999999999));
        assert_eq!(d.is_live, Some(false));
        assert!(d.headers.iter().all(|(k, _)| k != "Cookie"), "{:?}", d.headers);
        assert!(d.headers.iter().any(|(k, _)| k == "User-Agent"));
        let none = json!({"formats": [{"protocol": "https", "url": "https://cdn/x.mp4"}]});
        assert!(matches!(descriptor_from_yt_dlp(&none), Err(ExtractError::NoStream(_))));
    }

    #[test]
    fn reads_streamlink() {
        let v = json!({"type": "hls", "url": "https://cdn/720.m3u8", "master": "https://cdn/master.m3u8",
                       "headers": {"User-Agent": "Mozilla/5.0 Firefox"}});
        let d = descriptor_from_streamlink(&v).unwrap();
        assert_eq!(d.manifest_url, "https://cdn/master.m3u8");
        assert!(matches!(
            descriptor_from_streamlink(&json!({"type": "http", "url": "x"})),
            Err(ExtractError::NoStream(_))
        ));
        assert!(matches!(
            descriptor_from_streamlink(&json!({"error": "No plugin can handle URL: x"})),
            Err(ExtractError::Failed(_))
        ));
    }

    #[test]
    fn classifies_errors() {
        assert!(matches!(
            classify("yt-dlp", "ERROR: [vk] 1_2: Video not found"),
            ExtractError::NotFound(_)
        ));
        assert!(matches!(
            classify("yt-dlp", "ERROR: No video formats found!"),
            ExtractError::NoStream(_)
        ));
        assert!(matches!(
            classify("yt-dlp", "ERROR: Unable to download webpage: timed out"),
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
        let x = Extractor::new(
            Tools {
                yt_dlp: Some(script),
                streamlink: None,
                proxy: "http://127.0.0.1:9".into(),
                timeout: Duration::from_secs(5),
            },
            2,
        );
        let (a, b) = tokio::join!(
            x.descriptor("https://vk.com/video1_2", Priority::Playback),
            x.descriptor("https://vk.com/video1_2", Priority::Background)
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
            tool: "streamlink",
            manifest_url: "https://cdn/m.m3u8".into(),
            headers: vec![],
            is_live: None,
            title: None,
            duration: None,
            expires: None,
        };
        let x = Extractor::new(
            Tools {
                yt_dlp: None,
                streamlink: None,
                proxy: String::new(),
                timeout: Duration::from_secs(1),
            },
            1,
        );
        x.0.store("p", &Ok(d.clone()));
        let until = x.0.cache.lock().unwrap()["p"].until;
        assert!(until <= Instant::now() + SHORT_TTL);
        // An observed ENDLIST promotes it to a recording.
        x.observed_ended("p", &d);
        let until = x.0.cache.lock().unwrap()["p"].until;
        assert!(until > Instant::now() + Duration::from_secs(3600));
        // A live lookup is never promoted.
        let live = Descriptor {
            is_live: Some(true),
            ..d
        };
        x.0.store("q", &Ok(live.clone()));
        x.observed_ended("q", &live);
        assert!(x.0.cache.lock().unwrap()["q"].until <= Instant::now() + SHORT_TTL);
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
        let x = Extractor::new(
            Tools {
                yt_dlp: Some(script),
                streamlink: None,
                proxy: "http://127.0.0.1:9".into(),
                timeout: Duration::from_secs(5),
            },
            2,
        );
        // Fill background admission with lookups of distinct pages.
        let bg: Vec<_> = (0..20)
            .map(|i| {
                let x = x.clone();
                tokio::spawn(async move {
                    x.descriptor(&format!("https://vk.com/video1_{i}"), Priority::Background)
                        .await
                })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let r = x.descriptor("https://vk.com/video9_9", Priority::Playback).await;
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
        let x = Extractor::new(
            Tools {
                yt_dlp: Some(script),
                streamlink: None,
                proxy: "http://127.0.0.1:9".into(),
                timeout: Duration::from_millis(500),
            },
            1,
        );
        let _ = x.descriptor("https://vk.com/video1_2", Priority::Playback).await;
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
        let x = Extractor::new(
            Tools {
                yt_dlp: Some(script),
                streamlink: None,
                proxy: "http://127.0.0.1:9".into(),
                timeout: Duration::from_millis(300),
            },
            1,
        );
        let started = Instant::now();
        let r = x.descriptor("https://vk.com/video1_2", Priority::Playback).await;
        assert!(matches!(r, Err(ExtractError::Failed(m)) if m.contains("longer than")));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
