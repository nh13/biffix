//! No-browser paste-back parsing — pure.
//!
//! The user completes sign-in elsewhere and pastes back either the full redirect URL
//! (`http://localhost:8976/callback?code=…&state=…`) or the bare authorization code. `state` is
//! verified identically to the browser flow; a pasted URL whose `state` differs is rejected before
//! any credential is written. A bare code has no `state`; it is accepted only from a
//! terminal, bound to the login by PKCE instead (see [`AuthCallback::verify_paste`]).

use url::Url;

use crate::error::{CliError, CliResult};

/// What a paste-back yielded: the authorization `code`, and the `state` if the paste carried one
/// (a bare code carries none — see [`AuthCallback::verify_paste`]). A redirect reporting an `error`
/// has no code; the error is acted on only once its `state` is verified (see
/// [`AuthCallback::verify`]).
#[derive(Debug, PartialEq)]
pub(crate) struct AuthCallback {
    pub(crate) code: String,
    pub(crate) state: Option<String>,
    pub(crate) error: Option<String>,
}

/// The authorization server's `error` as it may be shown: control characters (a terminal escape
/// sequence) dropped, and cut short.
fn printable(error: &str) -> String {
    error.chars().filter(|c| !c.is_control()).take(200).collect()
}

impl AuthCallback {
    /// Verify a redirect's `state` against the login's expected `state` in constant time:
    /// it MUST be present and match. The one check both the loopback and the paste-back flow use.
    /// A verified redirect that reports an `error` is then refused naming it; an unverified one's
    /// text is never shown, so no other local process or page can print to the terminal.
    pub(crate) fn verify(&self, expected_state: &str) -> CliResult<()> {
        match &self.state {
            Some(state) if super::pkce::state_matches(expected_state, state) => self.no_error(),
            Some(_) => Err(CliError::InvalidInput(
                "the redirect's `state` does not match this login; aborting without saving \
                 credentials"
                    .into(),
            )),
            None => Err(CliError::InvalidInput(
                "the redirect carries no `state` to verify; aborting without saving credentials"
                    .into(),
            )),
        }
    }

    /// Verify a paste-back. A pasted URL is verified exactly like the loopback
    /// redirect. A bare code carries no `state`, so there is nothing to compare; it is accepted
    /// only from an `interactive` terminal — a person pasting what their own browser showed — and
    /// is still bound to this login by PKCE: the code only exchanges together with this login's
    /// `code_verifier`, so a code minted for anyone else's authorization request fails the
    /// exchange. Piped (non-interactive) input must be the full redirect URL.
    pub(crate) fn verify_paste(&self, expected_state: &str, interactive: bool) -> CliResult<()> {
        match (&self.state, interactive) {
            // The person pasted what their own browser showed.
            (None, _) if self.error.is_some() => self.no_error(),
            (None, true) => Ok(()),
            (None, false) => Err(CliError::InvalidInput(
                "a bare code carries no `state` to verify, and input is not a terminal; paste the \
                 full redirect URL instead"
                    .into(),
            )),
            (Some(_), _) => self.verify(expected_state),
        }
    }

    fn no_error(&self) -> CliResult<()> {
        match &self.error {
            Some(error) => Err(CliError::InvalidInput(format!(
                "the authorization server returned an error: {}",
                printable(error)
            ))),
            None => Ok(()),
        }
    }
}

/// Parse a paste-back that is expected to be a full redirect URL (with `code` and `state`).
pub(crate) fn parse_redirect_url(input: &str) -> CliResult<AuthCallback> {
    let url = Url::parse(input.trim())
        .map_err(|e| CliError::InvalidInput(format!("not a valid redirect URL: {e}")))?;
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    // An error redirect carries no code; it is reported once its state is verified.
    let code = match (code, &error) {
        (Some(code), _) => code,
        (None, Some(_)) => String::new(),
        (None, None) => {
            return Err(CliError::InvalidInput("redirect URL has no `code` parameter".into()))
        }
    };
    Ok(AuthCallback { code, state, error })
}

/// Parse a paste-back that may be either a full redirect URL or a bare code. A value that looks
/// like a URL is parsed as one (so its `state` is captured and checked); anything else is treated
/// as a bare code.
pub(crate) fn parse_paste(input: &str) -> CliResult<AuthCallback> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(CliError::InvalidInput("nothing pasted".into()));
    }
    // A scheme is case-insensitive: `HTTP://…` is a URL too, not a code.
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        parse_redirect_url(trimmed)
    } else {
        Ok(AuthCallback { code: trimmed.to_string(), state: None, error: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_redirect_url() {
        let cb = parse_paste("http://localhost:8976/callback?code=abc&state=xyz").unwrap();
        assert_eq!(cb.code, "abc");
        assert_eq!(cb.state.as_deref(), Some("xyz"));
        cb.verify("xyz").unwrap();
        assert!(cb.verify("nope").is_err());
        // A pasted URL is held to the same check whether or not input is a terminal.
        cb.verify_paste("xyz", false).unwrap();
        assert!(cb.verify_paste("nope", true).is_err());
    }

    #[test]
    fn a_pasted_url_is_a_url_whatever_the_case_of_its_scheme() {
        let cb = parse_paste("HTTP://localhost:8976/callback?code=abc&state=xyz").unwrap();
        assert_eq!(cb.code, "abc");
        assert_eq!(cb.state.as_deref(), Some("xyz"));
    }

    #[test]
    fn a_bare_code_is_accepted_only_from_a_terminal() {
        let cb = parse_paste("  bare-code-123  ").unwrap();
        assert_eq!(cb.code, "bare-code-123");
        assert_eq!(cb.state, None);
        // Nothing to state-verify: never accepted as a redirect...
        assert!(cb.verify("anything").is_err());
        // ...accepted when a person pasted it (PKCE still binds it to this login)...
        cb.verify_paste("anything", true).unwrap();
        // ...and refused from piped input, which must carry the full URL.
        let err = cb.verify_paste("anything", false).unwrap_err();
        assert!(
            matches!(&err, CliError::InvalidInput(m) if m.contains("full redirect URL")),
            "{err:?}"
        );
    }

    #[test]
    fn rejects_error_and_missing_code() {
        // The `error=` branch must be distinguishable from "no code": a URL that carries BOTH a
        // code AND an error still fails on the error, and the message names the server error — so a
        // deleted error branch (which would then succeed on the present code) is caught.
        let cb = parse_paste("http://localhost:8976/callback?code=c&error=access_denied&state=s")
            .unwrap();
        let err = cb
            .verify("s")
            .expect_err("an error param must be rejected even when a code is present");
        match err {
            CliError::InvalidInput(m) => assert!(
                m.contains("access_denied"),
                "the server error should be surfaced, got {m:?}"
            ),
            other => panic!("expected InvalidInput naming the error, got {other:?}"),
        }
        // An error from a redirect whose state does not verify is never shown (any local page can
        // send one), and a verified one is shown without control characters.
        let forged = parse_paste(
            "http://localhost:8976/callback?error=%1b%5b2Jyour%20account%20is%20locked&state=bad",
        )
        .unwrap();
        let err = forged.verify("s").unwrap_err();
        assert!(
            matches!(&err, CliError::InvalidInput(m) if m.contains("does not match")
                && !m.contains("locked")),
            "{err:?}"
        );
        let err = forged.verify("bad").unwrap_err();
        assert!(
            matches!(&err, CliError::InvalidInput(m) if m.ends_with("[2Jyour account is locked")
                && !m.contains('\u{1b}')),
            "{err:?}"
        );
        // Missing code (no error) is its own distinct failure.
        assert!(parse_paste("http://localhost:8976/callback?state=xyz").is_err());
        assert!(parse_paste("").is_err());
    }
}
