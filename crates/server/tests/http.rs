use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use lineup_server::{Store, http::router};
use serde_json::{Value, json};
use tower::ServiceExt;

const ACE: &str = "41169a3f280a5537b0611b775e926852d372743d";

async fn app() -> axum::Router {
    router(Arc::new(Store::open_memory().await.unwrap()))
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(b) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, headers, v)
}

#[tokio::test]
async fn crud_and_errors() {
    let app = app().await;
    let (s, _, _) = call(&app, "POST", "/api/v1/playlists", Some(json!({"slug": "jf"}))).await;
    assert_eq!(s, StatusCode::CREATED);

    let (s, _, ch) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "Alpha", "input": format!("acestream://{ACE}"), "playlists": ["jf"]})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{ch}");
    assert_eq!(ch["source"], json!({"kind": "acestream", "id": ACE}));

    let (s, _, e) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "Dup", "input": ACE})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(e["error"]["code"], "conflict");

    let (s, _, e) = call(&app, "POST", "/api/v1/channels", Some(json!({"nam": 1}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(e["error"]["code"], "invalid");

    let (s, _, e) = call(&app, "GET", "/api/v1/channels/999", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(e["error"]["code"], "not_found");

    // null clears, absent keeps
    let id = ch["id"].as_i64().unwrap();
    let (_, _, c) = call(
        &app,
        "PATCH",
        &format!("/api/v1/channels/{id}"),
        Some(json!({"tvg_id": "x.uk"})),
    )
    .await;
    assert_eq!(c["tvg_id"], "x.uk");
    let (_, _, c) = call(
        &app,
        "PATCH",
        &format!("/api/v1/channels/{id}"),
        Some(json!({"tvg_id": null})),
    )
    .await;
    assert_eq!(c["tvg_id"], Value::Null);
    assert_eq!(c["name"], "Alpha");

    let (s, _, _) = call(&app, "DELETE", &format!("/api/v1/channels/{id}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn m3u_output_and_etag() {
    let app = app().await;
    let (s, _, i) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/acestream",
        Some(json!({"kind": "acestream", "engine_url": "http://10.0.0.9:6878"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{i}");
    call(&app, "POST", "/api/v1/playlists", Some(json!({"slug": "jf"}))).await;
    call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "Alpha", "input": ACE, "playlists": ["jf"]})),
    )
    .await;

    let res = app
        .clone()
        .oneshot(Request::get("/p/jf.m3u").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "audio/x-mpegurl; charset=utf-8");
    let etag = res.headers()[header::ETAG].clone();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains(&format!("/ace/getstream?id={ACE}")));

    let res = app
        .clone()
        .oneshot(
            Request::get("/p/jf.m3u8")
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);

    // a playback URL change changes the body, so the old ETag no longer matches
    call(
        &app,
        "PATCH",
        "/api/v1/integrations/acestream",
        Some(json!({"kind": "acestream", "acexy_url": "http://10.0.0.9:8080"})),
    )
    .await;
    let res = app
        .clone()
        .oneshot(
            Request::get("/p/jf.m3u")
                .header(header::IF_NONE_MATCH, etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let (s, _, _) = call(&app, "GET", "/p/missing.m3u", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = call(&app, "GET", "/p/jf.txt", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn import_endpoint_and_routing() {
    let app = app().await;
    let text = format!("#EXTM3U\n#EXTINF:-1 group-title=\"Sports\",Alpha\nhttp://h:8080/ace/getstream?id={ACE}\n");
    let (s, _, r) = call(
        &app,
        "POST",
        "/api/v1/import",
        Some(json!({"m3u_text": text, "playlist_slug": "jf", "dry_run": true})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["entries"][0]["action"], "create");
    let (_, _, l) = call(&app, "GET", "/api/v1/playlists", None).await;
    assert_eq!(l, json!([]));

    let (s, _, _) = call(&app, "POST", "/api/v1/import", Some(json!({"playlist_slug": "jf"}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    let (s, _, e) = call(&app, "GET", "/api/v1/nope", None).await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );
    let (s, _, _) = call(&app, "GET", "/settings/anything", None).await;
    assert_eq!(s, StatusCode::OK, "SPA fallback");
    let (s, _, _) = call(&app, "GET", "/missing.js", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, doc) = call(&app, "GET", "/api/openapi.json", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(doc["paths"]["/channels/{id}"].is_object(), "{}", doc["paths"]);
}

#[tokio::test]
async fn path_and_query_errors_use_the_json_envelope() {
    let app = app().await;
    for uri in [
        "/api/v1/channels?kind=bogus",
        "/api/v1/channels/not-an-id",
        "/api/v1/playlists/x/items/abc",
    ] {
        let method = if uri.ends_with("abc") { "DELETE" } else { "GET" };
        let (s, _, e) = call(&app, method, uri, None).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{uri}: {e}");
        assert_eq!(e["error"]["code"], "invalid", "{uri}");
    }
    for uri in ["/api", "/p", "/api/", "/p/"] {
        let (s, _, _) = call(&app, "GET", uri, None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
    }
}

async fn big_body_server() -> String {
    use axum::routing::get;
    let big = "#EXTM3U\n".to_string() + &"#".repeat(lineup_server::http::IMPORT_MAX_BYTES + 10);
    let big2 = big.clone();
    let app = axum::Router::new()
        .route("/sized.m3u", get(move || async move { big.clone() }))
        .route(
            "/chunked.m3u",
            get(move || async move {
                let chunks: Vec<Result<String, std::io::Error>> = big2
                    .as_bytes()
                    .chunks(64 * 1024)
                    .map(|c| Ok(String::from_utf8_lossy(c).into_owned()))
                    .collect();
                Body::from_stream(futures_util::stream::iter(chunks))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn import_by_url_is_size_capped() {
    let base = big_body_server().await;
    let app = app().await;
    for path in ["sized.m3u", "chunked.m3u"] {
        let (s, _, e) = call(
            &app,
            "POST",
            "/api/v1/import",
            Some(json!({"url": format!("{base}/{path}"), "playlist_slug": "jf"})),
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{path}: {e}");
        assert!(e["error"]["message"].as_str().unwrap().contains("larger than"), "{e}");
    }
    let (_, _, l) = call(&app, "GET", "/api/v1/playlists", None).await;
    assert_eq!(l, json!([]), "nothing was created");
}

#[tokio::test]
async fn event_stream() {
    let store = Arc::new(Store::open_memory().await.unwrap());
    let app = router(store.clone());
    let res = app
        .clone()
        .oneshot(Request::builder().uri("/api/v1/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut body = res.into_body();
    let mut next = async || -> String {
        let f = tokio::time::timeout(std::time::Duration::from_secs(5), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        String::from_utf8(f.into_data().unwrap().to_vec()).unwrap()
    };
    assert!(next().await.starts_with("event: ready\n"));
    call(&app, "POST", "/api/v1/playlists", Some(json!({"slug": "jf"}))).await;
    let ev = next().await;
    assert!(
        ev.starts_with("event: playlist.changed\ndata: {\"slugs\":[\"jf\"]}"),
        "{ev}"
    );
}

async fn fake_services() -> String {
    use axum::routing::get;
    let app = axum::Router::new()
        .route("/ace/status", get(|| async { axum::Json(json!({ "streams": 0 })) }))
        .route(
            "/webui/api/service",
            get(
                |q: axum::extract::Query<std::collections::HashMap<String, String>>| async move {
                    assert_eq!(q.0["method"], "get_version");
                    axum::Json(
                        json!({ "result": { "code": 3, "platform": "linux", "version": "3.2.3" }, "error": null }),
                    )
                },
            ),
        )
        .route(
            "/slow/ace/status",
            get(|| async {
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                "late"
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn integration_connection_test() {
    let base = fake_services().await;
    let app = app().await;
    let t = |component: &str, url: Option<String>| json!({ "component": component, "url": url });
    let test = |kind: &str| format!("/api/v1/integrations/{kind}/test");

    let (s, _, r) = call(&app, "POST", &test("acestream"), Some(t("engine", Some(base.clone())))).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["ok"], true);
    assert_eq!(r["version"], "3.2.3");
    assert_eq!(r["url"], format!("{base}/webui/api/service?method=get_version"));

    let (_, _, r) = call(
        &app,
        "POST",
        &test("acestream"),
        Some(t("acexy", Some(format!("{base}/")))),
    )
    .await;
    assert_eq!(
        (r["ok"].clone(), r["url"].clone(), r["active_streams"].clone()),
        (json!(true), json!(format!("{base}/ace/status")), json!(0))
    );

    // The VK proxy root answers 404 here: something listens, and that is all it shows.
    let (_, _, r) = call(&app, "POST", &test("vk"), Some(t("proxy", Some(base.clone())))).await;
    assert_eq!((r["ok"].clone(), r["http_status"].clone()), (json!(true), json!(404)));

    // Nothing listening, or too slow: a negative result, not an error.
    let (s, _, r) = call(
        &app,
        "POST",
        &test("acestream"),
        Some(t("engine", Some("http://127.0.0.1:1".into()))),
    )
    .await;
    assert_eq!((s, r["ok"].clone()), (StatusCode::OK, json!(false)));
    let (_, _, r) = call(
        &app,
        "POST",
        &test("acestream"),
        Some(t("acexy", Some(format!("{base}/slow")))),
    )
    .await;
    assert_eq!(r["ok"], false);
    assert!(r["detail"].as_str().unwrap().contains("timed out"), "{r}");

    // An invalid URL, a component of another integration, or nothing saved to test: 422.
    for (kind, body) in [
        ("vk", t("proxy", Some("ftp://x".into()))),
        ("vk", t("engine", Some(base.clone()))),
        ("direct", t("proxy", Some(base.clone()))),
        ("acestream", json!({ "component": "acexy" })),
    ] {
        let (s, _, r) = call(&app, "POST", &test(kind), Some(body)).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{kind}: {r}");
    }

    // Without `url` the saved value is used; the status tests every configured component.
    call(
        &app,
        "PATCH",
        "/api/v1/integrations/acestream",
        Some(json!({ "kind": "acestream", "engine_url": base, "acexy_url": base })),
    )
    .await;
    let (_, _, r) = call(&app, "POST", &test("acestream"), Some(json!({ "component": "acexy" }))).await;
    assert_eq!(r["url"], format!("{base}/ace/status"));
    let (s, _, st) = call(&app, "GET", "/api/v1/integrations/acestream/status", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(st["configured"], true);
    let parts: Vec<_> = st["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["component"].clone(), c["ok"].clone()))
        .collect();
    assert_eq!(parts, [(json!("engine"), json!(true)), (json!("acexy"), json!(true))]);
    let (_, _, st) = call(&app, "GET", "/api/v1/integrations/vk/status", None).await;
    assert_eq!(
        (st["configured"].clone(), st["components"].clone()),
        (json!(false), json!([]))
    );
    let (s, _, _) = call(&app, "GET", "/api/v1/integrations/bogus/status", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn integrations_api() {
    let app = app().await;
    let (s, _, list) = call(&app, "GET", "/api/v1/integrations", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        list,
        json!([
            {"kind": "acestream", "configured": false, "channel_count": 0, "revision": 0,
             "config": {"kind": "acestream", "engine_url": "", "acexy_url": null, "probe_interval_hours": 0},
             "checks": {"available": false, "reason": "the Acestream engine is not configured"},
             "updated_at": list[0]["updated_at"]},
            {"kind": "vk", "configured": false, "channel_count": 0, "revision": 0,
             "config": {"kind": "vk", "url": ""},
             "checks": {"available": false, "reason": "this integration has no channel checks"},
             "updated_at": list[1]["updated_at"]},
            {"kind": "direct", "configured": true, "channel_count": 0, "revision": 0,
             "config": {"kind": "direct"},
             "checks": {"available": false, "reason": "this integration has no channel checks"},
             "updated_at": list[2]["updated_at"]},
        ])
    );
    // The kind in the body must match the route; unknown fields of another kind are not accepted.
    let (s, _, _) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/vk",
        Some(json!({"kind": "acestream", "engine_url": "http://e"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, i) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/acestream",
        Some(json!({"kind": "acestream", "engine_url": "http://e:6878/", "probe_interval_hours": 6})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(i["config"]["engine_url"], "http://e:6878");
    assert_eq!(
        (i["configured"].clone(), i["revision"].clone()),
        (json!(true), json!(1))
    );
    assert_eq!(i["checks"]["available"], false, "no acexy");
    let (_, _, i) = call(&app, "GET", "/api/v1/integrations/acestream", None).await;
    assert_eq!(i["config"]["probe_interval_hours"], 6);

    // A channel of an unconfigured integration: no stream URL, and no checks.
    let (s, _, ch) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "V", "input": "https://vk.com/video-1_2"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(
        (ch["stream_url"].clone(), ch["configured"].clone()),
        (Value::Null, json!(false))
    );
    let (_, _, r) = call(
        &app,
        "POST",
        "/api/v1/resolve",
        Some(json!({"input": "https://vk.com/video-1_3"})),
    )
    .await;
    assert_eq!(
        (r["stream_url"].clone(), r["configured"].clone()),
        (Value::Null, json!(false))
    );
    let (s, _, r) = call(&app, "POST", "/api/v1/probe", Some(json!({"ids": []}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{r}");
    assert!(r["error"]["message"].as_str().unwrap().contains("acexy"), "{r}");
}

#[tokio::test]
async fn settings_changes_are_announced() {
    let store = Arc::new(Store::open_memory().await.unwrap());
    let mut events = store.subscribe();
    let patch = |v: Value| serde_json::from_value::<api_types::SettingsPatch>(v).unwrap();
    store
        .patch_settings(patch(json!({ "epg_refresh_hours": 12 })))
        .await
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        lineup_server::Event::SettingsChanged {
            keys: vec!["epg_refresh_hours".into()],
            revision: 1
        }
    );
    assert_eq!(store.settings().await.unwrap().revision, 1);
    // Same values again: nothing, and the revision stays.
    let same = store
        .patch_settings(patch(json!({ "epg_refresh_hours": 12 })))
        .await
        .unwrap();
    assert!(events.try_recv().is_err());
    assert_eq!(same.revision, 1);
    // The revision is read-only: a client cannot set it.
    let s = store
        .patch_settings(patch(json!({ "epg_refresh_hours": 3, "revision": 99 })))
        .await
        .unwrap();
    assert_eq!(s.revision, 2);
    // Service URLs are no longer settings.
    let app = router(store);
    let (s, _, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        Some(json!({ "acexy_base": "http://a" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "unknown fields are ignored");
    let (_, _, v) = call(&app, "GET", "/api/v1/settings", None).await;
    assert_eq!(v, json!({ "epg_refresh_hours": 3, "revision": 2 }));
}

#[tokio::test]
async fn vk_test_needs_only_headers() {
    // Headers of a 404 arrive, then the body breaks off: something answered, and that is enough.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await;
        let _ = sock
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 100\r\nLocation: http://127.0.0.1:1/\r\n\r\nx")
            .await;
        drop(sock);
    });
    let app = app().await;
    let (_, _, r) = call(
        &app,
        "POST",
        "/api/v1/integrations/vk/test",
        Some(json!({ "component": "proxy", "url": format!("http://{addr}") })),
    )
    .await;
    assert_eq!(
        (r["ok"].clone(), r["http_status"].clone()),
        (json!(true), json!(404)),
        "{r}"
    );
}
