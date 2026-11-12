//! Routes: how channels are played, and the one validation path for every write that sets a
//! channel's source (create, edit, import, resync).
//!
//! A Direct route takes http(s) stream URLs as they are. A gateway route asks its gateway's rule to
//! match the input (`/v1/match`), which returns the canonical source, a de-duplication identity and
//! a play reference. Gateway calls happen here, before the store takes its write lock; the store
//! then checks that the route did not change meanwhile.

use api_types::{Hints, RouteConfig, ValidatedSource};
use gateway_contract::{Info, MatchResult, RuleInfo};

use crate::error::{Error, Result, invalid};
use crate::gateway::{Gateway, play_url};

/// A source as the route validated it, with what the store keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct Validated {
    pub source: String,
    pub identity: String,
    /// Gateway play reference (None for Direct).
    pub play: Option<String>,
    /// The rule's play revision when `play` was obtained (None for Direct).
    pub play_revision: Option<String>,
    pub hints: Hints,
    pub warnings: Vec<String>,
}

impl Validated {
    pub fn to_api(&self, config: &RouteConfig) -> ValidatedSource {
        ValidatedSource {
            source: self.source.clone(),
            identity: self.identity.clone(),
            play_url: channel_play_url(config, &self.source, self.play.as_deref()),
            hints: self.hints.clone(),
            warnings: self.warnings.clone(),
        }
    }
}

/// Per input: validated, or why the route did not accept it.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Ok(Validated),
    Rejected(String),
}

/// Results of validating a batch with one route.
#[derive(Debug, Clone)]
pub struct Batch {
    pub items: Vec<Outcome>,
    /// The gateway rule's description at validation time (gateway routes).
    pub rule: Option<RuleInfo>,
}

/// Validate `inputs` with a route config. Fails as a whole only when the gateway fails (not
/// reachable, an error answer, another contract, or no such rule).
pub async fn validate(gw: &Gateway, config: &RouteConfig, inputs: &[String]) -> Result<Batch> {
    match config {
        RouteConfig::Direct {} => Ok(Batch {
            items: inputs
                .iter()
                .map(|i| match direct(i) {
                    Ok(v) => Outcome::Ok(v),
                    Err(e) => Outcome::Rejected(e.to_string()),
                })
                .collect(),
            rule: None,
        }),
        RouteConfig::Gateway { base_url, rule } => {
            let info = gw.info(base_url).await?;
            let rule_info = find_rule(&info, base_url, rule)?.clone();
            let results = gw.match_inputs(base_url, rule, inputs).await?;
            let items = results
                .into_iter()
                .map(|r| match r {
                    MatchResult::Matched(m) => Outcome::Ok(Validated {
                        source: m.reference,
                        identity: m.identity,
                        play: Some(m.play),
                        play_revision: Some(rule_info.play_revision.clone()),
                        hints: hints(m.hints),
                        warnings: m.warnings,
                    }),
                    MatchResult::NoMatch { reason, .. } => Outcome::Rejected(match reason {
                        Some(r) => format!("the route does not accept this input: {r}"),
                        None => "the route does not accept this input".into(),
                    }),
                    MatchResult::Error { error, .. } => Outcome::Rejected(format!(
                        "the gateway could not process this input: {} ({})",
                        error.message, error.code
                    )),
                })
                .collect();
            Ok(Batch {
                items,
                rule: Some(rule_info),
            })
        }
    }
}

/// Validate one input; a rejection is an `invalid` error.
pub async fn validate_one(gw: &Gateway, config: &RouteConfig, input: &str) -> Result<(Validated, Option<RuleInfo>)> {
    let mut batch = validate(gw, config, &[input.to_string()]).await?;
    match batch.items.pop() {
        Some(Outcome::Ok(v)) => Ok((v, batch.rule)),
        Some(Outcome::Rejected(why)) => Err(invalid(why)),
        None => Err(Error::Internal("validation returned no result".into())),
    }
}

pub fn find_rule<'a>(info: &'a Info, base: &str, rule: &str) -> Result<&'a RuleInfo> {
    info.rule(rule).ok_or_else(|| {
        Error::GatewayError(format!(
            "the gateway at {base} has no rule {rule:?} (it has: {})",
            info.rules
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })
}

/// The URL players get for a channel.
pub fn channel_play_url(config: &RouteConfig, source: &str, play: Option<&str>) -> String {
    match (config, play) {
        (RouteConfig::Gateway { base_url, .. }, Some(p)) => play_url(base_url, p),
        _ => source.to_string(),
    }
}

/// A Direct source: an http(s) URL with a host, trimmed and otherwise kept as it is.
pub fn direct(input: &str) -> Result<Validated> {
    let s = input.trim();
    if s.is_empty() {
        return Err(invalid("empty URL"));
    }
    let u = url::Url::parse(s).map_err(|e| invalid(format!("not a URL ({e}): {s}")))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err(invalid(format!("a direct route needs an http(s) URL with a host: {s}")));
    }
    Ok(Validated {
        source: s.to_string(),
        identity: s.to_string(),
        play: None,
        play_revision: None,
        hints: Hints::default(),
        warnings: vec![],
    })
}

fn hints(h: gateway_contract::Hints) -> Hints {
    let clean = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Hints {
        title: clean(h.title),
        region: clean(h.region).map(|r| r.to_ascii_lowercase()),
        group: clean(h.group),
        event: h.event,
    }
}

/// Validate and normalise a route config: a gateway base URL (http(s), host, no query) and a rule
/// name.
pub fn normalize_config(config: RouteConfig) -> Result<RouteConfig> {
    match config {
        RouteConfig::Direct {} => Ok(RouteConfig::Direct {}),
        RouteConfig::Gateway { base_url, rule } => Ok(RouteConfig::Gateway {
            base_url: normalize_base(&base_url)?,
            rule: normalize_rule(&rule)?,
        }),
    }
}

pub fn normalize_base(value: &str) -> Result<String> {
    let v = value.trim().trim_end_matches('/');
    let u = url::Url::parse(v).map_err(|e| invalid(format!("base_url: invalid URL: {e}")))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err(invalid("base_url: must be an http(s) URL with a host"));
    }
    if u.query().is_some() || u.fragment().is_some() {
        return Err(invalid("base_url: must not contain a query or fragment"));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(invalid("base_url: must not contain credentials"));
    }
    Ok(v.to_string())
}

fn normalize_rule(rule: &str) -> Result<String> {
    let r = rule.trim();
    let ok = !r.is_empty()
        && r.len() <= 64
        && r.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if !ok {
        return Err(invalid(format!("rule {r:?}: use lowercase letters, digits, - and _")));
    }
    Ok(r.to_string())
}

pub fn normalize_route_name(name: &str) -> Result<String> {
    let n = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if n.is_empty() {
        return Err(invalid("route name must not be empty"));
    }
    if n.chars().count() > 64 {
        return Err(invalid("route name must be at most 64 characters"));
    }
    Ok(n)
}

/// A placeholder channel name for a validated source without a title hint.
pub fn default_name(config: &RouteConfig, v: &Validated) -> String {
    if let Some(t) = &v.hints.title {
        return t.clone();
    }
    match config {
        RouteConfig::Direct {} => match url::Url::parse(&v.source) {
            Ok(u) => {
                let host = u.host_str().unwrap_or_default();
                let last = u
                    .path_segments()
                    .and_then(|mut s| s.rfind(|x| !x.is_empty()))
                    .unwrap_or_default();
                if last.is_empty() {
                    host.to_string()
                } else {
                    format!("{host} {last}")
                }
            }
            Err(_) => v.source.clone(),
        },
        RouteConfig::Gateway { .. } => v.identity.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_urls() {
        let v = direct("  https://example.com/live/x.m3u8?token=a%20b ").unwrap();
        assert_eq!(v.source, "https://example.com/live/x.m3u8?token=a%20b");
        assert_eq!(v.identity, v.source);
        assert!(direct("ftp://x/y").is_err());
        assert!(direct("not a url").is_err());
        assert!(direct("").is_err());
        assert!(direct("http://x/канал").is_ok(), "non-ASCII paths are fine");
    }

    #[test]
    fn configs() {
        let c = normalize_config(RouteConfig::Gateway {
            base_url: " http://h:8099/gw/ ".into(),
            rule: " vk ".into(),
        })
        .unwrap();
        assert_eq!(
            c,
            RouteConfig::Gateway {
                base_url: "http://h:8099/gw".into(),
                rule: "vk".into()
            }
        );
        for (base, rule) in [
            ("h:1", "vk"),
            ("http://h/?q", "vk"),
            ("http://u:p@h", "vk"),
            ("http://h", "V K"),
        ] {
            assert!(
                normalize_config(RouteConfig::Gateway {
                    base_url: base.into(),
                    rule: rule.into()
                })
                .is_err(),
                "{base} {rule}"
            );
        }
    }

    #[test]
    fn placeholder_names() {
        let d = direct("http://example.com/live/one.m3u8").unwrap();
        assert_eq!(default_name(&RouteConfig::Direct {}, &d), "example.com one.m3u8");
        let d = direct("http://example.com/").unwrap();
        assert_eq!(default_name(&RouteConfig::Direct {}, &d), "example.com");
    }
}
