//! EPG fetching against a local HTTP server: generations, validators, failures and recovery.

use std::sync::{Arc, Mutex};

use api_types::{EpgFetchStatus, EpgSourceCreate, EpgSourcePatch};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use lineup_server::epg::Epg;
use lineup_server::{Event, Store};

fn guide(title: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<tv>
  <channel id="SportsOne.uk"><display-name>Sports One</display-name><icon src="http://img/s1.png"/></channel>
  <channel id="Arena1.de"><display-name>Arena 1</display-name></channel>
  <programme start="20990101100000 +0000" stop="20990101120000 +0000" channel="Arena1.de"><title>{title}</title></programme>
</tv>
"#
    )
}

#[derive(Default)]
struct Upstream {
    body: Vec<u8>,
    etag: String,
    /// If-None-Match header of each request.
    seen: Vec<Option<String>>,
}

type Shared = Arc<Mutex<Upstream>>;

async fn serve_guide(State(u): State<Shared>, headers: HeaderMap) -> axum::response::Response {
    let mut u = u.lock().unwrap();
    let inm = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    u.seen.push(inm.clone());
    if inm.as_deref() == Some(u.etag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    ([(header::ETAG, u.etag.clone())], u.body.clone()).into_response()
}

fn gz(s: &str) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(s.as_bytes()).unwrap();
    e.finish().unwrap()
}

async fn upstream() -> (String, Shared) {
    let shared: Shared = Arc::new(Mutex::new(Upstream {
        body: gz(&guide("v1")),
        etag: "\"v1\"".into(),
        ..Default::default()
    }));
    let app = axum::Router::new()
        .route("/guide.xml.gz", get(serve_guide))
        .route("/other.xml", get(serve_guide))
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), shared)
}

fn set(u: &Shared, body: Vec<u8>, etag: &str) {
    let mut u = u.lock().unwrap();
    u.body = body;
    u.etag = etag.into();
}

fn gens(dir: &std::path::Path, id: i64) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir.join("epg").join(id.to_string()))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn fetch_lifecycle() {
    let (base, up) = upstream().await;
    let data = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&data.path().join("lineup.db")).await.unwrap());
    let mut events = store.subscribe();
    let epg = Epg::new(store.clone(), data.path());
    epg.recover().await.unwrap();

    let src = store
        .create_epg_source(EpgSourceCreate {
            url: format!("{base}/guide.xml.gz"),
            name: None,
            priority: 0,
            enabled: true,
        })
        .await
        .unwrap();
    assert!(src.name.ends_with("guide.xml.gz"), "{}", src.name);

    // First fetch publishes generation 1, unconditionally.
    let r = epg.fetch(&[]).await.unwrap();
    assert_eq!(r[0].status, EpgFetchStatus::Updated, "{r:?}");
    assert_eq!(r[0].channels, Some(2));
    assert_eq!(r[0].programmes, Some(1));
    assert_eq!(events.try_recv().unwrap(), Event::EpgUpdated { sources: vec![src.id] });
    let s = store.get_epg_source(src.id).await.unwrap();
    assert_eq!(
        (s.current_gen, s.channel_count, s.last_status.as_deref()),
        (Some(1), 2, Some("ok"))
    );
    let chans = store.epg_channels().await.unwrap();
    assert_eq!(chans.len(), 2);
    assert_eq!(
        chans
            .iter()
            .find(|c| c.xmltv_id == "SportsOne.uk")
            .unwrap()
            .icon
            .as_deref(),
        Some("http://img/s1.png")
    );

    // Unchanged upstream: conditional request, 304, nothing published.
    let r = epg.fetch(&[src.id]).await.unwrap();
    assert_eq!(r[0].status, EpgFetchStatus::NotModified);
    assert!(events.try_recv().is_err());
    assert_eq!(up.lock().unwrap().seen, [None, Some("\"v1\"".to_string())]);

    // New content: generation 2, generation 1 kept as previous; then 3, and 1 is removed.
    set(&up, gz(&guide("v2")), "\"v2\"");
    assert_eq!(epg.fetch(&[]).await.unwrap()[0].status, EpgFetchStatus::Updated);
    assert_eq!(gens(data.path(), src.id), ["gen-1", "gen-2"]);
    set(&up, guide("v3").into_bytes(), "\"v3\""); // plain XML works too
    assert_eq!(epg.fetch(&[]).await.unwrap()[0].status, EpgFetchStatus::Updated);
    assert_eq!(gens(data.path(), src.id), ["gen-2", "gen-3"]);

    // A broken download records the error and leaves the served generation alone.
    set(&up, b"<tv><channel id=\"x\">".to_vec(), "\"bad\"");
    let r = epg.fetch(&[]).await.unwrap();
    assert_eq!(r[0].status, EpgFetchStatus::Error);
    let s = store.get_epg_source(src.id).await.unwrap();
    assert_eq!((s.current_gen, s.last_status.as_deref()), (Some(3), Some("error")));
    assert!(s.last_error.is_some());
    assert_eq!(
        gens(data.path(), src.id),
        ["gen-2", "gen-3"],
        "failed candidate removed"
    );
    while events.try_recv().is_ok() {}

    // URL change: the next request carries no validators even though the etag matches.
    set(&up, guide("v4").into_bytes(), "\"v3\"");
    store
        .patch_epg_source(
            src.id,
            EpgSourcePatch {
                url: Some(format!("{base}/other.xml")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        events.try_recv().is_err(),
        "URL change alone does not change guide content"
    );
    let r = epg.fetch(&[]).await.unwrap();
    assert_eq!(r[0].status, EpgFetchStatus::Updated);
    assert_eq!(up.lock().unwrap().seen.last().unwrap(), &None);
    assert_eq!(gens(data.path(), src.id), ["gen-3", "gen-4"]);

    // Disabled sources are not fetched.
    store
        .patch_epg_source(
            src.id,
            EpgSourcePatch {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(epg.fetch(&[]).await.unwrap()[0].status, EpgFetchStatus::Disabled);
    assert!(
        store.epg_channels().await.unwrap().is_empty(),
        "disabled sources are not offered"
    );
}

#[tokio::test]
async fn recovery() {
    let (base, up) = upstream().await;
    let data = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&data.path().join("lineup.db")).await.unwrap());
    let epg = Epg::new(store.clone(), data.path());
    let src = store
        .create_epg_source(EpgSourceCreate {
            url: format!("{base}/guide.xml.gz"),
            name: Some("Guide".into()),
            priority: 0,
            enabled: true,
        })
        .await
        .unwrap();
    epg.fetch(&[]).await.unwrap();
    set(&up, gz(&guide("v2")), "\"v2\"");
    epg.fetch(&[]).await.unwrap();
    assert_eq!(gens(data.path(), src.id), ["gen-1", "gen-2"]);

    // A crash left an incomplete candidate, and the current generation lost its marker.
    std::fs::create_dir(epg.gen_dir(src.id, 3)).unwrap();
    std::fs::remove_file(epg.gen_dir(src.id, 2).join("COMPLETE")).unwrap();
    // A directory for a source that no longer exists.
    std::fs::create_dir_all(data.path().join("epg").join("999")).unwrap();

    let epg = Epg::new(store.clone(), data.path());
    epg.recover().await.unwrap();
    assert_eq!(gens(data.path(), src.id), ["gen-1"]);
    assert!(!data.path().join("epg").join("999").exists());
    let s = store.get_epg_source(src.id).await.unwrap();
    assert_eq!((s.current_gen, s.channel_count), (Some(1), 2));

    // With no usable generation at all, the source serves nothing and refetches unconditionally.
    std::fs::remove_file(epg.gen_dir(src.id, 1).join("COMPLETE")).unwrap();
    epg.recover().await.unwrap();
    let s = store.get_epg_source(src.id).await.unwrap();
    assert_eq!((s.current_gen, s.channel_count), (None, 0));
    let r = epg.fetch(&[]).await.unwrap();
    assert_eq!(r[0].status, EpgFetchStatus::Updated);
    assert_eq!(up.lock().unwrap().seen.last().unwrap(), &None);
}

mod output {
    use super::*;
    use api_types::{ChannelCreate, ChannelPatch, PlaylistCreate};
    use axum::body::Body;
    use axum::http::Request;
    use chrono::{TimeZone, Utc};
    use http_body_util::BodyExt;
    use lineup_server::http::{App, router_for};
    use tower::ServiceExt;

    const ACE1: &str = "41169a3f280a5537b0611b775e926852d372743d";
    const ACE2: &str = "816b61ec91d5d4da0bc477a42cd6ee407fc07d44";
    const ACE3: &str = "914c3344a2cb94beeb26c187ca10e5c862090dba";

    fn ch(name: &str, input: &str, tvg_id: Option<&str>) -> ChannelCreate {
        ChannelCreate {
            name: name.into(),
            input: Some(input.into()),
            tvg_id: tvg_id.map(str::to_string),
            playlists: vec!["jf".into()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn playlist_guide() {
        let (base, _up) = upstream().await;
        let data = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&data.path().join("lineup.db")).await.unwrap());
        let app = App::new(store.clone(), data.path());
        let epg = app.epg.clone();
        let router = router_for(app);

        store
            .create_playlist(PlaylistCreate {
                slug: "jf".into(),
                name: None,
            })
            .await
            .unwrap();
        let arena = store
            .create_channel(ch("Arena", ACE1, Some("Arena1.de")))
            .await
            .unwrap();
        let mut sports = ch("Sports", ACE2, Some("SportsOne.uk"));
        sports.enabled = false;
        store.create_channel(sports).await.unwrap();
        let mut event = ch("Cup <Final>", "https://vk.com/video-1_2", None);
        event.is_event = true;
        event.kickoff_at = Some(Utc.with_ymd_and_hms(2099, 1, 1, 20, 0, 0).unwrap());
        let event = store.create_channel(event).await.unwrap();
        store
            .create_channel(ch("Unknown", ACE3, Some("Nope.xx")))
            .await
            .unwrap();

        // Before any source is fetched: only the event channel.
        let now = Utc.with_ymd_and_hms(2099, 1, 1, 18, 30, 0).unwrap();
        let r = epg.playlist_xml_at("jf", now).await.unwrap();
        let xml = std::str::from_utf8(&r.body).unwrap();
        assert!(!xml.contains("Arena1.de"), "{xml}");

        store
            .create_epg_source(EpgSourceCreate {
                url: format!("{base}/guide.xml.gz"),
                name: None,
                priority: 0,
                enabled: true,
            })
            .await
            .unwrap();
        epg.fetch(&[]).await.unwrap();

        let r = epg.playlist_xml_at("jf", now).await.unwrap();
        let xml = std::str::from_utf8(&r.body).unwrap().to_string();
        let ev_id = format!("lineup.{}", event.id);
        assert!(xml.starts_with("<?xml"), "{xml}");
        assert!(
            xml.contains("<channel id=\"Arena1.de\"><display-name>Arena 1</display-name></channel>"),
            "{xml}"
        );
        assert!(!xml.contains("SportsOne.uk"), "disabled channels are left out: {xml}");
        assert!(!xml.contains("Nope.xx"), "ids without guide data are left out");
        assert!(xml.contains(&format!(
            "<channel id=\"{ev_id}\"><display-name>Cup &lt;Final&gt;</display-name>"
        )));
        assert!(xml.contains("<title>v1</title>"));
        assert!(xml.contains(&format!(
            "<programme start=\"20990101180000 +0000\" stop=\"20990101200000 +0000\" channel=\"{ev_id}\"><title>Pre-match</title>"
        )));
        assert!(xml.contains("<title>Cup &lt;Final&gt; (ended)</title>"));
        // All channels before any programme.
        assert!(xml.rfind("<channel ").unwrap() < xml.find("<programme ").unwrap());
        assert!(xml.trim_end().ends_with("</tv>"));

        // Cached until something changes or the hour turns.
        let again = epg.playlist_xml_at("jf", now).await.unwrap();
        assert!(Arc::ptr_eq(&r, &again));
        let later = epg
            .playlist_xml_at("jf", now + chrono::Duration::hours(1))
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&again, &later));
        store
            .patch_channel(
                arena.id,
                ChannelPatch {
                    tvg_id: Some(Some("SportsOne.uk".into())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let r = epg
            .playlist_xml_at("jf", now + chrono::Duration::hours(1))
            .await
            .unwrap();
        let xml = std::str::from_utf8(&r.body).unwrap();
        assert!(xml.contains("SportsOne.uk") && !xml.contains("Arena1.de"), "{xml}");

        // Over HTTP: ETag and 304; unknown playlist is 404; the m3u announces the guide URL.
        let get = |uri: &str, inm: Option<&str>| {
            let mut req = Request::builder().uri(uri).header("host", "media-host:9300");
            if let Some(t) = inm {
                req = req.header("if-none-match", t);
            }
            router.clone().oneshot(req.body(Body::empty()).unwrap())
        };
        let res = get("/p/jf/epg.xml", None).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let etag = res.headers()["etag"].to_str().unwrap().to_string();
        assert!(
            res.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("application/xml")
        );
        let res = get("/p/jf/epg.xml", Some(&etag)).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            get("/p/nope/epg.xml", None).await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        let res = get("/p/jf.m3u", None).await.unwrap();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let m3u = std::str::from_utf8(&body).unwrap();
        assert!(
            m3u.starts_with("#EXTM3U url-tvg=\"http://media-host:9300/p/jf/epg.xml\"\n"),
            "{m3u}"
        );
    }
}

mod races {
    use super::*;

    #[tokio::test]
    async fn stale_publishes_are_discarded() {
        let data = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&data.path().join("lineup.db")).await.unwrap());
        let create = |url: &str| EpgSourceCreate {
            url: url.into(),
            name: None,
            priority: 0,
            enabled: true,
        };
        // Ids are never reused.
        let a = store.create_epg_source(create("http://a/x.xml")).await.unwrap();
        store.delete_epg_source(a.id).await.unwrap();
        let b = store.create_epg_source(create("http://a/x.xml")).await.unwrap();
        assert_ne!(a.id, b.id);
        assert!(
            !store
                .publish_epg_generation(a.id, "http://a/x.xml", 1, &[])
                .await
                .unwrap()
        );

        // A download of the old URL that finishes after a URL change publishes nothing and records
        // nothing.
        store
            .patch_epg_source(
                b.id,
                EpgSourcePatch {
                    url: Some("http://b/y.xml".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            !store
                .publish_epg_generation(b.id, "http://a/x.xml", 1, &[])
                .await
                .unwrap()
        );
        store
            .record_epg_fetch(
                b.id,
                "http://a/x.xml",
                lineup_server::store::FetchOutcome::Error,
                Some("old"),
            )
            .await
            .unwrap();
        let s = store.get_epg_source(b.id).await.unwrap();
        assert_eq!((s.current_gen, s.last_status), (None, None));
        assert!(
            store
                .publish_epg_generation(b.id, "http://b/y.xml", 1, &[])
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn recovery_checks_files_not_just_the_marker() {
        let (base, up) = upstream().await;
        let data = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&data.path().join("lineup.db")).await.unwrap());
        let epg = Epg::new(store.clone(), data.path());
        let src = store
            .create_epg_source(EpgSourceCreate {
                url: format!("{base}/guide.xml.gz"),
                name: None,
                priority: 0,
                enabled: true,
            })
            .await
            .unwrap();
        epg.fetch(&[]).await.unwrap();
        set(&up, gz(&guide("v2")), "\"v2\"");
        epg.fetch(&[]).await.unwrap();
        std::fs::remove_file(epg.gen_dir(src.id, 2).join("programmes.idx")).unwrap();

        // An unusable current generation is not trusted for a conditional request either.
        let r = epg.fetch(&[]).await.unwrap();
        assert_eq!(r[0].status, EpgFetchStatus::Updated);
        assert_eq!(up.lock().unwrap().seen.last().unwrap(), &None);
        assert_eq!(store.get_epg_source(src.id).await.unwrap().current_gen, Some(3));

        // Recovery falls back past a current generation with a missing file.
        std::fs::remove_file(epg.gen_dir(src.id, 3).join("channels.json")).unwrap();
        epg.recover().await.unwrap();
        let s = store.get_epg_source(src.id).await.unwrap();
        assert_eq!(s.current_gen, None, "generation 2 is broken too, so nothing is served");
    }
}
