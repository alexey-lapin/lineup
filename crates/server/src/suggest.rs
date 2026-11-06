//! Fuzzy matching of channel names against the guide sources' channels.

use api_types::EpgChannel;

/// Tokens of a name: lowercased alphanumeric runs, also split at camelCase and letter/digit
/// boundaries ("SportsOne2" → ["sports", "one", "2"]).
pub fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev = None;
            continue;
        }
        if let Some(p) = prev {
            let boundary = (p.is_lowercase() && c.is_uppercase())
                || (p.is_alphabetic() && c.is_numeric())
                || (p.is_numeric() && c.is_alphabetic());
            if boundary && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.extend(c.to_lowercase());
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Similarity of two token lists in [0, 1].
fn similarity(q: &[String], c: &[String]) -> f64 {
    if q.is_empty() || c.is_empty() {
        return 0.0;
    }
    let a: String = q.concat();
    let b: String = c.concat();
    if a == b {
        return 1.0;
    }
    let dice = strsim::sorensen_dice(&a, &b);
    let common = q.iter().filter(|t| c.contains(t)).count() as f64;
    let overlap = common / q.len().max(c.len()) as f64;
    let mut score = 0.6 * dice + 0.4 * overlap;
    // Typing part of a name ("sport") should find it.
    if a.len() >= 3 && b.contains(&a) {
        score = score.max(0.7 + 0.3 * a.len() as f64 / b.len() as f64);
    }
    score
}

/// Candidate names of a guide channel: its display names and its id without the region suffix.
fn candidates(ch: &EpgChannel) -> Vec<Vec<String>> {
    let id = match ch.xmltv_id.rsplit_once('.') {
        Some((head, tail)) if (2..=3).contains(&tail.len()) && tail.chars().all(char::is_alphabetic) => head,
        _ => ch.xmltv_id.as_str(),
    };
    std::iter::once(id)
        .chain(ch.display_names.iter().map(String::as_str))
        .map(tokens)
        .filter(|t| !t.is_empty())
        .collect()
}

/// Lowest score worth showing.
const MIN_SCORE: f64 = 0.3;
/// Added when the channel id ends with the region hint (".de" for a "[DE]" tag).
const REGION_BONUS: f64 = 0.1;

/// Best matches for `query`, highest score first, one entry per xmltv id. `all` is expected in
/// source priority order; ties keep that order. `region` is a lowercase country code.
pub fn rank(all: Vec<EpgChannel>, query: &str, region: Option<&str>, limit: usize) -> Vec<EpgChannel> {
    let q = tokens(query);
    if q.is_empty() {
        return vec![];
    }
    let suffix = region.map(|r| format!(".{r}"));
    // One definition per id: the highest-priority source's, which is the one that serves the guide.
    let mut seen = std::collections::HashSet::new();
    let all: Vec<EpgChannel> = all.into_iter().filter(|c| seen.insert(c.xmltv_id.clone())).collect();
    // Sort on the raw score so the region bonus still breaks ties between perfect matches.
    let mut scored: Vec<(f64, EpgChannel)> = all
        .into_iter()
        .filter_map(|mut ch| {
            let mut score = candidates(&ch).iter().map(|c| similarity(&q, c)).fold(0.0, f64::max);
            if suffix
                .as_ref()
                .is_some_and(|s| ch.xmltv_id.to_lowercase().ends_with(s.as_str()))
            {
                score += REGION_BONUS;
            }
            (score >= MIN_SCORE).then(|| {
                ch.score = Some((score.min(1.0) * 1000.0).round() / 1000.0);
                (score, ch)
            })
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut scored: Vec<EpgChannel> = scored.into_iter().map(|(_, c)| c).collect();
    scored.truncate(limit);
    scored
}

/// The tvg-id picker: everything (first `limit`, one per id) without a query, else ranked matches.
pub fn search(all: Vec<EpgChannel>, query: Option<&str>, limit: usize) -> Vec<EpgChannel> {
    match query.map(str::trim).filter(|q| !q.is_empty()) {
        Some(q) => rank(all, q, None, limit),
        None => {
            let mut seen = std::collections::HashSet::new();
            all.into_iter()
                .filter(|c| seen.insert(c.xmltv_id.clone()))
                .take(limit)
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(source_id: i64, id: &str, names: &[&str]) -> EpgChannel {
        EpgChannel {
            source_id,
            xmltv_id: id.into(),
            display_names: names.iter().map(|s| s.to_string()).collect(),
            icon: None,
            score: None,
        }
    }

    #[test]
    fn tokenizes() {
        assert_eq!(tokens("SportsOne2.uk"), ["sports", "one", "2", "uk"]);
        assert_eq!(tokens("Arena 1 Bar HD"), ["arena", "1", "bar", "hd"]);
    }

    #[test]
    fn ranks_with_region_and_dedup() {
        let all = vec![
            ch(1, "Arena1.de", &["Arena 1"]),
            ch(1, "Arena2.de", &["Arena 2"]),
            ch(1, "Arena1.uk", &["Arena 1 UK"]),
            ch(2, "Arena1.de", &["Arena 1"]),
            ch(1, "Weather.us", &["Weather Now"]),
        ];
        let r = rank(all.clone(), "Arena 1", Some("de"), 5);
        assert_eq!(r[0].xmltv_id, "Arena1.de");
        assert_eq!(r[0].source_id, 1, "higher-priority source kept");
        assert_eq!(r.iter().filter(|c| c.xmltv_id == "Arena1.de").count(), 1);
        assert!(r.iter().all(|c| c.xmltv_id != "Weather.us"));
        let r = rank(all.clone(), "Arena 1", Some("uk"), 5);
        assert_eq!(r[0].xmltv_id, "Arena1.uk");
        let r = search(all, Some("weath"), 5);
        assert_eq!(r[0].xmltv_id, "Weather.us");
    }
}
