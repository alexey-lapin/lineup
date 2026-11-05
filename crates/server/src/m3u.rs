//! Minimal extended-M3U parser (for import) and renderer (for playlist output).

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// EXTINF attributes in file order (keys lowercased).
    pub attrs: Vec<(String, String)>,
    pub url: String,
}

impl Entry {
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    /// Lines that were ignored because they could not be attached to an entry.
    pub skipped: Vec<String>,
}

pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut pending: Option<(String, Vec<(String, String)>)> = None;
    let mut pending_group: Option<String> = None;

    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            if let Some((name, _)) = pending.take() {
                out.skipped.push(format!("#EXTINF without URL: {name}"));
            }
            pending = Some(parse_extinf(rest));
            pending_group = None;
        } else if let Some(g) = line.strip_prefix("#EXTGRP:") {
            pending_group = Some(g.trim().to_string());
        } else if line.starts_with('#') {
            // #EXTM3U and other directives are ignored.
        } else if let Some((name, mut attrs)) = pending.take() {
            if let Some(g) = pending_group.take()
                && !attrs.iter().any(|(k, _)| k == "group-title")
            {
                attrs.push(("group-title".into(), g));
            }
            out.entries.push(Entry {
                name,
                attrs,
                url: line.to_string(),
            });
        } else {
            out.skipped.push(format!("URL without #EXTINF: {line}"));
        }
    }
    if let Some((name, _)) = pending {
        out.skipped.push(format!("#EXTINF without URL: {name}"));
    }
    out
}

/// Parse the part after `#EXTINF:` → (display name, attributes).
fn parse_extinf(s: &str) -> (String, Vec<(String, String)>) {
    // The display name follows the first comma that is outside double quotes.
    let mut in_quotes = false;
    let mut split = None;
    for (i, c) in s.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let (head, name) = match split {
        Some(i) => (&s[..i], s[i + 1..].trim()),
        None => (s, ""),
    };

    // head = "<duration> key="value" key2=value2 ..."
    let mut attrs = Vec::new();
    let mut rest = head.trim_start();
    // skip duration token
    rest = rest.find(char::is_whitespace).map_or("", |i| &rest[i..]).trim_start();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().to_ascii_lowercase();
        let after = &rest[eq + 1..];
        let (value, remaining) = if let Some(q) = after.strip_prefix('"') {
            match q.find('"') {
                Some(end) => (&q[..end], &q[end + 1..]),
                None => (q, ""),
            }
        } else {
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        if !key.is_empty() && !key.contains(char::is_whitespace) {
            attrs.push((key, value.to_string()));
        }
        rest = remaining.trim_start();
    }
    (name.to_string(), attrs)
}

#[derive(Debug, Clone)]
pub struct RenderItem<'a> {
    pub tvg_id: &'a str,
    pub chno: i64,
    pub logo: Option<&'a str>,
    pub group: Option<&'a str>,
    pub name: &'a str,
    pub url: &'a str,
}

pub fn render(items: &[RenderItem<'_>]) -> String {
    let mut out = String::from("#EXTM3U\n");
    for it in items {
        out.push('\n');
        let _ = write!(
            out,
            "#EXTINF:-1 tvg-id=\"{}\" tvg-chno=\"{}\"",
            attr(it.tvg_id),
            it.chno
        );
        if let Some(logo) = it.logo {
            let _ = write!(out, " tvg-logo=\"{}\"", attr(logo));
        }
        if let Some(group) = it.group {
            let _ = write!(out, " group-title=\"{}\"", attr(group));
        }
        let _ = writeln!(out, ",{}", single_line(it.name));
        let _ = writeln!(out, "{}", single_line(it.url));
    }
    out
}

fn attr(v: &str) -> String {
    single_line(v).replace('"', "'")
}

fn single_line(v: &str) -> String {
    v.replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "#EXTM3U\n\
#EXTINF:-1 tvg-id=\"SportsOne.uk\" tvg-logo=\"https://x/a,b.png\" group-title=\"Sports\",Sports One\n\
http://10.0.0.5:8080/ace/getstream?id=41169a3f280a5537b0611b775e926852d372743d\n\
\n\
#EXTINF:-1 tvg-id=\"kino.hd.ru\" group-title=\"Other\",Kino HD (RU)\n\
http://10.0.0.5:8080/ace/getstream?id=914c3344a2cb94beeb26c187ca10e5c862090dba\n\
#EXTINF:-1,No URL\n\
#EXTINF:0 tvg-id=plain,Plain, with comma\n\
#EXTGRP:VK\n\
http://10.0.0.5:8099/vk/-1_2?list=x\n\
http://orphan\n";

    #[test]
    fn parses_sample() {
        let p = parse(SAMPLE);
        assert_eq!(p.entries.len(), 3);
        let e = &p.entries[0];
        assert_eq!(e.name, "Sports One");
        assert_eq!(e.attr("tvg-id"), Some("SportsOne.uk"));
        assert_eq!(e.attr("tvg-logo"), Some("https://x/a,b.png"));
        assert_eq!(e.attr("group-title"), Some("Sports"));
        let e = &p.entries[2];
        assert_eq!(e.name, "Plain, with comma");
        assert_eq!(e.attr("tvg-id"), Some("plain"));
        assert_eq!(e.attr("group-title"), Some("VK"));
        assert_eq!(p.skipped.len(), 2, "{:?}", p.skipped);
    }

    #[test]
    fn renders() {
        let out = render(&[RenderItem {
            tvg_id: "a.b",
            chno: 3,
            logo: None,
            group: Some("Sp\"orts"),
            name: "Name\nX",
            url: "http://u",
        }]);
        assert_eq!(
            out,
            "#EXTM3U\n\n#EXTINF:-1 tvg-id=\"a.b\" tvg-chno=\"3\" group-title=\"Sp'orts\",Name X\nhttp://u\n"
        );
        // round trip
        let p = parse(&out);
        assert_eq!(p.entries[0].attr("tvg-chno"), Some("3"));
    }
}
