//! CLI behaviour against an in-process server and a stub gateway: exit codes and error
//! classification.

use std::sync::Arc;

use clap::Parser;
use lineup_cli::Cli;
use lineup_server::testing::{Failure, StubGateway};
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

/// A server with routes "ace" (acestream rule) and "vk" (vk rule) on a stub gateway.
async fn with_routes() -> (String, Arc<Store>, StubGateway) {
    let gw = StubGateway::start().await;
    let store = Arc::new(Store::open_memory().await.unwrap());
    let s = serve(router(store.clone())).await;
    for (name, rule) in [("ace", "acestream"), ("vk", "vk")] {
        let code = lineup(&s, &["route", "add", name, "--gateway", &gw.url, "--rule", rule]).await;
        assert_eq!(code, 0, "route add {name}");
    }
    (s, store, gw)
}

#[tokio::test]
async fn exit_codes() {
    let (s, _, gw) = with_routes().await;
    assert_eq!(lineup(&s, &["playlist", "create", "jf"]).await, 0);
    assert_eq!(
        lineup(&s, &["add", "-r", "ace", ACE1, "--name", "Alpha", "-p", "jf"]).await,
        0
    );
    assert_eq!(lineup(&s, &["show", "Alpha"]).await, 0);
    assert_eq!(
        lineup(&s, &["show", &format!("acestream://{ACE1}")]).await,
        0,
        "by stored source"
    );
    assert_eq!(lineup(&s, &["show", &format!("ace:{ACE1}")]).await, 0, "by identity");
    assert_eq!(lineup(&s, &["show", "nope"]).await, 3, "unknown name");
    assert_eq!(lineup(&s, &["show", "999"]).await, 3, "unknown id");
    assert_eq!(lineup(&s, &["add", "-r", "ace", ACE1]).await, 4, "duplicate");
    assert_eq!(
        lineup(&s, &["add", "-r", "ace", "not a source"]).await,
        2,
        "invalid input"
    );
    assert_eq!(lineup(&s, &["add", "-r", "nope", ACE1]).await, 3, "unknown route");
    assert_eq!(lineup(&s, &["playlist", "add", "missing", "Alpha"]).await, 3);

    // A failing gateway: exit 6.
    gw.fail(Failure::ServerError);
    assert_eq!(lineup(&s, &["add", "-r", "ace", ACE2]).await, 6);
    gw.fail(Failure::None);

    // malformed server URL is a usage error, not "unreachable"
    assert_eq!(lineup("::bad", &["ls"]).await, 2);
    // closed port: unreachable
    assert_eq!(lineup("http://127.0.0.1:1", &["ls"]).await, 5);
    // A route is required to add.
    assert!(Cli::try_parse_from(["lineup", "add", ACE1]).is_err());
}

#[tokio::test]
async fn batch_add_exit_policy() {
    let (s, _, _) = with_routes().await;
    assert_eq!(lineup(&s, &["add", "-r", "ace", ACE1]).await, 0);
    // partial success -> 1
    assert_eq!(lineup(&s, &["--json", "add", "-r", "ace", ACE1, ACE2]).await, 1);
    // all failed with the same error kind -> that code
    assert_eq!(lineup(&s, &["add", "-r", "ace", ACE1, ACE2]).await, 4);
    // mixed failures -> 1
    assert_eq!(lineup(&s, &["add", "-r", "ace", ACE1, "not a source"]).await, 1);
}

#[tokio::test]
async fn backup_is_a_complete_database() {
    let dbdir = tempfile::tempdir().unwrap();
    let store = Store::open(&dbdir.path().join("lineup.db")).await.unwrap();
    let s = serve(router(Arc::new(store))).await;
    assert_eq!(lineup(&s, &["playlist", "create", "jf"]).await, 0);
    assert_eq!(
        lineup(
            &s,
            &["add", "-r", "Direct", "http://x/a.m3u8", "--name", "Alpha", "-p", "jf"]
        )
        .await,
        0
    );
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
    assert_eq!(lineup(&s, &["settings", "set", "check_interval_hours", "6"]).await, 0);
    assert_eq!(lineup(&s, &["settings", "set", "acexy_base", &s]).await, 2);
}

#[tokio::test]
async fn add_applies_suggestions_only_with_yes() {
    let (s, store, _) = with_routes().await;
    assert_eq!(lineup(&s, &["add", "-r", "vk", "https://vk.com/video-1_2"]).await, 0);
    let c = &store.list_channels(&Default::default()).await.unwrap()[0];
    assert_eq!((c.name.as_str(), c.group_title.as_deref()), ("vk:-1_2", None));
    assert_eq!(
        lineup(&s, &["add", "-y", "-r", "vk", "https://vk.com/video-1_4"]).await,
        0
    );
    let all = store.list_channels(&Default::default()).await.unwrap();
    let c = all.iter().find(|c| c.identity == "vk:-1_4").unwrap();
    assert_eq!(c.group_title.as_deref(), Some("VK"), "suggestions applied with --yes");

    assert_eq!(
        lineup(
            &s,
            &["add", "-y", "-r", "vk", "https://vk.com/video-1_3", "--group", "Live"]
        )
        .await,
        0
    );
    let all = store.list_channels(&Default::default()).await.unwrap();
    let c = all.iter().find(|c| c.identity == "vk:-1_3").unwrap();
    assert_eq!(c.group_title.as_deref(), Some("Live"), "explicit flags win");
}

#[tokio::test]
async fn hook_commands() {
    let s = real_server().await;
    assert_eq!(lineup(&s, &["hook", "ls"]).await, 0);
    let add = [
        "hook",
        "add",
        "n",
        "http://127.0.0.1:1/x",
        "--header",
        "X-Key=1",
        "--events",
        "playlist.changed",
    ];
    assert_eq!(lineup(&s, &add).await, 0);
    assert_eq!(
        lineup(&s, &["hook", "add", "n", "http://127.0.0.1:1/x", "--events", "bogus"]).await,
        2
    );
    assert_eq!(
        lineup(&s, &["hook", "test", "1"]).await,
        1,
        "the receiver is unreachable"
    );
    assert_eq!(lineup(&s, &["hook", "deliveries", "1"]).await, 0);
    // Edit the URL; the header is kept unless --header is given.
    assert_eq!(
        lineup(
            &s,
            &["hook", "edit", "1", "--url", "http://127.0.0.1:1/y", "--name", "m"]
        )
        .await,
        0
    );
    assert_eq!(lineup(&s, &["hook", "edit", "1", "--url", "not a url"]).await, 2);
    assert_eq!(lineup(&s, &["hook", "disable", "1"]).await, 0);
    assert_eq!(lineup(&s, &["hook", "rm", "1"]).await, 0);
    assert_eq!(lineup(&s, &["hook", "test", "1"]).await, 3);
}

#[tokio::test]
async fn route_commands() {
    let gw = StubGateway::start().await;
    let s = real_server().await;
    assert_eq!(lineup(&s, &["route", "ls"]).await, 0);
    // A draft config: its rules are listed; a missing rule fails the test (exit 1).
    assert_eq!(
        lineup(&s, &["route", "test", "--gateway", &gw.url, "--rule", "vk"]).await,
        0
    );
    assert_eq!(lineup(&s, &["route", "test", "--gateway", &gw.url]).await, 1);
    assert_eq!(
        lineup(
            &s,
            &["route", "test", "--gateway", "http://127.0.0.1:1", "--rule", "vk"]
        )
        .await,
        1
    );
    assert_eq!(
        lineup(
            &s,
            &[
                "route",
                "test",
                "--gateway",
                &gw.url,
                "--rule",
                "vk",
                "--input",
                "https://vk.com/video1_2"
            ]
        )
        .await,
        0
    );
    assert_eq!(
        lineup(&s, &["route", "add", "x", "--gateway", "ftp://x", "--rule", "vk"]).await,
        2
    );
    assert_eq!(lineup(&s, &["route", "add", "x"]).await, 2, "a kind is required");
    assert_eq!(
        lineup(
            &s,
            &["route", "add", "Ace", "--gateway", &gw.url, "--rule", "acestream"]
        )
        .await,
        0
    );
    assert_eq!(
        lineup(&s, &["route", "add", "Ace", "--direct"]).await,
        4,
        "names are unique"
    );
    assert_eq!(lineup(&s, &["route", "show", "ace"]).await, 0);
    assert_eq!(lineup(&s, &["route", "test", "ace"]).await, 0);
    assert_eq!(lineup(&s, &["route", "resync", "ace"]).await, 0);
    assert_eq!(lineup(&s, &["route", "resync", "Direct"]).await, 2);
    assert_eq!(lineup(&s, &["add", "-r", "ace", ACE1, "--name", "A"]).await, 0);
    assert_eq!(lineup(&s, &["info", "A"]).await, 0);
    assert_eq!(lineup(&s, &["check", "A"]).await, 0);
    assert_eq!(lineup(&s, &["ls", "--route", "ace"]).await, 0);
    assert_eq!(lineup(&s, &["route", "rm", "ace"]).await, 4, "in use");
    assert_eq!(
        lineup(&s, &["route", "edit", "ace", "--disable", "--name", "Ace2"]).await,
        0
    );
    assert_eq!(
        lineup(&s, &["route", "edit", "Ace2", "--base-url", "http://127.0.0.1:1"]).await,
        6,
        "a base URL that does not answer is refused"
    );
    assert_eq!(lineup(&s, &["rm", "A"]).await, 0);
    assert_eq!(lineup(&s, &["route", "rm", "Ace2"]).await, 0);
}

/// A new installation: the Direct route plays URLs as they are; a gateway route is added for
/// Acestream ids; an m3u is imported route by route.
#[tokio::test]
async fn fresh_owner_workflow() {
    let gw = StubGateway::start().await;
    gw.set_title(ACE1, "Arena 1");
    let store = Arc::new(Store::open_memory().await.unwrap());
    let s = serve(router(store.clone())).await;
    assert_eq!(
        lineup(
            &s,
            &["route", "add", "Acestream", "--gateway", &gw.url, "--rule", "acestream"]
        )
        .await,
        0
    );
    let m3u = std::env::temp_dir().join(format!("lineup-cli-{}.m3u", std::process::id()));
    std::fs::write(
        &m3u,
        format!(
            "#EXTM3U\n#EXTINF:-1,Alpha\nhttp://old-host:8080/ace/getstream?id={ACE1}\n#EXTINF:-1,Web\nhttps://web/live.m3u8\n"
        ),
    )
    .unwrap();
    let path = m3u.to_str().unwrap();
    assert_eq!(lineup(&s, &["import", path, "-p", "jf", "-r", "Acestream"]).await, 0);
    assert_eq!(
        lineup(&s, &["import", path, "-p", "jf", "-r", "Direct", "--dry-run"]).await,
        0
    );
    let _ = std::fs::remove_file(&m3u);
    assert_eq!(lineup(&s, &["show", "Alpha"]).await, 0);
    assert_eq!(lineup(&s, &["info", "Alpha"]).await, 0);
    let out = store.render_m3u("jf").await.unwrap();
    assert!(
        out.contains(&format!("http://acexy:8080/ace/getstream?id={ACE1}")),
        "{out}"
    );
    assert!(!out.contains("Web"), "the acestream route skipped the web URL: {out}");
    assert_eq!(lineup(&s, &["check", "Alpha"]).await, 0);
    assert_eq!(lineup(&s, &["resolve", "-r", "Acestream", ACE1]).await, 0);
}
