//! Ambient proxy settings must not route public fetches (a proxy would resolve names past the
//! public-only rule). Its own test binary: it changes the process environment.

use std::time::Duration;

use lineup_gateway::net::{Allow, public_client};

#[tokio::test]
async fn ambient_proxies_are_ignored() {
    // A proxy that does not exist: any request routed through it fails.
    unsafe {
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:9");
        std::env::set_var("HTTPS_PROXY", "http://127.0.0.1:9");
        std::env::set_var("ALL_PROXY", "http://127.0.0.1:9");
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().route("/x", axum::routing::get(|| async { "direct" }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = public_client(&Allow::new(&["127.0.0.1".into()]), Duration::from_secs(5));
    let body = client
        .get(format!("http://{addr}/x"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "direct");
}
