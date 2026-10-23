//! The authenticated caller ([`AuthedActor`]), the one authorization check every request makes with
//! it ([`authorize`]), and its human label.

use crate::auth::rbac::Capability;
use crate::auth::Principal;
use crate::error::ServiceError;
use crate::AppState;

/// The authenticated caller attached to a request by [`require_auth`]. `Disabled` is the explicit
/// `BIFFIX_AUTH_DISABLED=true` local-dev (or test) composition, where capability checks are skipped.
#[derive(Clone)]
pub(crate) enum AuthedActor {
    Disabled,
    User(Principal),
}

/// The one authorization choke point for HTTP handlers. Returns the actor id (`sub`, or `local`
/// when auth is disabled) on success; else a 403 (or, with no auth context for a user, a 500), with
/// no message.
pub(crate) fn authorize(
    state: &AppState,
    actor: &AuthedActor,
    cap: Capability,
) -> Result<String, ServiceError> {
    match actor {
        AuthedActor::Disabled => Ok("local".to_string()),
        AuthedActor::User(principal) => {
            let auth = state.auth.as_ref().ok_or(ServiceError::Internal(String::new()))?;
            if auth.rbac.can(&principal.groups, cap) {
                Ok(principal.subject.clone())
            } else {
                Err(ServiceError::Forbidden(String::new()))
            }
        }
    }
}

/// [`authorize`] with a message: a 403 names the capability the caller lacks.
pub(crate) fn authorize_msg(
    state: &AppState,
    actor: &AuthedActor,
    cap: Capability,
) -> Result<String, ServiceError> {
    authorize(state, actor, cap).map_err(|e| match e {
        ServiceError::Forbidden(_) => {
            ServiceError::Forbidden(format!("your groups do not grant the {cap:?} capability"))
        }
        other => other.with_message("authorization is misconfigured on this deploy"),
    })
}

/// A human label for an actor, snapshotted onto immutable anchors (a run's submitter, a version's
/// registrant). Prefers the token's own `email`; else resolves the Cognito username to an email/name
/// via the identity resolver (access tokens lack `email`). Best-effort — a miss yields `None` rather
/// than an error, so it never blocks the caller.
pub(crate) async fn actor_label(state: &AppState, actor: &AuthedActor) -> Option<String> {
    let AuthedActor::User(principal) = actor else {
        return None;
    };
    if let Some(email) = &principal.email {
        return Some(email.clone());
    }
    let username = principal.username.as_deref()?;
    state.identity.label_for(username).await
}
