//! Authentication: PKCE/state generation, the loopback + paste-back login flows, the token
//! exchange/refresh, and the credential store.

pub(crate) mod browser;
pub(crate) mod paste;
pub mod pkce;
pub mod store;
pub(crate) mod token;

use std::sync::Arc;
use std::time::SystemTime;

use crate::error::{CliError, CliResult};
use crate::secret::Secret;
use store::{normalize_base, CredentialStore, Credentials, REFRESH_MARGIN};

/// The OAuth pieces a refresh needs: the token endpoint and the CLI's public client id.
#[derive(Debug, Clone)]
pub struct OAuthEndpoints {
    pub token_endpoint: String,
    pub client_id: String,
}

impl TryFrom<&crate::config::DiscoveredConfig> for OAuthEndpoints {
    type Error = CliError;

    /// The token endpoint and CLI client a deployment advertises; one advertising no CLI client
    /// cannot be signed in to (or refreshed against) from the CLI.
    fn try_from(d: &crate::config::DiscoveredConfig) -> CliResult<Self> {
        Ok(Self {
            token_endpoint: d.token_endpoint.clone(),
            client_id: d.cli_client_id.clone().ok_or_else(|| {
                CliError::Unclassified(
                    "the deployment did not advertise a CLI OAuth client at /auth/config; the \
                     CLI app client may not be deployed"
                        .into(),
                )
            })?,
        })
    }
}

/// The one choke point every authorized request resolves its bearer through.
/// It lives as long as the command's [`crate::api::ApiClient`], so a long
/// `runs follow` or `register --wait` keeps working past the access token's lifetime:
///
/// - cached and outside the refresh margin → reuse it;
/// - expired or inside [`REFRESH_MARGIN`] with a refresh token → refresh silently and persist;
/// - rejected by the API (`401`) → refresh once (unless another request already did) so the
///   caller can replay;
/// - otherwise → `AuthRequired` naming `biffix login`.
///
/// It also binds the credential to the API base it was issued for: a source is only ever built
/// for the base its credential names, so a token minted for one deployment is never sent to
/// another, whatever `--url` / `BIFFIX_API_URL` say.
pub struct TokenSource {
    store: Arc<dyn CredentialStore>,
    profile: String,
    api_base: String,
    http: reqwest::Client,
    /// Whether a refresh's discovery logs its attempts (`--verbose`).
    verbose: bool,
    state: tokio::sync::Mutex<SourceState>,
}

struct SourceState {
    creds: Credentials,
    /// `None` until a refresh first needs them; then discovered from the API's `/auth/config`.
    oauth: Option<OAuthEndpoints>,
}

impl TokenSource {
    /// Load `profile`'s credential for the API at `api_base`. Fails with `AuthRequired` when there
    /// is no credential (before any network call) or when it was issued for a different
    /// deployment.
    pub fn load(
        store: Arc<dyn CredentialStore>,
        profile: &str,
        api_base: &str,
        oauth: Option<OAuthEndpoints>,
        http: reqwest::Client,
    ) -> CliResult<Self> {
        let creds = store.load(profile)?.ok_or_else(|| {
            CliError::AuthRequired(format!(
                "not logged in for profile {profile:?}; run `biffix login`"
            ))
        })?;
        let api_base = normalize_base(api_base);
        check_binding(profile, &creds, &api_base)?;
        Ok(Self {
            store,
            profile: profile.to_string(),
            api_base,
            http,
            verbose: false,
            state: tokio::sync::Mutex::new(SourceState { creds, oauth }),
        })
    }

    /// Supply the OAuth endpoints a refresh uses (otherwise discovered on the first refresh).
    pub(crate) fn set_oauth(&mut self, oauth: OAuthEndpoints) {
        self.state.get_mut().oauth = Some(oauth);
    }

    /// Log the attempts of a refresh's discovery (`--verbose`).
    pub(crate) fn set_verbose(&mut self, verbose: bool) {
        self.verbose = verbose;
    }

    /// Whether the cached access token must be refreshed before its first use.
    pub(crate) async fn needs_refresh_now(&self) -> bool {
        self.state.lock().await.creds.needs_refresh(SystemTime::now(), REFRESH_MARGIN)
    }

    /// The bearer for the next request, refreshing first when the cached token is expired or
    /// inside the refresh margin.
    pub(crate) async fn bearer(&self) -> CliResult<Secret<String>> {
        let mut state = self.state.lock().await;
        if state.creds.needs_refresh(SystemTime::now(), REFRESH_MARGIN) {
            self.refresh_locked(&mut state).await?;
        }
        Ok(state.creds.access_token.clone())
    }

    /// The API rejected `rejected` (a `401`): refresh once so the caller can replay the request.
    /// When another request already replaced that token, the newer one is returned without a
    /// second refresh.
    pub(crate) async fn after_unauthorized(
        &self,
        rejected: &Secret<String>,
    ) -> CliResult<Secret<String>> {
        let mut state = self.state.lock().await;
        if state.creds.access_token != *rejected {
            return Ok(state.creds.access_token.clone());
        }
        self.refresh_locked(&mut state).await.map_err(|e| match e {
            CliError::AuthRequired(m) => {
                CliError::AuthRequired(format!("the API rejected the session token and {m}"))
            }
            other => other,
        })?;
        Ok(state.creds.access_token.clone())
    }

    async fn refresh_locked(&self, state: &mut SourceState) -> CliResult<()> {
        // Another `biffix` process may have refreshed (and rotated the refresh token) since this
        // one loaded: prefer its fresh token, and otherwise refresh with the newest refresh token.
        // A logout since then ends the session; a newer login of the profile to another
        // deployment is never written over (this command's refreshed token stays in memory).
        let rejected = state.creds.access_token.clone();
        let persist = match self.store.load(&self.profile)? {
            None => {
                return Err(CliError::AuthRequired(format!(
                    "profile {:?} was logged out while this command ran; run `biffix login`",
                    self.profile
                )))
            }
            Some(stored) if check_binding(&self.profile, &stored, &self.api_base).is_ok() => {
                let fresh = stored.access_token != rejected
                    && !stored.needs_refresh(SystemTime::now(), REFRESH_MARGIN);
                state.creds = stored;
                if fresh {
                    return Ok(());
                }
                true
            }
            Some(_) => false,
        };
        let Some(refresh_token) = state.creds.refresh_token.clone() else {
            return Err(CliError::AuthRequired(
                "the session expired and no refresh token is available; run `biffix login`".into(),
            ));
        };
        let oauth = match &state.oauth {
            Some(o) => o.clone(),
            None => {
                let d = crate::api::ApiClient::discover(&self.http, &self.api_base, self.verbose)
                    .await?;
                let o = OAuthEndpoints::try_from(&d)?;
                state.oauth = Some(o.clone());
                o
            }
        };
        let refreshed = token::refresh(
            &self.http,
            &oauth.token_endpoint,
            &oauth.client_id,
            refresh_token.expose(),
        )
        .await?;
        let new_creds =
            refreshed.into_credentials(SystemTime::now(), Some(refresh_token), &self.api_base);
        // Written only over the credential refreshed from: a logout or login during the refresh
        // wins, and this command keeps its token in memory.
        if persist {
            self.store.replace(&self.profile, &state.creds, &new_creds)?;
        }
        state.creds = new_creds;
        Ok(())
    }
}

/// Refuse a credential issued for a different API base than the one this command talks to.
fn check_binding(profile: &str, creds: &Credentials, api_base: &str) -> CliResult<()> {
    let issued_for = normalize_base(&creds.api_base_url);
    if issued_for.is_empty() {
        return Err(CliError::AuthRequired(format!(
            "the credentials for profile {profile:?} do not say which deployment issued them; run \
             `biffix login --url {api_base}`"
        )));
    }
    if issued_for != api_base {
        return Err(CliError::AuthRequired(format!(
            "the credentials for profile {profile:?} were issued for {issued_for}, not {api_base}; \
             run `biffix login --url {api_base}` (or pass the --profile logged in there)"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemStore(Mutex<BTreeMap<String, Credentials>>);

    impl CredentialStore for MemStore {
        fn load(&self, profile: &str) -> Result<Option<Credentials>, store::CredentialStoreError> {
            Ok(self.0.lock().unwrap().get(profile).cloned())
        }
        fn save(
            &self,
            profile: &str,
            creds: &Credentials,
        ) -> Result<(), store::CredentialStoreError> {
            self.0.lock().unwrap().insert(profile.to_string(), creds.clone());
            Ok(())
        }
        fn replace(
            &self,
            profile: &str,
            current: &Credentials,
            new: &Credentials,
        ) -> Result<bool, store::CredentialStoreError> {
            let mut map = self.0.lock().unwrap();
            if !map.get(profile).is_some_and(|c| c.is_same(current)) {
                return Ok(false);
            }
            map.insert(profile.to_string(), new.clone());
            Ok(true)
        }
        fn clear(&self, profile: &str) -> Result<(), store::CredentialStoreError> {
            self.0.lock().unwrap().remove(profile);
            Ok(())
        }
    }

    fn creds(base: &str, expires_at_unix: u64, refresh: Option<&str>) -> Credentials {
        Credentials {
            access_token: Secret::new("access".into()),
            refresh_token: refresh.map(|r| Secret::new(r.to_string())),
            expires_at_unix,
            subject: "s".into(),
            email: None,
            api_base_url: base.into(),
        }
    }

    fn store_with(c: Option<Credentials>) -> Arc<dyn CredentialStore> {
        let store = MemStore::default();
        if let Some(c) = c {
            store.save("default", &c).unwrap();
        }
        Arc::new(store)
    }

    fn load(store: Arc<dyn CredentialStore>, base: &str) -> CliResult<()> {
        TokenSource::load(store, "default", base, None, reqwest::Client::new()).map(|_| ())
    }

    #[test]
    fn no_credential_is_auth_required_naming_login() {
        let err = load(store_with(None), "https://a.example/api").unwrap_err();
        assert!(matches!(&err, CliError::AuthRequired(m) if m.contains("biffix login")), "{err:?}");
    }

    #[test]
    fn a_credential_is_only_sent_to_the_base_it_was_issued_for() {
        let c = creds("https://a.example/api", u64::MAX, None);
        // Trailing slashes never make one deployment look like two.
        load(store_with(Some(c.clone())), "https://a.example/api/").unwrap();
        let err = load(store_with(Some(c)), "https://b.example/api").unwrap_err();
        let CliError::AuthRequired(m) = err else {
            panic!("expected AuthRequired, got {err:?}");
        };
        assert!(m.contains("https://a.example/api") && m.contains("https://b.example/api"), "{m}");
    }

    #[tokio::test]
    async fn an_unexpired_token_is_reused_without_a_refresh() {
        let store = store_with(Some(creds("https://a.example/api", u64::MAX, None)));
        let source = TokenSource::load(
            store,
            "default",
            "https://a.example/api",
            None,
            reqwest::Client::new(),
        )
        .unwrap();
        assert!(!source.needs_refresh_now().await);
        assert_eq!(source.bearer().await.unwrap().expose(), "access");
    }

    /// A token endpoint that issues `refreshed`, first running `during` (another process's login
    /// or logout landing while the refresh is in flight).
    async fn token_endpoint(during: impl Fn() + Send + Sync + 'static) -> OAuthEndpoints {
        let during = Arc::new(during);
        let app = axum::Router::new().route(
            "/oauth2/token",
            axum::routing::post(move || {
                let during = during.clone();
                async move {
                    during();
                    axum::Json(serde_json::json!({"access_token": "refreshed", "expires_in": 3600}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        OAuthEndpoints {
            token_endpoint: format!("http://{addr}/oauth2/token"),
            client_id: "cli-1".into(),
        }
    }

    const A: &str = "https://a.example/api";
    const B: &str = "https://b.example/api";

    /// A source for deployment A over `store`, whose cached token is due for a refresh.
    fn expiring(store: &Arc<dyn CredentialStore>, oauth: OAuthEndpoints) -> TokenSource {
        store.save("default", &creds(A, 0, Some("r"))).unwrap();
        TokenSource::load(store.clone(), "default", A, Some(oauth), reqwest::Client::new()).unwrap()
    }

    fn stored(store: &Arc<dyn CredentialStore>) -> Option<(String, String)> {
        store.load("default").unwrap().map(|c| (c.api_base_url, c.access_token.expose().clone()))
    }

    #[tokio::test]
    async fn a_refresh_is_saved_over_the_credential_it_refreshed() {
        let store = store_with(None);
        let source = expiring(&store, token_endpoint(|| {}).await);
        assert_eq!(source.bearer().await.unwrap().expose(), "refreshed");
        assert_eq!(stored(&store), Some((A.into(), "refreshed".into())));
    }

    #[tokio::test]
    async fn a_refresh_never_brings_back_a_logged_out_profile() {
        let store = store_with(None);
        let source = expiring(&store, token_endpoint(|| {}).await);
        store.clear("default").unwrap();
        let err = source.bearer().await.unwrap_err();
        assert!(matches!(&err, CliError::AuthRequired(m) if m.contains("logged out")), "{err:?}");
        assert_eq!(stored(&store), None);
    }

    #[tokio::test]
    async fn a_refresh_never_overwrites_a_newer_login_to_another_deployment() {
        let store = store_with(None);
        let source = expiring(&store, token_endpoint(|| {}).await);
        store.save("default", &creds(B, u64::MAX, None)).unwrap();
        // This command keeps working on A with its refreshed token...
        assert_eq!(source.bearer().await.unwrap().expose(), "refreshed");
        // ...and the profile stays logged in to B.
        assert_eq!(stored(&store), Some((B.into(), "access".into())));
    }

    #[tokio::test]
    async fn a_login_or_logout_during_the_refresh_wins() {
        // A login to B lands while the refresh is in flight.
        let store = store_with(None);
        let during = store.clone();
        let oauth = token_endpoint(move || {
            during.save("default", &creds(B, u64::MAX, None)).unwrap();
        })
        .await;
        let source = expiring(&store, oauth);
        assert_eq!(source.bearer().await.unwrap().expose(), "refreshed");
        assert_eq!(stored(&store), Some((B.into(), "access".into())));

        // A logout lands while the refresh is in flight.
        let store = store_with(None);
        let during = store.clone();
        let oauth = token_endpoint(move || during.clear("default").unwrap()).await;
        let source = expiring(&store, oauth);
        source.bearer().await.unwrap();
        assert_eq!(stored(&store), None);
    }

    #[tokio::test]
    async fn an_expired_token_without_a_refresh_token_needs_login() {
        let store = store_with(Some(creds("https://a.example/api", 0, None)));
        let source = TokenSource::load(
            store,
            "default",
            "https://a.example/api",
            None,
            reqwest::Client::new(),
        )
        .unwrap();
        let err = source.bearer().await.unwrap_err();
        assert!(matches!(&err, CliError::AuthRequired(m) if m.contains("biffix login")), "{err:?}");
    }
}
