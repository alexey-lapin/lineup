//! Parsing user input into typed sources, canonical keys, and stream URL building.

use api_types::Source;
use url::Url;

use crate::error::{Result, invalid};

/// Parse free-form input: `acestream://<id>`, a bare 40-hex id, an acexy/engine URL with `id=`,
/// a vk.com / vkvideo.ru video URL, a VK proxy URL (`/vk/<owner>_<video>`), or any http(s) URL (direct).
pub fn parse_input(input: &str) -> Result<Source> {
    parse_input_inner(input).and_then(normalize)
}

fn parse_input_inner(input: &str) -> Result<Source> {
    let s = input.trim();
    if s.is_empty() {
        return Err(invalid("empty source input"));
    }

    if let Some(rest) = strip_prefix_ci(s, "acestream://") {
        let id = rest.trim_end_matches('/');
        return acestream(id);
    }
    if is_ace_id(s) {
        return acestream(s);
    }

    let url = Url::parse(s).map_err(|_| invalid(format!("unrecognised source input: {s}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid(format!("unsupported URL scheme: {}", url.scheme())));
    }

    // acexy (/ace/getstream?id=) or the engine itself (/ace/getstream, /ace/manifest.m3u8 ...).
    if url.path().starts_with("/ace/")
        && let Some(id) = query(&url, "id").or_else(|| query(&url, "content_id"))
        && is_ace_id(&id)
    {
        return acestream(&id);
    }

    // VK proxy: /vk/<owner>_<video>[?list=...]
    if let Some(rest) = url.path().strip_prefix("/vk/")
        && let Some((owner_id, video_id)) = parse_owner_video(rest)
    {
        return Ok(vk(owner_id, video_id, query(&url, "list")));
    }

    // vk.com / vkvideo.ru / m.vk.com etc.
    if let Some(host) = url.host_str()
        && is_vk_host(host)
    {
        let list = query(&url, "list");
        // ?z=video-1_2%2F...  (url crate already percent-decodes query values)
        if let Some(z) = query(&url, "z")
            && let Some(rest) = z.strip_prefix("video")
            && let Some((o, v)) = parse_owner_video(rest)
        {
            return Ok(vk(o, v, list));
        }
        // /video-1_2 or /video/-1_2 style paths; also /video_ext.php?oid=..&id=..
        if let Some((o, v)) = find_video_in_path(url.path()) {
            return Ok(vk(o, v, list));
        }
        if url.path().starts_with("/video_ext.php")
            && let (Some(oid), Some(id)) = (query(&url, "oid"), query(&url, "id"))
            && let (Ok(o), Ok(v)) = (oid.parse(), id.parse())
        {
            return Ok(vk(o, v, list.or_else(|| query(&url, "hash"))));
        }
        return Err(invalid(format!("could not find a video id in VK URL: {s}")));
    }

    Ok(Source::Direct { url: s.to_string() })
}

/// A placeholder name derived from the source alone.
pub fn default_name(s: &Source) -> String {
    match s {
        Source::Acestream { id } => format!("Acestream {}", &id[..8.min(id.len())]),
        Source::Vk { owner_id, video_id, .. } => format!("VK {owner_id}_{video_id}"),
        Source::Direct { url } => Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| url.clone()),
    }
}

/// Validate and canonicalise a structured source (lowercase ace ids, trimmed URLs).
pub fn normalize(source: Source) -> Result<Source> {
    match source {
        Source::Acestream { id } => acestream(id.trim()),
        Source::Vk {
            owner_id,
            video_id,
            list,
        } => {
            if video_id <= 0 {
                return Err(invalid("vk video_id must be positive"));
            }
            let list = list.map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
            Ok(Source::Vk {
                owner_id,
                video_id,
                list,
            })
        }
        Source::Direct { url } => {
            let url = url.trim().to_string();
            let parsed = Url::parse(&url).map_err(|e| invalid(format!("invalid direct URL: {e}")))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(invalid(format!("unsupported URL scheme: {}", parsed.scheme())));
            }
            Ok(Source::Direct { url })
        }
    }
}

/// Canonical de-duplication key (unique together with the source kind). Expects a normalised source.
pub fn source_key(source: &Source) -> String {
    match source {
        Source::Acestream { id } => id.clone(),
        Source::Vk { owner_id, video_id, .. } => format!("{owner_id}_{video_id}"),
        Source::Direct { url } => url.clone(),
    }
}

/// The URL emitted in playlists for this source, played through `base` (the integration's playback
/// base URL; ignored for direct URLs).
pub fn stream_url(source: &Source, base: &str) -> String {
    let base = base.trim_end_matches('/');
    match source {
        Source::Acestream { id } => format!("{base}/ace/getstream?id={id}"),
        Source::Vk {
            owner_id,
            video_id,
            list,
        } => {
            let mut u = format!("{base}/vk/{owner_id}_{video_id}");
            if let Some(list) = list {
                u.push_str("?list=");
                u.push_str(&url::form_urlencoded::byte_serialize(list.as_bytes()).collect::<String>());
            }
            u
        }
        Source::Direct { url } => url.clone(),
    }
}

/// Validate a base URL and strip any trailing slash.
pub fn normalize_base(name: &str, value: &str) -> Result<String> {
    let v = value.trim().trim_end_matches('/');
    let u = Url::parse(v).map_err(|e| invalid(format!("{name}: invalid URL: {e}")))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err(invalid(format!("{name}: must be an http(s) URL with a host")));
    }
    if u.query().is_some() || u.fragment().is_some() {
        return Err(invalid(format!("{name}: must not contain a query or fragment")));
    }
    Ok(v.to_string())
}

fn acestream(id: &str) -> Result<Source> {
    if !is_ace_id(id) {
        return Err(invalid(format!("acestream id must be 40 hex characters, got {id:?}")));
    }
    Ok(Source::Acestream {
        id: id.to_ascii_lowercase(),
    })
}

fn vk(owner_id: i64, video_id: i64, list: Option<String>) -> Source {
    Source::Vk {
        owner_id,
        video_id,
        list: list.filter(|l| !l.is_empty()),
    }
}

pub fn is_ace_id(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    // `get` returns None when the prefix length is not a char boundary (non-ASCII input).
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &s[prefix.len()..])
}

fn query(url: &Url, key: &str) -> Option<String> {
    url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

fn is_vk_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    ["vk.com", "vk.ru", "vkvideo.ru"]
        .iter()
        .any(|d| h == *d || h.ends_with(&format!(".{d}")))
}

/// Parse `-123_456` (optionally followed by `/...`, `?...` etc.).
fn parse_owner_video(s: &str) -> Option<(i64, i64)> {
    let (owner, rest) = s.split_once('_')?;
    let video: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let owner: i64 = owner.parse().ok()?;
    let video: i64 = video.parse().ok()?;
    (video > 0).then_some((owner, video))
}

fn find_video_in_path(path: &str) -> Option<(i64, i64)> {
    // Matches ".../video-123_456" or ".../video/-123_456" anywhere in the path.
    for (i, _) in path.match_indices("video") {
        let rest = &path[i + "video".len()..];
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        if let Some(ov) = parse_owner_video(rest) {
            return Some(ov);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACE: &str = "41169a3f280a5537b0611b775e926852d372743d";

    #[test]
    fn acestream_forms() {
        let want = Source::Acestream { id: ACE.into() };
        assert_eq!(parse_input(&format!("acestream://{ACE}")).unwrap(), want);
        assert_eq!(
            parse_input(&format!("ACESTREAM://{}", ACE.to_uppercase())).unwrap(),
            want
        );
        assert_eq!(parse_input(&format!("  {ACE}  ")).unwrap(), want);
        assert_eq!(
            parse_input(&format!("http://10.0.0.9:6878/ace/getstream?id={ACE}")).unwrap(),
            want
        );
        assert_eq!(
            parse_input(&format!("http://x:8080/ace/manifest.m3u8?content_id={ACE}")).unwrap(),
            want
        );
        assert!(parse_input("acestream://abc").is_err());
    }

    #[test]
    fn vk_forms() {
        let want = Source::Vk {
            owner_id: -100200300,
            video_id: 456000001,
            list: Some("4d2c9e1a7f".into()),
        };
        for input in [
            "https://vk.com/video-100200300_456000001?list=4d2c9e1a7f",
            "https://vkvideo.ru/video-100200300_456000001?list=4d2c9e1a7f",
            "https://m.vk.com/video-100200300_456000001?list=4d2c9e1a7f",
            "https://vk.com/somegroup?z=video-100200300_456000001%2Fpl_x&list=4d2c9e1a7f",
            "http://10.0.0.5:8099/vk/-100200300_456000001?list=4d2c9e1a7f",
        ] {
            assert_eq!(parse_input(input).unwrap(), want, "{input}");
        }
        let no_list = parse_input("https://vk.com/video123_45").unwrap();
        assert_eq!(
            no_list,
            Source::Vk {
                owner_id: 123,
                video_id: 45,
                list: None
            }
        );
        assert!(parse_input("https://vk.com/id1").is_err());
    }

    #[test]
    fn direct_and_errors() {
        assert_eq!(
            parse_input("https://example.com/live.m3u8").unwrap(),
            Source::Direct {
                url: "https://example.com/live.m3u8".into()
            }
        );
        assert!(parse_input("").is_err());
        assert!(parse_input("not a url").is_err());
        assert!(parse_input("ftp://x/y").is_err());
        // non-ASCII input must never panic
        assert_eq!(
            parse_input("http://x/канал").unwrap(),
            Source::Direct {
                url: "http://x/канал".into()
            }
        );
        for s in [
            "ккккккккккккк",
            "acestreamкк://",
            "😀😀😀😀😀😀😀😀",
            "https://vk.com/видео",
        ] {
            let _ = parse_input(s);
        }
        // free-form and structured paths share normalisation
        assert!(parse_input("https://vk.com/video_ext.php?oid=1&id=-2").is_err());
        assert!(
            normalize(Source::Direct {
                url: "rtmp://x/y".into()
            })
            .is_err()
        );
    }

    #[test]
    fn keys_and_urls() {
        let ace = parse_input(ACE).unwrap();
        assert_eq!(source_key(&ace), ACE);
        assert_eq!(
            stream_url(&ace, "http://10.0.0.5:8080"),
            format!("http://10.0.0.5:8080/ace/getstream?id={ACE}")
        );

        let vk = parse_input("https://vk.com/video-1_2?list=abc").unwrap();
        assert_eq!(source_key(&vk), "-1_2");
        assert_eq!(
            stream_url(&vk, "http://10.0.0.5:8099/"),
            "http://10.0.0.5:8099/vk/-1_2?list=abc"
        );

        let vk_nolist = Source::Vk {
            owner_id: -1,
            video_id: 2,
            list: None,
        };
        assert_eq!(
            stream_url(&vk_nolist, "http://10.0.0.5:8099"),
            "http://10.0.0.5:8099/vk/-1_2"
        );
    }

    #[test]
    fn base_validation() {
        assert_eq!(normalize_base("x", "http://a:1/").unwrap(), "http://a:1");
        assert!(normalize_base("x", "a:1").is_err());
        assert!(normalize_base("x", "http://a/?q=1").is_err());
    }
}
