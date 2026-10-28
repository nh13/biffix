//! Production wiring shared by the server, API Lambda and sweep entry points: the `AppState` an
//! entry point serves, built from its configuration ([`crate::config::env`]).

use std::sync::Arc;

use aws_config::BehaviorVersion;

use crate::auth::capability::CapabilityVerifier;
use crate::auth::rbac::Rbac;
use crate::auth::{JwtVerifier, AUTH_DISABLED_VAR};
use crate::aws::provider::AwsCloudProvider;
use crate::aws::ssm_projects::SsmProjectsLoader;
use crate::config::env::{ApiConfig, AuthConfig, BackendConfig, CognitoAuth, ProjectsSource};
use crate::config::projects::{FileProjectsLoader, ProjectsHandle, ProjectsLoader};
use crate::pg::event_sink::PgEventSink;
use crate::pg::registry::PgRegistryStore;
use crate::pg::settings::PgSettingsStore;
use crate::pg::PgStore;
use crate::{AppState, AuthContext};
use biffix_core::ids::RunId;

type BoxError = Box<dyn std::error::Error>;

/// Build the `AppState` the local `server` bin serves, migrating the database first. (The API
/// Lambda builds its router with [`build_lambda_router`], which migrates on first use instead.)
/// **Fail-closed:** it authenticates with Cognito unless its config explicitly turns auth off
/// (see [`AuthConfig`]).
pub async fn build_state(config: ApiConfig) -> Result<AppState, BoxError> {
    let auth = build_auth_context(config.auth).await?;
    let (state, pool) = compose(config.backend, auth).await?;
    // The one shared, lock-taking migration — the first query drives the lazy connect.
    biffix_db::migrate::run(&pool).await?;
    Ok(state)
}

/// The API Lambda's router: [`build_state`]'s, but without touching the database at cold start.
/// A paused Aurora takes 15 s or more to resume, past the Lambda's 10 s init limit, and the
/// browser's first request is `/auth/config`, which needs no database: waiting on the resume to
/// answer it leaves the app blank. The migration runs on the first request that needs the
/// database instead ([`migrate_first`]).
pub async fn build_lambda_router(config: ApiConfig) -> Result<axum::Router, BoxError> {
    let auth = build_auth_context(config.auth).await?;
    let (state, pool) = compose(config.backend, auth).await?;
    Ok(migrate_first(crate::api::router(state), move || {
        let pool = pool.clone();
        async move { biffix_db::migrate::run(&pool).await }
    }))
}

/// `router` with `migrate` run once, before the first request that may need the database
/// ([`crate::api::needs_database`]); the others are served at once. A migration that fails is
/// answered 503 and tried again on the next request. It is not cut short: one still waiting when
/// the Lambda times out (a resume past 29 s) ends that invocation, and the next request starts
/// it afresh.
fn migrate_first<F, Fut, E>(router: axum::Router, migrate: F) -> axum::Router
where
    F: Fn() -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), E>> + Send,
    E: std::fmt::Display,
{
    use axum::response::IntoResponse;

    let migrated = Arc::new(tokio::sync::OnceCell::new());
    router.layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let migrate = migrate.clone();
            let migrated = migrated.clone();
            async move {
                if crate::api::needs_database(request.uri().path()) {
                    if let Err(e) = migrated.get_or_try_init(migrate).await {
                        tracing::error!(error = %e, "migrate failed");
                        return crate::api::ApiError::Unavailable(
                            "the database is unavailable or could not be migrated; try again \
                             shortly, and if this persists see the API's logs"
                                .into(),
                        )
                        .into_response();
                    }
                }
                next.run(request).await
            }
        },
    ))
}

/// What scheduled background work (the sweep Lambda) runs on: its `AppState`, which has no auth
/// context (it serves no HTTP request), and the database pool behind it, NOT yet migrated. The
/// sweep migrates on its first invocation that touches the database (an idle tick never does) and
/// closes the pool's connections at the end of each one (`biffix_db::pool::release_idle`).
pub struct BackgroundState {
    pub state: AppState,
    pub pool: sqlx::PgPool,
}

/// Build the [`BackgroundState`], without touching the database.
pub async fn build_background_state(config: BackendConfig) -> Result<BackgroundState, BoxError> {
    let (state, pool) = compose(config, None).await?;
    Ok(BackgroundState { state, pool })
}

/// The production `AppState` from `config`, around the given auth context, and its pool, not yet
/// migrated.
async fn compose(
    config: BackendConfig,
    auth: Option<Arc<AuthContext>>,
) -> Result<(AppState, sqlx::PgPool), BoxError> {
    let database_url = biffix_db::url::resolve(&config.db).await?;
    // Connect lazily so startup never blocks on a live connection — the first query waits out an
    // Aurora resume via `acquire_timeout` instead of failing at boot. The Aurora pool tuning is the
    // ONE shared definition in `biffix_db::pool` (both crates connect to one cluster).
    let pool = biffix_db::pool::tuned_pool_options(8).connect_lazy(&database_url)?;
    let store = Arc::new(PgStore::new(pool.clone()));
    let registry = Arc::new(PgRegistryStore::new(pool.clone()));
    let settings = Arc::new(PgSettingsStore::new(pool.clone()));
    let references = Arc::new(crate::pg::references::PgReferenceStore::new(pool.clone()));
    let experiments = Arc::new(crate::pg::experiments::PgExperimentStore::new(pool.clone()));
    let events = Arc::new(PgEventSink::new(pool.clone()));

    let aws_cfg = aws_config::load_defaults(BehaviorVersion::latest()).await;

    // Identity resolution (submitter email/name at submit). Cognito when we can derive the pool id
    // from the OIDC issuer; otherwise a no-op (auth disabled / non-Cognito local runs).
    let identity: Arc<dyn crate::auth::identity::IdentityResolver> =
        match config.oidc_issuer.as_deref().and_then(crate::auth::identity::pool_id_from_issuer) {
            Some(pool_id) => {
                Arc::new(crate::aws::cognito::CognitoIdentityResolver::new(&aws_cfg, pool_id))
            }
            None => Arc::new(crate::auth::identity::NoopIdentityResolver),
        };

    let projects = Arc::new(build_projects(&aws_cfg, config.projects).await?);

    // One secret, one implementation: the token this mints at submit is exactly what `POST /events`
    // will accept. Fetched from Secrets Manager so the signing key is never in the CFN template or
    // in a Lambda env var. Absent secret => no ingest (the route fails closed) rather than an
    // unauthenticated ingest endpoint.
    let capability = match config.capability_secret_arn {
        Some(arn) => {
            let secret = biffix_db::secrets::fetch_secret_string(&aws_cfg, &arn)
                .await
                .map_err(|e| format!("capability signing key: {e}"))?;
            // An empty key would make every token forgeable: refuse to start rather than mint one.
            biffix_core::capability::check_secret(secret.as_bytes())
                .map_err(|e| format!("capability secret {arn}: {e}"))?;
            Some(Arc::new(CapabilityVerifier::new(secret.into_bytes())))
        }
        None => {
            tracing::warn!(
                "BIFFIX_CAPABILITY_SECRET_ARN is unset: engine ingest (POST /events) is disabled"
            );
            None
        }
    };
    let minter: Box<dyn Fn(&RunId) -> String + Send + Sync> = match &capability {
        Some(verifier) => verifier.minter(),
        // Without a signing secret a run cannot be given a usable ingest token. Emit an obviously
        // invalid one rather than a forgeable `cap-{run_id}`, so the failure is visible in ingest
        // logs instead of silently accepting anything.
        None => Box::new(|_: &RunId| "ingest-disabled".to_string()),
    };
    // Build the S3 client with request checksums set to `WhenRequired`. The SDK default
    // (`WhenSupported`) bakes a required `x-amz-checksum-crc32` header into a *presigned* PUT's
    // signature — which a plain HTTPS `PUT <body>` (how the CLI uploads a staged source tarball,
    // with no AWS SDK) cannot satisfy, so the upload would 403. `WhenRequired` keeps the presigned
    // PUT a simple body upload. None of the backend's other S3 calls send a body, so this is inert
    // for them.
    let s3 = aws_sdk_s3::Client::from_conf(
        aws_sdk_s3::config::Builder::from(&aws_cfg)
            .request_checksum_calculation(
                aws_sdk_s3::config::RequestChecksumCalculation::WhenRequired,
            )
            .build(),
    );
    let cloud = Arc::new(AwsCloudProvider::new(s3, aws_cfg.clone(), config.provider, minter));

    let deploy = config.deploy;
    // An HTTP entry point without it still serves the API (a CLI with a token works), but browser
    // and CLI sign-in cannot start: say which variables are missing at startup, not per request.
    if auth.is_some() {
        if let crate::auth::oauth_client::OAuthClient::Unconfigured(why) = &deploy.oauth_client {
            tracing::warn!(%why, "GET /auth/config will answer 503");
        }
    }

    let state = AppState {
        store,
        registry,
        events,
        cloud,
        auth,
        capability,
        deploy,
        projects,
        settings,
        references,
        experiments,
        identity,
    };
    Ok((state, pool))
}

/// Build Cognito RS256/JWKS authentication for an HTTP entry point. There is deliberately no
/// production HMAC fallback and no silent "auth off": `Ok(None)` only for the explicit
/// `BIFFIX_AUTH_DISABLED=true` opt-out (local development), which [`AuthConfig`] reads.
async fn build_auth_context(config: AuthConfig) -> Result<Option<Arc<AuthContext>>, BoxError> {
    let CognitoAuth { issuer, audiences, group_to_role, authenticated_default_role } = match config
    {
        AuthConfig::Cognito(cognito) => cognito,
        AuthConfig::Disabled => {
            tracing::warn!(
                "{AUTH_DISABLED_VAR}=true: authentication is OFF and every request is admitted as \
                 `local` with every capability; never set this on a deployed backend"
            );
            return Ok(None);
        }
    };
    let mut rbac = Rbac::new(group_to_role);
    if let Some(role) = authenticated_default_role {
        rbac = rbac.with_authenticated_default(role);
    }
    let verifier = JwtVerifier::cognito(&issuer)
        .await
        .map_err(|e| format!("BIFFIX_OIDC_ISSUER {issuer}: {e}"))?;

    Ok(Some(Arc::new(AuthContext { verifier, issuer, audiences, rbac: Arc::new(rbac) })))
}

/// The deploy's projects, from `source` (see [`ProjectsSource`]): read now and refreshed every
/// [`crate::config::projects::REFRESH_AFTER`].
async fn build_projects(
    aws_cfg: &aws_config::SdkConfig,
    source: ProjectsSource,
) -> Result<ProjectsHandle, BoxError> {
    let loader: Arc<dyn ProjectsLoader> = match source {
        ProjectsSource::Ssm { path, default_project } => {
            Arc::new(SsmProjectsLoader::new(aws_cfg, &path, &default_project))
        }
        ProjectsSource::File { path, default_project } => {
            Arc::new(FileProjectsLoader::new(path, default_project))
        }
    };
    let handle = ProjectsHandle::load(loader, crate::config::projects::REFRESH_AFTER)
        .await
        .map_err(|e| format!("project contexts: {e}"))?;
    let projects = handle.current().await;
    tracing::info!(
        projects = projects.ids().join(","),
        default = projects.default_project().id,
        "projects: loaded"
    );
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    /// A migration that fails on its first attempt only: the database-free routes are served
    /// without it, the first other request is a 503, the next migrates, and none migrates again.
    #[tokio::test]
    async fn the_database_free_routes_do_not_wait_and_a_failed_migration_is_retried_once() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let migrate = {
            let attempts = attempts.clone();
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    if attempt == 0 {
                        Err("down")
                    } else {
                        Ok(())
                    }
                }
            }
        };
        let router = super::migrate_first(
            axum::Router::new()
                .route("/auth/config", get(|| async { "config" }))
                .route("/healthz", get(|| async { "alive" }))
                .route("/runs", get(|| async { "runs" })),
            migrate,
        );
        let status = |path: &str| {
            let request = Request::get(path).body(axum::body::Body::empty()).unwrap();
            let router = router.clone();
            async move { router.oneshot(request).await.unwrap().status() }
        };
        assert_eq!(status("/auth/config").await, StatusCode::OK);
        assert_eq!(status("/healthz").await, StatusCode::OK);
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
        assert_eq!(status("/runs").await, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(status("/runs").await, StatusCode::OK);
        assert_eq!(status("/runs").await, StatusCode::OK);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
