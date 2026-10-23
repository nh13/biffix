//! `biffix login` / `logout` / `whoami`.

use std::io::{IsTerminal, Write};
use std::time::SystemTime;

use crate::auth::store::{CredentialStore, FileCredentialStore};
use crate::auth::{browser, paste, pkce, token, OAuthEndpoints};
use crate::config::{Config, DiscoveredConfig};
use crate::error::{CliError, CliResult};
use crate::render::OutputMode;

use super::{GlobalArgs, Session};

/// The OAuth bits a login needs, resolved from a fresh discovery: the endpoints a refresh uses,
/// plus where to authorize and the redirect URIs registered for the CLI (never empty).
struct OAuthResolved {
    endpoints: OAuthEndpoints,
    authorization_endpoint: String,
    redirect_uris: Vec<String>,
}

impl OAuthResolved {
    fn from_discovered(d: &DiscoveredConfig) -> CliResult<Self> {
        let endpoints = OAuthEndpoints::try_from(d)?;
        // The authorization server matches a redirect URI exactly, so without a registered one no
        // flow can complete: fail now rather than send an unregistered URI.
        if d.cli_redirect_uris.is_empty() {
            return Err(CliError::Unclassified(
                "the deployment advertises no registered CLI redirect URIs at /auth/config; the \
                 CLI app client may be misconfigured"
                    .into(),
            ));
        }
        Ok(Self {
            endpoints,
            authorization_endpoint: d.authorization_endpoint.clone(),
            redirect_uris: d.cli_redirect_uris.clone(),
        })
    }

    /// The first registered redirect URI, which the paste-back flow uses.
    fn first_redirect_uri(&self) -> &str {
        &self.redirect_uris[0]
    }
}

/// Run the login flow (browser by default; paste-back with `--no-browser`, or at once when no
/// browser opens, or after the browser flow times out).
///
/// Nothing is persisted until the token exchange succeeds: an aborted or failed login leaves the
/// profile's config and credentials exactly as they were (so a `login --url B` that fails never
/// re-points a profile's tenant-A credentials at B).
pub(crate) async fn login(session: &mut Session, no_browser: bool) -> CliResult<()> {
    let discovered = session.fetch_discovery().await?;
    let oauth = OAuthResolved::from_discovered(&discovered)?;

    let pkce = pkce::Pkce::generate();
    // Each flow returns the code AND the exact redirect_uri it used at authorize time — the token
    // exchange must echo that same value. The browser flow's is a variable pooled port, so a
    // fallback from a timed-out browser flow must reuse THAT redirect_uri (not blindly switch to
    // `redirect_uris[0]`), or a code minted for the bound port fails the exchange.
    let (callback, redirect_uri) = if no_browser {
        no_browser_flow(&oauth, &pkce, None)?
    } else {
        match browser::bind_from_pool(&oauth.redirect_uris).await {
            Ok(loopback) => {
                let redirect_uri = loopback.redirect_uri.clone();
                match browser_flow(loopback, &oauth, &pkce).await {
                    Ok(callback) => (callback, redirect_uri),
                    // No browser, or timed out waiting for the redirect: fall back to paste, REUSING the bound
                    // redirect_uri so a code from either URL still exchanges correctly.
                    Err(CliError::Unavailable(msg)) => {
                        eprintln!(
                            "[biffix] browser sign-in did not complete ({msg}); falling back to \
                             paste — use the URL below"
                        );
                        no_browser_flow(&oauth, &pkce, Some(redirect_uri))?
                    }
                    Err(e) => return Err(e),
                }
            }
            // All registered ports busy: fall back to paste. A malformed-URI/other error (also
            // `bind_from_pool`'s Unclassified) propagates rather than silently degrading.
            Err(CliError::Unavailable(msg)) => {
                eprintln!(
                    "[biffix] could not bind a loopback listener ({msg}); falling back to paste"
                );
                no_browser_flow(&oauth, &pkce, None)?
            }
            Err(e) => return Err(e),
        }
    };

    // Exchange the code (PKCE, no secret) and persist the credentials — bound to this API base —
    // for this profile.
    let tokens = token::exchange_code(
        &session.http,
        &oauth.endpoints.token_endpoint,
        &oauth.endpoints.client_id,
        &callback.code,
        pkce.verifier.expose(),
        &redirect_uri,
    )
    .await?;
    let creds = tokens.into_credentials(SystemTime::now(), None, &session.api_base);
    session.store.save(&session.profile_name, &creds)?;

    // Only now record the deployment in the profile. Adopt the just-logged-in profile as the
    // default when none was ever chosen, so subsequent bare commands resolve THIS profile (not an
    // unauthenticated "default").
    session.persist_discovery(discovered)?;
    if session.config.default_profile.is_none() {
        let name = session.profile_name.clone();
        session.config = Config::update(|c| {
            c.default_profile.get_or_insert(name);
        })?;
    }

    let who = creds.identity();
    match session.output {
        OutputMode::Json => crate::render::print_json(&serde_json::json!({
            "status": "logged_in", "identity": who, "profile": session.profile_name,
        })),
        OutputMode::Human => println!("logged in as {who} @ {}", session.profile_name),
    }
    Ok(())
}

/// Drive the browser flow against an already-bound loopback listener, returning just the callback
/// (the caller owns the bound `redirect_uri`). `Unavailable` when no browser opens, or none
/// redirects in time.
async fn browser_flow(
    loopback: browser::Loopback,
    oauth: &OAuthResolved,
    pkce: &pkce::Pkce,
) -> CliResult<paste::AuthCallback> {
    let url = browser::authorize_url(
        &oauth.authorization_endpoint,
        &oauth.endpoints.client_id,
        &loopback.redirect_uri,
        pkce.state.expose(),
        &pkce.challenge,
    )?;
    // With no browser here (a headless or SSH host), the redirect would go to another machine's
    // `localhost` and never arrive: fall back to paste at once rather than wait out the timeout.
    if !browser::open_browser(&url) {
        return Err(CliError::Unavailable("could not open a browser".into()));
    }
    eprintln!("[biffix] opening your browser to sign in…");
    loopback.wait_for_code(pkce.state.expose(), browser::DEFAULT_TIMEOUT).await
}

/// The paste-back flow. `redirect_uri_override` carries the browser flow's bound redirect_uri when
/// falling back from a timeout (so both authorize URLs use the same value); otherwise the first
/// registered URI. The URL and prompt go to stderr, so stdout carries only the command's result
/// (the JSON document in JSON mode).
fn no_browser_flow(
    oauth: &OAuthResolved,
    pkce: &pkce::Pkce,
    redirect_uri_override: Option<String>,
) -> CliResult<(paste::AuthCallback, String)> {
    let redirect_uri =
        redirect_uri_override.unwrap_or_else(|| oauth.first_redirect_uri().to_string());
    let url = browser::authorize_url(
        &oauth.authorization_endpoint,
        &oauth.endpoints.client_id,
        &redirect_uri,
        pkce.state.expose(),
        &pkce.challenge,
    )?;
    // A bare code can be accepted only from a person at a terminal (see `verify_paste`).
    let interactive = std::io::stdin().is_terminal();
    let what = if interactive {
        "the full redirect URL (or just its code)"
    } else {
        "the full redirect URL"
    };
    eprintln!(
        "Open this URL in a browser, sign in, then paste {what} below.\n\
         (The page it lands on may fail to load; its address is what to copy.)\n\n  {url}\n"
    );
    eprint!("redirect URL{}: ", if interactive { " or code" } else { "" });
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| CliError::InvalidInput(format!("could not read input: {e}")))?;
    let callback = paste::parse_paste(&line)?;
    // A pasted URL's state must match; a bare code is PKCE-bound instead.
    callback.verify_paste(pkce.state.expose(), interactive)?;
    Ok((callback, redirect_uri))
}

/// `biffix logout`: clear the cached credentials for the selected profile. It needs no API
/// URL and no network, and succeeds when there is nothing to clear. The profile is the flag →
/// `BIFFIX_PROFILE` → the config's default; an unreadable config only matters when neither of the
/// first two names the profile.
pub(crate) fn logout(args: &GlobalArgs) -> CliResult<()> {
    let explicit = args.profile.clone().or_else(crate::config::env_profile);
    let profile = match explicit {
        Some(p) => p,
        None => Config::load()
            .map_err(|e| {
                e.map_message(|m| format!("{m}; pass --profile to name the profile to log out of"))
            })?
            .effective_profile_name(None),
    };
    let store = FileCredentialStore::new(FileCredentialStore::default_path()?);
    store.clear(&profile)?;
    match OutputMode::resolve(args.output) {
        OutputMode::Json => crate::render::print_json(&serde_json::json!({
            "status": "logged_out", "profile": profile,
        })),
        OutputMode::Human => println!("logged out of {profile}"),
    }
    Ok(())
}

/// `biffix whoami`: show the cached identity and confirm the token is accepted by the API.
pub(crate) async fn whoami(session: &mut Session) -> CliResult<()> {
    // Confirm the token is accepted, exercising silent refresh through the one choke point. Probe
    // `/status` (a small fixed-size report) rather than `/runs` (which fetches and discards a full
    // page of runs). A 403 means the token IS accepted but the role is limited — still
    // authenticated, so it is not a whoami failure; only a rejected token (or an outage) is.
    let client = session.authed_client().await?;
    match client.get_status().await {
        Ok(_) => {}
        Err(CliError::Forbidden(_)) => {}
        Err(e) => return Err(e),
    }
    // Read the identity after the probe, so a refresh it triggered is reflected.
    let creds = session
        .store
        .load(&session.profile_name)?
        .ok_or_else(|| CliError::AuthRequired("not logged in; run `biffix login`".into()))?;
    let identity = creds.identity();
    match session.output {
        OutputMode::Json => crate::render::print_json(&serde_json::json!({
            "identity": identity, "subject": creds.subject, "profile": session.profile_name,
        })),
        OutputMode::Human => println!("{identity} @ {} (authenticated)", session.profile_name),
    }
    Ok(())
}
