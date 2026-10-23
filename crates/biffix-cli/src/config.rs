//! CLI configuration: profiles, precedence, and bootstrap discovery.
//!
//! Precedence for the API base URL and active profile is **flag → env → profile file**. A
//! non-HTTPS API base URL is refused unless an explicit local-dev opt-in is set; the only
//! permitted `http://` anywhere is the loopback OAuth redirect. OAuth endpoints are not configured
//! by hand — they are discovered from the deployment's own `GET /auth/config` and cached into the
//! profile (no built-in default deployment).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{CliError, CliResult};

/// The default profile name used when no default has ever been chosen.
pub(crate) const DEFAULT_PROFILE: &str = "default";

/// The profile `BIFFIX_PROFILE` names, when it is set and not empty.
pub(crate) fn env_profile() -> Option<String> {
    std::env::var("BIFFIX_PROFILE").ok().filter(|s| !s.is_empty())
}

/// The on-disk config: `~/.config/biffix/config.toml` (overridable via `BIFFIX_CONFIG_DIR`).
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Config {
    /// The profile bare commands resolve to. `None` means "never explicitly set" — resolution
    /// falls back to [`DEFAULT_PROFILE`]. Keeping it optional (rather than eagerly `"default"`)
    /// lets a first `login --profile X` adopt `X` as the default, so subsequent bare commands hit
    /// the profile the user actually authenticated (they otherwise resolved an empty "default").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default_profile: Option<String>,
    #[serde(default)]
    pub(crate) profiles: BTreeMap<String, Profile>,
}

/// One named deployment the CLI talks to. OAuth fields are discovered and cached; a fresh profile
/// has only `api_base_url`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Profile {
    /// e.g. `https://biffix.example.org/api`.
    pub(crate) api_base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) authorization_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cli_client_id: Option<String>,
    /// The registered loopback pool, cached from `/auth/config`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) cli_redirect_uris: Vec<String>,
}

/// The subset of `GET /auth/config` ([`biffix_api::OAuthClientConfig`]) the CLI caches into a
/// profile (bootstrap discovery). A subset rather than the shared type itself, so the CLI
/// does not fail to parse the response over a web-client field it never uses. It stays tied to the
/// shared type: the `From` impl below names each field, so a rename there is a compile error here,
/// and a test round-trips the shared type's JSON through this one.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DiscoveredConfig {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub cli_client_id: Option<String>,
    #[serde(default)]
    pub cli_redirect_uris: Vec<String>,
}

impl From<&biffix_api::OAuthClientConfig> for DiscoveredConfig {
    fn from(c: &biffix_api::OAuthClientConfig) -> Self {
        let biffix_api::OAuthClientConfig {
            authorization_endpoint,
            token_endpoint,
            cli_client_id,
            cli_redirect_uris,
            ..
        } = c;
        Self {
            authorization_endpoint: authorization_endpoint.clone(),
            token_endpoint: token_endpoint.clone(),
            cli_client_id: cli_client_id.clone(),
            cli_redirect_uris: cli_redirect_uris.clone(),
        }
    }
}

impl Profile {
    /// Merge a discovered `/auth/config` payload into this profile (endpoints + CLI client).
    pub(crate) fn apply_discovery(&mut self, discovered: DiscoveredConfig) {
        self.authorization_endpoint = Some(discovered.authorization_endpoint);
        self.token_endpoint = Some(discovered.token_endpoint);
        if discovered.cli_client_id.is_some() {
            self.cli_client_id = discovered.cli_client_id;
        }
        if !discovered.cli_redirect_uris.is_empty() {
            self.cli_redirect_uris = discovered.cli_redirect_uris;
        }
    }
}

impl DiscoveredConfig {
    /// Refuse an OAuth endpoint the deployment advertised over anything but HTTPS: the code and
    /// the refresh token are sent there. Plain HTTP only from a deployment reached over HTTP itself
    /// (`api_base`), which the local-dev opt-in already allowed.
    pub(crate) fn validate(&self, api_base: &str) -> CliResult<()> {
        let insecure_base = Url::parse(api_base).is_ok_and(|u| u.scheme() == "http");
        for (what, raw) in [
            ("authorization endpoint", &self.authorization_endpoint),
            ("token endpoint", &self.token_endpoint),
        ] {
            let scheme = Url::parse(raw).map(|u| u.scheme().to_string());
            match scheme.as_deref() {
                Ok("https") => {}
                Ok("http") if insecure_base => {}
                _ => {
                    return Err(CliError::Unclassified(format!(
                        "{api_base}/auth/config advertises the {what} {raw:?}, which is not an \
                         HTTPS URL; refusing to send credentials there"
                    )))
                }
            }
        }
        Ok(())
    }
}

/// Parse and validate an API base URL. Refuse non-HTTPS unless the local-dev opt-in is set.
pub(crate) fn validate_api_base(raw: &str, allow_insecure: bool) -> CliResult<Url> {
    let url = Url::parse(raw)
        .map_err(|e| CliError::InvalidInput(format!("invalid API base URL {raw:?}: {e}")))?;
    match url.scheme() {
        "https" => Ok(url),
        "http" if allow_insecure => Ok(url),
        "http" => Err(CliError::InvalidInput(format!(
            "refusing a non-HTTPS API base URL ({raw}); pass --allow-insecure (or set \
             BIFFIX_ALLOW_INSECURE=1) for local dev"
        ))),
        other => Err(CliError::InvalidInput(format!(
            "unsupported API base URL scheme {other:?} in {raw}"
        ))),
    }
}

/// The config directory: `$BIFFIX_CONFIG_DIR` if set, else `$XDG_CONFIG_HOME/biffix`, else
/// `~/.config/biffix` — on macOS too, not `~/Library/Application Support` (see [`dir_from`]).
pub(crate) fn config_dir() -> CliResult<PathBuf> {
    let home = std::env::home_dir().filter(|h| !h.as_os_str().is_empty());
    dir_from(std::env::var_os("BIFFIX_CONFIG_DIR"), std::env::var_os("XDG_CONFIG_HOME"), home)
        .ok_or_else(|| CliError::Unclassified("cannot determine a home directory".into()))
}

/// [`config_dir`] from its inputs: a non-empty `biffix_dir`, else an absolute `xdg_config_home`
/// joined with `biffix` (a relative one is ignored, as the XDG spec says), else
/// `home/.config/biffix`. The same on every platform, so the documented location is the one used.
fn dir_from(
    biffix_dir: Option<std::ffi::OsString>,
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(dir) = biffix_dir.filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if let Some(xdg) = xdg_config_home.map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Some(xdg.join("biffix"));
    }
    home.map(|h| h.join(".config").join("biffix"))
}

pub(crate) fn config_path() -> CliResult<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

impl Config {
    /// Load the config file, or a default empty config if it does not exist.
    pub(crate) fn load() -> CliResult<Self> {
        let path = config_path()?;
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                CliError::InvalidInput(format!("invalid config at {}: {e}", path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(CliError::Unclassified(format!(
                "cannot read config at {}: {e}",
                path.display()
            ))),
        }
    }

    /// Apply `edit` to the config file as it is on disk now, and persist the result: the
    /// read-modify-write holds an advisory lock, and the write is atomic (temp file + rename), so
    /// two `biffix` processes updating different profiles never drop each other's change and a
    /// crash never leaves a truncated file. Returns the updated config.
    pub(crate) fn update(edit: impl FnOnce(&mut Config)) -> CliResult<Config> {
        let path = config_path()?;
        let _lock = crate::fsutil::FileLock::acquire(&path)
            .map_err(|e| CliError::Unclassified(format!("cannot lock {}: {e}", path.display())))?;
        let mut config = Self::load()?;
        edit(&mut config);
        let text = toml::to_string_pretty(&config)
            .map_err(|e| CliError::Unclassified(format!("cannot serialize config: {e}")))?;
        crate::fsutil::write_atomic(&path, text.as_bytes(), false)
            .map_err(|e| CliError::Unclassified(format!("cannot write {}: {e}", path.display())))?;
        Ok(config)
    }

    /// The effective profile name: flag → `BIFFIX_PROFILE` env → the file's `default_profile` →
    /// [`DEFAULT_PROFILE`].
    pub(crate) fn effective_profile_name(&self, flag: Option<&str>) -> String {
        self.profile_name_from(flag, env_profile())
    }

    /// [`Self::effective_profile_name`] given `$BIFFIX_PROFILE` (`env`): the flag, else `env`,
    /// else the config's `default_profile`, else [`DEFAULT_PROFILE`].
    fn profile_name_from(&self, flag: Option<&str>, env: Option<String>) -> String {
        flag.map(str::to_string)
            .or(env)
            .or_else(|| self.default_profile.clone())
            .unwrap_or_else(|| DEFAULT_PROFILE.to_string())
    }

    pub(crate) fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }
}

/// Resolve the API base URL with full precedence: `url_flag` → `BIFFIX_API_URL` → profile file.
/// Returns the raw string; callers validate via [`validate_api_base`]. Errors if none is set.
pub(crate) fn resolve_api_base_url(
    config: &Config,
    profile_name: &str,
    url_flag: Option<&str>,
) -> CliResult<String> {
    if let Some(u) = url_flag {
        return Ok(u.to_string());
    }
    if let Ok(u) = std::env::var("BIFFIX_API_URL") {
        if !u.is_empty() {
            return Ok(u);
        }
    }
    config
        .profile(profile_name)
        .map(|p| p.api_base_url.clone())
        .filter(|u| !u.is_empty())
        .ok_or_else(|| {
            CliError::InvalidInput(format!(
                "no API URL for profile {profile_name:?}; pass --url, set BIFFIX_API_URL, or run \
                 `biffix login --url <https://…>`"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_dir_is_the_documented_one() {
        let home = || Some(PathBuf::from("/home/u"));
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        // `~/.config/biffix` on every unix, macOS included.
        assert_eq!(dir_from(None, None, home()), Some(PathBuf::from("/home/u/.config/biffix")));
        assert_eq!(dir_from(None, os("/xdg"), home()), Some(PathBuf::from("/xdg/biffix")));
        // A relative XDG_CONFIG_HOME is ignored; an empty BIFFIX_CONFIG_DIR is unset.
        assert_eq!(
            dir_from(os(""), os("rel"), home()),
            Some(PathBuf::from("/home/u/.config/biffix"))
        );
        assert_eq!(dir_from(os("/mine"), os("/xdg"), home()), Some(PathBuf::from("/mine")));
        assert_eq!(dir_from(None, None, None), None);
    }

    #[test]
    fn discovered_endpoints_are_https_unless_the_deployment_is_not() {
        let discovered = |token: &str| DiscoveredConfig {
            authorization_endpoint: "https://auth.example/oauth2/authorize".into(),
            token_endpoint: token.into(),
            cli_client_id: None,
            cli_redirect_uris: vec![],
        };
        let https = "https://x.example/api";
        discovered("https://auth.example/oauth2/token").validate(https).unwrap();
        let err = discovered("http://auth.example/oauth2/token").validate(https).unwrap_err();
        assert!(
            matches!(&err, CliError::Unclassified(m) if m.contains("not an HTTPS URL")),
            "{err:?}"
        );
        // A deployment reached over HTTP (the local-dev opt-in) may advertise HTTP endpoints.
        discovered("http://localhost:9/oauth2/token")
            .validate("http://localhost:8080/api")
            .unwrap();
    }

    #[test]
    fn https_required_unless_insecure_opt_in() {
        assert!(validate_api_base("https://x.example/api", false).is_ok());
        assert!(validate_api_base("http://localhost:8080/api", true).is_ok());
        // Each refusal is its own: the HTTPS one names both ways to opt in.
        let err = validate_api_base("http://x.example/api", false).unwrap_err();
        assert!(
            matches!(&err, CliError::InvalidInput(m) if m.contains("refusing a non-HTTPS")
                && m.contains("--allow-insecure") && m.contains("BIFFIX_ALLOW_INSECURE=1")),
            "{err:?}"
        );
        let err = validate_api_base("ftp://x/api", true).unwrap_err();
        assert!(
            matches!(&err, CliError::InvalidInput(m) if m.contains("unsupported API base URL scheme")),
            "{err:?}"
        );
    }

    #[test]
    fn url_flag_beats_env_beats_profile() {
        let mut config = Config::default();
        config.profiles.insert(
            "default".into(),
            Profile { api_base_url: "https://profile.example/api".into(), ..Default::default() },
        );
        // This is the only test in this module that touches BIFFIX_API_URL, so mutating it in a
        // single sequence here is race-free even under the default multi-threaded test runner.

        // Env beats profile.
        std::env::set_var("BIFFIX_API_URL", "https://env.example/api");
        assert_eq!(
            resolve_api_base_url(&config, "default", None).unwrap(),
            "https://env.example/api"
        );
        // Flag beats env (env still set).
        assert_eq!(
            resolve_api_base_url(&config, "default", Some("https://flag.example/api")).unwrap(),
            "https://flag.example/api"
        );
        // Profile is the fallback when neither flag nor env is set.
        std::env::remove_var("BIFFIX_API_URL");
        assert_eq!(
            resolve_api_base_url(&config, "default", None).unwrap(),
            "https://profile.example/api"
        );
    }

    #[test]
    fn discovery_populates_endpoints() {
        let mut profile =
            Profile { api_base_url: "https://x.example/api".into(), ..Default::default() };
        profile.apply_discovery(DiscoveredConfig {
            authorization_endpoint: "https://auth.example/oauth2/authorize".into(),
            token_endpoint: "https://auth.example/oauth2/token".into(),
            cli_client_id: Some("cli-123".into()),
            cli_redirect_uris: vec!["http://localhost:8976/callback".into()],
        });
        let expect = |profile: &Profile| {
            assert_eq!(
                profile.authorization_endpoint.as_deref(),
                Some("https://auth.example/oauth2/authorize")
            );
            assert_eq!(
                profile.token_endpoint.as_deref(),
                Some("https://auth.example/oauth2/token")
            );
            assert_eq!(profile.cli_client_id.as_deref(), Some("cli-123"));
            assert_eq!(
                profile.cli_redirect_uris,
                vec!["http://localhost:8976/callback".to_string()]
            );
        };
        expect(&profile);
        // A later discovery that leaves out the CLI client keeps the one already recorded.
        profile.apply_discovery(DiscoveredConfig {
            authorization_endpoint: "https://auth.example/oauth2/authorize".into(),
            token_endpoint: "https://auth.example/oauth2/token".into(),
            cli_client_id: None,
            cli_redirect_uris: vec![],
        });
        expect(&profile);
    }

    #[test]
    fn the_profile_is_the_flag_then_the_environment_then_the_config_s_default() {
        let mut config = Config::default();
        let env = || Some("from-env".to_string());
        assert_eq!(config.profile_name_from(None, None), DEFAULT_PROFILE);
        config.default_profile = Some("from-config".into());
        assert_eq!(config.profile_name_from(None, None), "from-config");
        assert_eq!(config.profile_name_from(None, env()), "from-env");
        assert_eq!(config.profile_name_from(Some("from-flag"), env()), "from-flag");
    }

    #[test]
    fn the_shared_auth_config_parses_into_the_cli_subset() {
        // What the server serializes (the shared type) is what the CLI reads: a serde rename on
        // either side would drop a field to its default and fail this comparison.
        let full = biffix_api::OAuthClientConfig {
            authorization_endpoint: "https://auth.example/oauth2/authorize".into(),
            token_endpoint: "https://auth.example/oauth2/token".into(),
            client_id: "web".into(),
            redirect_uri: "https://x.example/auth/callback".into(),
            logout_endpoint: "https://auth.example/logout".into(),
            logout_uri: "https://x.example/signed-out".into(),
            scopes: vec!["openid".into()],
            cli_client_id: Some("cli-123".into()),
            cli_redirect_uris: vec!["http://localhost:8976/callback".into()],
            user_pool_id: None,
            region: None,
        };
        let parsed: DiscoveredConfig =
            serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
        assert_eq!(parsed, DiscoveredConfig::from(&full));
        assert_eq!(parsed.cli_client_id.as_deref(), Some("cli-123"));
    }
}
