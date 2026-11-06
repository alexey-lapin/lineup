//! `/p/<slug>/epg.xml`: the guide for one playlist, built from the published generations.
//!
//! Channels come first, then programmes, as the XMLTV DTD requires. For each enabled non-event
//! channel, the highest-priority enabled source carrying its effective tvg-id supplies the
//! `<channel>` element and all of that source's programmes for it. Event channels always get a
//! synthetic channel and placeholder programmes (§5). Responses are cached per playlist and keyed by
//! the store's output version and the current hour.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

use api_types::Channel;
use chrono::{DateTime, Utc};
use quick_xml::escape::escape;

use super::Epg;
use super::xmltv::{self, ChannelEntry};
use crate::error::{Error, Result};

/// A rendered guide and its ETag.
pub struct Rendered {
    pub body: axum::body::Bytes,
    pub etag: String,
}

pub(super) struct CacheEntry {
    version: u64,
    hour: i64,
    rendered: Arc<Rendered>,
}

/// Index and channels of one generation, loaded on first use.
pub(super) struct GenData {
    index: HashMap<String, (u64, u64)>,
    channels: HashMap<String, ChannelEntry>,
}

const HOUR: i64 = 3600;
const EVENT_LENGTH: i64 = 2 * HOUR;
const WINDOW: i64 = 24 * HOUR;

impl Epg {
    pub async fn playlist_xml(&self, slug: &str) -> Result<Arc<Rendered>> {
        self.playlist_xml_at(slug, Utc::now()).await
    }

    /// As [`Epg::playlist_xml`], with an explicit clock (tests).
    pub async fn playlist_xml_at(&self, slug: &str, now: DateTime<Utc>) -> Result<Arc<Rendered>> {
        let hour = now.timestamp().div_euclid(HOUR) * HOUR;
        // Read the version before the data: a change racing with the build leaves an entry under
        // the old version, which the next request rebuilds.
        let version = self.store.output_version();
        if let Some(e) = self.xml_cache.lock().unwrap().get(slug)
            && e.version == version
            && e.hour == hour
        {
            return Ok(e.rendered.clone());
        }
        let detail = match self.store.get_playlist(slug).await {
            Ok(d) => d,
            Err(e) => {
                self.xml_cache.lock().unwrap().remove(slug);
                return Err(e);
            }
        };
        let channels: Vec<Channel> = detail
            .items
            .into_iter()
            .map(|i| i.channel)
            .filter(|c| c.enabled)
            .collect();
        let rendered = Arc::new(self.build(channels, hour).await?);
        self.xml_cache.lock().unwrap().insert(
            slug.to_string(),
            CacheEntry {
                version,
                hour,
                rendered: rendered.clone(),
            },
        );
        Ok(rendered)
    }

    async fn build(&self, channels: Vec<Channel>, hour: i64) -> Result<Rendered> {
        // One entry per effective tvg-id, in playlist order. An event channel takes the id over.
        let mut order: Vec<String> = Vec::new();
        let mut by_id: HashMap<String, Channel> = HashMap::new();
        for c in channels {
            let id = c.tvg_id.clone().unwrap_or_else(|| format!("lineup.{}", c.id));
            match by_id.get(&id) {
                None => {
                    order.push(id.clone());
                    by_id.insert(id, c);
                }
                Some(prev) if c.is_event && !prev.is_event => {
                    by_id.insert(id, c);
                }
                Some(_) => {}
            }
        }
        let upstream_ids: Vec<String> = order.iter().filter(|id| !by_id[*id].is_event).cloned().collect();
        // Hold off generation cleanup from before the generations are chosen until the files are read.
        let _files = self.files_lock.read().await;
        let lookup = self.store.epg_lookup(&upstream_ids).await?;
        let mut gens: HashMap<(i64, i64), Arc<GenData>> = HashMap::new();
        let wanted: std::collections::BTreeSet<(i64, i64)> = lookup.values().copied().collect();
        for (source, generation) in wanted {
            gens.insert((source, generation), self.gen_data(source, generation).await?);
        }

        let mut plan = Vec::new();
        for id in &order {
            let ch = &by_id[id];
            if ch.is_event {
                plan.push(Planned::Event {
                    id: id.clone(),
                    name: ch.name.clone(),
                    kickoff: ch.kickoff_at.map(|k| k.timestamp()),
                });
            } else if let Some(&(source, generation)) = lookup.get(id) {
                plan.push(Planned::Upstream {
                    id: id.clone(),
                    name: ch.name.clone(),
                    data: gens[&(source, generation)].clone(),
                    programmes: self.gen_dir(source, generation).join(xmltv::PROGRAMMES),
                });
            }
        }
        let body = tokio::task::spawn_blocking(move || render(&plan, hour))
            .await
            .map_err(|e| Error::Internal(e.to_string()))??;
        let etag = format!("\"{:016x}\"", crate::http::fnv1a64(body.as_bytes()));
        Ok(Rendered {
            body: body.into(),
            etag,
        })
    }

    async fn gen_data(&self, source: i64, generation: i64) -> Result<Arc<GenData>> {
        if let Some(d) = self.gen_cache.lock().unwrap().get(&(source, generation)) {
            return Ok(d.clone());
        }
        let dir = self.gen_dir(source, generation);
        let data = tokio::task::spawn_blocking(move || -> anyhow::Result<GenData> {
            let index = xmltv::read_index(&dir)?;
            let channels = xmltv::read_channels(&dir)?
                .into_iter()
                .map(|c| (c.id.clone(), c))
                .collect();
            Ok(GenData { index, channels })
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
        .map_err(|e| Error::Internal(format!("reading EPG generation {generation} of source {source}: {e:#}")))?;
        let data = Arc::new(data);
        let mut cache = self.gen_cache.lock().unwrap();
        // Only the served generation of each source stays loaded.
        cache.retain(|(s, g), _| *s != source || *g == generation);
        cache.insert((source, generation), data.clone());
        Ok(data)
    }
}

enum Planned {
    Upstream {
        id: String,
        name: String,
        data: Arc<GenData>,
        programmes: PathBuf,
    },
    Event {
        id: String,
        name: String,
        kickoff: Option<i64>,
    },
}

fn render(plan: &[Planned], hour: i64) -> Result<String> {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<tv generator-info-name=\"lineup\">\n");
    for p in plan {
        match p {
            Planned::Upstream { id, name, data, .. } => match data.channels.get(id).and_then(|c| c.xml.as_deref()) {
                Some(xml) => {
                    out.push_str(xml);
                    out.push('\n');
                }
                None => synthetic_channel(&mut out, id, name),
            },
            Planned::Event { id, name, .. } => synthetic_channel(&mut out, id, name),
        }
    }
    let mut files: HashMap<&PathBuf, File> = HashMap::new();
    let mut buf = Vec::new();
    for p in plan {
        match p {
            Planned::Upstream {
                id, data, programmes, ..
            } => {
                let Some(&(offset, len)) = data.index.get(id) else {
                    continue;
                };
                let f = match files.get_mut(programmes) {
                    Some(f) => f,
                    None => {
                        let f = File::open(programmes)
                            .map_err(|e| Error::Internal(format!("{}: {e}", programmes.display())))?;
                        files.entry(programmes).or_insert(f)
                    }
                };
                buf.resize(len as usize, 0);
                f.seek(SeekFrom::Start(offset))
                    .and_then(|_| f.read_exact(&mut buf))
                    .map_err(|e| Error::Internal(format!("{}: {e}", programmes.display())))?;
                out.push_str(std::str::from_utf8(&buf).map_err(|e| Error::Internal(e.to_string()))?);
            }
            Planned::Event { id, name, kickoff } => {
                for (start, stop, title, desc) in event_blocks(name, *kickoff, hour) {
                    let _ = write!(
                        out,
                        "<programme start=\"{}\" stop=\"{}\" channel=\"{}\"><title>{}</title>",
                        xmltv_time(start),
                        xmltv_time(stop),
                        escape(id.as_str()),
                        escape(title.as_str())
                    );
                    if let Some(d) = desc {
                        let _ = write!(out, "<desc>{}</desc>", escape(d.as_str()));
                    }
                    out.push_str("</programme>\n");
                }
            }
        }
    }
    out.push_str("</tv>\n");
    Ok(out)
}

fn synthetic_channel(out: &mut String, id: &str, name: &str) {
    let _ = writeln!(
        out,
        "<channel id=\"{}\"><display-name>{}</display-name></channel>",
        escape(id),
        escape(name)
    );
}

fn xmltv_time(ts: i64) -> String {
    DateTime::<Utc>::from_timestamp(ts, 0)
        .unwrap_or_default()
        .format("%Y%m%d%H%M%S +0000")
        .to_string()
}

/// Placeholder programmes for an event channel over `[hour, hour + 24h)`: "Pre-match" until
/// kickoff, the name for two hours, then "<name> (ended)". Without a kickoff, the name for the whole
/// window. Returns `(start, stop, title, desc)`.
pub fn event_blocks(name: &str, kickoff: Option<i64>, hour: i64) -> Vec<(i64, i64, String, Option<String>)> {
    let (lo, hi) = (hour, hour + WINDOW);
    let Some(k) = kickoff else {
        return vec![(lo, hi, name.to_string(), None)];
    };
    let kickoff_desc =
        DateTime::<Utc>::from_timestamp(k, 0).map(|t| format!("{name}, kickoff {} UTC", t.format("%Y-%m-%d %H:%M")));
    let blocks = [
        (i64::MIN, k, "Pre-match".to_string(), kickoff_desc),
        (k, k + EVENT_LENGTH, name.to_string(), None),
        (k + EVENT_LENGTH, i64::MAX, format!("{name} (ended)"), None),
    ];
    blocks
        .into_iter()
        .filter_map(|(s, e, title, desc)| {
            let (s, e) = (s.max(lo), e.min(hi));
            (s < e).then_some((s, e, title, desc))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 1_800_000_000 / HOUR * HOUR;

    #[test]
    fn event_blocks_follow_kickoff() {
        // Kickoff in 3h: pre-match, match, ended (clipped at the window end).
        let b = event_blocks("Final", Some(H + 3 * HOUR), H);
        let spans: Vec<_> = b.iter().map(|(s, e, t, _)| (s - H, e - H, t.as_str())).collect();
        assert_eq!(
            spans,
            [
                (0, 3 * HOUR, "Pre-match"),
                (3 * HOUR, 5 * HOUR, "Final"),
                (5 * HOUR, WINDOW, "Final (ended)")
            ]
        );
        // Kickoff 48h away: one pre-match block.
        let b = event_blocks("Final", Some(H + 48 * HOUR), H);
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].0, b[0].1, b[0].2.as_str()), (H, H + WINDOW, "Pre-match"));
        // Already over: one ended block from the current hour.
        let b = event_blocks("Final", Some(H - 10 * HOUR), H);
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].0, b[0].2.as_str()), (H, "Final (ended)"));
        // Live now: the match block starts at the hour.
        let b = event_blocks("Final", Some(H - HOUR), H);
        assert_eq!((b[0].0, b[0].1, b[0].2.as_str()), (H, H + HOUR, "Final"));
        // No kickoff: a rolling block.
        let b = event_blocks("Final", None, H);
        assert_eq!((b[0].0, b[0].1), (H, H + WINDOW));
    }

    #[test]
    fn times_are_xmltv() {
        assert_eq!(xmltv_time(0), "19700101000000 +0000");
    }
}
