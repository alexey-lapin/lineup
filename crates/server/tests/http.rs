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

    // a base-URL change changes the body, so the old ETag no longer matches
    call(
        &app,
        "PATCH",
        "/api/v1/settings",
        Some(json!({"acexy_base": "http://10.0.0.9:8080"})),
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
async fn settings_connection_test() {
    let base = fake_services().await;
    let app = app().await;
    let t = |target: &str, b: Option<String>| json!({ "target": target, "base": b });

    let (s, _, r) = call(
        &app,
        "POST",
        "/api/v1/settings/test",
        Some(t("engine", Some(base.clone()))),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["ok"], true);
    assert_eq!(r["version"], "3.2.3");
    assert_eq!(r["url"], format!("{base}/webui/api/service?method=get_version"));

    let (_, _, r) = call(
        &app,
        "POST",
        "/api/v1/settings/test",
        Some(t("acexy", Some(format!("{base}/")))),
    )
    .await;
    assert_eq!(
        (r["ok"].clone(), r["url"].clone()),
        (json!(true), json!(format!("{base}/ace/status")))
    );

    // The VK proxy root answers 404 here: something listens, and that is all it shows.
    let (_, _, r) = call(&app, "POST", "/api/v1/settings/test", Some(t("vk", Some(base.clone())))).await;
    assert_eq!((r["ok"].clone(), r["http_status"].clone()), (json!(true), json!(404)));

    // Nothing listening, or too slow: a negative result, not an error.
    let (s, _, r) = call(
        &app,
        "POST",
        "/api/v1/settings/test",
        Some(t("engine", Some("http://127.0.0.1:1".into()))),
    )
    .await;
    assert_eq!((s, r["ok"].clone()), (StatusCode::OK, json!(false)));
    let (_, _, r) = call(
        &app,
        "POST",
        "/api/v1/settings/test",
        Some(t("acexy", Some(format!("{base}/slow")))),
    )
    .await;
    assert_eq!(r["ok"], false);
    assert!(r["detail"].as_str().unwrap().contains("timed out"), "{r}");

    // An invalid URL is a 422; without `base` the saved value is used.
    let (s, _, _) = call(
        &app,
        "POST",
        "/api/v1/settings/test",
        Some(t("vk", Some("ftp://x".into()))),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, _, r) = call(
        &app,
        "POST",
        "/api/v1/settings/test",
        Some(json!({ "target": "acexy" })),
    )
    .await;
    assert_eq!(r["url"], "http://127.0.0.1:8080/ace/status");
}

#[tokio::test]
async fn settings_changes_are_announced() {
    let store = Arc::new(Store::open_memory().await.unwrap());
    let mut events = store.subscribe();
    let patch = |v: Value| serde_json::from_value::<api_types::SettingsPatch>(v).unwrap();
    store
        .patch_settings(patch(
            json!({ "engine_base": "http://e:6878", "probe_interval_hours": 6 }),
        ))
        .await
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        lineup_server::Event::SettingsChanged {
            keys: vec!["engine_base".into(), "probe_interval_hours".into()],
            revision: 1
        }
    );
    assert_eq!(store.settings().await.unwrap().revision, 1);
    // Same values again: nothing, and the revision stays.
    let same = store
        .patch_settings(patch(json!({ "engine_base": "http://e:6878/" })))
        .await
        .unwrap();
    assert!(events.try_recv().is_err());
    assert_eq!(same.revision, 1);
    // The revision is read-only: a client cannot set it.
    let s = store
        .patch_settings(patch(json!({ "vk_base": "http://v:8099", "revision": 99 })))
        .await
        .unwrap();
    assert_eq!(s.revision, 2);
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
        "/api/v1/settings/test",
        Some(json!({ "target": "vk", "base": format!("http://{addr}") })),
    )
    .await;
    assert_eq!(
        (r["ok"].clone(), r["http_status"].clone()),
        (json!(true), json!(404)),
        "{r}"
    );
}
