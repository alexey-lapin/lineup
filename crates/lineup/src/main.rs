use lineup_cli::Dispatch;

#[tokio::main]
async fn main() {
    let code = match lineup_cli::parse() {
        Dispatch::Serve(args) => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_env("LINEUP_LOG")
                        .unwrap_or_else(|_| "info,sqlx=warn".into()),
                )
                .init();
            let cfg = lineup_server::Config {
                bind: args.bind,
                data_dir: args.data_dir,
            };
            match lineup_server::serve(cfg).await {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("error: {e:#}");
                    1
                }
            }
        }
        Dispatch::Client(cli) => lineup_cli::run(cli).await,
    };
    std::process::exit(code);
}
