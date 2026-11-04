mod admission;
mod config;
mod error;
mod routes;
mod store;

use std::{env, net::SocketAddr, time::Duration};

use routes::{AppState, router};
use store::RedisStore;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .json()
        .init();

    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    let redis_timeout_ms: u64 = env::var("REDIS_OPERATION_TIMEOUT_MS")
        .unwrap_or_else(|_| "500".into())
        .parse()?;
    let bind_addr: SocketAddr = env::var("BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".into())
        .parse()?;

    let store = RedisStore::connect(&redis_url, Duration::from_millis(redis_timeout_ms)).await?;
    if store.initialize_defaults().await? {
        info!("initialized global rate-limit configuration at generation 1");
    } else {
        info!("using existing global rate-limit configuration");
    }

    let app = router(AppState { store }).layer(TraceLayer::new_for_http());
    let listener = TcpListener::bind(bind_addr).await?;
    info!(%bind_addr, "rate limiter listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => warn!("received Ctrl+C"),
        () = terminate => warn!("received SIGTERM"),
    }
}
