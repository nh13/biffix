//! Command implementations and the shared session plumbing (config → credential → authed client).

pub mod health;
pub(crate) mod inspect;
pub(crate) mod login;
pub mod runs;
pub mod workflows;

use std::sync::Arc;

use crate::api::ApiClient;
use crate::auth::store::{CredentialStore, FileCredentialStore};
use crate::auth::{OAuthEndpoints, TokenSource};
use crate::config::{self, Config, DiscoveredConfig, Profile};
use crate::error::CliResult;
use crate::render::{OutputFormat, OutputMode};

// Global flags shared by every subcommand (flattened into `cli::Cli` and parsed once there).
// `Default` (all `None`/`false`, output `Auto`) backs the `..Default::default()` used in tests and
// matches clap's own defaults. A `//` comment, not a doc comment: clap would take a doc comment on
// a flattened struct as help text.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct GlobalArgs {
    /// Named deployment profile. Default: $BIFFIX_PROFILE, else the config's default_profile, else
    /// `default`.
    #[arg(long, global = true)]
    pub profile: Option<String>,
    /// API base URL, e.g. https://biffix.example.org/api. Default: $BIFFIX_API_URL, else the
    /// profile's (recorded at login).
    #[arg(long, global = true)]
    pub url: Option<String>,
    /// Output format.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub output: OutputFormat,
    /// Verbose diagnostics to stderr (method/URL/status; never secrets).
    #[arg(long, global = true)]
    pub verbose: bool,
    /// Allow a non-HTTPS API base URL (local dev only; BIFFIX_ALLOW_INSECURE=1 does the same).
    #[arg(long, global = true)]
    pub allow_insecure: bool,
}

/// A resolved session: the effective profile, validated API base URL, credential store, and HTTP
/// client — everything a command needs before touching the network.
pub struct Session {
    pub(crate) config: Config,
    pub(crate) profile_name: String,
    pub(crate) api_base: String,
    pub(crate) store: FileCredentialStore,
    /// The one HTTP client (connection pool) of the invocation, shared by every API call.
    pub(crate) http: reqwest::Client,
    pub(crate) output: OutputMode,
    pub(crate) verbose: bool,
}

impl Session {
    /// Build the session from global flags, resolving the profile and API base URL with full
    /// precedence and validating the base URL (HTTPS unless the local-dev opt-in is set).
    pub fn resolve(args: &GlobalArgs) -> CliResult<Self> {
        let allow_insecure = args.allow_insecure
            || std::env::var("BIFFIX_ALLOW_INSECURE").map(|v| v == "1").unwrap_or(false);
        let config = Config::load()?;
        let profile_name = config.effective_profile_name(args.profile.as_deref());
        let api_base = config::resolve_api_base_url(&config, &profile_name, args.url.as_deref())?;
        // Validate now so every command fails fast on a bad/insecure URL.
        config::validate_api_base(&api_base, allow_insecure)?;
        let store_path = FileCredentialStore::default_path()?;
        Ok(Self {
            config,
            profile_name,
            api_base,
            store: FileCredentialStore::new(store_path),
            http: reqwest::Client::new(),
            output: OutputMode::resolve(args.output),
            verbose: args.verbose,
        })
    }

    /// The current profile's cached config (may lack discovered OAuth endpoints).
    pub(crate) fn profile(&self) -> Profile {
        self.config.profile(&self.profile_name).cloned().unwrap_or_else(|| Profile {
            api_base_url: self.api_base.clone(),
            ..Default::default()
        })
    }

    /// Fetch `/auth/config` from this session's API, without persisting anything.
    pub(crate) async fn fetch_discovery(&self) -> CliResult<DiscoveredConfig> {
        ApiClient::discover(&self.http, &self.api_base, self.verbose).await
    }

    /// Record `discovered` (and this session's API base) as the profile's cached config.
    pub(crate) fn persist_discovery(&mut self, discovered: DiscoveredConfig) -> CliResult<()> {
        let mut profile = self.profile();
        profile.api_base_url = self.api_base.clone();
        profile.apply_discovery(discovered);
        let name = self.profile_name.clone();
        self.config = Config::update(|c| {
            c.profiles.insert(name, profile);
        })?;
        Ok(())
    }

    /// The profile's cached token endpoint + CLI client, when they are for this session's API.
    fn cached_oauth(&self) -> Option<OAuthEndpoints> {
        let p = self.config.profile(&self.profile_name)?;
        if crate::auth::store::normalize_base(&p.api_base_url)
            != crate::auth::store::normalize_base(&self.api_base)
        {
            return None;
        }
        Some(OAuthEndpoints {
            token_endpoint: p.token_endpoint.clone()?,
            client_id: p.cli_client_id.clone()?,
        })
    }

    /// Discover the OAuth endpoints and cache them into the profile.
    async fn discover_oauth(&mut self) -> CliResult<OAuthEndpoints> {
        let discovered = self.fetch_discovery().await?;
        let endpoints = OAuthEndpoints::try_from(&discovered)?;
        self.persist_discovery(discovered)?;
        Ok(endpoints)
    }

    /// Build an authorized API client whose bearer resolves through the one token choke point
    /// ([`TokenSource`]) for every request, so it is refreshed as the command runs.
    ///
    /// The credential is checked first: no credential is `AuthRequired` naming `biffix login`
    /// before any network call, and a credential issued for another API base is refused.
    /// Discovery happens only when a refresh is due and the endpoints are not cached.
    pub(crate) async fn authed_client(&mut self) -> CliResult<ApiClient> {
        let store: Arc<dyn CredentialStore> = Arc::new(self.store.clone());
        let oauth = self.cached_oauth();
        let cached = oauth.is_some();
        let mut source =
            TokenSource::load(store, &self.profile_name, &self.api_base, oauth, self.http.clone())?;
        source.set_verbose(self.verbose);
        if !cached && source.needs_refresh_now().await {
            // Cache the endpoints in the profile now, rather than rediscovering on every command.
            source.set_oauth(self.discover_oauth().await?);
        }
        Ok(ApiClient::with_token_source(self.http.clone(), &self.api_base, source, self.verbose))
    }
}
