use clap::Parser;

/// lineup-gateway: turns references into playable URLs for lineup, by configured rules.
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
    /// Check that the gateway configured by --config answers, and exit 0 or 1 (for container
    /// healthchecks, whatever port it listens on).
    #[arg(long)]
    healthcheck: bool,
    /// Load the config, compile every rule and check its examples, then exit 0 or 1. Starts
    /// nothing: no tools, no proxy, no network.
    #[arg(long)]
    check_config: bool,
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
    let cfg = match lineup_gateway::config::Config::parse(&text) {
        Ok(c) => c,
        Err(e) if args.check_config => {
            eprintln!("{}: {e}", args.config.display());
            std::process::exit(1);
        }
        Err(e) => return Err(e),
    };
    if args.check_config {
        for (name, rule) in &cfg.rules {
            println!("rules.{name}: {}, {} example(s) ok", rule.kind(), rule.examples().len());
        }
        return Ok(());
    }
    if args.healthcheck {
        std::process::exit(if lineup_gateway::healthcheck(&cfg).await { 0 } else { 1 });
    }
    lineup_gateway::serve(cfg).await
}
