//! HLS relay for page rules.
//!
//! `GET /play/<rule>.m3u8?ref=<ref>` looks the page up (cached), opens a session bound to that
//! lookup, and returns the master playlist with every URI rewritten to `s/<session>/<n>.<ext>`.
//! Child playlists and segments are then served from `/play/s/<session>/<n>.<ext>`: playlists are
//! fetched fresh on every request (live playlists change) and rewritten again; segments, keys and
//! init sections are streamed, with `Range` passed through. Only the gateway talks to the CDN, with
//! the headers the lookup found; client headers other than `Range`/`If-Range` are never forwarded.
//!
//! An upstream 400/401/403/404/410 means the signed URLs expired or were revoked (VK answers 400
//! for a rejected signature). The lookup is dropped from the cache. A child playlist is then
//! refreshed once: the page is looked up again and the session continues only if the new master
//! has a playlist with exactly the same definition (variant attributes or rendition tag); otherwise,
//! and for segments, the client gets 410 and reopening the play URL starts afresh. A session never
//! splices another rendition or a recording into its timeline.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use url::Url;

use crate::extract::{Descriptor, ExtractError, Extractor, Priority};
use crate::hls::{self, Kind, Summary};
use crate::net::{Allow, check_url, read_capped};

struct Session {
    page: String,
    descriptor: Descriptor,
    /// Upstream URL, kind and identifying context (see `hls::rewrite`) of each mapped resource.
    entries: Vec<(Url, Kind, String)>,
    index: HashMap<String, usize>,
    last_used: Instant,
    /// The one compatible refresh after expired URLs was used.
    refreshed: bool,
    /// Serialises refreshes: concurrent expired requests join the one in progress.
    refresh_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    /// What each media playlist entry last served, to prove a refresh continues its timeline.
    served: HashMap<usize, Summary>,
}

/// A session used this recently is treated as being watched and is never evicted.
const ACTIVE: std::time::Duration = std::time::Duration::from_secs(60);

pub struct Relay {
    pub client: reqwest::Client,
    pub allow: Allow,
    pub extractor: Extractor,
    sessions: Mutex<HashMap<String, Session>>,
    max_sessions: usize,
    max_resources: usize,
    idle: std::time::Duration,
    manifest_bytes: usize,
}

const HLS_TYPE: &str = "application/vnd.apple.mpegurl";

impl Relay {
    pub fn new(client: reqwest::Client, allow: Allow, extractor: Extractor, limits: &crate::config::Limits) -> Self {
        Relay {
            client,
            allow,
            extractor,
            sessions: Mutex::new(HashMap::new()),
            max_sessions: limits.sessions,
            max_resources: limits.session_resources,
            idle: limits.session_idle(),
            manifest_bytes: limits.manifest_bytes,
        }
    }

    /// Open a session for `page` and answer with its rewritten master playlist. HEAD answers the
    /// headers only, without a lookup or a session.
    pub async fn play(&self, page: &str, head: bool) -> Response {
        if head {
            return playlist_response(String::new(), true);
        }
        let d = match self.extractor.descriptor(page, Priority::Playback).await {
            Ok(d) => d,
            Err(e) => return extract_error(&e),
        };
        let Some(sid) = self.open_session(page, &d) else {
            return text(
                StatusCode::SERVICE_UNAVAILABLE,
                "too many streams are being watched; try again later",
            );
        };
        let (body, _) = match self.fetch_playlist(&sid, &d.manifest_url, None).await {
            Ok(r) => r,
            Err(Refetch::Response(r)) => return r,
            Err(Refetch::Expired) => return self.expired(&sid),
        };
        playlist_response(body, false)
    }

    /// Serve `/play/s/<sid>/<file>`.
    pub async fn resource(&self, sid: &str, file: &str, req: &HeaderMap, head: bool) -> Response {
        let Some(n) = file.split('.').next().and_then(|s| s.parse::<usize>().ok()) else {
            return text(StatusCode::NOT_FOUND, "no such resource");
        };
        let entry = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(s) = sessions.get_mut(sid) else {
                return text(
                    StatusCode::GONE,
                    "this playback session expired; open the channel again",
                );
            };
            s.last_used = Instant::now();
            s.entries
                .get(n)
                .cloned()
                .map(|(u, k, _)| (u, k, s.descriptor.headers.clone(), s.page.clone()))
        };
        let Some((url, kind, headers, page)) = entry else {
            return text(StatusCode::NOT_FOUND, "no such resource");
        };
        match kind {
            Kind::Playlist => match self.fetch_playlist(sid, url.as_str(), Some(n)).await {
                Ok((body, _)) => playlist_response(body, head),
                Err(Refetch::Expired) => match self.refresh(sid, n, &url).await {
                    Some(new_url) => match self.fetch_playlist(sid, new_url.as_str(), Some(n)).await {
                        Ok((body, _)) => playlist_response(body, head),
                        Err(_) => self.expired(sid),
                    },
                    None => self.expired(sid),
                },
                Err(Refetch::Response(r)) => r,
            },
            Kind::Resource => self.stream(&url, &headers, &page, req, head).await,
        }
    }

    /// End a session whose URLs expired; the client reopens the play URL.
    fn expired(&self, sid: &str) -> Response {
        self.forget(sid);
        text(StatusCode::GONE, "the stream's URLs expired; open the channel again")
    }

    /// After `failed` (entry `n`'s URL) expired: look the page up again and, once per session,
    /// move entry `n` onto the new master's playlist with the same definition, if its timeline
    /// provably continues the one served so far. Concurrent callers join one refresh; a caller whose
    /// URL was already replaced gets the replacement. None when no continuation is possible.
    async fn refresh(&self, sid: &str, n: usize, failed: &Url) -> Option<Url> {
        let lock = self.sessions.lock().unwrap().get(sid)?.refresh_lock.clone();
        let _one = lock.lock().await;
        let (page, context, old_live, served) = {
            let sessions = self.sessions.lock().unwrap();
            let s = sessions.get(sid)?;
            let (current, _, context) = s.entries.get(n)?;
            if current != failed {
                // Another request already refreshed this entry: a late answer for the old URL.
                return Some(current.clone());
            }
            if s.refreshed || context.is_empty() {
                return None;
            }
            (
                s.page.clone(),
                context.clone(),
                s.descriptor.is_live,
                s.served.get(&n).cloned()?,
            )
        };
        self.extractor.invalidate(&page);
        let d = self.extractor.descriptor(&page, Priority::Playback).await.ok()?;
        if d.is_live != old_live {
            return None; // live and recording are different timelines
        }
        let (body, final_url) = self.get_text(&d.manifest_url, &d.headers).await.ok()?;
        let mut found = None;
        hls::rewrite(&body, &final_url, |u, k, c| {
            if k == Kind::Playlist && c == context && found.is_none() {
                found = Some(u);
            }
            Ok(String::new())
        })
        .ok()?;
        let new_url = found?;
        let (media, media_url) = self.get_text(new_url.as_str(), &d.headers).await.ok()?;
        let (_, next) = hls::rewrite(&media, &media_url, |_, _, _| Ok(String::new())).ok()?;
        if !continues(&served, &next) {
            tracing::info!("session {sid}: refreshed playlist does not continue the timeline; ending the session");
            return None;
        }
        let mut sessions = self.sessions.lock().unwrap();
        let s = sessions.get_mut(sid)?;
        s.refreshed = true;
        s.descriptor = d;
        s.index.insert(new_url.to_string(), n);
        s.entries[n].0 = new_url.clone();
        tracing::info!("session {sid}: refreshed expired URLs for the same playlist");
        Some(new_url)
    }

    /// The content state of `page`: its lookup's live flag and its first media playlist's ENDLIST.
    pub async fn probe(&self, page: &str, priority: Priority) -> Result<(Descriptor, Summary), ExtractError> {
        let d = self.extractor.descriptor(page, priority).await?;
        let mut url = d.manifest_url.clone();
        for _ in 0..2 {
            let (body, final_url) = match self.get_text(&url, &d.headers).await {
                Ok(r) => r,
                Err(Upstream::Gone) => {
                    // The lookup's URLs no longer work: the next lookup must be a new one.
                    self.extractor.invalidate(page);
                    return Err(ExtractError::Failed("the stream's URLs expired".into()));
                }
                Err(e) => return Err(ExtractError::Failed(format!("fetching the playlist: {e}"))),
            };
            let mut first_variant = None;
            let (_, summary) = hls::rewrite(&body, &final_url, |u, k, _| {
                if k == Kind::Playlist && first_variant.is_none() {
                    first_variant = Some(u.to_string());
                }
                Ok(String::new())
            })
            .map_err(|e| ExtractError::Failed(format!("the playlist: {e}")))?;
            match (summary.master, first_variant) {
                (true, Some(v)) => url = v,
                _ => {
                    if summary.ended && summary.segments > 0 {
                        self.extractor.observed_ended(page, &d);
                    }
                    return Ok((d, summary));
                }
            }
        }
        Err(ExtractError::Failed("nested master playlists".into()))
    }

    /// A new session, or None when every slot is held by a session watched right now (active
    /// viewers are never evicted; idle ones are, least recently used first).
    fn open_session(&self, page: &str, d: &Descriptor) -> Option<String> {
        let sid = new_id();
        let mut sessions = self.sessions.lock().unwrap();
        let now = Instant::now();
        sessions.retain(|_, s| now.duration_since(s.last_used) < self.idle);
        while sessions.len() >= self.max_sessions {
            let oldest = sessions
                .iter()
                .filter(|(_, s)| now.duration_since(s.last_used) >= ACTIVE)
                .min_by_key(|(_, s)| s.last_used)
                .map(|(k, _)| k.clone())?;
            sessions.remove(&oldest);
        }
        sessions.insert(
            sid.clone(),
            Session {
                page: page.to_string(),
                descriptor: d.clone(),
                entries: Vec::new(),
                index: HashMap::new(),
                last_used: now,
                refreshed: false,
                refresh_lock: Default::default(),
                served: HashMap::new(),
            },
        );
        Some(sid)
    }

    /// Fetch and rewrite a playlist of session `sid`. `top`: the master, served at
    /// `/play/<rule>.m3u8`, whose URIs need the `s/<sid>/` prefix.
    /// `entry`: the media playlist's entry index; None for the master, served at
    /// `/play/<rule>.m3u8`, whose URIs need the `s/<sid>/` prefix.
    async fn fetch_playlist(&self, sid: &str, url: &str, entry: Option<usize>) -> Result<(String, Summary), Refetch> {
        let top = entry.is_none();
        let headers = {
            let sessions = self.sessions.lock().unwrap();
            match sessions.get(sid) {
                Some(s) => s.descriptor.headers.clone(),
                None => {
                    return Err(Refetch::Response(text(
                        StatusCode::GONE,
                        "this playback session expired; open the channel again",
                    )));
                }
            }
        };
        let (body, final_url) = match self.get_text(url, &headers).await {
            Ok(r) => r,
            Err(Upstream::Gone) if top => return Err(Refetch::Response(self.expired(sid))),
            Err(Upstream::Gone) => return Err(Refetch::Expired),
            Err(e) => return Err(Refetch::Response(text(StatusCode::BAD_GATEWAY, &e.to_string()))),
        };
        let mut sessions = self.sessions.lock().unwrap();
        let Some(s) = sessions.get_mut(sid) else {
            return Err(Refetch::Response(text(
                StatusCode::GONE,
                "this playback session expired; open the channel again",
            )));
        };
        let max = self.max_resources;
        let rewritten = hls::rewrite(&body, &final_url, |u, k, context| {
            let key = u.to_string();
            let n = match s.index.get(&key) {
                Some(n) => *n,
                None => {
                    if s.entries.len() >= max {
                        return Err("this playback session has too many resources".into());
                    }
                    s.entries.push((u.clone(), k, context.to_string()));
                    s.index.insert(key, s.entries.len() - 1);
                    s.entries.len() - 1
                }
            };
            let ext = match k {
                Kind::Playlist => "m3u8".to_string(),
                Kind::Resource => extension(&u),
            };
            Ok(if top {
                format!("s/{sid}/{n}.{ext}")
            } else {
                format!("{n}.{ext}")
            })
        });
        match rewritten {
            Ok((body, summary)) => {
                if summary.ended && !summary.master && summary.segments > 0 {
                    // A finished media playlist with content: unless the lookup said live, keep it
                    // longer. An empty one is a recording still being processed.
                    self.extractor.observed_ended(&s.page, &s.descriptor);
                }
                if let Some(n) = entry
                    && !summary.master
                {
                    s.served.insert(n, summary.clone());
                }
                Ok((body, summary))
            }
            Err(e) => Err(Refetch::Response(text(
                StatusCode::BAD_GATEWAY,
                &format!("cannot relay this stream: {e}"),
            ))),
        }
    }

    fn forget(&self, sid: &str) {
        let page = self.sessions.lock().unwrap().remove(sid).map(|s| s.page);
        if let Some(p) = page {
            self.extractor.invalidate(&p);
        }
    }

    async fn get_text(&self, url: &str, headers: &[(String, String)]) -> Result<(String, Url), Upstream> {
        let u = Url::parse(url).map_err(|_| Upstream::Failed("bad upstream URL".into()))?;
        check_url(&u, &self.allow).map_err(|e| Upstream::Failed(e.to_string()))?;
        let mut req = self.client.get(u);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let res = req
            .send()
            .await
            .map_err(|e| Upstream::Failed(e.without_url().to_string()))?;
        let status = res.status().as_u16();
        if expired_status(status) {
            return Err(Upstream::Gone);
        }
        if !res.status().is_success() {
            return Err(Upstream::Failed(format!("upstream answered HTTP {status}")));
        }
        let final_url = res.url().clone();
        let bytes = read_capped(res, self.manifest_bytes)
            .await
            .map_err(|e| Upstream::Failed(e.to_string()))?;
        let text = String::from_utf8(bytes).map_err(|_| Upstream::Failed("the playlist is not UTF-8".into()))?;
        Ok((text, final_url))
    }

    async fn stream(
        &self,
        url: &Url,
        headers: &[(String, String)],
        page: &str,
        req: &HeaderMap,
        head: bool,
    ) -> Response {
        if let Err(e) = check_url(url, &self.allow) {
            return text(StatusCode::FORBIDDEN, &e.to_string());
        }
        let mut up = if head {
            self.client.head(url.clone())
        } else {
            self.client.get(url.clone())
        };
        for (k, v) in headers {
            up = up.header(k, v);
        }
        for name in [header::RANGE, header::IF_RANGE] {
            if let Some(v) = req.get(&name).and_then(|v| v.to_str().ok()) {
                if name == header::RANGE && !valid_range(v) {
                    return text(StatusCode::RANGE_NOT_SATISFIABLE, "only one byte range is supported");
                }
                up = up.header(name.as_str(), v);
            }
        }
        let res = match up.send().await {
            Ok(r) => r,
            Err(e) => return text(StatusCode::BAD_GATEWAY, &format!("upstream: {}", e.without_url())),
        };
        let status = res.status().as_u16();
        if expired_status(status) {
            self.extractor.invalidate(page);
            return text(StatusCode::GONE, "the stream's URLs expired; open the channel again");
        }
        if !(res.status().is_success() || status == 416) {
            return text(StatusCode::BAD_GATEWAY, &format!("upstream answered HTTP {status}"));
        }
        let mut out = Response::builder().status(status);
        for name in [
            header::CONTENT_TYPE,
            header::CONTENT_LENGTH,
            header::CONTENT_RANGE,
            header::ACCEPT_RANGES,
            header::LAST_MODIFIED,
            header::ETAG,
        ] {
            if let Some(v) = res.headers().get(&name) {
                out = out.header(name, v.clone());
            }
        }
        out = out.header(header::CACHE_CONTROL, "no-store");
        let body = if head {
            Body::empty()
        } else {
            Body::from_stream(res.bytes_stream())
        };
        out.body(body)
            .unwrap_or_else(|_| text(StatusCode::BAD_GATEWAY, "bad upstream response"))
    }
}

/// Whether `next` (the refreshed media playlist) continues `prev` (what the session served): same
/// target duration, its media sequence inside the window already served or just after it, and no
/// switch from a growing playlist to a finished one: a live broadcast is never continued as a
/// finished playlist (it might be the recording); the player reopens instead.
fn continues(prev: &Summary, next: &Summary) -> bool {
    let (a, b) = (prev.media_sequence.unwrap_or(0), next.media_sequence.unwrap_or(0));
    prev.target_duration == next.target_duration
        && !(next.ended && !prev.ended)
        && next.segments > 0
        && b >= a
        && b <= a + prev.segments as u64
}

/// Statuses a CDN answers for an expired or rejected signed URL (VK: 400).
fn expired_status(status: u16) -> bool {
    matches!(status, 400 | 401 | 403 | 404 | 410)
}

enum Refetch {
    /// The playlist's URL expired; a compatible refresh may help.
    Expired,
    Response(Response),
}

#[derive(Debug, thiserror::Error)]
enum Upstream {
    #[error("the stream's URLs expired")]
    Gone,
    #[error("{0}")]
    Failed(String),
}

fn playlist_response(body: String, head: bool) -> Response {
    let len = body.len();
    let mut r = if head { Body::empty() } else { Body::from(body) }.into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(HLS_TYPE));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    r
}

pub fn text(status: StatusCode, msg: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        msg.to_string(),
    )
        .into_response()
}

fn extract_error(e: &ExtractError) -> Response {
    match e {
        ExtractError::NotFound(m) => text(StatusCode::NOT_FOUND, m),
        ExtractError::NoStream(m) => text(StatusCode::SERVICE_UNAVAILABLE, m),
        ExtractError::Failed(m) => text(StatusCode::BAD_GATEWAY, m),
    }
}

/// `bytes=a-b`, `bytes=a-` or `bytes=-n`: one range only.
fn valid_range(v: &str) -> bool {
    let Some(r) = v.trim().strip_prefix("bytes=") else {
        return false;
    };
    if r.contains(',') {
        return false;
    }
    let Some((a, b)) = r.split_once('-') else { return false };
    let num = |s: &str| s.is_empty() || s.bytes().all(|c| c.is_ascii_digit());
    num(a) && num(b) && !(a.is_empty() && b.is_empty())
}

/// A short extension from the upstream path, for players that look at it.
fn extension(u: &Url) -> String {
    let last = u.path_segments().and_then(|mut s| s.next_back()).unwrap_or_default();
    match last.rsplit_once('.') {
        Some((_, e)) if (1..=5).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_alphanumeric()) => {
            e.to_ascii_lowercase()
        }
        _ => "bin".into(),
    }
}

/// A session id: 64 bits from a randomly keyed hasher (a new key per process and call).
fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(N.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuity() {
        let s = |seq: u64, segs: usize, ended: bool| Summary {
            ended,
            master: false,
            segments: segs,
            media_sequence: Some(seq),
            target_duration: Some("6".into()),
        };
        assert!(
            continues(&s(0, 1361, true), &s(0, 1361, true)),
            "the same recording, re-signed"
        );
        assert!(
            continues(&s(100, 10, false), &s(104, 10, false)),
            "a live window that moved on"
        );
        assert!(
            !continues(&s(100, 10, false), &s(105, 10, true)),
            "live is never continued as finished"
        );
        assert!(
            !continues(&s(5000, 10, false), &s(0, 1361, true)),
            "live replaced by its recording"
        );
        assert!(
            !continues(&s(0, 10, false), &s(0, 1361, true)),
            "growing to finished with a reset"
        );
        assert!(!continues(&s(100, 10, false), &s(50, 10, false)), "sequence went back");
        assert!(!continues(&s(100, 10, false), &s(200, 10, false)), "a gap");
        let mut other = s(0, 1361, true);
        other.target_duration = Some("4".into());
        assert!(!continues(&s(0, 1361, true), &other), "another encode");
    }

    #[test]
    fn ranges() {
        assert!(valid_range("bytes=0-99"));
        assert!(valid_range("bytes=100-"));
        assert!(valid_range("bytes=-500"));
        for bad in ["bytes=0-1,5-6", "items=0-1", "bytes=-", "bytes=a-b"] {
            assert!(!valid_range(bad), "{bad}");
        }
    }

    #[test]
    fn extensions() {
        let e = |s: &str| extension(&Url::parse(s).unwrap());
        assert_eq!(e("https://c/a/seg-1.ts?x=1"), "ts");
        assert_eq!(e("https://c/a/init.MP4"), "mp4");
        assert_eq!(e("https://c/key"), "bin");
    }
}
