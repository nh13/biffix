//! Site-settings endpoints: `GET /settings` (any viewer) and `PUT /settings` (developer; admin for
//! the support contact). Back the UI's configurable title + logo, and the support contact Lab users
//! ask. Plus the operational-alert recipients (`/settings/alerts`, developer): the email
//! subscribers of the deploy's alerts SNS topic.

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};

pub(crate) use biffix_api::{
    AlertRecipient, AlertRecipientAddRequest, AlertRecipientList, AlertRecipientRemoveRequest,
    SiteSettings, SiteSettingsUpdateRequest, SupportContact,
};

use crate::api::ApiError;
use crate::auth::actor::{authorize_msg, AuthedActor};
use crate::auth::rbac::Capability;
use crate::aws::AwsError;
use crate::AppState;

/// Logo cap: a data URL beyond this is rejected (a logo is a small icon, not an upload channel).
const MAX_LOGO_BYTES: usize = 512 * 1024;
/// Title cap, in characters (it is a header, not a paragraph).
const MAX_TITLE_CHARS: usize = 100;
/// The image types a logo may be. Raster only: an SVG can carry script and markup, and every viewer
/// is served the logo, so accepting one would need a sanitizer this deploy does not have.
const LOGO_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Read the site settings (title + logo). Viewer-gated; every page fetches it for its chrome.
#[utoipa::path(get, path = "/settings", responses((status = 200, body = SiteSettings), (status = 403)))]
pub(crate) async fn get_settings(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
) -> Result<Json<SiteSettings>, ApiError> {
    authorize_msg(&state, &actor, Capability::ViewRuns)?;
    let settings = state.settings.get_settings().await?;
    Ok(Json(settings))
}

/// Whether `bytes` start as an image of `media_type` (one of [`LOGO_TYPES`]) does.
fn looks_like(media_type: &str, bytes: &[u8]) -> bool {
    match media_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()),
        _ => false,
    }
}

/// Check a logo: a base64 `data:` URL of a raster image type, within the size cap, whose data is
/// base64 and starts as an image of its type does (every viewer is served it as that image).
fn check_logo(logo: &str) -> Result<(), String> {
    use base64::Engine;
    if logo.len() > MAX_LOGO_BYTES {
        return Err(format!("logo too large ({} bytes; max {MAX_LOGO_BYTES})", logo.len()));
    }
    let (header, data) = logo
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .ok_or("logo must be a data: URL")?;
    let mut parts = header.split(';');
    let media_type = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    if !LOGO_TYPES.contains(&media_type.as_str()) {
        return Err(format!("logo must be one of {} (got `{media_type}`)", LOGO_TYPES.join(", ")));
    }
    if !parts.any(|p| p.trim().eq_ignore_ascii_case("base64")) {
        return Err("logo must be a base64 data: URL".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .map_err(|_| "logo's data is not valid base64".to_string())?;
    if !looks_like(&media_type, &bytes) {
        return Err(format!("logo's data is not a {media_type} image"));
    }
    Ok(())
}

/// Check a support contact (the same rules as a workflow's `catalog.contact`), trimmed; `Err`
/// names the field.
fn check_support(c: SupportContact) -> Result<SupportContact, String> {
    biffix_workflow::formschema::normalize_contact(&c.into())
        .map(SupportContact::from)
        .map_err(|(field, why)| format!("support.{field}: {why}"))
}

/// Update the site settings. Developer-gated (same capability as registering a workflow); a body
/// that sets or clears the support contact also needs an Admin ([`Capability::ManageSupportContact`]).
///
/// A partial update: a field the body leaves out keeps its stored value; `null` or an empty (for
/// the title, blank) string clears it.
#[utoipa::path(
    put, path = "/settings",
    request_body(content = SiteSettingsUpdateRequest, description = "absent field: keep; null or empty: clear"),
    responses((status = 200, body = SiteSettings), (status = 400), (status = 403), (status = 503))
)]
pub(crate) async fn put_settings(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<SiteSettingsUpdateRequest>,
) -> Result<Json<SiteSettings>, ApiError> {
    authorize_msg(&state, &actor, Capability::RegisterWorkflow)?;
    if req.support.is_some() {
        authorize_msg(&state, &actor, Capability::ManageSupportContact)?;
    }
    let bad = ApiError::BadRequest;
    // `Some(None)` / an empty value clears; a value is checked.
    let title = req.title.map(|t| t.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()));
    if let Some(Some(t)) = &title {
        if t.chars().count() > MAX_TITLE_CHARS {
            return Err(bad(format!("title must be at most {MAX_TITLE_CHARS} characters")));
        }
    }
    let logo = req.logo_data_url.map(|l| l.filter(|l| !l.is_empty()));
    if let Some(Some(l)) = &logo {
        check_logo(l).map_err(bad)?;
    }
    let support = req.support.map(|c| c.map(check_support).transpose()).transpose().map_err(bad)?;
    // Merged in the store, in one write: two partial updates at once keep each other's fields.
    let update = SiteSettingsUpdateRequest { title, logo_data_url: logo, support };
    let settings = state.settings.update_settings(&update).await?;
    Ok(Json(settings))
}

// ── alert recipients ─────────────────────────────────────────────────────────────────────────

fn alerts_error(e: AwsError) -> ApiError {
    match e {
        AwsError::AlertsNotConfigured => ApiError::Conflict(e.to_string()),
        _ => ApiError::from(e),
    }
}

async fn recipients(state: &AppState) -> Result<AlertRecipientList, ApiError> {
    match state.cloud.list_alert_subscribers().await {
        Ok(subs) => {
            let mut recipients: Vec<AlertRecipient> = subs
                .into_iter()
                .map(|s| AlertRecipient {
                    email: s.email,
                    status: if s.subscription_arn.is_some() { "confirmed" } else { "pending" }
                        .into(),
                })
                .collect();
            recipients.sort_by_key(|r| r.email.to_lowercase());
            Ok(AlertRecipientList { configured: true, recipients })
        }
        Err(AwsError::AlertsNotConfigured) => {
            Ok(AlertRecipientList { configured: false, recipients: vec![] })
        }
        Err(e) => Err(alerts_error(e)),
    }
}

/// List the operational-alert recipients. Developer-gated, like editing settings.
#[utoipa::path(
    get, path = "/settings/alerts",
    responses((status = 200, body = AlertRecipientList), (status = 403))
)]
pub(crate) async fn get_alert_recipients(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
) -> Result<Json<AlertRecipientList>, ApiError> {
    authorize_msg(&state, &actor, Capability::RegisterWorkflow)?;
    Ok(Json(recipients(&state).await?))
}

/// Add an alert recipient. AWS emails it a confirmation link; it receives alerts once confirmed.
/// Adding an address that is already subscribed is a no-op.
#[utoipa::path(
    post, path = "/settings/alerts",
    request_body = AlertRecipientAddRequest,
    responses((status = 200, body = AlertRecipientList), (status = 400), (status = 403), (status = 409))
)]
pub(crate) async fn add_alert_recipient(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<AlertRecipientAddRequest>,
) -> Result<Json<AlertRecipientList>, ApiError> {
    authorize_msg(&state, &actor, Capability::RegisterWorkflow)?;
    let email = req.email.trim();
    // SNS's own confirmation email is the real validation; this rejects typos and injection first.
    if !biffix_workflow::text::plausible_email(email) {
        return Err(ApiError::BadRequest(format!(
            "not a valid email address: {email:?} ({})",
            biffix_workflow::text::EMAIL_RULE
        )));
    }
    let current = recipients(&state).await?;
    if !current.configured {
        return Err(alerts_error(AwsError::AlertsNotConfigured));
    }
    if !current.recipients.iter().any(|r| r.email.eq_ignore_ascii_case(email)) {
        state.cloud.add_alert_subscriber(email).await.map_err(alerts_error)?;
    }
    Ok(Json(recipients(&state).await?))
}

/// Remove a confirmed alert recipient. A pending one cannot be removed (SNS has no id for it until
/// it is confirmed); it lapses on its own after 3 days unconfirmed. The address is in the body,
/// keeping it out of the request URL, where access logs keep it.
#[utoipa::path(
    post, path = "/settings/alerts/remove",
    request_body = AlertRecipientRemoveRequest,
    responses((status = 204), (status = 403), (status = 404), (status = 409))
)]
pub(crate) async fn remove_alert_recipient(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<AlertRecipientRemoveRequest>,
) -> Result<StatusCode, ApiError> {
    // Trimmed once: matched and echoed in the refusals as it was meant, not as pasted.
    let (state, actor, email) = (&state, &actor, req.email.trim());
    authorize_msg(state, actor, Capability::RegisterWorkflow)?;
    let subs = state.cloud.list_alert_subscribers().await.map_err(alerts_error)?;
    let matching: Vec<_> = subs.iter().filter(|s| s.email.eq_ignore_ascii_case(email)).collect();
    if matching.is_empty() {
        return Err(ApiError::NotFound(format!("{email} is not a recipient")));
    }
    let confirmed: Vec<&str> =
        matching.iter().filter_map(|s| s.subscription_arn.as_deref()).collect();
    if confirmed.is_empty() {
        return Err(ApiError::Conflict(format!(
            "{email} has not confirmed its subscription yet, so it cannot be removed; an \
                 unconfirmed subscription lapses on its own after 3 days"
        )));
    }
    for arn in confirmed {
        state.cloud.remove_alert_subscriber(arn).await.map_err(alerts_error)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// Site settings and alert recipients: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/settings", axum::routing::get(get_settings).put(put_settings))
        .route(
            "/settings/alerts",
            axum::routing::get(get_alert_recipients).post(add_alert_recipient),
        )
        .route("/settings/alerts/remove", axum::routing::post(remove_alert_recipient))
}

/// Site settings and alert recipients: the OpenAPI operations, tagged `settings` in
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        get_settings,
        put_settings,
        get_alert_recipients,
        add_alert_recipient,
        remove_alert_recipient
    ),
    components(schemas(
        biffix_api::SiteSettings,
        biffix_api::SupportContact,
        biffix_api::SiteSettingsUpdateRequest,
        biffix_api::AlertRecipient,
        biffix_api::AlertRecipientList,
        biffix_api::AlertRecipientAddRequest,
        biffix_api::AlertRecipientRemoveRequest
    ))
)]
pub(super) struct Doc;

#[cfg(test)]
mod tests {
    use super::{check_logo, MAX_LOGO_BYTES};

    #[test]
    fn a_logo_is_a_small_base64_raster_data_url() {
        for ok in [
            "data:image/png;base64,iVBORw0KGgo=",
            "data:IMAGE/JPEG;base64,/9j/4AAQ",
            "data:image/gif;base64,R0lGODlh",
            "data:image/webp;charset=binary;base64,UklGRgAAAABXRUJQ",
        ] {
            assert_eq!(check_logo(ok), Ok(()), "{ok}");
        }
        // Each refusal by its whole message, so one cannot stand in for another.
        for (bad, why) in [
            ("https://example.org/logo.png", "logo must be a data: URL"),
            ("data:image/png;base64", "logo must be a data: URL"),
            (
                "data:text/html;base64,PHNjcmlwdD4=",
                "logo must be one of image/png, image/jpeg, image/gif, image/webp (got \
                 `text/html`)",
            ),
            (
                "data:image/svg+xml;base64,PHN2Zz4=",
                "logo must be one of image/png, image/jpeg, image/gif, image/webp (got \
                 `image/svg+xml`)",
            ),
            ("data:image/png,rawbytes", "logo must be a base64 data: URL"),
            ("data:image/png;base64,not base64!", "logo's data is not valid base64"),
            // A JPEG's bytes labelled PNG, and text.
            ("data:image/png;base64,/9j/4AAQ", "logo's data is not a image/png image"),
            ("data:image/gif;base64,aGVsbG8=", "logo's data is not a image/gif image"),
        ] {
            assert_eq!(check_logo(bad), Err(why.to_string()), "{bad}");
        }
        let huge = format!("data:image/png;base64,{}", "A".repeat(MAX_LOGO_BYTES));
        assert!(check_logo(&huge).unwrap_err().contains("too large"));
    }
}
