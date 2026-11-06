//! EPG ingestion: downloading XMLTV sources into generation directories and publishing them.
//!
//! Layout: `<data>/epg/<source_id>/gen-<n>/` (see [`xmltv`] for the files). The database row's
//! `current_gen` names the generation being served. Fetches of all sources run one at a time under
//! `fetch_lock`, which also guards deleting directories.

pub mod xmltv;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use api_types::{EpgFetchResult, EpgFetchStatus, EpgSource};
use reqwest::StatusCode;
use reqwest::header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
use tokio::io::AsyncWriteExt;

use crate::error::{Error, Result};
use crate::store::{FetchOutcome, Store};

/// Largest download accepted for one source.
pub const MAX_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024;
/// Delay before the first scheduled fetch after start.
const FIRST_RUN_DELAY: Duration = Duration::from_secs(180);
/// How often the scheduler checks whether a run is due (wall clock, so host sleep is noticed).
const SCHEDULER_TICK: Duration = Duration::from_secs(600);

pub struct Epg {
    store: Arc<Store>,
    dir: PathBuf,
    client: reqwest::Client,
    fetch_lock: tokio::sync::Mutex<()>,
}

impl Epg {
    pub fn new(store: Arc<Store>, data_dir: &Path) -> Arc<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .user_agent(concat!("lineup/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        Arc::new(Epg {
            store,
            dir: data_dir.join("epg"),
            client,
            fetch_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    fn source_dir(&self, id: i64) -> PathBuf {
        self.dir.join(id.to_string())
    }

    pub fn gen_dir(&self, id: i64, generation: i64) -> PathBuf {
        self.source_dir(id).join(format!("gen-{generation}"))
    }

    /// Startup recovery (§6): delete incomplete generations and directories of deleted sources, and
    /// if a source's current generation is unusable, fall back to its previous one or to nothing.
    pub async fn recover(&self) -> anyhow::Result<()> {
        let _lock = self.fetch_lock.lock().await;
        tokio::fs::create_dir_all(&self.dir).await?;
        let rows = self.store.epg_generations().await?;
        let known: HashSet<String> = rows.iter().map(|(id, _, _)| id.to_string()).collect();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if !known.contains(&entry.file_name().to_string_lossy().to_string()) {
                tracing::info!("removing EPG data of deleted source: {}", entry.path().display());
                remove_dir(&entry.path()).await;
            }
        }
        for (id, current, previous) in rows {
            for (_, dir) in list_gens(&self.source_dir(id)) {
                if !xmltv::is_complete(&dir) {
                    tracing::info!("removing incomplete EPG generation {}", dir.display());
                    remove_dir(&dir).await;
                }
            }
            let Some(cur) = current else { continue };
            if xmltv::is_complete(&self.gen_dir(id, cur)) {
                continue;
            }
            let fallback = previous.filter(|p| xmltv::is_complete(&self.gen_dir(id, *p)));
            let channels = match fallback {
                Some(p) => {
                    let dir = self.gen_dir(id, p);
                    tokio::task::spawn_blocking(move || xmltv::read_channels(&dir)).await??
                }
                None => vec![],
            };
            tracing::warn!("EPG source {id}: generation {cur} is unusable; now serving {fallback:?}");
            let indexed: Vec<_> = channels.iter().map(|c| c.indexed()).collect();
            self.store.republish_epg_generation(id, fallback, &indexed).await?;
        }
        Ok(())
    }

    /// Fetch the given sources (all when empty), one at a time.
    pub async fn fetch(&self, ids: &[i64]) -> Result<Vec<EpgFetchResult>> {
        let sources = if ids.is_empty() {
            self.store.list_epg_sources().await?
        } else {
            let mut v = Vec::new();
            for id in ids {
                v.push(self.store.get_epg_source(*id).await?);
            }
            v
        };
        let _lock = self.fetch_lock.lock().await;
        let mut out = Vec::new();
        for src in sources {
            // Re-read: the row may have changed while waiting for the lock.
            let src = match self.store.get_epg_source(src.id).await {
                Ok(s) => s,
                Err(Error::NotFound(_)) => continue,
                Err(e) => return Err(e),
            };
            out.push(self.fetch_locked(&src).await);
        }
        Ok(out)
    }

    /// Fetch one source in the background (after it was created or its URL changed).
    pub fn spawn_fetch(self: &Arc<Self>, id: i64) {
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.fetch(&[id]).await {
                tracing::warn!("EPG fetch of source {id}: {e}");
            }
        });
    }

    /// Remove a deleted source's directory once no fetch is using it.
    pub fn spawn_forget(self: &Arc<Self>, id: i64) {
        let me = self.clone();
        tokio::spawn(async move {
            let _lock = me.fetch_lock.lock().await;
            if matches!(me.store.get_epg_source(id).await, Err(Error::NotFound(_))) {
                remove_dir(&me.source_dir(id)).await;
            }
        });
    }

    /// Boot-relative schedule: first run shortly after start, then every `epg_refresh_hours`.
    pub fn spawn_scheduler(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(FIRST_RUN_DELAY).await;
            let mut last_run: Option<SystemTime> = None;
            loop {
                let hours = match me.store.settings().await {
                    Ok(s) => s.epg_refresh_hours,
                    Err(e) => {
                        tracing::warn!("EPG scheduler: reading settings: {e}");
                        0
                    }
                };
                let due = hours > 0
                    && last_run.is_none_or(|t| {
                        t.elapsed()
                            .map_or(true, |el| el >= Duration::from_secs(u64::from(hours) * 3600))
                    });
                if due {
                    last_run = Some(SystemTime::now());
                    match me.fetch(&[]).await {
                        Ok(results) => {
                            for r in results.iter().filter(|r| r.status == EpgFetchStatus::Error) {
                                tracing::warn!("EPG source {}: {}", r.source_id, r.error.as_deref().unwrap_or("?"));
                            }
                        }
                        Err(e) => tracing::warn!("scheduled EPG fetch: {e}"),
                    }
                }
                tokio::time::sleep(SCHEDULER_TICK).await;
            }
        });
    }

    async fn fetch_locked(&self, src: &EpgSource) -> EpgFetchResult {
        let mut result = EpgFetchResult {
            source_id: src.id,
            status: EpgFetchStatus::Error,
            error: None,
            channels: None,
            programmes: None,
        };
        if !src.enabled {
            result.status = EpgFetchStatus::Disabled;
            return result;
        }
        match self.download_and_publish(src).await {
            Ok(Fetched::NotModified) => {
                result.status = EpgFetchStatus::NotModified;
                if let Err(e) = self
                    .store
                    .record_epg_fetch(src.id, FetchOutcome::NotModified, None)
                    .await
                {
                    tracing::warn!("recording EPG fetch: {e}");
                }
            }
            Ok(Fetched::Published { channels, programmes }) => {
                result.status = EpgFetchStatus::Updated;
                result.channels = Some(channels as i64);
                result.programmes = Some(programmes as i64);
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::warn!("EPG source {} ({}): {msg}", src.id, src.url);
                if let Err(e) = self
                    .store
                    .record_epg_fetch(src.id, FetchOutcome::Error, Some(&msg))
                    .await
                {
                    tracing::warn!("recording EPG fetch: {e}");
                }
                result.error = Some(msg);
            }
        }
        result
    }

    async fn download_and_publish(&self, src: &EpgSource) -> anyhow::Result<Fetched> {
        let source_dir = self.source_dir(src.id);
        tokio::fs::create_dir_all(&source_dir).await?;

        // Validators only when the served generation is complete and came from the configured URL.
        let validators = src
            .current_gen
            .map(|g| self.gen_dir(src.id, g))
            .filter(|d| xmltv::is_complete(d))
            .and_then(|d| xmltv::read_meta(&d))
            .filter(|m| m.url == src.url);

        let mut req = self.client.get(&src.url);
        if let Some(m) = &validators {
            if let Some(e) = &m.etag {
                req = req.header(IF_NONE_MATCH, e);
            }
            if let Some(l) = &m.last_modified {
                req = req.header(IF_MODIFIED_SINCE, l);
            }
        }
        let mut res = req.send().await?;
        if res.status() == StatusCode::NOT_MODIFIED {
            anyhow::ensure!(validators.is_some(), "server answered 304 to an unconditional request");
            return Ok(Fetched::NotModified);
        }
        anyhow::ensure!(res.status().is_success(), "HTTP {}", res.status());
        if res.content_length().is_some_and(|n| n > MAX_DOWNLOAD_BYTES) {
            anyhow::bail!("larger than {MAX_DOWNLOAD_BYTES} bytes");
        }
        let header = |name| {
            res.headers()
                .get(name)
                .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
                .map(str::to_string)
        };
        let meta = xmltv::Meta {
            url: src.url.clone(),
            etag: header(ETAG),
            last_modified: header(LAST_MODIFIED),
            fetched_at: chrono::Utc::now().to_rfc3339(),
        };

        // Always a new directory: max(existing)+1, so nothing complete is overwritten in place.
        let generation = list_gens(&source_dir).iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;
        let dir = self.gen_dir(src.id, generation);
        tokio::fs::create_dir(&dir).await?;
        let built = async {
            let mut file = tokio::fs::File::create(dir.join(xmltv::RAW)).await?;
            let mut total: u64 = 0;
            while let Some(chunk) = res.chunk().await? {
                total += chunk.len() as u64;
                anyhow::ensure!(total <= MAX_DOWNLOAD_BYTES, "larger than {MAX_DOWNLOAD_BYTES} bytes");
                file.write_all(&chunk).await?;
            }
            file.flush().await?;
            drop(file);
            let d = dir.clone();
            let now = chrono::Utc::now().timestamp();
            let meta = meta.clone();
            tokio::task::spawn_blocking(move || {
                let built = xmltv::build(&d, now)?;
                xmltv::finish(&d, &meta)?;
                anyhow::Ok(built)
            })
            .await?
        }
        .await;
        let built = match built {
            Ok(b) => b,
            Err(e) => {
                remove_dir(&dir).await;
                return Err(e);
            }
        };

        let indexed: Vec<_> = built.channels.iter().map(|c| c.indexed()).collect();
        if let Err(e) = self.store.publish_epg_generation(src.id, generation, &indexed).await {
            remove_dir(&dir).await;
            return Err(e.into());
        }
        // Keep only the generation just published and the one it replaced.
        let keep = [Some(generation), src.current_gen];
        for (n, d) in list_gens(&source_dir) {
            if !keep.contains(&Some(n)) {
                remove_dir(&d).await;
            }
        }
        Ok(Fetched::Published {
            channels: built.channels.len(),
            programmes: built.programmes,
        })
    }
}

enum Fetched {
    NotModified,
    Published { channels: usize, programmes: usize },
}

/// `gen-<n>` directories of a source, in no particular order.
fn list_gens(source_dir: &Path) -> Vec<(i64, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(source_dir) else {
        return vec![];
    };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let n = name.strip_prefix("gen-")?.parse().ok()?;
            Some((n, e.path()))
        })
        .collect()
}

async fn remove_dir(path: &Path) {
    if let Err(e) = tokio::fs::remove_dir_all(path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("removing {}: {e}", path.display());
    }
}
