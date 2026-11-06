pub mod epg;
pub mod error;
pub mod hooks;
pub mod m3u;
pub mod resolve;
pub mod source;
pub mod store;
pub mod suggest;

pub use error::{Error, Result};
pub use store::{ChannelFilter, Event, Store};
pub mod http;
mod web;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

/// Boot configuration (from env / flags).
#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
}

/// Run the server until SIGINT/SIGTERM.
pub async fn serve(cfg: Config) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(&cfg.data_dir).await?;
    let store = Arc::new(Store::open(&cfg.data_dir.join("lineup.db")).await?);
    let app = http::App::new(store, &cfg.data_dir);
    app.epg.recover().await?;
    app.epg.spawn_scheduler();
    app.hooks.spawn();
    let handle = app.clone();
    let router = http::router_for(app);
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!("lineup listening on http://{}", listener.local_addr()?);
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown().await;
            handle.begin_shutdown();
        })
        .await?;
    Ok(())
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutting down");
}
