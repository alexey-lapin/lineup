//! Rewriting HLS playlists so every URI points back at the gateway.
//!
//! Line-based on purpose: the standard tags are kept as they are except their URIs, so timing,
//! sequence, discontinuity and byte-range tags pass through unchanged. Every other line is dropped:
//! comments, vendor tags (`#EXT-X-TWITCH-INFO`, `#EXT-X-CUE-OUT`, ...), `#EXT-X-DATERANGE` and
//! `#EXT-X-SESSION-DATA`, whose attributes and values can carry upstream URLs (ad trackers,
//! interstitial `X-ASSET-URI`s, CDN nodes) and the viewer's address. URIs are resolved against the playlist's final (post-redirect) URL and
//! replaced by whatever the caller maps them to. Features a relay can't serve safely are refused
//! instead of leaking a CDN URL to the player or misnumbering segments: LL-HLS parts and hints,
//! delta updates (`EXT-X-SKIP`), content steering, and keys other than plain AES-128. Kept tags are
//! passed on with their values as they are (an `EXTINF` title, for example).

use url::Url;

/// Tags passed on to the player (RFC 8216 and its successor, without low-latency HLS,
/// `EXT-X-DATERANGE` and `EXT-X-SESSION-DATA`). Anything else is dropped.
const KEEP: [&str; 22] = [
    "EXT-X-VERSION",
    "EXTINF",
    "EXT-X-BYTERANGE",
    "EXT-X-DISCONTINUITY",
    "EXT-X-KEY",
    "EXT-X-MAP",
    "EXT-X-PROGRAM-DATE-TIME",
    "EXT-X-GAP",
    "EXT-X-BITRATE",
    "EXT-X-TARGETDURATION",
    "EXT-X-MEDIA-SEQUENCE",
    "EXT-X-DISCONTINUITY-SEQUENCE",
    "EXT-X-ENDLIST",
    "EXT-X-PLAYLIST-TYPE",
    "EXT-X-I-FRAMES-ONLY",
    "EXT-X-MEDIA",
    "EXT-X-STREAM-INF",
    "EXT-X-I-FRAME-STREAM-INF",
    "EXT-X-SESSION-KEY",
    "EXT-X-INDEPENDENT-SEGMENTS",
    "EXT-X-START",
    "EXT-X-ALLOW-CACHE",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Another playlist (a variant or rendition).
    Playlist,
    /// A segment, key or init section.
    Resource,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HlsError {
    #[error("not an HLS playlist")]
    NotHls,
    #[error("unsupported HLS feature: {0}")]
    Unsupported(String),
    #[error("bad URI {0:?}")]
    BadUri(String),
    #[error("{0}")]
    Map(String),
}

/// Facts about a rewritten playlist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// A media playlist with `#EXT-X-ENDLIST`: it won't grow.
    pub ended: bool,
    /// A master playlist (has variants).
    pub master: bool,
    pub segments: usize,
    /// `#EXT-X-MEDIA-SEQUENCE` of a media playlist.
    pub media_sequence: Option<u64>,
    /// `#EXT-X-TARGETDURATION` of a media playlist.
    pub target_duration: Option<String>,
    /// Each segment's `#EXTINF` duration, in order: identifies the encode's timeline.
    pub durations: Vec<String>,
    /// Each segment's effective discontinuity number (`#EXT-X-DISCONTINUITY-SEQUENCE` plus the
    /// `#EXT-X-DISCONTINUITY` tags before it), in order.
    pub discontinuities: Vec<u64>,
    /// Init sections: their URL path (no query, which carries signatures) and byte range.
    pub maps: Vec<String>,
}

/// Rewrite `body` (fetched from `base`). `map` gets each absolute URI, its kind and its context,
/// and returns the text to write instead. The context identifies a playlist independently of its
/// (signed) URL: the variant's `EXT-X-STREAM-INF` line, or the rendition tag without its URI;
/// it is empty for resources.
pub fn rewrite(
    body: &str,
    base: &Url,
    mut map: impl FnMut(Url, Kind, &str) -> Result<String, String>,
) -> Result<(String, Summary), HlsError> {
    let body = body.strip_prefix('\u{feff}').unwrap_or(body);
    let mut lines = body.lines();
    if lines.next().map(str::trim) != Some("#EXTM3U") {
        return Err(HlsError::NotHls);
    }
    let mut out = String::with_capacity(body.len() + 256);
    out.push_str("#EXTM3U\n");
    let mut summary = Summary::default();
    let mut variant_tag: Option<String> = None;
    let mut pending_duration: Option<String> = None;
    let mut discontinuity: u64 = 0;
    for raw in lines {
        let line = raw.trim_end_matches('\r');
        let t = line.trim();
        if t.is_empty() {
            out.push('\n');
            continue;
        }
        if let Some(tag) = t.strip_prefix('#') {
            if !tag.starts_with("EXT") {
                continue; // a comment
            }
            let name = tag.split(':').next().unwrap_or(tag);
            match name {
                "EXT-X-PART"
                | "EXT-X-PRELOAD-HINT"
                | "EXT-X-RENDITION-REPORT"
                | "EXT-X-CONTENT-STEERING"
                | "EXT-X-SKIP"
                | "EXT-X-DEFINE" => {
                    return Err(HlsError::Unsupported(name.into()));
                }
                "EXT-X-KEY" | "EXT-X-SESSION-KEY" => {
                    let method = attr(tag, "METHOD").unwrap_or_default();
                    let format = attr(tag, "KEYFORMAT").unwrap_or_else(|| "identity".into());
                    if method != "NONE" && (method != "AES-128" || format != "identity") {
                        return Err(HlsError::Unsupported(format!(
                            "{name} METHOD={method} KEYFORMAT={format}"
                        )));
                    }
                }
                "EXT-X-ENDLIST" => summary.ended = true,
                "EXT-X-DISCONTINUITY-SEQUENCE" => {
                    discontinuity = tag
                        .split_once(':')
                        .and_then(|(_, v)| v.trim().parse().ok())
                        .unwrap_or(0)
                }
                "EXT-X-DISCONTINUITY" => discontinuity += 1,
                "EXTINF" => {
                    pending_duration = tag
                        .split_once(':')
                        .map(|(_, v)| v.split(',').next().unwrap_or("").trim().to_string())
                }
                "EXT-X-MAP" => {
                    if let Some(uri) = attr(tag, "URI") {
                        let path = resolve(base, &uri)?.path().to_string();
                        summary
                            .maps
                            .push(format!("{path} {}", attr(tag, "BYTERANGE").unwrap_or_default()));
                    }
                }
                "EXT-X-MEDIA-SEQUENCE" => {
                    summary.media_sequence = tag.split_once(':').and_then(|(_, v)| v.trim().parse().ok())
                }
                "EXT-X-TARGETDURATION" => {
                    summary.target_duration = tag.split_once(':').map(|(_, v)| v.trim().to_string())
                }
                "EXT-X-STREAM-INF" => {
                    summary.master = true;
                    variant_tag = Some(t.to_string());
                }
                _ => {}
            }
            if !KEEP.contains(&name) {
                continue; // a vendor or unsupported tag: never reaches the player
            }
            let kind = match name {
                "EXT-X-MEDIA" | "EXT-X-I-FRAME-STREAM-INF" => Some(Kind::Playlist),
                "EXT-X-KEY" | "EXT-X-SESSION-KEY" | "EXT-X-MAP" => Some(Kind::Resource),
                _ => None,
            };
            // A kept tag that carries a URI this code does not map would leak it: refuse it.
            if kind.is_none() && attr(tag, "URI").is_some() {
                return Err(HlsError::Unsupported(format!("{name} with URI")));
            }
            match kind.zip(attr(tag, "URI")) {
                Some((k, uri)) => {
                    let abs = resolve(base, &uri)?;
                    let context = match k {
                        Kind::Playlist => format!("#{}", replace_attr(tag, "URI", "")),
                        Kind::Resource => String::new(),
                    };
                    let mapped = map(abs, k, &context).map_err(HlsError::Map)?;
                    out.push('#');
                    out.push_str(&replace_attr(tag, "URI", &mapped));
                }
                None => out.push_str(line),
            }
            out.push('\n');
            continue;
        }
        // A URI line: a variant playlist after EXT-X-STREAM-INF, otherwise a segment.
        let (kind, context) = match variant_tag.take() {
            Some(tag) => (Kind::Playlist, tag),
            None => {
                summary.segments += 1;
                summary.durations.push(pending_duration.take().unwrap_or_default());
                summary.discontinuities.push(discontinuity);
                (Kind::Resource, String::new())
            }
        };
        let abs = resolve(base, t)?;
        out.push_str(&map(abs, kind, &context).map_err(HlsError::Map)?);
        out.push('\n');
    }
    Ok((out, summary))
}

fn resolve(base: &Url, uri: &str) -> Result<Url, HlsError> {
    let u = base.join(uri).map_err(|_| HlsError::BadUri(uri.into()))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(HlsError::BadUri(uri.into()));
    }
    Ok(u)
}

/// The value of attribute `name` in a tag's attribute list (quoted or not).
pub fn attr(tag: &str, name: &str) -> Option<String> {
    let list = tag.split_once(':')?.1;
    for (k, v) in attrs(list) {
        if k == name {
            return Some(v.trim_matches('"').to_string());
        }
    }
    None
}

/// Split an attribute list at commas outside quotes.
fn attrs(list: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in list.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.into_iter()
        .filter_map(|p| {
            p.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// The tag with attribute `name`'s quoted value replaced.
fn replace_attr(tag: &str, name: &str, value: &str) -> String {
    let Some((head, list)) = tag.split_once(':') else {
        return tag.to_string();
    };
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in list.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ',' if !quoted => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let parts: Vec<String> = parts
        .into_iter()
        .map(|p| match p.split_once('=') {
            Some((k, _)) if k.trim() == name => format!("{k}=\"{value}\""),
            _ => p,
        })
        .collect();
    format!("{head}:{}", parts.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    type Rewritten = (String, Vec<(String, Kind)>, Summary);

    fn run(body: &str, base: &str) -> Result<Rewritten, HlsError> {
        let mut seen = Vec::new();
        let (out, s) = rewrite(body, &Url::parse(base).unwrap(), |u, k, _| {
            seen.push((u.to_string(), k));
            Ok(format!("R{}", seen.len()))
        })?;
        Ok((out, seen, s))
    }

    #[test]
    fn master_with_renditions_and_iframes() {
        let body = "#EXTM3U\n#EXT-X-INDEPENDENT-SEGMENTS\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"en, main\",URI=\"audio/en.m3u8\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=1280000,CODECS=\"avc1.4d401f,mp4a.40.2\",AUDIO=\"a\"\n\
            720/index.m3u8?sig=1\n\
            #EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=86000,URI=\"/iframes.m3u8\"\n";
        let (out, seen, s) = run(body, "https://cdn.example/v/master.m3u8?token=x").unwrap();
        assert!(s.master);
        assert_eq!(
            seen,
            vec![
                ("https://cdn.example/v/audio/en.m3u8".into(), Kind::Playlist),
                ("https://cdn.example/v/720/index.m3u8?sig=1".into(), Kind::Playlist),
                ("https://cdn.example/iframes.m3u8".into(), Kind::Playlist),
            ]
        );
        assert!(out.contains("NAME=\"en, main\",URI=\"R1\""), "{out}");
        assert!(
            out.contains("CODECS=\"avc1.4d401f,mp4a.40.2\",AUDIO=\"a\"\nR2\n"),
            "{out}"
        );
        assert!(out.contains("#EXT-X-INDEPENDENT-SEGMENTS"));
        assert!(!out.contains("cdn.example"), "no CDN URL leaks: {out}");
    }

    #[test]
    fn vendor_tags_comments_and_dateranges_are_dropped() {
        let body = "#EXTM3U\n# a comment with https://cdn.example/x\n\
            #EXT-X-SESSION-DATA:DATA-ID=\"USER-IP\",VALUE=\"203.0.113.9\"\n\
            #EXT-X-SESSION-DATA:DATA-ID=\"x\",URI=\"https://cdn.example/d.json\"\n\
            #EXT-X-TWITCH-INFO:NODE=\"n\",USER-IP=\"203.0.113.9\",C=\"aHR0cHM6Ly9jZG4\"\n\
            #EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:5\n#EXT-X-MEDIA-SEQUENCE:6\n\
            #EXT-X-DATERANGE:ID=\"ad\",START-DATE=\"2026-11-20T04:41:46Z\",X-TRIGGER-URL=\"https://ads.example/t\"\n\
            #EXT-X-DATERANGE:ID=\"i\",CLASS=\"com.apple.hls.interstitial\",START-DATE=\"2026-11-20T04:41:46Z\",X-ASSET-URI=\"https://ads.example/a.m3u8\"\n\
            #EXT-X-CUE-OUT:30\n#EXT-X-PROGRAM-DATE-TIME:2026-11-20T04:41:40.224Z\n\
            #EXTINF:2.000,Amazon|1\n6.ts\n#EXT-X-CUE-IN\n#EXTINF:2.000,live\n7.ts\n";
        let (out, seen, s) = run(body, "https://cdn.example/v/index.m3u8").unwrap();
        // A vendor tag with a URI is dropped too, not refused: the stream still plays.
        let thumbs = "#EXTM3U\n#EXT-X-IMAGE-STREAM-INF:BANDWIDTH=1,URI=\"https://cdn.example/thumbs.m3u8\"\n";
        assert_eq!(run(thumbs, "https://c/x.m3u8").unwrap().0, "#EXTM3U\n");
        assert_eq!(s.segments, 2);
        assert_eq!(seen.len(), 2);
        assert_eq!(
            out,
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:5\n#EXT-X-MEDIA-SEQUENCE:6\n\
             #EXT-X-PROGRAM-DATE-TIME:2026-11-20T04:41:40.224Z\n#EXTINF:2.000,Amazon|1\nR1\n\
             #EXTINF:2.000,live\nR2\n"
        );
    }

    #[test]
    fn media_with_keys_maps_byteranges_and_endlist() {
        let body = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:0\n\
            #EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"720@0\"\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"https://keys.example/k1\",IV=0x1\n\
            #EXTINF:6.0,\n#EXT-X-BYTERANGE:1000@720\nmedia.mp4\n\
            #EXT-X-DISCONTINUITY\n#EXTINF:6.0,\n#EXT-X-BYTERANGE:1000\nmedia.mp4\n\
            #EXT-X-KEY:METHOD=NONE\n#EXTINF:4.5,\n?seg=3\n#EXT-X-ENDLIST\n";
        let (out, seen, s) = run(body, "https://cdn.example/v/720/index.m3u8").unwrap();
        assert!(s.ended && !s.master);
        assert_eq!(s.segments, 3);
        assert_eq!(seen[0], ("https://cdn.example/v/720/init.mp4".into(), Kind::Resource));
        assert_eq!(seen[1], ("https://keys.example/k1".into(), Kind::Resource));
        assert_eq!(seen[2].0, "https://cdn.example/v/720/media.mp4");
        assert_eq!(seen[4].0, "https://cdn.example/v/720/index.m3u8?seg=3");
        for keep in [
            "#EXT-X-MEDIA-SEQUENCE:0",
            "#EXT-X-BYTERANGE:1000@720",
            "#EXT-X-BYTERANGE:1000\n",
            "#EXT-X-DISCONTINUITY",
            "BYTERANGE=\"720@0\"",
            "IV=0x1",
            "#EXT-X-KEY:METHOD=NONE",
            "#EXT-X-ENDLIST",
        ] {
            assert!(out.contains(keep), "{keep} missing in {out}");
        }
    }

    #[test]
    fn contexts_identify_playlists_without_their_urls() {
        let body = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",URI=\"a.m3u8?sig=1\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=1000\nv.m3u8?sig=1\n";
        let mut ctx = Vec::new();
        rewrite(body, &Url::parse("https://c/m.m3u8").unwrap(), |_, _, c| {
            ctx.push(c.to_string());
            Ok(String::new())
        })
        .unwrap();
        assert_eq!(
            ctx,
            [
                "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",URI=\"\"",
                "#EXT-X-STREAM-INF:BANDWIDTH=1000"
            ]
        );
    }

    #[test]
    fn refuses_what_it_cannot_relay() {
        for body in [
            "#EXTM3U\n#EXT-X-PART:DURATION=1,URI=\"p.mp4\"\n",
            "#EXTM3U\n#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"p.mp4\"\n",
            "#EXTM3U\n#EXT-X-CONTENT-STEERING:SERVER-URI=\"s.json\"\n",
            "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\"\n",
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n",
            "#EXTM3U\n#EXT-X-DEFINE:NAME=\"x\",VALUE=\"y\"\n",
            // A delta update: dropping the tag would renumber every segment (and implicit IVs).
            "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:100\n#EXT-X-SKIP:SKIPPED-SEGMENTS=6\n#EXTINF:6,\nseg106.ts\n",
        ] {
            assert!(
                matches!(run(body, "https://c/x.m3u8"), Err(HlsError::Unsupported(_))),
                "{body}"
            );
        }
        assert_eq!(run("<html>", "https://c/x.m3u8").unwrap_err(), HlsError::NotHls);
        assert!(matches!(
            run("#EXTM3U\n#EXTINF:1,\njavascript:alert(1)\n", "https://c/x"),
            Err(HlsError::BadUri(_))
        ));
        // Session data without a URI is fine.
        assert!(
            run(
                "#EXTM3U\n#EXT-X-SESSION-DATA:DATA-ID=\"x\",VALUE=\"y\"\n",
                "https://c/x.m3u8"
            )
            .is_ok()
        );
    }
}
