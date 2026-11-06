//! The add flow: resolving an input into a source plus suggested channel metadata.

use std::time::Duration;

use api_types::{ResolveResponse, Source};

use crate::error::Result;
use crate::store::Store;
use crate::suggest;

/// Best-match score from which the match's tvg-id and logo are suggested.
const SUGGEST_SCORE: f64 = 0.6;
const ENGINE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MATCHES: usize = 5;

/// Resolve `input` and add suggestions: the engine's name for acestream ids (cleaned), guide
/// matches for the name, a group and the event flag.
pub async fn resolve(store: &Store, client: &reqwest::Client, input: &str) -> Result<ResolveResponse> {
    let mut r = store.resolve(input).await?;
    let mut region = None;
    match &r.source {
        Source::Acestream { id } => {
            let settings = store.settings().await?;
            match engine_name(client, &settings.engine_base, id).await {
                Ok(Some(raw)) => {
                    let (name, reg) = clean_name(&raw);
                    if !name.is_empty() {
                        r.suggestion.name = name;
                    }
                    region = reg;
                    r.engine_name = Some(raw);
                }
                Ok(None) => r.warnings.push("the engine has no name for this id".into()),
                Err(e) => r.warnings.push(format!("engine name lookup failed: {e}")),
            }
            if looks_like_sports(&r.suggestion.name) {
                r.suggestion.group_title = Some("Sports".into());
            }
        }
        Source::Vk { .. } => {
            r.suggestion.group_title = Some("VK".into());
            r.suggestion.is_event = true;
        }
        Source::Raw { .. } => {}
    }
    // Placeholder names ("Acestream 1a2b3c4d") match nothing useful.
    if r.engine_name.is_some() {
        let all = store.epg_channels().await?;
        r.matches = suggest::rank(all, &r.suggestion.name, region.as_deref(), MAX_MATCHES);
        if let Some(best) = r.matches.first().filter(|m| m.score.unwrap_or(0.0) >= SUGGEST_SCORE) {
            r.suggestion.tvg_id = Some(best.xmltv_id.clone());
            r.suggestion.tvg_logo = best.icon.clone();
        }
    }
    Ok(r)
}

/// `GET {engine}/server/api?method=get_media_files&content_id=<id>`. The engine answers
/// `{"result": {"0": "<name>"}, "error": null}`; the first name is used.
async fn engine_name(client: &reqwest::Client, engine_base: &str, id: &str) -> anyhow::Result<Option<String>> {
    let url = format!("{}/server/api", engine_base.trim_end_matches('/'));
    let res = client
        .get(url)
        .query(&[("method", "get_media_files"), ("content_id", id)])
        .timeout(ENGINE_TIMEOUT)
        .send()
        .await?;
    anyhow::ensure!(res.status().is_success(), "HTTP {}", res.status());
    let body: serde_json::Value = res.json().await?;
    if let Some(e) = body.get("error").filter(|e| !e.is_null()) {
        anyhow::bail!("engine error: {e}");
    }
    Ok(first_string(body.get("result")).map(|s| strip_extension(&s)))
}

fn first_string(v: Option<&serde_json::Value>) -> Option<String> {
    use serde_json::Value;
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => a.iter().find_map(|x| first_string(Some(x))),
        Value::Object(o) => {
            // Keys are file indexes ("0", "1", ...): take the lowest.
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort_by_key(|k| (k.parse::<u64>().unwrap_or(u64::MAX), k.to_string()));
            keys.into_iter().find_map(|k| first_string(o.get(k)))
        }
        _ => None,
    }
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

fn strip_extension(s: &str) -> String {
    for ext in [".mp4", ".mkv", ".ts", ".avi", ".m2ts"] {
        if s.len() > ext.len() && s.to_ascii_lowercase().ends_with(ext) {
            return s[..s.len() - ext.len()].to_string();
        }
    }
    s.to_string()
}

/// Quality and encoding words dropped from engine names.
const NOISE: &[&str] = &[
    "hd", "fhd", "uhd", "sd", "hq", "lq", "4k", "8k", "bar", "hevc", "h264", "h265", "x264", "x265", "1080p", "1080i",
    "720p", "576p", "50fps", "60fps", "25fps", "multi", "backup",
];

/// Clean an engine name: drop `[XX]`/`(XX)` tags (a two-letter one becomes the region hint) and
/// quality words, and collapse spaces. "Arena 1 Bar HD [DE]" → ("Arena 1", Some("de")).
pub fn clean_name(raw: &str) -> (String, Option<String>) {
    let mut region = None;
    let mut text = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        let close = match c {
            '[' => ']',
            '(' => ')',
            _ => {
                text.push(c);
                continue;
            }
        };
        let mut tag = String::new();
        for d in chars.by_ref() {
            if d == close {
                break;
            }
            tag.push(d);
        }
        let tag = tag.trim();
        if region.is_none() && tag.len() == 2 && tag.chars().all(|c| c.is_ascii_alphabetic()) {
            region = Some(tag.to_ascii_lowercase());
        }
        text.push(' ');
    }
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == '|' || c == '_')
        .filter(|w| !w.is_empty() && *w != "-")
        .filter(|w| !NOISE.contains(&w.to_ascii_lowercase().as_str()))
        .collect();
    (words.join(" "), region)
}

const SPORTS_WORDS: &[&str] = &[
    "sport",
    "sports",
    "football",
    "soccer",
    "arena",
    "match",
    "league",
    "liga",
    "cup",
    "f1",
    "racing",
    "golf",
    "tennis",
    "fight",
    "boxing",
    "ufc",
    "nba",
    "nfl",
    "nhl",
    "mlb",
    "rugby",
    "cricket",
    "motor",
    "bein",
    "eurosport",
    "espn",
    "sky sports",
];

fn looks_like_sports(name: &str) -> bool {
    let tokens = suggest::tokens(name);
    let joined = tokens.join(" ");
    SPORTS_WORDS.iter().any(|w| {
        if w.contains(' ') {
            joined.contains(w)
        } else {
            tokens.iter().any(|t| t == w)
        }
    }) || tokens.iter().any(|t| t.starts_with("sport"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_engine_names() {
        assert_eq!(clean_name("Arena 1 Bar HD [DE]"), ("Arena 1".into(), Some("de".into())));
        assert_eq!(
            clean_name("Sports One FHD (UK) [backup]"),
            ("Sports One".into(), Some("uk".into()))
        );
        assert_eq!(clean_name("  Movie  Max | 1080p  "), ("Movie Max".into(), None));
    }

    #[test]
    fn engine_payloads() {
        let v = serde_json::json!({"1": "b.mp4", "0": "Arena 1 HD.mkv"});
        assert_eq!(
            first_string(Some(&v)).map(|s| strip_extension(&s)).as_deref(),
            Some("Arena 1 HD")
        );
        assert_eq!(first_string(Some(&serde_json::json!(["x"]))).as_deref(), Some("x"));
        assert_eq!(first_string(Some(&serde_json::json!({}))), None);
    }

    #[test]
    fn sports_heuristic() {
        assert!(looks_like_sports("Arena 1"));
        assert!(looks_like_sports("SportsOne"));
        assert!(!looks_like_sports("Movie Max"));
    }
}
