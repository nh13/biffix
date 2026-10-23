//! The caller's own state: `GET/PUT /me/prefs`, their display preferences (Lab/Expert view, pinned
//! pipelines, first-visit tour); `/me/notifications`, the bell (their runs that ended, and which
//! of those they have read); and `/me/attention/dismiss`, taking failed runs out of their *Needs
//! attention* (apart from reading them). Keyed by the caller's id, so a user only ever reads or
//! writes their own rows; any viewer may, since none of them grants anything.

use axum::extract::{Query, State};
use axum::{Extension, Json};

pub(crate) use biffix_api::{
    AttentionCount, AttentionDismissRequest, NotificationList, NotificationsQuery,
    NotificationsReadRequest, UnreadCounts, UserPrefs, UserPrefsUpdateRequest,
};

use crate::api::ApiError;
use crate::auth::actor::{authorize_msg, AuthedActor};
use crate::auth::rbac::Capability;
use crate::AppState;

use crate::store::notifications::{DismissVia, NotificationSelection, MAX_NOTIFICATIONS};
use crate::store::settings::{MAX_PINS, MAX_PIN_CHARS};

/// How many notifications `GET /me/notifications` returns when the caller does not say.
const DEFAULT_NOTIFICATIONS: i64 = 20;

/// The caller's newest notifications (their runs that ended, newest first, at most `limit`:
/// default 20, clamped to 1..=100), each with whether they have read it, and how many of all theirs
/// are unread.
#[utoipa::path(
    get, path = "/me/notifications",
    params(NotificationsQuery),
    responses((status = 200, body = NotificationList), (status = 403))
)]
pub(crate) async fn list_notifications(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Query(q): Query<NotificationsQuery>,
) -> Result<Json<NotificationList>, ApiError> {
    let user = authorize_msg(&state, &actor, Capability::ViewRuns)?;
    // The one place the limit is bounded: the stores take it as given.
    let limit = q.limit.unwrap_or(DEFAULT_NOTIFICATIONS).clamp(1, MAX_NOTIFICATIONS);
    let list = state.store.list_notifications(&user, limit).await?;
    Ok(Json(list))
}

/// The most ids (or run ids) one read request may name.
pub(crate) const MAX_READ_IDS: usize = 500;

/// The refusal of a request whose `field` names `n` entries, more than [`MAX_READ_IDS`].
fn too_many(field: &str, n: usize) -> String {
    format!("`{field}` names {n} entries; at most {MAX_READ_IDS} per request")
}

/// The selection a read request makes: exactly one of `ids`, `run_ids` and `all: true`, naming at
/// most [`MAX_READ_IDS`]. The error says what was sent.
fn selection(req: NotificationsReadRequest) -> Result<NotificationSelection, String> {
    match (req.ids, req.run_ids, req.all) {
        (Some(ids), None, None | Some(false)) if ids.len() > MAX_READ_IDS => {
            Err(too_many("ids", ids.len()))
        }
        (None, Some(runs), None | Some(false)) if runs.len() > MAX_READ_IDS => {
            Err(too_many("run_ids", runs.len()))
        }
        // An empty list selects nothing, as `dismiss_run_ids` refuses one.
        (Some(ids), None, None | Some(false)) if ids.is_empty() => {
            Err("`ids` is empty: give the notifications to mark read".into())
        }
        (None, Some(runs), None | Some(false)) if runs.is_empty() => {
            Err("`run_ids` is empty: give the runs whose notifications to mark read".into())
        }
        (Some(ids), None, None | Some(false)) => Ok(NotificationSelection::Ids(ids)),
        (None, Some(runs), None | Some(false)) => {
            Ok(NotificationSelection::Runs(runs.into_iter().map(|r| r.0).collect()))
        }
        (None, None, Some(true)) => Ok(NotificationSelection::All),
        (None, None, Some(false)) => {
            Err("`all: false` selects nothing: give `ids`, `run_ids` or `all: true`".into())
        }
        (None, None, None) => Err("nothing selected: give `ids`, `run_ids` or `all: true`".into()),
        (ids, runs, all) => {
            let sent: Vec<&str> = [
                ids.is_some().then_some("`ids`"),
                runs.is_some().then_some("`run_ids`"),
                (all == Some(true)).then_some("`all: true`"),
            ]
            .into_iter()
            .flatten()
            .collect();
            Err(format!(
                "give only one of `ids`, `run_ids` and `all: true` (got {})",
                sent.join(" and ")
            ))
        }
    }
}

/// Mark some of the caller's notifications read: `ids`, every notification of `run_ids`, or
/// `all: true` (exactly one, and at most 500 ids, else 400). Ids and runs that are not the
/// caller's are ignored. Returns the caller's unread counts after.
#[utoipa::path(
    post, path = "/me/notifications/read",
    request_body = NotificationsReadRequest,
    responses((status = 200, body = UnreadCounts), (status = 400), (status = 403))
)]
pub(crate) async fn read_notifications(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<NotificationsReadRequest>,
) -> Result<Json<UnreadCounts>, ApiError> {
    let user = authorize_msg(&state, &actor, Capability::ViewRuns)?;
    let selection = selection(req).map_err(ApiError::BadRequest)?;
    let unread = state.store.mark_notifications_read(&user, &selection).await?;
    Ok(Json(unread))
}

/// Dismiss some of the caller's failed runs from their *Needs attention*: `run_ids` (1 to 500,
/// else 400), exactly the runs the caller was shown (one row, or the rows on screen), never an
/// "everything up to now" cutoff that could take one they have not seen. Runs that are not the
/// caller's, or did not fail, are ignored. Idempotent; durable and per user
/// (`notification_dismissals`, recorded `via: dismiss`). Reading a notification is separate: this
/// neither reads it nor is undone by reading it. Returns how many still need attention; once the
/// dismissal is recorded it is a 200 even if that count cannot be read (`attention` is then
/// absent), so a client never shows a recorded dismissal as refused.
#[utoipa::path(
    post, path = "/me/attention/dismiss",
    request_body = AttentionDismissRequest,
    responses((status = 200, body = AttentionCount), (status = 400), (status = 403), (status = 503))
)]
pub(crate) async fn dismiss_attention(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<AttentionDismissRequest>,
) -> Result<Json<AttentionCount>, ApiError> {
    let user = authorize_msg(&state, &actor, Capability::ViewRuns)?;
    let run_ids = dismiss_run_ids(req).map_err(ApiError::BadRequest)?;
    state.store.dismiss_attention(&user, &run_ids, DismissVia::Dismiss).await?;
    // Committed: the count is a convenience for the client's refresh, not part of the answer.
    let attention = attention_count(&state, &user)
        .await
        .inspect_err(|e| {
            tracing::warn!(error = %e, "dismiss_attention: dismissed, but the count read failed");
        })
        .ok();
    Ok(Json(AttentionCount { attention }))
}

/// The runs a dismissal names: trimmed, blanks and repeats dropped, 1 to [`MAX_READ_IDS`] of them.
fn dismiss_run_ids(req: AttentionDismissRequest) -> Result<Vec<String>, String> {
    if req.run_ids.len() > MAX_READ_IDS {
        return Err(too_many("run_ids", req.run_ids.len()));
    }
    let mut ids: Vec<String> = req
        .run_ids
        .iter()
        .map(|s| s.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Err("nothing selected: give the `run_ids` to dismiss".into());
    }
    Ok(ids)
}

/// How many of `user`'s runs need their attention (the run list's `attention` filter's total).
async fn attention_count(state: &AppState, user: &str) -> Result<i64, crate::store::StoreError> {
    let query = crate::store::runs::RunQuery {
        submitter: Some(biffix_core::event::user_submitter(user)),
        attention: Some(user.to_string()),
        limit: 1,
        ..Default::default()
    };
    Ok(state.store.list_runs(&query).await?.total)
}

/// Read the caller's preferences (the defaults when they have none stored).
#[utoipa::path(get, path = "/me/prefs", responses((status = 200, body = UserPrefs), (status = 403)))]
pub(crate) async fn get_prefs(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
) -> Result<Json<UserPrefs>, ApiError> {
    let user = authorize_msg(&state, &actor, Capability::ViewRuns)?;
    let prefs = state.settings.get_user_prefs(&user).await?;
    Ok(Json(prefs))
}

/// One pin key as stored: trimmed, 1 to [`MAX_PIN_CHARS`] characters. The error names the key
/// as `field` (`pin`, `unpin`).
fn pin_key<'a>(field: &str, pin: &'a str) -> Result<&'a str, String> {
    let key = pin.trim();
    let chars = key.chars().count();
    if chars == 0 || chars > MAX_PIN_CHARS {
        // Quote a long value's start only: the message is for a person, not an echo.
        let cut = biffix_workflow::text::truncate_chars(pin, 40);
        let shown = if cut.len() < pin.len() { format!("{cut}…") } else { pin.to_string() };
        return Err(format!("{field}: `{shown}` must be 1 to {MAX_PIN_CHARS} characters"));
    }
    Ok(key)
}

/// Check and normalize an update's pin change in place: a `pin`/`unpin` key trimmed and checked;
/// at most one of the two.
fn normalize_update(req: &mut UserPrefsUpdateRequest) -> Result<(), String> {
    if req.pin.is_some() && req.unpin.is_some() {
        return Err("give at most one of `pin` and `unpin` in one update".into());
    }
    for (field, op) in [("pin", &mut req.pin), ("unpin", &mut req.unpin)] {
        if let Some(key) = op {
            *op = Some(pin_key(field, key)?.to_string());
        }
    }
    Ok(())
}

/// Update the caller's preferences. A partial update: a field the body leaves out keeps its
/// stored value. `pin` and `unpin` change one (trimmed) key against the stored list atomically; at
/// most one of them per body (400), and a pin past the cap is a 400 naming it. An unknown `view`
/// is a body the API cannot read (422).
#[utoipa::path(
    put, path = "/me/prefs",
    request_body(content = UserPrefsUpdateRequest, description = "absent field: keep"),
    responses((status = 200, body = UserPrefs), (status = 400), (status = 403), (status = 422))
)]
pub(crate) async fn put_prefs(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(mut req): Json<UserPrefsUpdateRequest>,
) -> Result<Json<UserPrefs>, ApiError> {
    let user = authorize_msg(&state, &actor, Capability::ViewRuns)?;
    normalize_update(&mut req).map_err(ApiError::BadRequest)?;
    let prefs = state.settings.update_user_prefs(&user, &req).await.map_err(|e| match e {
        // The only conflict an update has: a `pin` past the cap (nothing was written).
        crate::store::StoreError::Conflict(_) => {
            ApiError::BadRequest(format!("pin: at most {MAX_PINS} pinned pipelines"))
        }
        e => ApiError::from(e),
    })?;
    Ok(Json(prefs))
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// The caller's own preferences and notifications: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/me/prefs", axum::routing::get(get_prefs).put(put_prefs))
        .route("/me/notifications", axum::routing::get(list_notifications))
        .route("/me/notifications/read", axum::routing::post(read_notifications))
        .route("/me/attention/dismiss", axum::routing::post(dismiss_attention))
}

/// The caller's own preferences and notifications: the OpenAPI operations, tagged `me` in
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(get_prefs, put_prefs, list_notifications, read_notifications, dismiss_attention),
    components(schemas(
        biffix_api::UserPrefs,
        biffix_api::Notification,
        biffix_api::NotificationList,
        biffix_api::NotificationsReadRequest,
        biffix_api::UnreadCounts,
        biffix_api::AttentionDismissRequest,
        biffix_api::AttentionCount,
        biffix_api::UserPrefsUpdateRequest,
        biffix_api::ViewMode
    ))
)]
pub(super) struct Doc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_selects_exactly_one_way() {
        let ids = || Some(vec!["r1:failed".to_string()]);
        let req = |ids, run_ids: Option<Vec<String>>, all| NotificationsReadRequest {
            ids,
            run_ids: run_ids.map(|r| r.into_iter().map(Into::into).collect()),
            all,
        };
        assert_eq!(
            selection(req(ids(), None, None)).unwrap(),
            NotificationSelection::Ids(vec!["r1:failed".into()])
        );
        assert_eq!(
            selection(req(None, ids(), Some(false))).unwrap(),
            NotificationSelection::Runs(vec!["r1:failed".into()])
        );
        assert_eq!(selection(req(None, None, Some(true))).unwrap(), NotificationSelection::All);
        for (bad, says) in [
            (req(None, None, None), "nothing selected"),
            (req(None, None, Some(false)), "`all: false` selects nothing"),
            (req(ids(), ids(), None), "(got `ids` and `run_ids`)"),
            (req(ids(), None, Some(true)), "(got `ids` and `all: true`)"),
            (req(None, ids(), Some(true)), "(got `run_ids` and `all: true`)"),
            (req(Some(vec![]), None, None), "`ids` is empty"),
            (req(None, Some(vec![]), None), "`run_ids` is empty"),
        ] {
            let err = selection(bad).unwrap_err();
            assert!(err.contains(says), "{err}");
        }
    }

    #[test]
    fn a_read_names_at_most_500() {
        let n = |k: usize| Some((0..k).map(|i| format!("r{i}:failed")).collect::<Vec<_>>());
        let req = |ids, run_ids: Option<Vec<String>>| NotificationsReadRequest {
            ids,
            run_ids: run_ids.map(|r| r.into_iter().map(Into::into).collect()),
            all: None,
        };
        assert!(selection(req(n(MAX_READ_IDS), None)).is_ok());
        assert!(selection(req(None, n(MAX_READ_IDS))).is_ok());
        let err = selection(req(n(MAX_READ_IDS + 1), None)).unwrap_err();
        assert!(err.contains("`ids` names 501 entries"), "{err}");
        let err = selection(req(None, n(MAX_READ_IDS + 1))).unwrap_err();
        assert!(err.contains("`run_ids` names 501 entries"), "{err}");
    }

    #[test]
    fn a_pin_key_is_trimmed_and_its_length_limit_is_inclusive() {
        assert_eq!(pin_key("pin", " wgs "), Ok("wgs"));
        // Characters, not bytes: a multi-byte key of exactly the cap passes.
        let longest = "é".repeat(MAX_PIN_CHARS);
        assert!(pin_key("pin", &longest).is_ok());
        let too_long = "é".repeat(MAX_PIN_CHARS + 1);
        assert!(pin_key("pin", &too_long).is_err());
    }

    #[test]
    fn at_most_one_pin_change_per_update_and_its_key_is_checked() {
        let mut ops = UserPrefsUpdateRequest {
            pin: Some("a".into()),
            unpin: Some("b".into()),
            ..Default::default()
        };
        assert!(normalize_update(&mut ops).unwrap_err().contains("at most one"));
        let mut pin =
            UserPrefsUpdateRequest { pin: Some("  [\"p\",\"wf\"] ".into()), ..Default::default() };
        normalize_update(&mut pin).unwrap();
        assert_eq!(pin.pin.as_deref(), Some("[\"p\",\"wf\"]"));
        let mut blank = UserPrefsUpdateRequest { unpin: Some(" ".into()), ..Default::default() };
        assert_eq!(
            normalize_update(&mut blank).unwrap_err(),
            "unpin: ` ` must be 1 to 512 characters"
        );
    }
}
