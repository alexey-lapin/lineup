use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use lineup_server::testing::{Failure, StubGateway};
use lineup_server::{Store, http::router};
use serde_json::{Value, json};
use tower::ServiceExt;

const ACE: &str = "41169a3f280a5537b0611b775e926852d372743d";
const ACE2: &str = "816b61ec91d5d4da0bc477a42cd6ee407fc07d44";
const DIRECT: i64 = 1;

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

/// Create a gateway route for `rule` on the stub; returns its id.
async fn gateway_route(app: &axum::Router, gw: &StubGateway, name: &str, rule: &str) -> i64 {
    let (s, _, r) = call(
        app,
        "POST",
        "/api/v1/routes",
        Some(json!({"name": name, "config": {"kind": "gateway", "base_url": gw.url, "rule": rule}})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    r["id"].as_i64().unwrap()
}

async fn m3u(app: &axum::Router, slug: &str) -> String {
    let res = app
        .clone()
        .oneshot(Request::get(format!("/p/{slug}.m3u")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&body).into_owned()
}

#[tokio::test]
async fn crud_and_errors() {
    let gw = StubGateway::start().await;
    let app = app().await;
    let ace = gateway_route(&app, &gw, "Acestream", "acestream").await;
    let (s, _, _) = call(&app, "POST", "/api/v1/playlists", Some(json!({"slug": "jf"}))).await;
    assert_eq!(s, StatusCode::CREATED);

    let (s, _, ch) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "Alpha", "route_id": ace, "input": format!("acestream://{ACE}"), "playlists": ["jf"]})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{ch}");
    assert_eq!(ch["source"], format!("acestream://{ACE}"));
    assert_eq!(ch["identity"], format!("ace:{ACE}"));
    assert_eq!(ch["play_url"], format!("http://acexy:8080/ace/getstream?id={ACE}"));
    assert_eq!(
        (ch["route_id"].as_i64(), ch["route_name"].as_str()),
        (Some(ace), Some("Acestream"))
    );

    // Another input form of the same identity is a duplicate.
    let (s, _, e) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "Dup", "route_id": ace, "input": ACE.to_uppercase()})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["error"]["code"], "conflict");

    // Input the route does not accept.
    let (s, _, e) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "X", "route_id": ace, "input": "https://example.com/live.m3u8"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
    assert!(
        e["error"]["message"].as_str().unwrap().contains("not an Acestream"),
        "{e}"
    );
    // The same URL on the direct route is fine.
    let (s, _, d) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "Direct", "route_id": DIRECT, "input": " https://example.com/live.m3u8 "})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{d}");
    assert_eq!(d["play_url"], "https://example.com/live.m3u8");

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

    // A new input is revalidated with the route.
    let (s, _, c) = call(
        &app,
        "PATCH",
        &format!("/api/v1/channels/{id}"),
        Some(json!({"input": ACE2})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["identity"], format!("ace:{ACE2}"));

    // Channels filter by route.
    let (_, _, l) = call(&app, "GET", &format!("/api/v1/channels?route_id={ace}"), None).await;
    assert_eq!(l.as_array().unwrap().len(), 1);

    let (s, _, _) = call(&app, "DELETE", &format!("/api/v1/channels/{id}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn gateway_failures_fail_writes_and_keep_output() {
    let gw = StubGateway::start().await;
    let app = app().await;
    let ace = gateway_route(&app, &gw, "Acestream", "acestream").await;
    call(&app, "POST", "/api/v1/playlists", Some(json!({"slug": "jf"}))).await;
    let (s, _, _) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "Alpha", "route_id": ace, "input": ACE, "playlists": ["jf"]})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let before = m3u(&app, "jf").await;

    gw.fail(Failure::ServerError);
    let (s, _, e) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "Beta", "route_id": ace, "input": ACE2})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{e}");
    assert_eq!(e["error"]["code"], "gateway_error");
    let text = format!("#EXTM3U\n#EXTINF:-1,Beta\n{ACE2}\n");
    let (s, _, e) = call(
        &app,
        "POST",
        "/api/v1/import",
        Some(json!({"m3u_text": text, "playlist_slug": "new", "route_id": ace})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{e}");
    let (_, _, l) = call(&app, "GET", "/api/v1/playlists", None).await;
    assert_eq!(l.as_array().unwrap().len(), 1, "the import wrote nothing: {l}");
    assert_eq!(m3u(&app, "jf").await, before, "stored output is unaffected");

    gw.fail(Failure::WrongContract);
    let (s, _, e) = call(
        &app,
        "POST",
        "/api/v1/resolve",
        Some(json!({"route_id": ace, "input": ACE2})),
    )
    .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("contract_mismatch"))
    );

    // A gateway that does not answer at all.
    let (_, _, dead) = call(
        &app,
        "POST",
        "/api/v1/routes",
        Some(json!({"name": "Dead", "config": {"kind": "gateway", "base_url": "http://127.0.0.1:1", "rule": "acestream"}})),
    )
    .await;
    let (s, _, e) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "Beta", "route_id": dead["id"], "input": ACE2})),
    )
    .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("gateway_unreachable"))
    );
}

#[tokio::test]
async fn resolve_suggests_and_reports_duplicates() {
    let gw = StubGateway::start().await;
    gw.set_title(ACE, "Arena 1");
    let app = app().await;
    let ace = gateway_route(&app, &gw, "Acestream", "acestream").await;
    let vk = gateway_route(&app, &gw, "VK", "vk").await;

    let (s, _, r) = call(
        &app,
        "POST",
        "/api/v1/resolve",
        Some(json!({"route_id": ace, "input": ACE})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["suggestion"]["name"], "Arena 1");
    assert_eq!(r["hints"]["title"], "Arena 1");
    assert_eq!(r["duplicate_of"], Value::Null);

    let (_, _, r) = call(
        &app,
        "POST",
        "/api/v1/resolve",
        Some(json!({"route_id": vk, "input": "https://vkvideo.ru/video-1_2?list=a"})),
    )
    .await;
    assert_eq!(r["source"], "https://vk.com/video-1_2?list=a");
    assert_eq!(
        r["suggestion"]["name"], "vk:-1_2",
        "no title: the identity is the placeholder"
    );
    assert_eq!(
        (
            r["suggestion"]["group_title"].as_str(),
            r["suggestion"]["is_event"].as_bool()
        ),
        (Some("VK"), Some(true))
    );
    assert!(
        r["play_url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{}/play/vk.m3u8?ref=", gw.url)),
        "{r}"
    );

    let (_, _, c) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "Match", "route_id": vk, "input": "https://vk.com/video-1_2?list=a"})),
    )
    .await;
    // The same video with a new list token: a duplicate whose stored source differs.
    let (_, _, r) = call(
        &app,
        "POST",
        "/api/v1/resolve",
        Some(json!({"route_id": vk, "input": "https://vk.com/video-1_2?list=b"})),
    )
    .await;
    assert_eq!(r["duplicate_of"], c["id"]);
    assert_eq!(r["duplicate_source"], "https://vk.com/video-1_2?list=a");
    // Updating it is a channel edit with the new input.
    let (_, _, c2) = call(
        &app,
        "PATCH",
        &format!("/api/v1/channels/{}", c["id"]),
        Some(json!({"input": "https://vk.com/video-1_2?list=b"})),
    )
    .await;
    assert_eq!(c2["source"], "https://vk.com/video-1_2?list=b");
}

#[tokio::test]
async fn routes_api() {
    let gw = StubGateway::start().await;
    let app = app().await;
    let (_, _, l) = call(&app, "GET", "/api/v1/routes", None).await;
    assert_eq!(l.as_array().unwrap().len(), 1);
    assert_eq!(l[0]["config"], json!({"kind": "direct"}));

    // Test a draft config before saving it.
    let (s, _, t) = call(
        &app,
        "POST",
        "/api/v1/routes/test",
        Some(json!({"config": {"kind": "gateway", "base_url": gw.url, "rule": "vk"}, "input": "https://vk.com/video1_2"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["ok"], true, "{t}");
    assert_eq!(t["gateway"]["rules"].as_array().unwrap().len(), 2);
    assert_eq!(t["sample"]["identity"], "vk:1_2");
    let (_, _, t) = call(
        &app,
        "POST",
        "/api/v1/routes/test",
        Some(json!({"config": {"kind": "gateway", "base_url": gw.url, "rule": "nope"}})),
    )
    .await;
    assert_eq!(t["ok"], false);
    assert!(t["detail"].as_str().unwrap().contains("no rule"), "{t}");
    let (_, _, t) = call(
        &app,
        "POST",
        "/api/v1/routes/test",
        Some(json!({"config": {"kind": "gateway", "base_url": "http://127.0.0.1:1", "rule": "vk"}})),
    )
    .await;
    assert_eq!(t["ok"], false);
    let (s, _, _) = call(
        &app,
        "POST",
        "/api/v1/routes/test",
        Some(json!({"config": {"kind": "gateway", "base_url": "ftp://x", "rule": "vk"}})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    let id = gateway_route(&app, &gw, "Acestream", "acestream").await;
    let (_, _, st) = call(&app, "GET", &format!("/api/v1/routes/{id}/status"), None).await;
    assert_eq!(
        (st["ok"].as_bool(), st["pending_channels"].as_i64()),
        (Some(true), Some(0)),
        "{st}"
    );
    assert_eq!(st["rule"]["capabilities"], json!(["observe", "check"]));

    // The rule can't change; only name, enabled and base_url can.
    let (s, _, r) = call(
        &app,
        "PATCH",
        &format!("/api/v1/routes/{id}"),
        Some(json!({"name": "Ace", "enabled": false})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!((r["name"].as_str(), r["enabled"].as_bool()), (Some("Ace"), Some(false)));
    let (s, _, _) = call(&app, "PATCH", "/api/v1/routes/1", Some(json!({"base_url": "http://x"}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "direct routes have no base URL");

    let (s, _, rep) = call(&app, "POST", &format!("/api/v1/routes/{id}/resync"), None).await;
    assert_eq!(s, StatusCode::OK, "{rep}");
    assert_eq!(rep["complete"], true);
    let (s, _, _) = call(&app, "POST", "/api/v1/routes/1/resync", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    let (s, _, _) = call(&app, "DELETE", &format!("/api/v1/routes/{id}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn m3u_output_and_etag_follow_route_changes() {
    let gw = StubGateway::start().await;
    let app = app().await;
    let ace = gateway_route(&app, &gw, "Acestream", "acestream").await;
    call(&app, "POST", "/api/v1/playlists", Some(json!({"slug": "jf"}))).await;
    call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "Alpha", "route_id": ace, "input": ACE, "playlists": ["jf"]})),
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
    assert!(String::from_utf8_lossy(&body).contains(&format!("http://acexy:8080/ace/getstream?id={ACE}")));

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

    // The gateway now plays from another acexy: a resync updates the stored play URL, which
    // changes the body, so the old ETag no longer matches.
    gw.set_acexy("http://acexy-new:8080", "2");
    let (_, _, rep) = call(&app, "POST", &format!("/api/v1/routes/{ace}/resync"), None).await;
    assert_eq!(rep["updated"], 1, "{rep}");
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
    assert!(m3u(&app, "jf").await.contains("acexy-new"));

    let (s, _, _) = call(&app, "GET", "/p/missing.m3u", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = call(&app, "GET", "/p/jf.txt", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn base_url_changes_revalidate_every_channel() {
    let gw = StubGateway::start().await;
    let other = StubGateway::start().await;
    let app = app().await;
    let vk = gateway_route(&app, &gw, "VK", "vk").await;
    call(&app, "POST", "/api/v1/playlists", Some(json!({"slug": "jf"}))).await;
    call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "M", "route_id": vk, "input": "https://vk.com/video-1_2", "playlists": ["jf"]})),
    )
    .await;
    assert!(m3u(&app, "jf").await.contains(&format!("{}/play/vk.m3u8", gw.url)));

    // An unreachable new base is refused; nothing changes.
    let (s, _, e) = call(
        &app,
        "PATCH",
        &format!("/api/v1/routes/{vk}"),
        Some(json!({"base_url": "http://127.0.0.1:1"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{e}");
    assert!(m3u(&app, "jf").await.contains(&gw.url));

    // A working one moves every channel (relative play paths join the new base).
    let (s, _, r) = call(
        &app,
        "PATCH",
        &format!("/api/v1/routes/{vk}"),
        Some(json!({"base_url": format!("{}/", other.url)})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["config"]["base_url"], other.url);
    assert_eq!(r["last_resync"]["trigger"], "base_url_change");
    assert!(m3u(&app, "jf").await.contains(&format!("{}/play/vk.m3u8", other.url)));
}

#[tokio::test]
async fn channel_info_observes_and_checks_record() {
    let gw = StubGateway::start().await;
    gw.set_check(
        gateway_contract::CheckOutcome::Ok,
        Some(gateway_contract::ContentState::Recording),
    );
    let app = app().await;
    let ace = gateway_route(&app, &gw, "Acestream", "acestream").await;
    let vk = gateway_route(&app, &gw, "VK", "vk").await;
    let (_, _, a) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "A", "route_id": ace, "input": ACE})),
    )
    .await;
    let (_, _, v) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "V", "route_id": vk, "input": "https://vk.com/video-1_2"})),
    )
    .await;
    let (_, _, d) = call(
        &app,
        "POST",
        "/api/v1/channels",
        Some(json!({"name": "D", "route_id": DIRECT, "input": "http://x/y.m3u8"})),
    )
    .await;

    let (_, _, i) = call(&app, "GET", &format!("/api/v1/channels/{}/info", a["id"]), None).await;
    assert_eq!(i["checks"]["available"], true, "{i}");
    assert_eq!(i["observation"]["facts"][0]["label"], "Peers");
    assert_eq!(i["observation"]["content_state"], "recording");
    let (_, _, i) = call(&app, "GET", &format!("/api/v1/channels/{}/info", d["id"]), None).await;
    assert_eq!(
        (i["checks"]["available"].as_bool(), &i["observation"]),
        (Some(false), &Value::Null)
    );

    let (s, _, r) = call(&app, "POST", "/api/v1/checks", Some(json!({"ids": [a["id"]]}))).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["results"][0]["outcome"], "ok");
    assert_eq!(r["results"][0]["recorded"], true);
    let (_, _, c) = call(&app, "GET", &format!("/api/v1/channels/{}", a["id"]), None).await;
    assert_eq!(c["health"]["conclusive"], "ok");
    assert_eq!(c["health"]["content_state"], "recording");
    // Health filter.
    let (_, _, l) = call(&app, "GET", "/api/v1/channels?health=ok", None).await;
    assert_eq!(l.as_array().unwrap().len(), 1);

    // The vk rule has no checks; direct routes neither.
    for id in [&v["id"], &d["id"]] {
        let (s, _, _) = call(&app, "POST", "/api/v1/checks", Some(json!({"ids": [id]}))).await;
        assert_eq!(s, StatusCode::CONFLICT);
    }
    let (s, _, r) = call(&app, "POST", "/api/v1/checks", Some(json!({}))).await;
    assert_eq!((s, r["background"].as_bool()), (StatusCode::ACCEPTED, Some(true)));
}

#[tokio::test]
async fn import_endpoint_and_routing() {
    let gw = StubGateway::start().await;
    let app = app().await;
    let ace = gateway_route(&app, &gw, "Acestream", "acestream").await;
    let text = format!(
        "#EXTM3U\n#EXTINF:-1 group-title=\"Sports\",Alpha\nhttp://h:8080/ace/getstream?id={ACE}\n#EXTINF:-1,Web\nhttps://x/y.m3u8\n"
    );
    let (s, _, r) = call(
        &app,
        "POST",
        "/api/v1/import",
        Some(json!({"m3u_text": text, "playlist_slug": "jf", "route_id": ace, "dry_run": true})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["entries"][0]["action"], "create");
    assert_eq!(r["entries"][0]["identity"], format!("ace:{ACE}"));
    assert_eq!(r["entries"].as_array().unwrap().len(), 1);
    assert!(r["skipped_lines"][0].as_str().unwrap().contains("Web"), "{r}");
    let (_, _, l) = call(&app, "GET", "/api/v1/playlists", None).await;
    assert_eq!(l, json!([]));

    let (s, _, _) = call(
        &app,
        "POST",
        "/api/v1/import",
        Some(json!({"playlist_slug": "jf", "route_id": ace})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = call(
        &app,
        "POST",
        "/api/v1/import",
        Some(json!({"m3u_text": "#EXTM3U\n", "playlist_slug": "jf"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "a route is required");

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
    assert!(doc["paths"]["/routes/{id}/resync"].is_object());
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
    let app = router(store);
    let (s, _, e) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        Some(json!({ "check_interval_hours": 1000 })),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
    let (_, _, v) = call(&app, "GET", "/api/v1/settings", None).await;
    assert_eq!(
        v,
        json!({ "epg_refresh_hours": 3, "check_interval_hours": 0, "revision": 2 })
    );
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

#[tokio::test]
async fn path_and_query_errors_use_the_json_envelope() {
    let app = app().await;
    for uri in [
        "/api/v1/channels?route_id=bogus",
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
            Some(json!({"url": format!("{base}/{path}"), "playlist_slug": "jf", "route_id": DIRECT})),
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{path}: {e}");
        assert!(e["error"]["message"].as_str().unwrap().contains("larger than"), "{e}");
    }
    let (_, _, l) = call(&app, "GET", "/api/v1/playlists", None).await;
    assert_eq!(l, json!([]), "nothing was created");
}
