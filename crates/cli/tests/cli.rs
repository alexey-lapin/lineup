//! CLI behaviour against an in-process server: exit codes and error classification.

use std::sync::Arc;

use clap::Parser;
use lineup_cli::Cli;
use lineup_server::{Store, http::router};

const ACE1: &str = "41169a3f280a5537b0611b775e926852d372743d";
const ACE2: &str = "816b61ec91d5d4da0bc477a42cd6ee407fc07d44";

async fn serve(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn real_server() -> String {
    serve(router(Arc::new(Store::open_memory().await.unwrap()))).await
}

async fn lineup(server: &str, args: &[&str]) -> i32 {
    let mut argv = vec!["lineup", "--server", server];
    argv.extend_from_slice(args);
    lineup_cli::run(Cli::try_parse_from(argv).expect("args parse")).await
}

#[tokio::test]
async fn exit_codes() {
    let s = real_server().await;
    assert_eq!(lineup(&s, &["playlist", "create", "jf"]).await, 0);
    assert_eq!(lineup(&s, &["add", ACE1, "--name", "Alpha", "-p", "jf"]).await, 0);
    assert_eq!(lineup(&s, &["show", "Alpha"]).await, 0);
    assert_eq!(lineup(&s, &["show", &format!("acestream://{ACE1}")]).await, 0);
    assert_eq!(lineup(&s, &["show", "nope"]).await, 3, "unknown name");
    assert_eq!(lineup(&s, &["show", "999"]).await, 3, "unknown id");
    assert_eq!(lineup(&s, &["add", ACE1]).await, 4, "duplicate");
    assert_eq!(lineup(&s, &["add", "not a source"]).await, 2, "invalid input");
    assert_eq!(lineup(&s, &["playlist", "add", "missing", "Alpha"]).await, 3);

    // malformed server URL is a usage error, not "unreachable"
    assert_eq!(lineup("::bad", &["ls"]).await, 2);
    // closed port: unreachable
    assert_eq!(lineup("http://127.0.0.1:1", &["ls"]).await, 5);
    // invalid enum value is rejected by clap (exit 2 from the binary)
    assert!(Cli::try_parse_from(["lineup", "ls", "--kind", "bogus"]).is_err());
}

#[tokio::test]
async fn batch_add_exit_policy() {
    let s = real_server().await;
    assert_eq!(lineup(&s, &["add", ACE1]).await, 0);
    // partial success -> 1
    assert_eq!(lineup(&s, &["--json", "add", ACE1, ACE2]).await, 1);
    // all failed with the same error kind -> that code
    assert_eq!(lineup(&s, &["add", ACE1, ACE2]).await, 4);
    // mixed failures -> 1
    assert_eq!(lineup(&s, &["add", ACE1, "not a source"]).await, 1);
}

#[tokio::test]
async fn resolver_failures_are_not_reported_as_not_found() {
    use axum::Json;
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    let app = axum::Router::new()
        .route("/api/v1/channels", get(|| async { Json(serde_json::json!([])) }))
        .route(
            "/api/v1/resolve",
            post(|| async {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": {"code": "internal", "message": "boom"}})),
                )
            }),
        );
    let s = serve(app).await;
    assert_eq!(lineup(&s, &["show", &format!("acestream://{ACE1}")]).await, 1);
}

#[tokio::test]
async fn backup_is_a_complete_database() {
    let dbdir = tempfile::tempdir().unwrap();
    let store = Store::open(&dbdir.path().join("lineup.db")).await.unwrap();
    let s = serve(router(Arc::new(store))).await;
    assert_eq!(lineup(&s, &["playlist", "create", "jf"]).await, 0);
    assert_eq!(lineup(&s, &["add", ACE1, "--name", "Alpha", "-p", "jf"]).await, 0);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("b.db");
    let out_s = out.to_str().unwrap();
    assert_eq!(lineup(&s, &["backup", out_s]).await, 0);
    assert_eq!(lineup(&s, &["backup", out_s]).await, 2, "refuses to overwrite");

    let restored = Store::open(&out).await.unwrap();
    let pl = restored.get_playlist("jf").await.unwrap();
    assert_eq!(pl.items.len(), 1);
    assert_eq!(pl.items[0].channel.name, "Alpha");
}

#[tokio::test]
async fn backup_never_overwrites() {
    let dbdir = tempfile::tempdir().unwrap();
    let store = Store::open(&dbdir.path().join("lineup.db")).await.unwrap();
    let s = serve(router(Arc::new(store))).await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("b.db");
    std::fs::write(&out, "keep me").unwrap();
    assert_eq!(lineup(&s, &["backup", out.to_str().unwrap()]).await, 2);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "keep me");
}

#[tokio::test]
async fn epg_commands() {
    let s = real_server().await;
    assert_eq!(lineup(&s, &["epg", "sources", "ls"]).await, 0);
    assert_eq!(lineup(&s, &["epg", "sources", "add", "not a url"]).await, 2);
    // Disabled straight away, so the background fetch has nothing to reach.
    assert_eq!(
        lineup(&s, &["epg", "sources", "add", "http://127.0.0.1:1/guide.xml"]).await,
        0
    );
    assert_eq!(lineup(&s, &["epg", "sources", "edit", "1", "--disable"]).await, 0);
    assert_eq!(
        lineup(&s, &["epg", "fetch", "1"]).await,
        0,
        "disabled: skipped, not an error"
    );
    assert_eq!(lineup(&s, &["epg", "fetch", "7"]).await, 3);
    assert_eq!(lineup(&s, &["epg", "search", "sports"]).await, 0);
    assert_eq!(lineup(&s, &["epg", "sources", "rm", "1"]).await, 0);
    assert_eq!(lineup(&s, &["epg", "sources", "rm", "1"]).await, 3);
    assert_eq!(lineup(&s, &["settings", "set", "epg_refresh_hours", "0"]).await, 0);
    assert_eq!(lineup(&s, &["settings", "set", "epg_refresh_hours", "x"]).await, 2);
    assert_eq!(lineup(&s, &["settings", "set", "epg_refresh_hours", "9999"]).await, 2);
}

#[tokio::test]
async fn add_applies_suggestions_only_with_yes() {
    let store = Arc::new(Store::open_memory().await.unwrap());
    let s = serve(router(store.clone())).await;
    assert_eq!(lineup(&s, &["add", "https://vk.com/video-1_2"]).await, 0);
    let c = &store.list_channels(&Default::default()).await.unwrap()[0];
    assert_eq!(
        (c.name.as_str(), c.is_event, c.group_title.as_deref()),
        ("VK -1_2", false, None)
    );

    assert_eq!(
        lineup(&s, &["add", "-y", "https://vk.com/video-1_3", "--group", "Live"]).await,
        0
    );
    let all = store.list_channels(&Default::default()).await.unwrap();
    let c = all.iter().find(|c| c.source_key == "-1_3").unwrap();
    assert!(c.is_event);
    assert_eq!(c.group_title.as_deref(), Some("Live"), "explicit flags win");
}
