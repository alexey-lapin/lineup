//! What the server writes to its log: request lines at the right level, and no secrets from source or
//! hook URLs, request queries, headers or bodies. One test, since it installs the global subscriber.

use std::io::{self, Write};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use api_types::{HookConfig, HookCreate};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use lineup_server::Store;
use lineup_server::epg::Epg;
use lineup_server::hooks::{Hooks, Timing};
use lineup_server::http::router;
use serde_json::json;
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

impl Capture {
    /// Everything logged so far; clears the buffer.
    fn take(&self) -> String {
        String::from_utf8(std::mem::take(&mut *self.0.lock().unwrap())).unwrap()
    }
}

fn has_line(logs: &str, level: &str, text: &str) -> bool {
    logs.lines().any(|l| l.contains(level) && l.contains(text))
}

fn assert_secret_free(logs: &str, secrets: &[&str]) {
    for s in secrets {
        assert!(!logs.contains(s), "logs disclosed {s}:\n{logs}");
    }
}

#[tokio::test]
async fn logs_are_useful_and_secret_free() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("lineup_server=debug")
        .with_ansi(false)
        .without_time()
        .with_writer(capture.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let store = Arc::new(Store::open_memory().await.unwrap());
    let app = router(store.clone());

    // A change: info, with the client; no query, header or body content.
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/playlists?secret=QUERY-SECRET")
        .header("Authorization", "Bearer HEADER-SECRET")
        .header("Content-Type", "application/json")
        .extension(ConnectInfo("192.0.2.10:54321".parse::<SocketAddr>().unwrap()))
        .body(Body::from(r#"{"slug":"logged","name":"BODY-SECRET"}"#))
        .unwrap();
    app.clone().oneshot(req).await.unwrap();
    let logs = capture.take();
    assert!(
        has_line(&logs, "INFO", "192.0.2.10 POST /api/v1/playlists 201"),
        "{logs}"
    );
    assert_secret_free(&logs, &["QUERY-SECRET", "HEADER-SECRET", "BODY-SECRET"]);

    // An ordinary read: debug only.
    let req = Request::builder().uri("/api/v1/status").body(Body::empty()).unwrap();
    app.clone().oneshot(req).await.unwrap();
    let logs = capture.take();
    assert!(has_line(&logs, "DEBUG", "GET /api/v1/status"), "{logs}");
    assert!(!logs.contains("INFO"), "{logs}");

    // A playlist download: info, without the query.
    let req = Request::builder()
        .uri("/p/logged.m3u?secret=DOWNLOAD-SECRET")
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(req).await.unwrap();
    let logs = capture.take();
    assert!(has_line(&logs, "INFO", "GET /p/logged.m3u 200"), "{logs}");
    assert_secret_free(&logs, &["DOWNLOAD-SECRET"]);

    // Guide sources whose URLs carry credentials, unnamed so their names derive from the URLs; the
    // fetches fail (nothing listens on port 1).
    let urls = [
        "http://user:SOURCE-PASSWORD@127.0.0.1:1/xmltv/SOURCE-PATH-TOKEN?token=SOURCE-QUERY-TOKEN",
        "http://user:ROOT-PASSWORD@127.0.0.1:1/?token=ROOT-QUERY-TOKEN",
    ];
    let mut ids = vec![];
    for url in urls {
        let source = store
            .create_epg_source(serde_json::from_value(json!({ "url": url })).unwrap())
            .await
            .unwrap();
        ids.push(source.id);
    }
    let dir = tempfile::tempdir().unwrap();
    let epg = Epg::new(store.clone(), dir.path());
    epg.fetch(&ids).await.unwrap();
    let logs = capture.take();
    for id in ids {
        assert!(
            has_line(&logs, "INFO", &format!("guide source {id}: fetching")),
            "{logs}"
        );
        assert!(has_line(&logs, "WARN", &format!("guide source {id}: ")), "{logs}");
    }
    assert_secret_free(
        &logs,
        &[
            "SOURCE-PASSWORD",
            "SOURCE-PATH-TOKEN",
            "SOURCE-QUERY-TOKEN",
            "ROOT-PASSWORD",
            "ROOT-QUERY-TOKEN",
        ],
    );

    // A webhook test against a URL with tokens: the outcome is logged, the URL is not.
    let hook = store
        .create_hook(HookCreate {
            name: "receiver".into(),
            config: HookConfig::Webhook {
                method: "POST".into(),
                url: "http://127.0.0.1:1/hook/HOOK-PATH-TOKEN?key=HOOK-QUERY-TOKEN".into(),
                headers: [("X-Token".to_string(), "HOOK-HEADER-SECRET".to_string())].into(),
            },
            events: None,
            debounce_secs: None,
            enabled: true,
        })
        .await
        .unwrap();
    let hooks = Hooks::new(store.clone(), reqwest::Client::new(), Timing::default());
    hooks.test(hook.id).await.unwrap();
    let logs = capture.take();
    assert!(
        has_line(&logs, "WARN", "hook 1 (receiver) attempt 1 for hook.test"),
        "{logs}"
    );
    assert_secret_free(&logs, &["HOOK-PATH-TOKEN", "HOOK-QUERY-TOKEN", "HOOK-HEADER-SECRET"]);
}
