//! Matching inputs with configured patterns, and rendering identities and refs from what they
//! capture. Pure: no network, no tools, so examples and `--check-config` can run it anywhere.
//!
//! A rule's patterns are tried in order and the first that matches gives the variables. A pattern
//! matches when every field it has matches:
//!
//! - `input`: a regex searched in the trimmed raw input (for non-URL forms; anchor it to match the
//!   whole input).
//! - `hosts`: the URL's host is one of them or a subdomain of one.
//! - `path`: a regex searched in the URL's path.
//! - `query`: an ordered list of `{param, re, optional}`. The parameter's first value, decoded,
//!   must match `re` in full. A required entry that is missing or does not match fails the
//!   pattern; an optional one is skipped then.
//! - `require`: variables that must be non-empty in the end.
//!
//! Patterns with `hosts`, `path` or `query` need an http(s) URL with a host and no user info.
//!
//! Variables are the regexes' named captures. Fields are evaluated in order (input, path, then the
//! query entries) and the first non-empty value of a variable wins. `vars` then normalises them:
//! `int` (a signed 64-bit decimal, written canonically), `pos` (as `int`, above zero), `lower`.
//! A value that does not convert fails the pattern, and the next one is tried.

use std::collections::{BTreeMap, BTreeSet};

use regex::{Regex, RegexBuilder};
use serde::Deserialize;

use crate::template::{Template, Use, Vars};

/// Inputs longer than this are refused before any regex runs.
pub const MAX_INPUT: usize = 4096;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct PatternConfig {
    pub input: Option<String>,
    pub hosts: Option<Vec<String>>,
    pub path: Option<String>,
    #[serde(default)]
    pub query: Vec<QueryConfig>,
    #[serde(default)]
    pub require: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct QueryConfig {
    pub param: String,
    pub re: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum VarType {
    Str,
    Int,
    Pos,
    Lower,
}

/// An input a rule must (or must not) accept, checked whenever the config is loaded.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct ExampleConfig {
    pub input: String,
    pub identity: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub play: Option<String>,
    /// `false`: the rule must not accept the input.
    #[serde(rename = "match", default = "yes")]
    pub matches: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone)]
struct Pattern {
    input: Option<Regex>,
    hosts: Option<Vec<String>>,
    path: Option<Regex>,
    query: Vec<(String, Regex, bool)>,
    require: Vec<String>,
}

/// A rule's compiled patterns and identity/ref templates.
#[derive(Debug, Clone)]
pub struct Matcher {
    patterns: Vec<Pattern>,
    vars: BTreeMap<String, VarType>,
    identity: Template,
    reference: Template,
    /// The rule's own play path (`play/<rule>.m3u8`) for relay rules: its `ref` is matched again.
    own_play: Option<String>,
}

/// What a matched input gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub vars: Vars,
    pub identity: String,
    pub reference: String,
}

fn compile(re: &str, anchored: bool) -> anyhow::Result<Regex> {
    let src = if anchored {
        format!("^(?:{re})$")
    } else {
        re.to_string()
    };
    RegexBuilder::new(&src)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| anyhow::anyhow!("bad regex {re:?}: {e}"))
}

fn capture_names(r: &Regex) -> impl Iterator<Item = String> + '_ {
    r.capture_names().flatten().map(str::to_string)
}

impl Matcher {
    /// Compile a rule's patterns and templates. `own_play`: the rule's relay play path, if any.
    pub fn new(
        patterns: &[PatternConfig],
        vars: &BTreeMap<String, VarType>,
        identity: &str,
        reference: &str,
        own_play: Option<String>,
    ) -> anyhow::Result<Self> {
        if patterns.is_empty() {
            anyhow::bail!("match: at least one pattern");
        }
        let mut compiled = Vec::new();
        let mut names = BTreeSet::new();
        for (i, p) in patterns.iter().enumerate() {
            let at = |e: anyhow::Error| anyhow::anyhow!("match[{i}]: {e}");
            if p.input.is_none() && p.hosts.is_none() && p.path.is_none() && p.query.is_empty() {
                anyhow::bail!("match[{i}]: a pattern needs input, hosts, path or query");
            }
            if p.input.is_some() && (p.hosts.is_some() || p.path.is_some() || !p.query.is_empty()) {
                anyhow::bail!("match[{i}]: input matches the raw text; it does not combine with hosts, path or query");
            }
            let input = p.input.as_deref().map(|r| compile(r, false)).transpose().map_err(at)?;
            let path = p.path.as_deref().map(|r| compile(r, false)).transpose().map_err(at)?;
            let mut query = Vec::new();
            for q in &p.query {
                let r = compile(&q.re, true).map_err(at)?;
                names.extend(capture_names(&r));
                query.push((q.param.clone(), r, q.optional));
            }
            for r in input.iter().chain(path.iter()) {
                names.extend(capture_names(r));
            }
            let hosts = p
                .hosts
                .as_ref()
                .map(|h| h.iter().map(|x| x.trim().to_ascii_lowercase()).collect::<Vec<_>>());
            if hosts
                .as_ref()
                .is_some_and(|h| h.is_empty() || h.iter().any(String::is_empty))
            {
                anyhow::bail!("match[{i}]: hosts must not be empty");
            }
            for r in &p.require {
                if !names.contains(r) {
                    anyhow::bail!("match[{i}]: require names {r:?}, which no pattern captures");
                }
            }
            compiled.push(Pattern {
                input,
                hosts,
                path,
                query,
                require: p.require.clone(),
            });
        }
        for v in vars.keys() {
            if !names.contains(v) {
                anyhow::bail!("vars: {v:?} is not captured by any pattern");
            }
        }
        let identity = Template::parse(identity, Use::Identity).map_err(|e| anyhow::anyhow!("identity: {e}"))?;
        let reference = Template::parse(reference, Use::Ref).map_err(|e| anyhow::anyhow!("ref: {e}"))?;
        for (what, t) in [("identity", &identity), ("ref", &reference)] {
            if let Some(n) = t.names().iter().find(|n| !names.contains(*n)) {
                anyhow::bail!("{what}: {{{n}}} is not captured by any pattern");
            }
        }
        Ok(Matcher {
            patterns: compiled,
            vars: vars.clone(),
            identity,
            reference,
            own_play,
        })
    }

    /// Every variable the patterns can capture.
    pub fn names(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for p in &self.patterns {
            for r in p.input.iter().chain(p.path.iter()).chain(p.query.iter().map(|q| &q.1)) {
                out.extend(capture_names(r));
            }
        }
        out
    }

    pub fn matches(&self, input: &str) -> Option<Match> {
        self.matches_at(input, true)
    }

    fn matches_at(&self, input: &str, outer: bool) -> Option<Match> {
        let input = input.trim();
        if input.len() > MAX_INPUT {
            return None;
        }
        let url = url::Url::parse(input).ok().filter(|u| {
            matches!(u.scheme(), "http" | "https")
                && u.host_str().is_some()
                && u.username().is_empty()
                && u.password().is_none()
        });
        // This gateway's own play URL for the rule (any host or prefix): the ref inside it, once.
        if outer
            && let (Some(own), Some(u)) = (&self.own_play, &url)
            && u.path().ends_with(&format!("/{own}"))
            && let Some((_, r)) = u.query_pairs().find(|(k, _)| k == "ref")
        {
            return self.matches_at(&r, false);
        }
        self.patterns
            .iter()
            .find_map(|p| self.try_pattern(p, input, url.as_ref()))
    }

    fn try_pattern(&self, p: &Pattern, input: &str, url: Option<&url::Url>) -> Option<Match> {
        let mut vars = Vars::new();
        let mut take = |r: &Regex, text: &str| -> bool {
            let Some(c) = r.captures(text) else { return false };
            for name in r.capture_names().flatten() {
                if let Some(m) = c.name(name).filter(|m| !m.as_str().is_empty()) {
                    vars.entry(name.to_string()).or_insert_with(|| m.as_str().to_string());
                }
            }
            true
        };
        if let Some(r) = &p.input
            && !take(r, input)
        {
            return None;
        }
        if p.hosts.is_some() || p.path.is_some() || !p.query.is_empty() {
            let u = url?;
            if let Some(hosts) = &p.hosts {
                let host = u.host_str()?.to_ascii_lowercase();
                if !hosts.iter().any(|h| host == *h || host.ends_with(&format!(".{h}"))) {
                    return None;
                }
            }
            if let Some(r) = &p.path
                && !take(r, u.path())
            {
                return None;
            }
            for (param, r, optional) in &p.query {
                let value = u.query_pairs().find(|(k, _)| k == param).map(|(_, v)| v.into_owned());
                let ok = value.as_deref().is_some_and(|v| take(r, v));
                if !ok && !optional {
                    return None;
                }
            }
        }
        for (name, ty) in &self.vars {
            if let Some(v) = vars.get_mut(name) {
                *v = convert(v, *ty)?;
            }
        }
        if p.require.iter().any(|r| vars.get(r).is_none_or(String::is_empty)) {
            return None;
        }
        Some(Match {
            identity: self.identity.render(&vars),
            reference: self.reference.render(&vars),
            vars,
        })
    }
}

fn convert(v: &str, ty: VarType) -> Option<String> {
    match ty {
        VarType::Str => Some(v.to_string()),
        VarType::Lower => Some(v.to_lowercase()),
        VarType::Int => v.parse::<i64>().ok().map(|n| n.to_string()),
        VarType::Pos => v.parse::<i64>().ok().filter(|n| *n > 0).map(|n| n.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(toml: &str) -> Vec<PatternConfig> {
        #[derive(Deserialize)]
        struct W {
            #[serde(rename = "match")]
            m: Vec<PatternConfig>,
        }
        toml::from_str::<W>(toml).unwrap().m
    }

    #[test]
    fn first_pattern_and_first_value_win() {
        let m = Matcher::new(
            &pat(r#"
                [[match]]
                hosts = ["vk.com"]
                query = [{ param = "z", re = 'video(?P<o>-?[0-9]+)_(?P<v>[0-9]+)(?:[^0-9].*)?' }]
                [[match]]
                hosts = ["vk.com"]
                path = 'video(?P<o>-?[0-9]+)_(?P<v>[0-9]+)'
                query = [
                  { param = "list", re = '(?P<l>[a-z0-9]{1,8})', optional = true },
                  { param = "hash", re = '(?P<l>[a-z0-9]{1,8})', optional = true },
                ]
            "#),
            &[("o".into(), VarType::Int), ("v".into(), VarType::Pos)].into(),
            "x:{o}_{v}",
            "https://vk.com/video{o}_{v}{?l}",
            Some("play/x.m3u8".into()),
        )
        .unwrap();
        let id = |i: &str| m.matches(i).map(|x| x.identity);
        assert_eq!(
            id("https://vk.com/video1_2?z=video3_4%2Fpl"),
            Some("x:3_4".into()),
            "z first"
        );
        assert_eq!(id("https://m.vk.com/video-001_02x"), Some("x:-1_2".into()), "typed");
        assert_eq!(id("https://vk.com/video1_0"), None, "pos");
        assert_eq!(id("https://vk.com/video99999999999999999999_1"), None, "bounded");
        assert_eq!(id("https://notvk.com/video1_2"), None);
        assert_eq!(id("https://u:p@vk.com/video1_2"), None, "no user info");
        let r = |i: &str| m.matches(i).unwrap().reference;
        assert_eq!(
            r("https://vk.com/video1_2?list=bad%22&hash=abc"),
            "https://vk.com/video1_2?l=abc"
        );
        assert_eq!(
            r("https://vk.com/video1_2?hash=abc&list=def"),
            "https://vk.com/video1_2?l=def",
            "list first"
        );
        assert_eq!(
            r("https://vk.com/video1_2?list=a&list=b"),
            "https://vk.com/video1_2?l=a",
            "first value"
        );
        assert_eq!(
            r("http://gw/prefix/play/x.m3u8?ref=https%3A%2F%2Fvk.com%2Fvideo1_2"),
            "https://vk.com/video1_2",
            "own play URL"
        );
        let nested =
            "http://gw/play/x.m3u8?ref=http%3A%2F%2Fgw%2Fplay%2Fx.m3u8%3Fref%3Dhttps%253A%252F%252Fvk.com%252Fvideo1_2";
        assert_eq!(m.matches(nested), None, "one level only");
        assert_eq!(
            m.matches(&format!("https://vk.com/video1_2?x={}", "a".repeat(5000))),
            None
        );
    }

    #[test]
    fn refuses_bad_definitions() {
        let vars = BTreeMap::new();
        let one = |toml: &str, id: &str, r: &str| Matcher::new(&pat(toml), &vars, id, r, None);
        assert!(one("match = []", "x", "a://x").is_err());
        assert!(one("[[match]]\n", "x", "a://x").is_err(), "empty pattern");
        assert!(one("[[match]]\ninput = '('", "x", "a://x").is_err(), "bad regex");
        assert!(
            one("[[match]]\ninput = '(?P<a>x)'", "{b}", "a://{a}").is_err(),
            "unknown var"
        );
        assert!(one("[[match]]\ninput = '(?P<a>x)'\npath = 'x'", "{a}", "a://{a}").is_err());
        assert!(one("[[match]]\ninput = '(?P<a>x)'\nrequire = ['b']", "{a}", "a://{a}").is_err());
        assert!(
            one("[[match]]\ninput = '(?P<a>x)'", "{a}", "https://{a}/").is_err(),
            "authority"
        );
        assert!(one("[[match]]\ninput = '(?P<a>x)'", "{a}", "a://{a}").is_ok());
        let typed = [("b".to_string(), VarType::Int)].into();
        assert!(Matcher::new(&pat("[[match]]\ninput = '(?P<a>x)'"), &typed, "{a}", "a://{a}", None).is_err());
    }
}
