//! The deploy's public OAuth client configuration (what `GET /auth/config` serves the browser and
//! the CLI), read once with the entry point's config (`crate::config::env`) rather than per
//! request.

use biffix_api::OAuthClientConfig;

/// The deploy variables an OAuth client configuration cannot do without.
pub(crate) const REQUIRED_VARS: [&str; 6] = [
    "BIFFIX_OAUTH_AUTHORIZATION_ENDPOINT",
    "BIFFIX_OAUTH_TOKEN_ENDPOINT",
    "BIFFIX_OIDC_AUDIENCE",
    "BIFFIX_OAUTH_REDIRECT_URI",
    "BIFFIX_OAUTH_LOGOUT_ENDPOINT",
    "BIFFIX_OAUTH_LOGOUT_URI",
];

/// The OAuth client configuration, or why the deploy has none.
#[derive(Clone, Debug, PartialEq)]
pub enum OAuthClient {
    Configured(Box<OAuthClientConfig>),
    /// What is missing, for `GET /auth/config`'s 503 (a deploy without a Cognito hosted-UI domain
    /// sets no hosted-UI endpoints, so browser sign-in is off rather than the API refusing to start).
    Unconfigured(String),
}

impl Default for OAuthClient {
    fn default() -> Self {
        Self::Unconfigured("this deploy configures no OAuth client".into())
    }
}

impl OAuthClient {
    /// From `lookup` (a variable's value by name). A blank value counts as unset, and every
    /// missing required variable is named, not just the first. Every variable is looked up,
    /// whatever is missing, so a recording lookup lists them all.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let get = |name: &str| lookup(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let mut missing: Vec<&str> =
            REQUIRED_VARS.into_iter().filter(|name| get(name).is_none()).collect();
        // `BIFFIX_OIDC_AUDIENCE` is a CSV list (see `crate::config::env::AuthConfig`); the browser
        // client is its first entry, read with the same parser the token verifier uses.
        let client_id = get("BIFFIX_OIDC_AUDIENCE")
            .and_then(|csv| crate::auth::accepted_audiences(&csv, None).into_iter().next());
        let cli_client_id = get("BIFFIX_CLI_CLIENT_ID");
        let cli_redirect_uris =
            biffix_core::env::parse_list(&get("BIFFIX_CLI_REDIRECT_URIS").unwrap_or_default());
        let user_pool_id = get("BIFFIX_OIDC_ISSUER")
            .and_then(|iss| crate::auth::identity::pool_id_from_issuer(&iss));
        // The Lambda runtime always sets `AWS_REGION` (the function's, i.e. the deploy's, region); a
        // local server may only have the SDK's `AWS_DEFAULT_REGION`.
        let region = get("AWS_REGION").or(get("AWS_DEFAULT_REGION"));
        if client_id.is_none() && !missing.contains(&"BIFFIX_OIDC_AUDIENCE") {
            missing.push("BIFFIX_OIDC_AUDIENCE");
        }
        if !missing.is_empty() {
            return Self::Unconfigured(format!(
                "the OAuth client is not configured: {} unset",
                missing.join(", ")
            ));
        }
        let required = |name: &str| get(name).unwrap_or_default();
        Self::Configured(Box::new(OAuthClientConfig {
            authorization_endpoint: required("BIFFIX_OAUTH_AUTHORIZATION_ENDPOINT"),
            token_endpoint: required("BIFFIX_OAUTH_TOKEN_ENDPOINT"),
            client_id: client_id.unwrap_or_default(),
            redirect_uri: required("BIFFIX_OAUTH_REDIRECT_URI"),
            logout_endpoint: required("BIFFIX_OAUTH_LOGOUT_ENDPOINT"),
            logout_uri: required("BIFFIX_OAUTH_LOGOUT_URI"),
            scopes: vec!["openid".to_owned(), "email".to_owned()],
            cli_client_id,
            cli_redirect_uris,
            user_pool_id,
            region,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn from(vars: &[(&str, &str)]) -> OAuthClient {
        let vars: BTreeMap<String, String> =
            vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        OAuthClient::from_lookup(|name| vars.get(name).cloned())
    }

    fn complete() -> Vec<(&'static str, &'static str)> {
        vec![
            ("BIFFIX_OAUTH_AUTHORIZATION_ENDPOINT", "https://auth/authorize"),
            ("BIFFIX_OAUTH_TOKEN_ENDPOINT", "https://auth/token"),
            ("BIFFIX_OIDC_AUDIENCE", " , web-client , other"),
            ("BIFFIX_OAUTH_REDIRECT_URI", "https://app/auth/callback"),
            ("BIFFIX_OAUTH_LOGOUT_ENDPOINT", "https://auth/logout"),
            ("BIFFIX_OAUTH_LOGOUT_URI", "https://app/signed-out"),
        ]
    }

    #[test]
    fn a_complete_environment_configures_the_client() {
        let mut vars = complete();
        vars.push(("BIFFIX_CLI_CLIENT_ID", " cli "));
        vars.push(("BIFFIX_CLI_REDIRECT_URIS", "http://localhost:1/cb, ,http://localhost:2/cb"));
        vars.push((
            "BIFFIX_OIDC_ISSUER",
            "https://cognito-idp.us-east-1.amazonaws.com/us-east-1_Pool/",
        ));
        let OAuthClient::Configured(c) = from(&vars) else {
            panic!("configured");
        };
        // The first non-empty audience entry, as the verifier reads it.
        assert_eq!(c.client_id, "web-client");
        assert_eq!(c.cli_client_id.as_deref(), Some("cli"));
        assert_eq!(c.cli_redirect_uris, ["http://localhost:1/cb", "http://localhost:2/cb"]);
        assert_eq!(c.user_pool_id.as_deref(), Some("us-east-1_Pool"));
        assert_eq!(c.token_endpoint, "https://auth/token");
        assert_eq!(c.region, None);

        let OAuthClient::Configured(bare) = from(&complete()) else {
            panic!("configured");
        };
        assert_eq!(bare.cli_client_id, None);
        assert!(bare.cli_redirect_uris.is_empty());
        assert_eq!(bare.user_pool_id, None);
    }

    #[test]
    fn the_deploy_region_is_served_for_console_links() {
        let mut vars = complete();
        vars.push(("AWS_DEFAULT_REGION", "eu-west-1"));
        let OAuthClient::Configured(c) = from(&vars) else {
            panic!("configured");
        };
        assert_eq!(c.region.as_deref(), Some("eu-west-1"));
        // The Lambda runtime's own variable wins over the SDK default.
        vars.push(("AWS_REGION", " us-west-2 "));
        let OAuthClient::Configured(c) = from(&vars) else {
            panic!("configured");
        };
        assert_eq!(c.region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn every_missing_variable_is_named() {
        // No logout variables, and a blank token endpoint (a later entry overrides an earlier one).
        let mut vars = complete();
        vars.retain(|(k, _)| !k.contains("LOGOUT"));
        vars.push(("BIFFIX_OAUTH_TOKEN_ENDPOINT", "  "));
        let OAuthClient::Unconfigured(why) = from(&vars) else {
            panic!("unconfigured");
        };
        for name in [
            "BIFFIX_OAUTH_TOKEN_ENDPOINT",
            "BIFFIX_OAUTH_LOGOUT_ENDPOINT",
            "BIFFIX_OAUTH_LOGOUT_URI",
        ] {
            assert!(why.contains(name), "{why}");
        }
        assert!(!why.contains("BIFFIX_OAUTH_REDIRECT_URI"), "{why}");
        // An audience that names no client is missing too.
        let mut vars = complete();
        vars.retain(|(k, _)| *k != "BIFFIX_OIDC_AUDIENCE");
        vars.push(("BIFFIX_OIDC_AUDIENCE", " , "));
        assert!(
            matches!(from(&vars), OAuthClient::Unconfigured(w) if w.contains("BIFFIX_OIDC_AUDIENCE"))
        );
        assert!(matches!(from(&[]), OAuthClient::Unconfigured(_)));
    }
}
