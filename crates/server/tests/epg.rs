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
