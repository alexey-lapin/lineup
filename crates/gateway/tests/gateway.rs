//! The gateway over HTTP, against fake services: an Acestream engine and acexy, and a CDN whose
//! pages a fake yt-dlp "extracts".

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use lineup_gateway::config::Config;
use lineup_gateway::rules::acestream::ProbeTiming;
use serde_json::{Value, json};

const ACE: &str = "41169a3f280a5537b0611b775e926852d372743d";

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[derive(Default)]
struct Engine {
    /// Requests seen, as "path?query".
    seen: Vec<String>,
    /// Clients acexy reports for the stream.
    clients: u64,
    peers: u64,
    /// Probe sessions open now, and the most ever open at once.
    open: u64,
    max_open: u64,
}

type Shared = Arc<Mutex<Engine>>;

/// A fake engine + acexy on one port.
async fn engine(shared: Shared) -> String {
    async fn log(s: &Shared, path: &str, q: &std::collections::HashMap<String, String>) {
        let mut qs: Vec<String> = q.iter().map(|(k, v)| format!("{k}={v}")).collect();
        qs.sort();
        s.lock().unwrap().seen.push(format!("{path}?{}", qs.join("&")));
    }
    type Q = Query<std::collections::HashMap<String, String>>;
    let app = Router::new()
        .route(
            "/server/api",
            get(|State(s): State<Shared>, Query(q): Q| async move {
                log(&s, "/server/api", &q).await;
                axum::Json(json!({"result": {"0": "Arena 1 Bar HD [DE].mkv"}, "error": null}))
            }),
        )
        .route(
            "/ace/getstream",
            get(|State(s): State<Shared>, Query(q): Q| async move {
                log(&s, "/ace/getstream", &q).await;
                {
                    let mut e = s.lock().unwrap();
                    e.open += 1;
                    e.max_open = e.max_open.max(e.open);
                }
                axum::Json(json!({"response": {
                    "stat_url": "http://127.0.0.1:6878/ace/stat/abc/s1",
                    "command_url": "http://127.0.0.1:6878/ace/cmd/abc/s1"
                }, "error": null}))
            }),
        )
        .route(
            "/ace/stat/abc/s1",
            get(|State(s): State<Shared>, Query(q): Q| async move {
                log(&s, "/ace/stat/abc/s1", &q).await;
                let peers = s.lock().unwrap().peers;
                let status = if peers > 0 { "dl" } else { "prebuf" };
                axum::Json(json!({"response": {"status": status, "peers": peers, "speed_down": 100}, "error": null}))
            }),
        )
        .route(
            "/ace/cmd/abc/s1",
            get(|State(s): State<Shared>, Query(q): Q| async move {
                log(&s, "/ace/cmd/abc/s1", &q).await;
                {
                    let mut e = s.lock().unwrap();
                    e.open = e.open.saturating_sub(1);
                }
                axum::Json(json!({"response": "ok", "error": null}))
            }),
        )
        .route(
            "/ace/status",
            get(|State(s): State<Shared>, Query(q): Q| async move {
                log(&s, "/ace/status", &q).await;
                let clients = s.lock().unwrap().clients;
                if clients == 0 {
                    return (StatusCode::NOT_FOUND, "").into_response();
                }
                axum::Json(json!({"clients": clients, "stat_url": "http://127.0.0.1:6878/ace/stat/abc/s1"}))
                    .into_response()
            }),
        )
        .with_state(shared);
    serve(app).await
}

#[derive(Default)]
struct Cdn {
    ended: bool,
    /// Answer 403 to everything (expired URLs).
    expired: bool,
    ranges: Vec<String>,
    user_agents: Vec<String>,
    /// URL generation the CDN signs now; older generations answer 400 (a rejected signature).
    generation: u32,
    /// Bandwidth announced for the variant (changing it makes the new master incompatible).
    bandwidth: u32,
}

type CdnState = Arc<Mutex<Cdn>>;

async fn cdn(state: CdnState) -> String {
    let app = Router::new()
        .route(
            "/v/master.m3u8",
            get(|State(s): State<CdnState>, h: HeaderMap| async move {
                let mut c = s.lock().unwrap();
                c.user_agents
                    .push(h.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("").into());
                if c.expired {
                    return (StatusCode::FORBIDDEN, "").into_response();
                }
                format!(
                    "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH={}\n720/index.m3u8?gen={}\n",
                    c.bandwidth.max(1000),
                    c.generation
                )
                .into_response()
            }),
        )
        .route(
            "/v/720/index.m3u8",
            get(
                |State(s): State<CdnState>, Query(q): Query<std::collections::HashMap<String, String>>| async move {
                    let c = s.lock().unwrap();
                    if c.expired {
                        return (StatusCode::FORBIDDEN, "").into_response();
                    }
                    if q.get("gen").and_then(|g| g.parse::<u32>().ok()) != Some(c.generation) {
                        return (StatusCode::BAD_REQUEST, "bad signature").into_response();
                    }
                    let mut body = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:0\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"/keys/k1\"\n#EXTINF:6.0,\nseg0.ts\n#EXTINF:6.0,\nseg1.ts\n"
                        .to_string();
                    if c.ended {
                        body.push_str("#EXT-X-ENDLIST\n");
                    }
                    body.into_response()
                },
            ),
        )
        .route(
            "/v/720/seg0.ts",
            get(|State(s): State<CdnState>, h: HeaderMap| async move {
                let mut c = s.lock().unwrap();
                if c.expired {
                    return (StatusCode::FORBIDDEN, "").into_response();
                }
                let data = b"0123456789";
                match h.get(header::RANGE).and_then(|v| v.to_str().ok()) {
                    Some(r) => {
                        c.ranges.push(r.to_string());
                        (
                            StatusCode::PARTIAL_CONTENT,
                            [
                                (header::CONTENT_RANGE, "bytes 2-4/10"),
                                (header::CONTENT_TYPE, "video/mp2t"),
                            ],
                            data[2..5].to_vec(),
                        )
                            .into_response()
                    }
                    None => ([(header::CONTENT_TYPE, "video/mp2t")], data.to_vec()).into_response(),
                }
            }),
        )
        .route("/keys/k1", get(|| async { vec![7u8; 16] }))
        .with_state(state);
    serve(app).await
}

/// A fake yt-dlp printing an info dict for the CDN.
fn fake_yt_dlp(dir: &std::path::Path, cdn: &str, live: bool) -> std::path::PathBuf {
    let p = dir.join("yt-dlp");
    let info = json!({
        "title": "Match", "is_live": live, "duration": 8163.0,
        "formats": [{"protocol": "m3u8_native", "url": format!("{cdn}/v/720/index.m3u8"),
                     "manifest_url": format!("{cdn}/v/master.m3u8"),
                     "http_headers": {"User-Agent": "Mozilla/5.0 Chrome/140", "Cookie": "secret"}}]
    });
    std::fs::write(&p, format!("#!/bin/sh\ncat <<'JSON'\n{info}\nJSON\n")).unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    p
}

struct Fixture {
    url: String,
    engine: Shared,
    cdn: CdnState,
    cdn_url: String,
    _dir: tempfile::TempDir,
}

async fn fixture(live: bool) -> Fixture {
    fixture_with(live, true).await
}

async fn fixture_with(live: bool, acexy: bool) -> Fixture {
    let shared = Shared::default();
    let engine_url = engine(shared.clone()).await;
    let cdn_state = CdnState::default();
    let cdn_url = cdn(cdn_state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let yt = fake_yt_dlp(dir.path(), &cdn_url, live);
    let cfg = Config::parse(&format!(
        r#"
        [acestream]
        engine_control = "{engine_url}"
        {}
        acexy_public = "http://192.168.1.50:8080"
        [vk]
        [tools]
        yt_dlp = "{}"
        [egress]
        allow = ["127.0.0.1"]
        "#,
        if acexy {
            format!("acexy_control = \"{engine_url}\"")
        } else {
            String::new()
        },
        yt.display()
    ))
    .unwrap();
    let g = lineup_gateway::Gateway::new(
        &cfg,
        ProbeTiming {
            poll_every: std::time::Duration::from_millis(20),
            give_up_after: std::time::Duration::from_millis(100),
        },
    )
    .await
    .unwrap();
    let url = serve(lineup_gateway::router(g)).await;
    Fixture {
        url,
        engine: shared,
        cdn: cdn_state,
        cdn_url,
        _dir: dir,
    }
}

async fn post(url: &str, body: Value) -> (u16, Value) {
    let r = reqwest::Client::new().post(url).json(&body).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn info_and_acestream_match() {
    let f = fixture(true).await;
    let info: Value = reqwest::get(format!("{}/v1/info", f.url))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["contract"], 1);
    let names: Vec<&str> = info["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["acestream", "vk"]);

    let (s, r) = post(
        &format!("{}/v1/match", f.url),
        json!({"rule": "acestream", "inputs": [format!("acestream://{ACE}"), "nope", ACE.to_uppercase()]}),
    )
    .await;
    assert_eq!(s, 200);
    let m = &r["results"][0];
    assert_eq!(m["status"], "matched");
    assert_eq!(m["identity"], format!("ace:{ACE}"));
    assert_eq!(m["play"], format!("http://192.168.1.50:8080/ace/getstream?id={ACE}"));
    assert_eq!(m["hints"]["title"], "Arena 1");
    assert_eq!(m["hints"]["region"], "de");
    assert_eq!(m["hints"]["group"], "Sports");
    assert_eq!(r["results"][1]["status"], "no_match");
    assert_eq!(
        r["results"][2]["input"],
        ACE.to_uppercase(),
        "results echo inputs in order"
    );

    let (s, e) = post(&format!("{}/v1/match", f.url), json!({"rule": "nope", "inputs": []})).await;
    assert_eq!((s, e["error"]["code"].as_str()), (404, Some("unknown_rule")));
    let many: Vec<String> = (0..501).map(|i| i.to_string()).collect();
    let (s, _) = post(&format!("{}/v1/match", f.url), json!({"rule": "vk", "inputs": many})).await;
    assert_eq!(s, 400);
}

#[tokio::test]
async fn acestream_check_guards_probes_and_stops() {
    let f = fixture(true).await;
    let url = format!("{}/v1/check", f.url);
    let req = |force: bool| json!({"rule": "acestream", "ref": format!("acestream://{ACE}"), "force": force});

    // Watched: skipped, no session started.
    f.engine.lock().unwrap().clients = 1;
    let (_, r) = post(&url, req(false)).await;
    assert_eq!(
        (r["outcome"].as_str(), r["reason"].as_str()),
        (Some("skipped"), Some("busy"))
    );
    assert!(
        !f.engine
            .lock()
            .unwrap()
            .seen
            .iter()
            .any(|s| s.starts_with("/ace/getstream"))
    );

    // Not watched, no peers: dead, and the session it started is stopped.
    f.engine.lock().unwrap().clients = 0;
    let (_, r) = post(&url, req(false)).await;
    assert_eq!(r["outcome"], "dead", "{r}");
    let seen = f.engine.lock().unwrap().seen.clone();
    let start = seen.iter().find(|s| s.starts_with("/ace/getstream")).unwrap();
    assert!(
        start.contains("format=json") && start.contains("pid=lineup-gateway-probe-"),
        "{start}"
    );
    assert!(seen.iter().any(|s| s == "/ace/cmd/abc/s1?method=stop"), "{seen:?}");

    // Peers: ok. Forced checks skip the guard.
    f.engine.lock().unwrap().peers = 3;
    f.engine.lock().unwrap().clients = 2;
    let (_, r) = post(&url, req(true)).await;
    assert_eq!(r["outcome"], "ok", "{r}");
}

#[tokio::test]
async fn acestream_observe_reads_without_starting() {
    let f = fixture(true).await;
    let url = format!("{}/v1/observe", f.url);
    let body = json!({"rule": "acestream", "refs": [format!("acestream://{ACE}")]});
    let (_, r) = post(&url, body.clone()).await;
    assert_eq!(r["results"][0]["watching"], false);
    f.engine.lock().unwrap().clients = 2;
    f.engine.lock().unwrap().peers = 9;
    let (_, r) = post(&url, body).await;
    let o = &r["results"][0];
    assert_eq!(o["watching"], true);
    let facts: Vec<(String, String)> = o["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["label"].as_str().unwrap().into(), x["value"].as_str().unwrap().into()))
        .collect();
    assert!(facts.contains(&("Players".into(), "2".into())), "{facts:?}");
    assert!(facts.contains(&("Peers".into(), "9".into())), "{facts:?}");
    assert!(
        !f.engine
            .lock()
            .unwrap()
            .seen
            .iter()
            .any(|s| s.starts_with("/ace/getstream") || s.starts_with("/ace/cmd"))
    );
}

#[tokio::test]
async fn vk_play_relays_everything_through_the_gateway() {
    let f = fixture(true).await;
    let (_, r) = post(
        &format!("{}/v1/match", f.url),
        json!({"rule": "vk", "inputs": ["https://sport.vkvideo.ru/live-100001_200002?list=0a1b2c3d4e5f6a7b8c"]}),
    )
    .await;
    let m = &r["results"][0];
    assert_eq!(m["ref"], "https://vk.com/video-100001_200002?list=0a1b2c3d4e5f6a7b8c");
    let play = m["play"].as_str().unwrap();
    // Behind a reverse-proxy prefix the play path is appended below the base; here the base is the root.
    let master_url = format!("{}/{play}", f.url);
    let res = reqwest::get(&master_url).await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "application/vnd.apple.mpegurl");
    let master = res.text().await.unwrap();
    assert!(!master.contains(&f.cdn_url), "no CDN URL reaches the player: {master}");
    let child = master.lines().find(|l| l.starts_with("s/")).unwrap();
    assert!(child.ends_with(".m3u8"), "{master}");

    let child_url = url::Url::parse(&master_url).unwrap().join(child).unwrap();
    let media = reqwest::get(child_url.clone()).await.unwrap().text().await.unwrap();
    assert!(!media.contains(&f.cdn_url), "{media}");
    assert!(media.contains("#EXT-X-KEY:METHOD=AES-128,URI=\""), "{media}");
    let seg = media.lines().find(|l| l.ends_with(".ts")).unwrap();
    let seg_url = child_url.join(seg).unwrap();
    let body = reqwest::get(seg_url.clone()).await.unwrap().bytes().await.unwrap();
    assert_eq!(&body[..], b"0123456789");
    // Range passes through; the upstream's partial answer is kept.
    let r = reqwest::Client::new()
        .get(seg_url.clone())
        .header("range", "bytes=2-4")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(r.headers()["content-range"], "bytes 2-4/10");
    assert_eq!(f.cdn.lock().unwrap().ranges, ["bytes=2-4"]);
    let r = reqwest::Client::new()
        .get(seg_url.clone())
        .header("range", "bytes=0-1,4-5")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 416);
    // The CDN only ever saw the lookup's user agent.
    assert!(
        f.cdn
            .lock()
            .unwrap()
            .user_agents
            .iter()
            .all(|u| u == "Mozilla/5.0 Chrome/140")
    );

    // Expired URLs: the client is told to reopen; reopening works again once the CDN answers.
    f.cdn.lock().unwrap().expired = true;
    let r = reqwest::get(seg_url).await.unwrap();
    assert_eq!(r.status(), 410);
    f.cdn.lock().unwrap().expired = false;
    assert_eq!(reqwest::get(&master_url).await.unwrap().status(), 200);
}

#[tokio::test]
async fn vk_observe_and_check_report_content_state() {
    let f = fixture(false).await;
    let r#ref = "https://vk.com/video-1_2";
    let (_, r) = post(&format!("{}/v1/check", f.url), json!({"rule": "vk", "ref": r#ref})).await;
    assert_eq!(
        (r["outcome"].as_str(), r["content_state"].as_str()),
        (Some("ok"), Some("live")),
        "{r}"
    );
    f.cdn.lock().unwrap().ended = true;
    let (_, r) = post(&format!("{}/v1/observe", f.url), json!({"rule": "vk", "refs": [r#ref]})).await;
    let o = &r["results"][0];
    assert_eq!(o["content_state"], "recording", "{o}");
    assert_eq!(o["title"], "Match");
}

#[tokio::test]
async fn public_fetches_refuse_private_addresses() {
    // No allow list: the fake CDN on 127.0.0.1 is private, so the relay refuses it.
    let cdn_state = CdnState::default();
    let cdn_url = cdn(cdn_state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let yt = fake_yt_dlp(dir.path(), &cdn_url, true);
    let cfg = Config::parse(&format!("[vk]\n[tools]\nyt_dlp = \"{}\"\n", yt.display())).unwrap();
    let g = lineup_gateway::Gateway::new(&cfg, ProbeTiming::default())
        .await
        .unwrap();
    let url = serve(lineup_gateway::router(g)).await;
    let r = reqwest::get(format!("{url}/play/vk.m3u8?ref=https%3A%2F%2Fvk.com%2Fvideo-1_2"))
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    assert!(r.text().await.unwrap().contains("not a public address"));
    assert!(
        cdn_state.lock().unwrap().user_agents.is_empty(),
        "the CDN was never contacted"
    );
    let r = reqwest::get(format!("{url}/play/vk.m3u8?ref=https%3A%2F%2Fexample.com%2Fx"))
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}

#[tokio::test]
async fn a_cancelled_check_still_owns_the_engine_until_its_session_stops() {
    let f = fixture(true).await;
    let url = format!("{}/v1/check", f.url);
    let req = json!({"rule": "acestream", "ref": format!("acestream://{ACE}")});
    // The first caller goes away while its probe runs; a second check comes right after.
    let first = tokio::spawn({
        let (url, req) = (url.clone(), req.clone());
        async move { post(&url, req).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    first.abort();
    let (_, r) = post(&url, req).await;
    assert_eq!(r["outcome"], "dead", "{r}");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let e = f.engine.lock().unwrap();
    assert_eq!(e.max_open, 1, "probes never overlap");
    assert_eq!(e.open, 0, "every probe session was stopped");
}

#[tokio::test]
async fn without_acexy_there_are_no_checks_but_names_still_show() {
    let f = fixture_with(true, false).await;
    let info: Value = reqwest::get(format!("{}/v1/info", f.url))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["rules"][0]["capabilities"], json!(["observe"]));
    for force in [false, true] {
        let (_, r) = post(
            &format!("{}/v1/check", f.url),
            json!({"rule": "acestream", "ref": format!("acestream://{ACE}"), "force": force}),
        )
        .await;
        assert_eq!(
            (r["outcome"].as_str(), r["reason"].as_str()),
            (Some("skipped"), Some("guard_unavailable"))
        );
    }
    assert!(
        !f.engine
            .lock()
            .unwrap()
            .seen
            .iter()
            .any(|s| s.starts_with("/ace/getstream"))
    );
    let (_, r) = post(
        &format!("{}/v1/observe", f.url),
        json!({"rule": "acestream", "refs": [format!("acestream://{ACE}")]}),
    )
    .await;
    assert_eq!(
        r["results"][0]["title"], "Arena 1 Bar HD [DE]",
        "the engine's own name: {r}"
    );
}

#[tokio::test]
async fn expired_playlists_refresh_once_onto_the_same_variant() {
    let f = fixture(false).await;
    f.cdn.lock().unwrap().generation = 1;
    let master_url = format!("{}/play/vk.m3u8?ref=https%3A%2F%2Fvk.com%2Fvideo-1_2", f.url);
    let master = reqwest::get(&master_url).await.unwrap().text().await.unwrap();
    let child = url::Url::parse(&master_url)
        .unwrap()
        .join(master.lines().find(|l| l.starts_with("s/")).unwrap())
        .unwrap();
    assert_eq!(reqwest::get(child.clone()).await.unwrap().status(), 200);

    // The CDN rejects the old signature with 400: one refresh onto the same variant.
    f.cdn.lock().unwrap().generation = 2;
    let r = reqwest::get(child.clone()).await.unwrap();
    assert_eq!(r.status(), 200, "refreshed");
    assert!(r.text().await.unwrap().contains(".ts"));
    // Only once per session.
    f.cdn.lock().unwrap().generation = 3;
    assert_eq!(reqwest::get(child.clone()).await.unwrap().status(), 410);

    // A new session whose variant changed meanwhile is not moved onto another one.
    let master = reqwest::get(&master_url).await.unwrap().text().await.unwrap();
    let child = url::Url::parse(&master_url)
        .unwrap()
        .join(master.lines().find(|l| l.starts_with("s/")).unwrap())
        .unwrap();
    {
        let mut c = f.cdn.lock().unwrap();
        c.generation = 4;
        c.bandwidth = 2000;
    }
    assert_eq!(reqwest::get(child).await.unwrap().status(), 410);
}

#[tokio::test]
async fn an_ended_playlist_from_a_live_lookup_is_not_a_recording() {
    let f = fixture(true).await;
    f.cdn.lock().unwrap().ended = true;
    let (_, r) = post(
        &format!("{}/v1/check", f.url),
        json!({"rule": "vk", "ref": "https://vk.com/video-1_2"}),
    )
    .await;
    assert_eq!(
        (r["outcome"].as_str(), r["content_state"].as_str(), r["reason"].as_str()),
        (Some("unknown"), Some("ended_processing"), Some("recording_not_ready")),
        "{r}"
    );
}

#[tokio::test]
async fn a_refresh_never_splices_and_concurrent_expiries_share_one_refresh() {
    let f = fixture(false).await;
    f.cdn.lock().unwrap().generation = 1;
    let master_url = format!("{}/play/vk.m3u8?ref=https%3A%2F%2Fvk.com%2Fvideo-1_3", f.url);
    let child = |master: &str| {
        url::Url::parse(&master_url)
            .unwrap()
            .join(master.lines().find(|l| l.starts_with("s/")).unwrap())
            .unwrap()
    };
    let c = child(&reqwest::get(&master_url).await.unwrap().text().await.unwrap());
    assert_eq!(reqwest::get(c.clone()).await.unwrap().status(), 200);
    // Several requests hit the expired URL at once: one refresh, all served, the session survives.
    f.cdn.lock().unwrap().generation = 2;
    let all = futures_util::future::join_all((0..4).map(|_| reqwest::get(c.clone()))).await;
    for r in all {
        assert_eq!(r.unwrap().status(), 200);
    }
    assert_eq!(
        reqwest::get(c).await.unwrap().status(),
        200,
        "the refreshed session still plays"
    );

    // A growing playlist whose refresh comes back finished is not continued (no splicing).
    let c = child(&reqwest::get(&master_url).await.unwrap().text().await.unwrap());
    assert_eq!(reqwest::get(c.clone()).await.unwrap().status(), 200);
    {
        let mut cdn = f.cdn.lock().unwrap();
        cdn.generation = 3;
        cdn.ended = true;
    }
    assert_eq!(reqwest::get(c).await.unwrap().status(), 410);
}
