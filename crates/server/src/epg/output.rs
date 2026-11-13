//! `/p/<slug>/epg.xml`: the guide for one playlist, built from the published generations.
//!
//! Channels come first, then programmes, as the XMLTV DTD requires. For each enabled non-event
//! channel, the highest-priority enabled source carrying its effective tvg-id supplies the
//! `<channel>` element and all of that source's programmes for it. A channel no source carries gets
//! a synthetic channel and its name as hourly programmes (§5), so it still shows in guides. Responses
//! are cached per playlist and keyed by the store's output version and the current hour.

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
            .filter(crate::store::eligible)
            .collect();
        let rendered = Arc::new(self.build(channels, hour).await?);
        let mut cache = self.xml_cache.lock().unwrap();
        // Entries from older versions (including deleted playlists) would be rebuilt anyway.
        cache.retain(|_, e| e.version == version);
        cache.insert(
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
        // One entry per effective tvg-id, in playlist order: the first eligible channel names it.
        let mut order: Vec<String> = Vec::new();
        let mut by_id: HashMap<String, Channel> = HashMap::new();
        for c in channels {
            let id = c.tvg_id.clone().unwrap_or_else(|| format!("lineup.{}", c.id));
            if let std::collections::hash_map::Entry::Vacant(e) = by_id.entry(id.clone()) {
                order.push(id);
                e.insert(c);
            }
        }
        // Hold off generation cleanup from before the generations are chosen until the files are read.
        let _files = self.files_lock.read().await;
        let lookup = self.store.epg_lookup(&order).await?;
        let mut gens: HashMap<(i64, i64), Arc<GenData>> = HashMap::new();
        let wanted: std::collections::BTreeSet<(i64, i64)> = lookup.values().copied().collect();
        for (source, generation) in wanted {
            gens.insert((source, generation), self.gen_data(source, generation).await?);
        }

        let mut plan = Vec::new();
        for id in &order {
            let ch = &by_id[id];
            if let Some(&(source, generation)) = lookup.get(id) {
                plan.push(Planned::Upstream {
                    id: id.clone(),
                    name: ch.name.clone(),
                    data: gens[&(source, generation)].clone(),
                    programmes: self.gen_dir(source, generation).join(xmltv::PROGRAMMES),
                });
            } else {
                // No enabled source carries this id: the channel's name, hour by hour.
                plan.push(Planned::Fallback {
                    id: id.clone(),
                    name: ch.name.clone(),
                });
            }
        }
        let offset = local_offset;
        let body = tokio::task::spawn_blocking(move || render(&plan, hour, &offset))
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
    Fallback {
        id: String,
        name: String,
    },
}

fn render(plan: &[Planned], hour: i64, offset: &dyn Fn(i64) -> i32) -> Result<String> {
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
            Planned::Fallback { id, name } => synthetic_channel(&mut out, id, name),
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
            Planned::Fallback { id, name } => {
                let bounds = hour_boundaries(hour, hour + WINDOW, offset);
                for w in bounds.windows(2) {
                    let _ = writeln!(
                        out,
                        "<programme start=\"{}\" stop=\"{}\" channel=\"{}\"><title>{}</title></programme>",
                        xmltv_time(w[0]),
                        xmltv_time(w[1]),
                        escape(id.as_str()),
                        escape(name.as_str())
                    );
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

/// The server's UTC offset (seconds east) at `ts`, from its timezone (`TZ`, read at start).
fn local_offset(ts: i64) -> i32 {
    use chrono::{Local, Offset, TimeZone};
    DateTime::<Utc>::from_timestamp(ts, 0)
        .map(|t| Local.offset_from_utc_datetime(&t.naive_utc()).fix().local_minus_utc())
        .unwrap_or(0)
}

/// Programme boundaries over `[from, to)`: `from`, every instant where local wall time is a whole
/// hour, every instant the UTC offset changes (so a half-hour daylight-saving change still leaves no
/// block longer than an hour), and `to`. `offset` gives the UTC offset in seconds at an instant.
fn hour_boundaries(from: i64, to: i64, offset: &dyn Fn(i64) -> i32) -> Vec<i64> {
    let mut out = vec![from];
    let mut t = from;
    while t < to {
        let o = i64::from(offset(t));
        // The next whole local hour, assuming the offset holds.
        let mut next = t + HOUR - (t + o).rem_euclid(HOUR);
        if i64::from(offset(next)) != o {
            // The offset changes on the way: the first second with the new offset is a boundary.
            let (mut lo, mut hi) = (t, next);
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if i64::from(offset(mid)) == o {
                    lo = mid
                } else {
                    hi = mid
                }
            }
            next = hi;
        }
        t = next.min(to);
        out.push(t);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An hour boundary in UTC (2027-01-15 08:00 UTC, a Friday), and day starts around it.
    const H: i64 = 1_800_000_000 / HOUR * HOUR;

    /// Contiguous, at most one hour each, exactly covering the window; every interior boundary is a
    /// whole local hour or an offset change.
    fn check(from: i64, offset: &dyn Fn(i64) -> i32) -> Vec<i64> {
        let b = hour_boundaries(from, from + WINDOW, offset);
        assert_eq!((b[0], *b.last().unwrap()), (from, from + WINDOW));
        for w in b.windows(2) {
            assert!(w[0] < w[1] && w[1] - w[0] <= HOUR, "{}..{}", w[0], w[1]);
        }
        for &t in &b[1..b.len() - 1] {
            let whole = (t + i64::from(offset(t))).rem_euclid(HOUR) == 0;
            let change = offset(t - 1) != offset(t);
            assert!(
                whole || change,
                "{t} is neither a whole local hour nor an offset change"
            );
        }
        b
    }

    /// An offset that is `before` until `at` and `after` from then on.
    fn switch(at: i64, before: i32, after: i32) -> impl Fn(i64) -> i32 {
        move |t| if t < at { before } else { after }
    }

    #[test]
    fn utc_gives_24_hourly_blocks() {
        assert_eq!(check(H, &|_| 0).len(), 25);
        // A window starting mid-hour: a short first block, then whole hours.
        let b = check(H + 1200, &|_| 0);
        assert_eq!(b[1], H + HOUR);
    }

    #[test]
    fn new_york_across_both_changes() {
        // Spring forward 02:00 EST -> 03:00 EDT (07:00 UTC); fall back 02:00 EDT -> 01:00 EST (06:00 UTC).
        let spring = H + 3 * HOUR;
        let b = check(H, &switch(spring, -5 * 3600, -4 * 3600));
        assert!(b.contains(&spring));
        assert_eq!(b.len(), 25, "whole-hour offsets: still hourly");
        let fall = H + 3 * HOUR;
        let b = check(H, &switch(fall, -4 * 3600, -5 * 3600));
        assert_eq!(b.len(), 25);
        // 01:00 EDT and 01:00 EST are distinct instants an hour apart.
        assert!(b.contains(&(fall - HOUR)) && b.contains(&fall));
    }

    #[test]
    fn kolkata_has_half_hour_boundaries() {
        let b = check(H, &|_| 19_800);
        assert_eq!(b[1], H + 1800, "first block to the next whole local hour");
        assert_eq!(b.len(), 26, "25 blocks, first and last clipped");
    }

    #[test]
    fn lord_howe_half_hour_changes_keep_blocks_at_most_an_hour() {
        // Back: 02:00 +11:00 -> 01:30 +10:30 at 15:00 UTC; the next whole hour 02:00 +10:30 is 15:30 UTC.
        let back = H + 2 * HOUR;
        let b = check(H, &switch(back, 39_600, 37_800));
        assert!(b.contains(&back) && b.contains(&(back + 1800)));
        // Forward: 02:00 +10:30 -> 02:30 +11:00.
        let fwd = H + 2 * HOUR + 1800;
        let b = check(H, &switch(fwd, 37_800, 39_600));
        assert!(b.contains(&fwd));
    }

    #[test]
    fn times_are_xmltv() {
        assert_eq!(xmltv_time(0), "19700101000000 +0000");
    }
}
