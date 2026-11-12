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

pub struct Extractor {
    tools: Tools,
    all: Arc<Semaphore>,
    background: Arc<Semaphore>,
    cache: Mutex<HashMap<String, Entry>>,
    /// Per-page locks for coalescing.
    inflight: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

const MAX_OUTPUT: usize = 16 * 1024 * 1024;
const RECORDING_TTL: Duration = Duration::from_secs(3 * 3600);
const LIVE_TTL: Duration = Duration::from_secs(60);
/// Backoff after failures: 1, 2, 5, 10 minutes.
const BACKOFF: [u64; 4] = [60, 120, 300, 600];

impl Extractor {
    pub fn new(tools: Tools, concurrency: usize) -> Self {
        Extractor {
            tools,
            all: Arc::new(Semaphore::new(concurrency.max(1))),
            background: Arc::new(Semaphore::new(1)),
            cache: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// The descriptor of `page`, from the cache or a new extraction.
    pub async fn descriptor(&self, page: &str, priority: Priority) -> Result<Descriptor, ExtractError> {
        if let Some(r) = self.cached(page) {
            return r;
        }
        let lock = {
            let mut m = self.inflight.lock().unwrap();
            m.entry(page.to_string()).or_default().clone()
        };
        let _g = lock.lock().await;
        if let Some(r) = self.cached(page) {
            return r;
        }
        let result = self.extract(page, priority).await;
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap();
        let failures = match &result {
            Ok(_) => 0,
            Err(_) => cache.get(page).map_or(0, |e| e.failures) + 1,
        };
        let ttl = match &result {
            Ok(d) => ttl_for(d),
            Err(ExtractError::Failed(_)) => Duration::from_secs(15),
            Err(_) => Duration::from_secs(BACKOFF[(failures as usize - 1).min(BACKOFF.len() - 1)]),
        };
        cache.insert(
            page.to_string(),
            Entry {
                result: result.clone(),
                until: now + ttl,
                failures,
            },
        );
        drop(cache);
        self.inflight.lock().unwrap().remove(page);
        result
    }

    /// Forget a page's descriptor (its URLs stopped working).
    pub fn invalidate(&self, page: &str) {
        self.cache.lock().unwrap().remove(page);
    }

    fn cached(&self, page: &str) -> Option<Result<Descriptor, ExtractError>> {
        let cache = self.cache.lock().unwrap();
        cache
            .get(page)
            .filter(|e| e.until > Instant::now())
            .map(|e| e.result.clone())
    }

    async fn extract(&self, page: &str, priority: Priority) -> Result<Descriptor, ExtractError> {
        let _bg = match priority {
            Priority::Background => Some(self.background.clone().acquire_owned().await.expect("semaphore")),
            Priority::Playback => None,
        };
        let _slot = self.all.clone().acquire_owned().await.expect("semaphore");
        let mut errors = Vec::new();
        if let Some(y) = &self.tools.yt_dlp {
            match self.yt_dlp(y, page).await {
                Ok(d) => return Ok(d),
                Err(ExtractError::NotFound(m)) => return Err(ExtractError::NotFound(m)),
                Err(e) => errors.push(e),
            }
        }
        if let Some(s) = &self.tools.streamlink {
            match self.streamlink(s, page).await {
                Ok(d) => return Ok(d),
                Err(e) => errors.push(e),
            }
        }
        // Report the most telling failure: no stream, then a tool failure.
        Err(errors
            .iter()
            .find(|e| matches!(e, ExtractError::NoStream(_)))
            .or(errors.first())
            .cloned()
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

fn ttl_for(d: &Descriptor) -> Duration {
    let base = if d.is_live == Some(true) {
        LIVE_TTL
    } else {
        RECORDING_TTL
    };
    match d.expires {
        Some(exp) => {
            let left = exp - chrono::Utc::now().timestamp() - 600;
            base.min(Duration::from_secs(left.max(0) as u64))
        }
        None => base,
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
    let pid = child.id();
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
    let result = tokio::time::timeout(timeout, work).await;
    let kill_group = || {
        #[cfg(unix)]
        if let Some(pid) = pid {
            // The whole group: helpers the tool started die too.
            unsafe_kill_group(pid);
        }
    };
    match result {
        Err(_) => {
            kill_group();
            Err(ExtractError::Failed(format!(
                "{name} took longer than {}s",
                timeout.as_secs()
            )))
        }
        Ok(Err(e)) => {
            kill_group();
            Err(ExtractError::Failed(format!("{name}: {e}")))
        }
        Ok(Ok((status, out, err))) => {
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
fn unsafe_kill_group(pid: u32) {
    // `kill -KILL -<pgid>` without libc bindings.
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// What a tool's error line says about the page.
fn classify(tool: &str, line: &str) -> ExtractError {
    let l = line.to_ascii_lowercase();
    let msg = format!("{tool}: {line}");
    if [
        "not found",
        "removed",
        "private",
        "does not exist",
        "has been deleted",
        "access restricted",
        "http error 404",
    ]
    .iter()
    .any(|k| l.contains(k))
    {
        ExtractError::NotFound(msg)
    } else if [
        "no video formats",
        "no playable streams",
        "no streams found",
        "processing",
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
        let x = Arc::new(Extractor::new(
            Tools {
                yt_dlp: Some(script),
                streamlink: None,
                proxy: "http://127.0.0.1:9".into(),
                timeout: Duration::from_secs(5),
            },
            2,
        ));
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
