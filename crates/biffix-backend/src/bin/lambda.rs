//! biffix control-plane Lambda entry — the same axum app served through API Gateway (HTTP API) via
//! `lambda_http`. This is the production backend: ~$0 idle, native API Gateway integration, no load
//! balancer or always-on task.
//!
//!   cargo lambda build --release --features aws --bin biffix-backend-lambda
//!
//! Composes the identical AppState as the server binary (`biffix_backend::config::compose`), so the
//! two entry points can't drift, but migrates on the first request that needs the database rather
//! than at cold start (`build_lambda_router`), so sign-in starts while a paused Aurora resumes.
//! Live UI updates are via client polling (no SSE on Lambda).
//! (`required-features` keeps Cargo from building it without `aws`.)

#[tokio::main]
async fn main() -> Result<(), lambda_http::Error> {
    biffix_core::telemetry::init_lambda();

    let config = biffix_backend::config::env::ApiConfig::from_process_env()?;
    let app = biffix_backend::config::compose::build_lambda_router(config)
        .await
        .map_err(|e| lambda_http::Error::from(e.to_string()))?;

    // Run the axum Router as a Lambda HTTP handler (API Gateway HTTP API events), each response
    // sent as `LambdaReply` sends it (a text body byte for byte).
    let app = lambda_http::tower::ServiceExt::<lambda_http::Request>::map_response(
        app,
        biffix_backend::api::lambda::LambdaReply,
    );
    lambda_http::run(app).await
}
