use std::path::PathBuf;
use wb_relay::{serve, Config};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = env("WB_ADDR").unwrap_or_else(|| "127.0.0.1:8787".into());
    let cfg = Config {
        db_path: env("WB_DB").unwrap_or_else(|| "whiteboard.db".into()),
        ttl_days: env("WB_TTL_DAYS").and_then(|v| v.parse().ok()).unwrap_or(90),
        trust_proxy: env("WB_TRUST_PROXY").is_some_and(|v| v == "1"),
        web_dir: env("WB_WEB_DIR").map(PathBuf::from),
        unload_grace: wb_relay::rooms::UNLOAD_GRACE,
    };
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!(
        "wb-relay: listening on http://{addr}, database {}, boards kept {} days after last use",
        cfg.db_path,
        if cfg.ttl_days == 0 { "for ever".to_string() } else { cfg.ttl_days.to_string() }
    );
    let stop = async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    };
    serve(listener, cfg, stop).await
}
