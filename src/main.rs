//! `mirra-lingua-server [config.toml]` – starts the HTTP server.

use anyhow::Context;
use mirra_lingua::{api, build_state, config::Config, CONTENT_CHECK_INTERVAL};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        // No colour codes when the output goes to journald or a file.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();

    let config_path = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "mirra-lingua.toml".into()));
    let config = Config::load(&config_path)?;
    let addr: SocketAddr = format!("{}:{}", config.bind, config.port).parse().context("invalid bind address/port")?;

    let state = build_state(config, CONTENT_CHECK_INTERVAL)?;
    tracing::info!(
        "content directory: {} ({} course(s))",
        state.courses.dir().display(),
        state.courses.list().len()
    );

    // Daily housekeeping: expired sessions and old idempotency records.
    let maintenance = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
        loop {
            tick.tick().await;
            let sessions = maintenance.db.purge_idle_sessions(maintenance.config.session_max_idle_ms());
            let events = maintenance.db.purge_old_events(400 * 86_400_000);
            match (sessions, events) {
                (Ok(s), Ok(e)) if s + e > 0 => tracing::info!("maintenance: removed {s} idle session(s), {e} old event id(s)"),
                (Err(e), _) | (_, Err(e)) => tracing::warn!("maintenance failed: {e}"),
                _ => {}
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("cannot listen on {addr}"))?;
    tracing::info!("MirraLingua server {} listening on {addr}", api::VERSION);
    axum::serve(listener, api::router(state).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
