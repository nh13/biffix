//! The backend entry points' configuration, read once from the environment at startup (through
//! [`biffix_core::env`], so every binary parses a shape one way) and passed down; nothing else in
//! the crate reads the environment.
//!
//! - [`BackendConfig`]: what every entry point reads (the database, the projects, the AWS provider,
//!   the capability secret and the deploy settings handlers read). The sweep Lambda reads only it.
//! - [`ApiConfig`]: an HTTP entry point's (the API Lambda, the `server` bin): the above plus its
//!   authentication ([`AuthConfig`]).
//!
//! The variable names are a contract with the CDK that sets them
//! (`infra/biffix_infra/constructs/{backend_api,backend_sweep,control_plane}.py`,
//! `infra/biffix_infra/stacks/api_stack.py`); `tests/it/config_env.rs` holds each Lambda to it.
//!
//! The shared rules: a value is trimmed and a blank optional value is unset. Kept as they were, so
//! no value the backend accepted is refused:
//! - `BIFFIX_COORDINATOR_QUEUE`, `BIFFIX_WORKER_IMAGE_REPO`, `BIFFIX_INGEST_URL`,
//!   `BIFFIX_BUILDS_IMAGE_REPO`, `BIFFIX_DEFAULT_PROJECT` (with `BIFFIX_PROJECTS_PATH`) and
//!   `BIFFIX_OIDC_AUDIENCE` must be set but are taken as set, blank included ([`Env::present`]);
//!   a blank (or whitespace) `BIFFIX_BUILDS_IMAGE_REPO` or an audience list naming no client is
//!   still refused. The local `server` relies on the others being blank (`dev/README.md`).
//! - `BIFFIX_CAPABILITY_SECRET_ARN` set but blank still fails startup (fetching it) rather than
//!   turning engine ingest off, as an unset one does.

use std::collections::BTreeMap;

use biffix_core::env::{ConfigErrors, DbConfig, Env, EnvError, Errors};
use biffix_workflow::images::Arch;

use crate::auth::oauth_client::OAuthClient;
use crate::auth::rbac::{parse_group_role_map, Role, RoleMapError};
use crate::auth::{accepted_audiences, AuthMode, AuthModeError, AUTH_DISABLED_VAR};
use crate::DeployConfig;

/// One configuration error, naming its variable.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(transparent)]
    Env(#[from] EnvError),
    #[error(transparent)]
    Auth(#[from] AuthModeError),
    #[error(transparent)]
    RoleMap(#[from] RoleMapError),
}

/// An entry point's startup errors, every one at once.
pub type StartupErrors = ConfigErrors<ConfigError>;

/// The variable an image-building CodeBuild project for `arch` is named in
/// (`BIFFIX_IMAGE_BUILD_PROJECT_<ARCH>`).
pub(crate) fn image_build_project_var(arch: Arch) -> String {
    format!("BIFFIX_IMAGE_BUILD_PROJECT_{}", arch.as_str().to_ascii_uppercase())
}

/// What every backend entry point reads.
#[derive(Debug, Default)]
pub struct BackendConfig {
    /// The database (`DATABASE_URL` or `BIFFIX_DB_SECRET_ARN`).
    pub(crate) db: DbConfig,
    /// The Cognito issuer (`BIFFIX_OIDC_ISSUER`), for resolving a submitter's name and email;
    /// `None` (auth disabled, a non-Cognito local run) resolves nothing.
    pub(crate) oidc_issuer: Option<String>,
    /// Where the deploy's project contexts are.
    pub(crate) projects: ProjectsSource,
    /// What the AWS provider needs.
    pub(crate) provider: ProviderConfig,
    /// The capability signing key's secret (`BIFFIX_CAPABILITY_SECRET_ARN`); `None` (unset)
    /// disables engine ingest.
    pub(crate) capability_secret_arn: Option<String>,
    /// The deploy settings handlers read.
    pub(crate) deploy: DeployConfig,
}

/// Where the deploy's project contexts are read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectsSource {
    /// One SSM parameter per project under `path` (`BIFFIX_PROJECTS_PATH`), the default named by
    /// `default_project` (`BIFFIX_DEFAULT_PROJECT`, required with it). What a deploy sets.
    Ssm { path: String, default_project: String },
    /// The contexts in the JSON file `path` (`BIFFIX_PROJECTS_FILE`), the default
    /// `BIFFIX_DEFAULT_PROJECT` when set, else the file's first. Outside a deploy (the `server`
    /// bin, local development).
    File { path: String, default_project: Option<String> },
}

impl Default for ProjectsSource {
    fn default() -> Self {
        Self::File { path: String::new(), default_project: None }
    }
}

impl ProjectsSource {
    fn from_env(env: &Env) -> Result<Self, EnvError> {
        let path = env.string("BIFFIX_PROJECTS_PATH");
        let file = env.string("BIFFIX_PROJECTS_FILE");
        match (path, file) {
            (Some(path), _) => {
                Ok(Self::Ssm { path, default_project: env.present("BIFFIX_DEFAULT_PROJECT")? })
            }
            (None, Some(path)) => {
                Ok(Self::File { path, default_project: env.string("BIFFIX_DEFAULT_PROJECT") })
            }
            (None, None) => {
                // Read, so every entry point lists the variable whichever source it uses.
                env.raw("BIFFIX_DEFAULT_PROJECT");
                Err(EnvError::MissingOneOf {
                    vars: vec!["BIFFIX_PROJECTS_PATH".into(), "BIFFIX_PROJECTS_FILE".into()],
                    why: "the deploy's SSM project contexts, or a JSON file of them".into(),
                })
            }
        }
    }
}

/// What the AWS provider needs, supplied by the deploy.
///
/// Nothing here is per project: a run's project (bucket, roles, coordinator template, task log
/// group) is the caller's, resolved per request (`crate::config::projects`).
#[derive(Clone, Debug, Default)]
pub(crate) struct ProviderConfig {
    /// The deployed base coordinator image, `<repo>:<tag>` (`BIFFIX_COORDINATOR_IMAGE`); `None`
    /// where no variants are built.
    pub(crate) coordinator_image: Option<String>,
    /// The CodeBuild project that builds coordinator variants (`BIFFIX_RUNTIME_BUILD_PROJECT`).
    pub(crate) runtime_build_project: Option<String>,
    /// The CodeBuild project that imports workflow images (`BIFFIX_IMAGE_IMPORT_PROJECT`).
    pub(crate) image_import_project: Option<String>,
    /// The CodeBuild projects that build workflow images from their recipes, per architecture
    /// (`BIFFIX_IMAGE_BUILD_PROJECT_<ARCH>`).
    pub(crate) image_build_projects: BTreeMap<Arch, String>,
    /// The name prefix of variant job definitions (IAM allows registering only these;
    /// `BIFFIX_VARIANT_JOB_DEFINITION_PREFIX`).
    pub(crate) variant_job_definition_prefix: Option<String>,
    /// The tenant ECR repository URI (no tag) that holds workflow worker images
    /// (`BIFFIX_WORKER_IMAGE_REPO`): the repository of a run's worker image
    /// (`<repo>@<version.image_digest>`) when its version recorded none
    /// ([`crate::aws::batch::CoordinatorLaunch::worker_image_repo_uri`] is `None`).
    pub(crate) worker_image_repo: String,
    /// Ingest URL passed to the coordinator for POSTing RunEvents (`BIFFIX_INGEST_URL`).
    pub(crate) ingest_url: String,
    /// The operational alerts SNS topic whose email subscribers the Settings page manages
    /// (`BIFFIX_ALERT_TOPIC_ARN`); `None` when the deploy has none.
    pub(crate) alert_topic_arn: Option<String>,
}

impl BackendConfig {
    /// From the process environment.
    pub fn from_process_env() -> Result<Self, StartupErrors> {
        Self::from_env(&Env::process())
    }

    /// From `env`.
    pub fn from_env(env: &Env) -> Result<Self, StartupErrors> {
        let mut errors = Errors::default();
        let config = Self::read(env, &mut errors);
        errors.finish(config)
    }

    fn read(env: &Env, errors: &mut Errors<ConfigError>) -> Self {
        let worker_image_repo = errors.take(env.present("BIFFIX_WORKER_IMAGE_REPO"));
        let provider = ProviderConfig {
            coordinator_image: env.string("BIFFIX_COORDINATOR_IMAGE"),
            runtime_build_project: env.string("BIFFIX_RUNTIME_BUILD_PROJECT"),
            image_import_project: env.string("BIFFIX_IMAGE_IMPORT_PROJECT"),
            image_build_projects: Arch::ALL
                .into_iter()
                .filter_map(|arch| Some((arch, env.string(&image_build_project_var(arch))?)))
                .collect(),
            variant_job_definition_prefix: env.string("BIFFIX_VARIANT_JOB_DEFINITION_PREFIX"),
            worker_image_repo: worker_image_repo.clone(),
            ingest_url: errors.take(env.present("BIFFIX_INGEST_URL")),
            alert_topic_arn: env.string("BIFFIX_ALERT_TOPIC_ARN"),
        };
        // The queue-class menu (registration validation) and the coordinator queue (submit). The
        // menu is a comma-separated list of worker queue-classes; the coordinator queue is a single
        // on-demand queue.
        let deploy = DeployConfig {
            coordinator_queue: errors.take(env.present("BIFFIX_COORDINATOR_QUEUE")),
            queue_classes: env.list("BIFFIX_QUEUE_CLASSES"),
            worker_image_repo,
            // Required and non-empty: an empty one would trust a built image from any repository.
            builds_image_repo: errors.take(env.present("BIFFIX_BUILDS_IMAGE_REPO").and_then(
                |repo| {
                    // Whitespace alone is as empty as nothing.
                    let repo = repo.trim();
                    if repo.is_empty() {
                        Err(EnvError::rejected("BIFFIX_BUILDS_IMAGE_REPO", "it is empty"))
                    } else {
                        Ok(repo.to_string())
                    }
                },
            )),
            queue_capacities: errors.take(crate::config::queue_capacity::parse_queue_capacities(
                env.raw(crate::config::queue_capacity::VAR).unwrap_or_default(),
            )),
            // Retention is a control, not a nicety: a malformed value fails startup (exactly as
            // the reconcile Lambda does) rather than silently turning retention off. Unset or
            // blank = none. Read as 32 bits, as the launch form shows it.
            retention_days: errors.take(env.u32("BIFFIX_RETENTION_DAYS")),
            oauth_client: OAuthClient::from_lookup(|name| env.raw(name).map(str::to_owned)),
        };
        Self {
            db: errors.take(DbConfig::from_env(env)),
            oidc_issuer: env.string("BIFFIX_OIDC_ISSUER"),
            projects: errors.take(ProjectsSource::from_env(env)),
            provider,
            // As set, blank included: see the module docs.
            capability_secret_arn: env.raw("BIFFIX_CAPABILITY_SECRET_ARN").map(str::to_owned),
            deploy,
        }
    }
}

/// An HTTP entry point's configuration (the API Lambda, the `server` bin).
#[derive(Debug)]
pub struct ApiConfig {
    pub(crate) backend: BackendConfig,
    pub(crate) auth: AuthConfig,
}

impl ApiConfig {
    /// From the process environment.
    pub fn from_process_env() -> Result<Self, StartupErrors> {
        Self::from_env(&Env::process())
    }

    /// Whether authentication is off (`BIFFIX_AUTH_DISABLED=true`): every request is admitted with
    /// every capability.
    pub fn auth_disabled(&self) -> bool {
        matches!(self.auth, AuthConfig::Disabled)
    }

    /// From `env`.
    pub fn from_env(env: &Env) -> Result<Self, StartupErrors> {
        let mut errors = Errors::default();
        let auth = AuthConfig::read(env, &mut errors);
        let backend = BackendConfig::read(env, &mut errors);
        errors.finish(Self { backend, auth })
    }
}

/// How an HTTP entry point authenticates. **Fail-closed:** it authenticates with Cognito, and
/// refuses to start without `BIFFIX_OIDC_ISSUER` unless `BIFFIX_AUTH_DISABLED=true` explicitly
/// turns auth off.
#[derive(Debug)]
pub(crate) enum AuthConfig {
    /// `BIFFIX_AUTH_DISABLED=true` (local development): no other auth variable is read.
    Disabled,
    Cognito(CognitoAuth),
}

/// Cognito RS256/JWKS authentication settings.
#[derive(Debug, Default)]
pub(crate) struct CognitoAuth {
    /// The user pool (`BIFFIX_OIDC_ISSUER`).
    pub(crate) issuer: String,
    /// The accepted app clients: the web client (`BIFFIX_OIDC_AUDIENCE`, required) plus the
    /// optional native CLI client (`BIFFIX_CLI_CLIENT_ID`). Never empty.
    pub(crate) audiences: Vec<String>,
    /// IdP group → role (`BIFFIX_GROUP_ROLE_MAP`, a JSON object); empty when unset.
    pub(crate) group_to_role: BTreeMap<String, Role>,
    /// The role of a signed-in user in no mapped group (`BIFFIX_AUTHENTICATED_DEFAULT_ROLE`).
    pub(crate) authenticated_default_role: Option<Role>,
}

impl AuthConfig {
    fn read(env: &Env, errors: &mut Errors<ConfigError>) -> Self {
        let mode =
            AuthMode::from_env_values(env.raw("BIFFIX_OIDC_ISSUER"), env.raw(AUTH_DISABLED_VAR));
        let issuer = match mode {
            Ok(AuthMode::Disabled) => return Self::Disabled,
            Ok(AuthMode::Cognito { issuer }) => issuer,
            // Read the rest anyway, to report every error at once.
            Err(e) => {
                errors.push(e);
                String::new()
            }
        };
        let audience_var = "BIFFIX_OIDC_AUDIENCE";
        let cli_client = env.raw("BIFFIX_CLI_CLIENT_ID");
        let audiences = errors.take(env.present(audience_var).and_then(|web| {
            let audiences = accepted_audiences(&web, cli_client);
            if audiences.is_empty() {
                Err(EnvError::rejected(audience_var, "it names no app client id"))
            } else {
                Ok(audiences)
            }
        }));
        let group_to_role = errors.take(
            env.string("BIFFIX_GROUP_ROLE_MAP").map(|raw| parse_group_role_map(&raw)).transpose(),
        );
        let authenticated_default_role = errors.take(env.parse::<Role>(
            "BIFFIX_AUTHENTICATED_DEFAULT_ROLE",
            "one of viewer, operator, developer, admin",
        ));
        Self::Cognito(CognitoAuth {
            issuer,
            audiences,
            group_to_role: group_to_role.unwrap_or_default(),
            authenticated_default_role,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        Env::from_pairs(pairs.iter().copied())
    }

    /// What a deploy sets for the sweep Lambda (and, with auth, the API Lambda).
    fn deploy() -> Vec<(&'static str, &'static str)> {
        vec![
            ("BIFFIX_DB_SECRET_ARN", "arn:db"),
            ("BIFFIX_PROJECTS_PATH", "/biffix/t/projects"),
            ("BIFFIX_DEFAULT_PROJECT", "demo"),
            ("BIFFIX_WORKER_IMAGE_REPO", "1.dkr.ecr.r.amazonaws.com/workers"),
            ("BIFFIX_INGEST_URL", "https://api"),
            ("BIFFIX_COORDINATOR_QUEUE", "arn:queue"),
            ("BIFFIX_BUILDS_IMAGE_REPO", "1.dkr.ecr.r.amazonaws.com/builds"),
            ("BIFFIX_IMAGE_BUILD_PROJECT_AMD64", "build-amd64"),
            ("BIFFIX_COORDINATOR_IMAGE", " "),
        ]
    }

    fn api() -> Vec<(&'static str, &'static str)> {
        let mut vars = deploy();
        vars.extend([
            ("BIFFIX_OIDC_ISSUER", "https://cognito-idp.r.amazonaws.com/r_Pool"),
            ("BIFFIX_OIDC_AUDIENCE", "web"),
            ("BIFFIX_CLI_CLIENT_ID", "cli"),
            ("BIFFIX_GROUP_ROLE_MAP", r#"{"admins":"admin"}"#),
            ("BIFFIX_AUTHENTICATED_DEFAULT_ROLE", "viewer"),
        ]);
        vars
    }

    #[test]
    fn a_deploy_s_environment_configures_the_backend() {
        let config = BackendConfig::from_env(&env(&deploy())).unwrap();
        assert_eq!(config.db, DbConfig::Secret("arn:db".into()));
        assert_eq!(
            config.projects,
            ProjectsSource::Ssm {
                path: "/biffix/t/projects".into(),
                default_project: "demo".into()
            }
        );
        assert_eq!(config.provider.coordinator_image, None);
        assert_eq!(
            config.provider.image_build_projects.values().collect::<Vec<_>>(),
            ["build-amd64"]
        );
        assert_eq!(config.deploy.coordinator_queue, "arn:queue");
        assert_eq!(config.deploy.worker_image_repo, config.provider.worker_image_repo);
        assert_eq!(config.deploy.retention_days, None);
        assert!(config.deploy.queue_classes.is_empty());
        assert_eq!(config.capability_secret_arn, None);
        assert!(matches!(config.deploy.oauth_client, OAuthClient::Unconfigured(_)));
    }

    #[test]
    fn every_startup_error_is_reported_at_once() {
        let errors = BackendConfig::from_env(&env(&[
            ("BIFFIX_BUILDS_IMAGE_REPO", ""),
            ("BIFFIX_RETENTION_DAYS", "4294967296"),
            ("BIFFIX_QUEUE_CAPACITIES", "not json"),
        ]))
        .unwrap_err()
        .to_string();
        for var in [
            "BIFFIX_WORKER_IMAGE_REPO",
            "BIFFIX_INGEST_URL",
            "BIFFIX_COORDINATOR_QUEUE",
            "BIFFIX_BUILDS_IMAGE_REPO: it is empty",
            "BIFFIX_RETENTION_DAYS",
            "BIFFIX_QUEUE_CAPACITIES",
            "DATABASE_URL or BIFFIX_DB_SECRET_ARN",
            "BIFFIX_PROJECTS_PATH or BIFFIX_PROJECTS_FILE",
        ] {
            assert!(errors.contains(var), "{var}: {errors}");
        }
        // A builds repository of whitespace alone trusts nothing: refused as empty too.
        let errors = BackendConfig::from_env(&env(&[("BIFFIX_BUILDS_IMAGE_REPO", "  ")]))
            .unwrap_err()
            .to_string();
        assert!(errors.contains("BIFFIX_BUILDS_IMAGE_REPO: it is empty"), "{errors}");
    }

    #[test]
    fn a_projects_file_outside_a_deploy() {
        let source = |pairs: &[(&str, &str)]| ProjectsSource::from_env(&env(pairs));
        assert_eq!(
            source(&[("BIFFIX_PROJECTS_FILE", "p.json")]).unwrap(),
            ProjectsSource::File { path: "p.json".into(), default_project: None }
        );
        // The SSM path wins, and needs its default.
        let err = source(&[("BIFFIX_PROJECTS_PATH", "/p"), ("BIFFIX_PROJECTS_FILE", "p.json")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("BIFFIX_DEFAULT_PROJECT"), "{err}");
        assert!(matches!(
            source(&[
                ("BIFFIX_PROJECTS_PATH", "/p"),
                ("BIFFIX_PROJECTS_FILE", "p.json"),
                ("BIFFIX_DEFAULT_PROJECT", "a"),
            ]),
            Ok(ProjectsSource::Ssm { .. })
        ));
    }

    #[test]
    fn an_api_entry_point_configures_cognito() {
        let config = ApiConfig::from_env(&env(&api())).unwrap();
        let AuthConfig::Cognito(auth) = config.auth else {
            panic!("cognito");
        };
        assert_eq!(auth.audiences, ["web", "cli"]);
        assert_eq!(auth.group_to_role["admins"], Role::Admin);
        assert_eq!(auth.authenticated_default_role, Some(Role::Viewer));
        assert_eq!(
            config.backend.oidc_issuer.as_deref(),
            Some("https://cognito-idp.r.amazonaws.com/r_Pool")
        );
    }

    /// The production auth composition fails closed: with neither `BIFFIX_OIDC_ISSUER` nor the
    /// explicit opt-out set, it refuses to start rather than silently turning auth off.
    #[test]
    fn an_api_entry_point_refuses_to_start_without_an_issuer() {
        let errors = ApiConfig::from_env(&env(&deploy())).unwrap_err().to_string();
        assert!(errors.contains("BIFFIX_OIDC_ISSUER is unset"), "{errors}");
        assert!(errors.contains(AUTH_DISABLED_VAR), "{errors}");
        // Auth explicitly off reads no other auth variable.
        let mut vars = deploy();
        vars.extend([(AUTH_DISABLED_VAR, "true"), ("BIFFIX_GROUP_ROLE_MAP", "[")]);
        let config = ApiConfig::from_env(&env(&vars)).unwrap();
        assert!(matches!(config.auth, AuthConfig::Disabled));
    }

    #[test]
    fn auth_values_keep_what_they_accepted() {
        let with = |extra: &[(&'static str, &'static str)]| {
            let mut vars = api();
            vars.extend_from_slice(extra);
            ApiConfig::from_env(&env(&vars))
        };
        let error =
            with(&[("BIFFIX_AUTHENTICATED_DEFAULT_ROLE", "amdin")]).unwrap_err().to_string();
        assert!(error.contains("BIFFIX_AUTHENTICATED_DEFAULT_ROLE"), "{error}");
        let error = with(&[("BIFFIX_OIDC_AUDIENCE", " , "), ("BIFFIX_CLI_CLIENT_ID", "")])
            .unwrap_err()
            .to_string();
        assert!(error.contains("names no app client id"), "{error}");
    }
}
