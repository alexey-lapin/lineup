//! Templates for identities, refs and play URLs.
//!
//! - `{var}`: the variable. In URL templates every byte outside the URI unreserved set
//!   (`A-Z a-z 0-9 - . _ ~`) is percent-encoded; in raw templates (identities, which are opaque keys
//!   and never fetched) it is inserted as it is.
//! - `{?a,b}`: `?a=<a>&b=<b>`, `{&a}`: `&a=<a>`; values encoded as above, empty or missing
//!   variables skipped (and `{?…}` with nothing to add adds nothing).
//!
//! A missing variable renders as the empty string. Which variables exist is checked when a rule
//! is loaded, against the captures its patterns can produce.

use std::collections::{BTreeMap, BTreeSet};

pub type Vars = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    Var(String),
    /// `{?a,b}` (first = '?') or `{&a}` (first = '&').
    Query {
        first: char,
        names: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    source: String,
    parts: Vec<Part>,
    raw: bool,
}

/// How a template is used, which decides what may vary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    /// An identity: variables raw, anything goes.
    Identity,
    /// A ref: an http(s) URL (then fetchable: scheme and authority literal) or an opaque
    /// `scheme://…` reference whose variables may follow the literal `scheme://`.
    Ref,
    /// A play URL: always fetchable, so scheme and authority are literal.
    Play,
}

impl Template {
    pub fn parse(source: &str, usage: Use) -> anyhow::Result<Self> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut rest = source;
        while let Some(open) = rest.find(['{', '}']) {
            if rest.as_bytes()[open] == b'}' {
                anyhow::bail!("unmatched '}}' in {source:?}");
            }
            lit.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let close = after
                .find('}')
                .ok_or_else(|| anyhow::anyhow!("unclosed '{{' in {source:?}"))?;
            let inner = &after[..close];
            if !lit.is_empty() {
                parts.push(Part::Literal(std::mem::take(&mut lit)));
            }
            parts.push(match inner.chars().next() {
                Some(c @ ('?' | '&')) => {
                    let names: Vec<String> = inner[1..].split(',').map(|n| n.trim().to_string()).collect();
                    for n in &names {
                        check_name(n, source)?;
                    }
                    Part::Query { first: c, names }
                }
                _ => {
                    check_name(inner, source)?;
                    Part::Var(inner.to_string())
                }
            });
            rest = &after[close + 1..];
        }
        lit.push_str(rest);
        if !lit.is_empty() {
            parts.push(Part::Literal(lit));
        }
        let t = Template {
            source: source.to_string(),
            parts,
            raw: usage == Use::Identity,
        };
        t.check_use(usage)?;
        Ok(t)
    }

    /// Fetchable URLs keep their scheme and authority literal, so no captured text can choose
    /// where the gateway or a player connects.
    fn check_use(&self, usage: Use) -> anyhow::Result<()> {
        let prefix = match self.parts.first() {
            Some(Part::Literal(l)) => l.as_str(),
            _ => "",
        };
        let literal_only = self.parts.len() == 1 && matches!(self.parts[0], Part::Literal(_));
        // URI schemes are case-insensitive: HTTPS:// is as fetchable as https://.
        let lower = prefix.to_ascii_lowercase();
        let scheme_len = ["http://", "https://"]
            .iter()
            .find(|s| lower.starts_with(**s))
            .map(|s| s.len());
        let fetchable = usage == Use::Play || scheme_len.is_some();
        match usage {
            Use::Identity => Ok(()),
            _ if fetchable => {
                let Some(after) = scheme_len.map(|n| &prefix[n..]) else {
                    anyhow::bail!("{:?}: must start with http:// or https://", self.source);
                };
                // The authority ends at the first '/', '?' or '#' (or the end of a template without
                // variables), which must be literal too.
                let host_end = after.find(['/', '?', '#']).or(literal_only.then_some(after.len()));
                if host_end.is_none_or(|i| i == 0) {
                    anyhow::bail!(
                        "{:?}: the scheme and host must be literal text, with no variable before the path",
                        self.source
                    );
                }
                Ok(())
            }
            _ => {
                // An opaque reference: a literal scheme, then anything.
                let scheme = prefix.split_once("://").map(|(s, _)| s);
                if !scheme.is_some_and(|s| {
                    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
                }) {
                    anyhow::bail!("{:?}: a ref starts with a literal scheme://", self.source);
                }
                Ok(())
            }
        }
    }

    /// Variables substituted as `{var}`: they must have a value (query expansions may be empty).
    pub fn required(&self) -> BTreeSet<String> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Var(n) => Some(n.clone()),
                _ => None,
            })
            .collect()
    }

    /// Every variable the template reads.
    pub fn names(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for p in &self.parts {
            match p {
                Part::Var(n) => {
                    out.insert(n.clone());
                }
                Part::Query { names, .. } => out.extend(names.iter().cloned()),
                Part::Literal(_) => {}
            }
        }
        out
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn render(&self, vars: &Vars) -> String {
        let mut out = String::new();
        let get = |n: &str| vars.get(n).map(String::as_str).unwrap_or("");
        for p in &self.parts {
            match p {
                Part::Literal(l) => out.push_str(l),
                Part::Var(n) if self.raw => out.push_str(get(n)),
                Part::Var(n) => out.push_str(&encode(get(n))),
                Part::Query { first, names } => {
                    let mut sep = *first;
                    for n in names {
                        let v = get(n);
                        if v.is_empty() {
                            continue;
                        }
                        out.push(sep);
                        out.push_str(&encode(n));
                        out.push('=');
                        out.push_str(&encode(v));
                        sep = '&';
                    }
                }
            }
        }
        out
    }
}

fn check_name(n: &str, source: &str) -> anyhow::Result<()> {
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        anyhow::bail!("{source:?}: {n:?} is not a variable name (letters, digits, '_')");
    }
    Ok(())
}

/// Percent-encode every byte outside the URI unreserved set.
pub fn encode(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(kv: &[(&str, &str)]) -> Vars {
        kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn renders_and_encodes() {
        let t = Template::parse("https://video.example/video{owner}_{video}{?list}", Use::Ref).unwrap();
        assert_eq!(
            t.render(&vars(&[("owner", "-1"), ("video", "2"), ("list", "a+b/c&d=e?f#g %ü")])),
            "https://video.example/video-1_2?list=a%2Bb%2Fc%26d%3De%3Ff%23g%20%25%C3%BC"
        );
        assert_eq!(
            t.render(&vars(&[("owner", "-1"), ("video", "2")])),
            "https://video.example/video-1_2"
        );
        let q = Template::parse("https://h/p{?a,b}{&c}", Use::Ref).unwrap();
        assert_eq!(q.render(&vars(&[("b", "2"), ("c", "3")])), "https://h/p?b=2&c=3");
        let id = Template::parse("v:{owner}_{video}", Use::Identity).unwrap();
        assert_eq!(id.render(&vars(&[("owner", "-1"), ("video", "2")])), "v:-1_2");
        let opaque = Template::parse("id://{id}", Use::Ref).unwrap();
        assert_eq!(opaque.render(&vars(&[("id", "abc")])), "id://abc");
        assert_eq!(
            opaque.names().into_iter().collect::<Vec<_>>(),
            ["id"],
            "names lists the variables"
        );
    }

    #[test]
    fn fetchable_templates_keep_their_authority() {
        for bad in [
            ("https://{host}/x", Use::Play),
            ("https://h{port}/x", Use::Play),
            ("{base}/x", Use::Play),
            ("id://{id}", Use::Play),
            ("https://{host}/x", Use::Ref),
            ("{x}", Use::Ref),
            ("://{x}", Use::Ref),
            ("https://h/{x", Use::Ref),
            ("https://h/x}", Use::Ref),
            ("https://h/{a-b}", Use::Ref),
            ("HTTPS://{host}/x", Use::Ref),
            ("Http://{host}/x", Use::Ref),
            ("https://", Use::Ref),
        ] {
            assert!(Template::parse(bad.0, bad.1).is_err(), "{bad:?}");
        }
        for good in [
            ("http://media-host:8080/stream?id={id}", Use::Play),
            ("https://h/x{?a}", Use::Play),
            ("id://{id}", Use::Ref),
            ("https://video.example/video{o}", Use::Ref),
            ("{anything}", Use::Identity),
            ("https://v.example", Use::Ref),
            ("https://v.example", Use::Play),
            ("HTTPS://v.example/{x}", Use::Ref),
        ] {
            assert!(Template::parse(good.0, good.1).is_ok(), "{good:?}");
        }
    }
}
