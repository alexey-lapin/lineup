use clap::Parser;

/// lineup-gateway: turns references (Acestream ids, video pages) into playable URLs for lineup.
#[derive(Parser)]
#[command(name = "lineup-gateway", version)]
struct Args {
    /// Config file (TOML).
    #[arg(
        long,
        short,
        env = "LINEUP_GATEWAY_CONFIG",
        default_value = "/etc/lineup-gateway.toml"
    )]
    config: std::path::PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("LINEUP_GATEWAY_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let args = Args::parse();
    let text =
        std::fs::read_to_string(&args.config).map_err(|e| anyhow::anyhow!("reading {}: {e}", args.config.display()))?;
    let cfg = lineup_gateway::config::Config::parse(&text)?;
    lineup_gateway::serve(cfg).await
}
