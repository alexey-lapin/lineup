//! Building a generation from a raw XMLTV download, and reading it back.
//!
//! A generation directory holds:
//! - `raw`: the download as received (plain or gzipped; detected by magic bytes).
//! - `programmes.xml`: every kept `<programme>` element, re-emitted event by event from the source
//!   (so no metadata is dropped), grouped by channel and sorted by start time.
//! - `programmes.idx`: JSON map of channel id → `[offset, len]` of that channel's block.
//! - `channels.json`: the channels: id, display names, icon and the `<channel>` element.
//! - `meta.json`: the validators of this download and the URL they came from.
//! - `COMPLETE`: written last; a directory without it is never served.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::{BytesStart, Event};
use serde::{Deserialize, Serialize};

use crate::store::IndexedChannel;

pub const RAW: &str = "raw";
pub const PROGRAMMES: &str = "programmes.xml";
pub const INDEX: &str = "programmes.idx";
pub const CHANNELS: &str = "channels.json";
pub const META: &str = "meta.json";
pub const COMPLETE: &str = "COMPLETE";

/// Programmes that ended longer ago than this are dropped when a generation is built.
pub const KEEP_PAST_SECS: i64 = 6 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Meta {
    pub url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub fetched_at: String,
}

/// One `<channel>` of a generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelEntry {
    pub id: String,
    pub display_names: Vec<String>,
    pub icon: Option<String>,
    /// The `<channel>` element as found in the source; `None` for ids that only appear in programmes.
    pub xml: Option<String>,
}

impl ChannelEntry {
    pub fn indexed(&self) -> IndexedChannel {
        IndexedChannel {
            xmltv_id: self.id.clone(),
            display_names: self.display_names.clone(),
            icon: self.icon.clone(),
        }
    }
}

/// Summary of a built generation.
#[derive(Debug, Clone)]
pub struct Built {
    pub channels: Vec<ChannelEntry>,
    pub programmes: usize,
}

/// Parse `dir/raw` and write `programmes.*` and `channels.json`. `now` is unix seconds.
pub fn build(dir: &Path, now: i64) -> anyhow::Result<Built> {
    let raw = open_raw(&dir.join(RAW))?;
    let mut reader = Reader::from_reader(raw);
    reader.config_mut().expand_empty_elements = false;

    // Programmes are first written in file order to a scratch file, then copied grouped by channel.
    let scratch_path = dir.join("programmes.unsorted");
    let mut scratch = BufWriter::new(File::create(&scratch_path)?);
    let mut scratch_len: u64 = 0;
    // (channel index, start, offset, len)
    let mut progs: Vec<(u32, i64, u64, u32)> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    let mut id_index: HashMap<String, u32> = HashMap::new();
    let mut channels: Vec<ChannelEntry> = Vec::new();
    let mut seen_channel: HashMap<String, usize> = HashMap::new();
    let cutoff = now - KEEP_PAST_SECS;

    let mut saw_tv_end = false;
    let mut buf = Vec::new();
    loop {
        let ev = reader.read_event_into(&mut buf)?;
        match ev {
            Event::End(ref e) if e.name().as_ref() == "tv" => saw_tv_end = true,
            Event::Start(ref e) if e.name().as_ref() == "channel" => {
                let start = e.clone().into_owned();
                let ch = read_channel(&mut reader, start)?;
                if let Some(ch) = ch {
                    match seen_channel.get(&ch.id) {
                        // Duplicate <channel>: keep the first.
                        Some(_) => {}
                        None => {
                            seen_channel.insert(ch.id.clone(), channels.len());
                            channels.push(ch);
                        }
                    }
                }
            }
            Event::Start(ref e) if e.name().as_ref() == "programme" => {
                let start_ev = e.clone().into_owned();
                let attrs = attrs_of(&start_ev);
                let xml = capture(&mut reader, Event::Start(start_ev))?;
                if let Some((chan, start, stop)) = programme_key(&attrs)
                    && stop.unwrap_or(start) >= cutoff
                {
                    let ci = intern(&mut ids, &mut id_index, chan);
                    scratch.write_all(&xml)?;
                    progs.push((ci, start, scratch_len, xml.len() as u32));
                    scratch_len += xml.len() as u64;
                }
            }
            Event::Empty(ref e) if e.name().as_ref() == "channel" => {
                let start = e.clone().into_owned();
                if let Some(id) = attrs_of(&start).get("id").filter(|i| !i.is_empty()).cloned()
                    && !seen_channel.contains_key(&id)
                {
                    seen_channel.insert(id.clone(), channels.len());
                    let xml = capture_one(Event::Empty(start))?;
                    channels.push(ChannelEntry {
                        id,
                        display_names: vec![],
                        icon: None,
                        xml: Some(xml),
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    // A download cut off between elements still parses; the closing tag proves it is whole.
    anyhow::ensure!(saw_tv_end, "not a complete XMLTV document (no closing </tv>)");
    scratch.flush()?;
    drop(scratch);

    // Ids with programmes but no <channel> element still get an index entry.
    for id in &ids {
        if !seen_channel.contains_key(id) {
            seen_channel.insert(id.clone(), channels.len());
            channels.push(ChannelEntry {
                id: id.clone(),
                display_names: vec![],
                icon: None,
                xml: None,
            });
        }
    }

    progs.sort_by_key(|p| (p.0, p.1));
    let mut src = File::open(&scratch_path)?;
    let mut out = BufWriter::new(File::create(dir.join(PROGRAMMES))?);
    let mut index: HashMap<&str, (u64, u64)> = HashMap::new();
    let mut offset: u64 = 0;
    let mut chunk = Vec::new();
    for &(ci, _, off, len) in &progs {
        chunk.resize(len as usize, 0);
        src.seek(SeekFrom::Start(off))?;
        src.read_exact(&mut chunk)?;
        out.write_all(&chunk)?;
        out.write_all(b"\n")?;
        let e = index.entry(ids[ci as usize].as_str()).or_insert((offset, 0));
        e.1 += len as u64 + 1;
        offset += len as u64 + 1;
    }
    out.flush()?;
    out.into_inner()?.sync_all()?;
    drop(src);
    fs::remove_file(&scratch_path)?;

    write_json(&dir.join(INDEX), &index)?;
    write_json(&dir.join(CHANNELS), &channels)?;
    Ok(Built {
        channels,
        programmes: progs.len(),
    })
}

/// Write `meta.json`, sync the directory's files, then write `COMPLETE`.
pub fn finish(dir: &Path, meta: &Meta) -> anyhow::Result<()> {
    write_json(&dir.join(META), meta)?;
    for f in [RAW, PROGRAMMES, INDEX, CHANNELS, META] {
        File::open(dir.join(f))?.sync_all()?;
    }
    let mut c = File::create(dir.join(COMPLETE))?;
    c.write_all(b"ok\n")?;
    c.sync_all()?;
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    Ok(())
}

pub fn is_complete(dir: &Path) -> bool {
    dir.join(COMPLETE).is_file()
}

pub fn read_meta(dir: &Path) -> Option<Meta> {
    serde_json::from_slice(&fs::read(dir.join(META)).ok()?).ok()
}

pub fn read_channels(dir: &Path) -> anyhow::Result<Vec<ChannelEntry>> {
    Ok(serde_json::from_slice(&fs::read(dir.join(CHANNELS))?)?)
}

pub fn read_index(dir: &Path) -> anyhow::Result<HashMap<String, (u64, u64)>> {
    Ok(serde_json::from_slice(&fs::read(dir.join(INDEX))?)?)
}

fn write_json<T: Serialize + ?Sized>(path: &Path, v: &T) -> anyhow::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    serde_json::to_writer(&mut w, v)?;
    w.flush()?;
    Ok(())
}

/// Open the raw download, transparently gunzipping it.
fn open_raw(path: &Path) -> anyhow::Result<Box<dyn BufRead>> {
    let mut f = BufReader::new(File::open(path)?);
    let magic = f.fill_buf()?;
    if magic.starts_with(&[0x1f, 0x8b]) {
        Ok(Box::new(BufReader::new(flate2::bufread::MultiGzDecoder::new(f))))
    } else {
        Ok(Box::new(f))
    }
}

fn intern(ids: &mut Vec<String>, index: &mut HashMap<String, u32>, id: &str) -> u32 {
    if let Some(i) = index.get(id) {
        return *i;
    }
    let i = ids.len() as u32;
    ids.push(id.to_string());
    index.insert(id.to_string(), i);
    i
}

fn attrs_of(e: &BytesStart<'_>) -> HashMap<String, String> {
    e.attributes()
        .flatten()
        .filter_map(|a| {
            let v = a
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()?
                .into_owned();
            Some((a.key.as_ref().to_string(), v))
        })
        .collect()
}

/// `(channel, start, stop)` of a programme; `None` if it has no channel or unparseable start.
fn programme_key(attrs: &HashMap<String, String>) -> Option<(&str, i64, Option<i64>)> {
    let chan = attrs.get("channel").map(String::as_str).filter(|c| !c.is_empty())?;
    let start = parse_time(attrs.get("start")?)?;
    let stop = attrs.get("stop").and_then(|s| parse_time(s));
    Some((chan, start, stop))
}

/// XMLTV time: `YYYYMMDDhhmmss[ ±hhmm]` (seconds, and the offset, may be missing). Unix seconds.
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    let rest = s[digits.len()..].trim();
    let dt = match digits.len() {
        14 => chrono::NaiveDateTime::parse_from_str(&digits, "%Y%m%d%H%M%S").ok()?,
        12 => chrono::NaiveDateTime::parse_from_str(&format!("{digits}00"), "%Y%m%d%H%M%S").ok()?,
        _ => return None,
    };
    let offset = if rest.is_empty() {
        0
    } else {
        let (sign, hm) = match rest.as_bytes()[0] {
            b'+' => (1, &rest[1..]),
            b'-' => (-1, &rest[1..]),
            _ => return None,
        };
        if hm.len() != 4 || !hm.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let h: i64 = hm[..2].parse().ok()?;
        let m: i64 = hm[2..].parse().ok()?;
        sign * (h * 3600 + m * 60)
    };
    Some(dt.and_utc().timestamp() - offset)
}

/// Copy the element that `start` opened (through its matching end) into a byte buffer.
fn capture<R: BufRead>(reader: &mut Reader<R>, start: Event<'static>) -> anyhow::Result<Vec<u8>> {
    let mut w = Writer::new(Vec::new());
    w.write_event(start)?;
    let mut depth = 1usize;
    let mut buf = Vec::new();
    while depth > 0 {
        let ev = reader.read_event_into(&mut buf)?;
        match &ev {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            Event::Eof => anyhow::bail!("unexpected end of file inside an element"),
            _ => {}
        }
        w.write_event(ev)?;
        buf.clear();
    }
    Ok(w.into_inner())
}

fn capture_one(ev: Event<'static>) -> anyhow::Result<String> {
    let mut w = Writer::new(Vec::new());
    w.write_event(ev)?;
    Ok(String::from_utf8(w.into_inner())?)
}

/// Read a `<channel>` element: its id, display names and icon, and the element itself.
fn read_channel<R: BufRead>(
    reader: &mut Reader<R>,
    start: BytesStart<'static>,
) -> anyhow::Result<Option<ChannelEntry>> {
    let id = attrs_of(&start).get("id").cloned().unwrap_or_default();
    let mut w = Writer::new(Vec::new());
    w.write_event(Event::Start(start))?;
    let mut depth = 1usize;
    let mut buf = Vec::new();
    let mut display_names = Vec::new();
    let mut icon = None;
    // Raw (escaped) text of the display-name being read, when inside one at depth 2.
    let mut text: Option<String> = None;
    while depth > 0 {
        let ev = reader.read_event_into(&mut buf)?;
        match &ev {
            Event::Start(e) => {
                depth += 1;
                if depth == 2 && e.name().as_ref() == "display-name" {
                    text = Some(String::new());
                }
            }
            Event::Empty(e) if depth == 1 && e.name().as_ref() == "icon" && icon.is_none() => {
                icon = attrs_of(e).get("src").cloned().filter(|s| !s.is_empty());
            }
            Event::Text(t) => {
                if let Some(s) = text.as_mut() {
                    s.push_str(&t.xml10_content());
                }
            }
            Event::CData(t) => {
                if let Some(s) = text.as_mut() {
                    // CDATA is literal; escape so the whole buffer can be unescaped at the end.
                    s.push_str(&quick_xml::escape::escape(&**t));
                }
            }
            Event::GeneralRef(r) => {
                if let Some(s) = text.as_mut() {
                    s.push('&');
                    s.push_str(r);
                    s.push(';');
                }
            }
            Event::End(e) => {
                if depth == 2
                    && e.name().as_ref() == "display-name"
                    && let Some(raw) = text.take()
                {
                    let name = quick_xml::escape::unescape(&raw).map(|c| c.into_owned()).unwrap_or(raw);
                    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !name.is_empty() && !display_names.contains(&name) {
                        display_names.push(name);
                    }
                }
                depth -= 1;
            }
            Event::Eof => anyhow::bail!("unexpected end of file inside <channel>"),
            _ => {}
        }
        w.write_event(ev)?;
        buf.clear();
    }
    if id.is_empty() {
        return Ok(None);
    }
    Ok(Some(ChannelEntry {
        id,
        display_names,
        icon,
        xml: Some(String::from_utf8(w.into_inner())?),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE tv SYSTEM "xmltv.dtd">
<tv generator-info-name="test">
  <channel id="SportsOne.uk">
    <display-name lang="en">Sports One</display-name>
    <display-name>Sports 1 &amp; More</display-name>
    <icon src="http://img/s1.png"/>
  </channel>
  <channel id="Arena1.de"><display-name>Arena 1</display-name></channel>
  <programme start="20261105100000 +0000" stop="20261105120000 +0000" channel="Arena1.de">
    <title lang="de">Late &lt;b&gt;</title><category>Sport</category>
  </programme>
  <programme start="20261105090000 +0000" stop="20261105100000 +0000" channel="Arena1.de">
    <title>Early</title>
  </programme>
  <programme start="20261105120000 +0100" stop="20261105130000 +0100" channel="SportsOne.uk">
    <title>Match</title>
  </programme>
  <programme start="20261101000000 +0000" stop="20261101010000 +0000" channel="SportsOne.uk">
    <title>Old</title>
  </programme>
  <programme start="20261105120000 +0000" stop="20261105130000 +0000" channel="Orphan.fr">
    <title>No channel element</title>
  </programme>
</tv>
"#;

    fn now() -> i64 {
        parse_time("20261105110000 +0000").unwrap()
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("20261105120000 +0100"), parse_time("20261105110000 +0000"));
        assert_eq!(parse_time("202611051100 -0030"), parse_time("20261105113000"));
        assert_eq!(parse_time("garbage"), None);
    }

    fn built(gz: bool) -> (tempfile::TempDir, Built) {
        let dir = tempfile::tempdir().unwrap();
        let raw = if gz {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(SAMPLE.as_bytes()).unwrap();
            e.finish().unwrap()
        } else {
            SAMPLE.as_bytes().to_vec()
        };
        fs::write(dir.path().join(RAW), raw).unwrap();
        let b = build(dir.path(), now()).unwrap();
        (dir, b)
    }

    #[test]
    fn builds_plain_and_gzip() {
        for gz in [false, true] {
            let (dir, b) = built(gz);
            let ids: Vec<_> = b.channels.iter().map(|c| c.id.as_str()).collect();
            assert_eq!(ids, ["SportsOne.uk", "Arena1.de", "Orphan.fr"]);
            assert_eq!(b.channels[0].display_names, ["Sports One", "Sports 1 & More"]);
            assert_eq!(b.channels[0].icon.as_deref(), Some("http://img/s1.png"));
            assert!(b.channels[2].xml.is_none());
            assert_eq!(b.programmes, 4, "the old programme is dropped");

            let idx = read_index(dir.path()).unwrap();
            let all = fs::read_to_string(dir.path().join(PROGRAMMES)).unwrap();
            let (off, len) = idx["Arena1.de"];
            let block = &all[off as usize..(off + len) as usize];
            let early = block.find("Early").unwrap();
            let late = block.find("Late").unwrap();
            assert!(early < late, "sorted by start: {block}");
            assert!(
                block.contains("<title lang=\"de\">Late &lt;b&gt;</title>"),
                "kept verbatim: {block}"
            );
            assert!(block.contains("<category>Sport</category>"));
            assert!(!all.contains("Old"));
            let ch = b.channels[0].xml.as_deref().unwrap();
            assert!(
                ch.starts_with("<channel id=\"SportsOne.uk\">") && ch.ends_with("</channel>"),
                "{ch}"
            );
            assert!(ch.contains("Sports 1 &amp; More"));
        }
    }

    #[test]
    fn rejects_truncated() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(RAW), &SAMPLE[..SAMPLE.len() / 2]).unwrap();
        assert!(build(dir.path(), now()).is_err());
    }
}
