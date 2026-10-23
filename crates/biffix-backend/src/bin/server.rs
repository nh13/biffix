//! biffix control-plane server entry — long-running axum server (e.g. local dev, or a Fargate
//! deploy if you prefer a persistent server over Lambda).
//!
//!   cargo run -p biffix-backend --features aws --bin biffix-backend
//!
//! Production deploys the Lambda entry (`biffix-backend-lambda`) instead; both compose the same
//! AppState via `biffix_backend::config::compose` and read the same variables
//! (`biffix_backend::config::env`). Like the Lambda, it refuses to start without
//! `BIFFIX_OIDC_ISSUER`; for local development with no Cognito pool, set
//! `BIFFIX_AUTH_DISABLED=true` (logged as a warning; every request then has every capability).
//! Besides the database (`DATABASE_URL`) and the projects (outside a deploy,
//! `BIFFIX_PROJECTS_FILE` names a JSON file of its project contexts), it needs
//! `BIFFIX_WORKER_IMAGE_REPO`, `BIFFIX_INGEST_URL` and `BIFFIX_COORDINATOR_QUEUE` set (blank is
//! accepted) and `BIFFIX_BUILDS_IMAGE_REPO` set to a repository URI (`dev/README.md` has a recipe
//! that starts). Without `BIFFIX_CAPABILITY_SECRET_ARN` no run launches and engine ingest is off.
//!
//! It listens on port 8080: on every interface, or on loopback only (`127.0.0.1`) when
//! authentication is off, so an open server is not exposed to the network. `RUST_LOG` sets its log
//! filter (default `info`).

#[cfg(feature = "aws")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::net::SocketAddr;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = biffix_backend::config::env::ApiConfig::from_process_env()?;
    let host = if config.auth_disabled() { [127, 0, 0, 1] } else { [0, 0, 0, 0] };
    let state = biffix_backend::config::compose::build_state(config).await?;
    let app = biffix_backend::api::router(state);

    let addr = SocketAddr::from((host, 8080));
    tracing::info!(%addr, "biffix-backend listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(not(feature = "aws"))]
fn main() {
    eprintln!(
        "biffix-backend requires --features aws to run (real store + cloud provider).\n\
         Build: cargo build -p biffix-backend --features aws --bin biffix-backend"
    );
    std::process::exit(1);
}
