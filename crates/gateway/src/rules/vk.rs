//! The `vk` rule: VK video pages, live or recorded, relayed as HLS.
//!
//! match (no extraction): `vk.com`, `vk.ru`, `vkvideo.ru` (and subdomains such as
//! `sport.vkvideo.ru`, `m.vk.com`) video links in their usual forms — `/video-1_2`,
//! `/video/-1_2`, `/live-1_2`, `?z=video-1_2%2F…`, `/video_ext.php?oid=-1&id=2` — legacy vkserve
//! URLs (`…/vk/-1_2`), and this gateway's own play URLs. Identity `vk:<owner>_<video>`; ref the
//! canonical `https://vk.com/video<owner>_<video>[?list=…]` (the `list` access token is kept: some
//! videos need it); play `play/vk.m3u8?ref=<ref>`.

use gateway_contract::{Hints, MatchResult, Matched};

pub const MATCH_REVISION: &str = "1";
pub const PLAY_REVISION: &str = "1";

/// Owner, video and access token of a VK video reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VkRef {
    pub owner: i64,
    pub video: i64,
    pub list: Option<String>,
}

impl VkRef {
    pub fn canonical(&self) -> String {
        match &self.list {
            Some(l) => format!(
                "https://vk.com/video{}_{}?list={}",
                self.owner,
                self.video,
                url::form_urlencoded::byte_serialize(l.as_bytes()).collect::<String>()
            ),
            None => format!("https://vk.com/video{}_{}", self.owner, self.video),
        }
    }

    pub fn identity(&self) -> String {
        format!("vk:{}_{}", self.owner, self.video)
    }

    pub fn play(&self) -> String {
        format!(
            "play/vk.m3u8?ref={}",
            url::form_urlencoded::byte_serialize(self.canonical().as_bytes()).collect::<String>()
        )
    }
}

pub fn match_inputs(inputs: &[String]) -> Vec<MatchResult> {
    inputs
        .iter()
        .map(|input| match parse(input) {
            Some(r) => MatchResult::Matched(Matched {
                input: input.clone(),
                identity: r.identity(),
                reference: r.canonical(),
                play: r.play(),
                hints: Hints {
                    title: None,
                    region: None,
                    group: Some("VK".into()),
                },
                warnings: vec![],
            }),
            None => MatchResult::NoMatch {
                input: input.clone(),
                reason: Some("not a VK video link".into()),
            },
        })
        .collect()
}

/// Parse any accepted form.
pub fn parse(input: &str) -> Option<VkRef> {
    let u = url::Url::parse(input.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let q = |k: &str| u.query_pairs().find(|(key, _)| key == k).map(|(_, v)| v.into_owned());
    let list = q("list").filter(|l| {
        !l.is_empty() && l.len() <= 64 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    });
    // This gateway's play URL (any host or prefix): the ref inside it.
    if u.path().ends_with("/play/vk.m3u8") {
        return parse(&q("ref")?);
    }
    let host = u.host_str()?.to_ascii_lowercase();
    let vk_host = ["vk.com", "vk.ru", "vkvideo.ru"]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")));
    if !vk_host {
        // Legacy vkserve proxy URLs: /vk/<owner>_<video>, on any host.
        let path = u.path();
        let rest = path.rfind("/vk/").map(|i| &path[i + 4..])?;
        let (o, v) = owner_video(rest)?;
        return Some(VkRef {
            owner: o,
            video: v,
            list,
        });
    }
    if let Some(z) = q("z")
        && let Some(rest) = z.strip_prefix("video")
        && let Some((o, v)) = owner_video(rest)
    {
        return Some(VkRef {
            owner: o,
            video: v,
            list,
        });
    }
    if u.path().starts_with("/video_ext.php") {
        let (o, v) = (q("oid")?.parse().ok()?, q("id")?.parse().ok()?);
        return (v > 0).then_some(VkRef {
            owner: o,
            video: v,
            list: list.or_else(|| q("hash")),
        });
    }
    for marker in ["video", "live-"] {
        for (i, _) in u.path().match_indices(marker) {
            let rest = &u.path()[i + marker.len()..];
            let rest = if marker == "live-" {
                // "live-222_456": the dash is the owner's sign.
                &u.path()[i + marker.len() - 1..]
            } else {
                rest.strip_prefix('/').unwrap_or(rest)
            };
            if let Some((o, v)) = owner_video(rest) {
                return Some(VkRef {
                    owner: o,
                    video: v,
                    list,
                });
            }
        }
    }
    None
}

/// `-123_456` followed by anything that is not a digit.
fn owner_video(s: &str) -> Option<(i64, i64)> {
    let (owner, rest) = s.split_once('_')?;
    let video: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let owner: i64 = owner.parse().ok()?;
    let video: i64 = video.parse().ok()?;
    (video > 0).then_some((owner, video))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_forms() {
        let want = VkRef {
            owner: -100001,
            video: 200002,
            list: Some("0a1b2c3d4e5f6a7b8c".into()),
        };
        for input in [
            "https://sport.vkvideo.ru/live-100001_200002?list=0a1b2c3d4e5f6a7b8c",
            "https://vk.com/video-100001_200002?list=0a1b2c3d4e5f6a7b8c",
            "https://vkvideo.ru/video-100001_200002?list=0a1b2c3d4e5f6a7b8c",
            "https://m.vk.com/video-100001_200002?list=0a1b2c3d4e5f6a7b8c",
            "https://vk.com/somegroup?z=video-100001_200002%2Fpl_x&list=0a1b2c3d4e5f6a7b8c",
            "https://vkvideo.ru/video/-100001_200002?list=0a1b2c3d4e5f6a7b8c",
            "http://192.168.1.50:8099/vk/-100001_200002?list=0a1b2c3d4e5f6a7b8c",
            "http://proxy/vkproxy/vk/-100001_200002?list=0a1b2c3d4e5f6a7b8c",
            "http://gw:8099/play/vk.m3u8?ref=https%3A%2F%2Fvk.com%2Fvideo-100001_200002%3Flist%3D0a1b2c3d4e5f6a7b8c",
            "http://proxy/gw/play/vk.m3u8?ref=https%3A%2F%2Fvk.com%2Fvideo-100001_200002%3Flist%3D0a1b2c3d4e5f6a7b8c",
        ] {
            assert_eq!(parse(input).as_ref(), Some(&want), "{input}");
        }
        assert_eq!(
            want.canonical(),
            "https://vk.com/video-100001_200002?list=0a1b2c3d4e5f6a7b8c"
        );
        assert_eq!(want.identity(), "vk:-100001_200002");
        assert_eq!(parse(&want.canonical()), Some(want.clone()), "canonical parses back");
        let ext = parse("https://vk.com/video_ext.php?oid=-1&id=2&hash=abc").unwrap();
        assert_eq!((ext.owner, ext.video, ext.list.as_deref()), (-1, 2, Some("abc")));
        let no_list = parse("https://vk.com/video123_45").unwrap();
        assert_eq!(no_list.canonical(), "https://vk.com/video123_45");
        for bad in [
            "https://vk.com/id1",
            "https://example.com/video-1_2",
            "https://vk.com/video_ext.php?oid=1&id=-2",
            "ftp://vk.com/video1_2",
            "https://vk.com/видео",
            "not a url",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
        // A list token with odd characters is dropped rather than carried.
        assert_eq!(parse("https://vk.com/video1_2?list=a%22b").unwrap().list, None);
    }
}
