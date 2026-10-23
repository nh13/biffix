//! HTTP surface (axum router + utoipa OpenAPI).
//!
//! Outside the crate only [`router`], [`ApiDoc`] and, with `aws`, `lambda` (how the Lambda bin
//! answers) are API; the handler modules are crate-private (their wire types are `biffix_api`'s).
//!
//! This is the top layer: the handlers authorize, call the services (`crate::runs`,
//! `crate::register`, `crate::qc`, …) and answer their [`crate::error::ServiceError`] as an
//! [`ApiError`]. Nothing outside it depends on it.

pub(crate) mod actor;
pub(crate) mod auth_config;
pub(crate) mod data;
pub(crate) mod error;
pub(crate) mod events;
pub(crate) mod experiments;
pub(crate) mod health;
#[cfg(feature = "aws")]
pub mod lambda;
pub(crate) mod me;
mod nul;
pub(crate) mod outputs;
pub(crate) mod projects;
pub(crate) mod provenance;
pub(crate) mod qc;
pub(crate) mod references;
pub(crate) mod register;
pub(crate) mod reports;
pub(crate) mod runs;
pub(crate) mod samples;
pub(crate) mod settings;
pub(crate) mod submit;
pub(crate) mod tasks;
pub(crate) mod uploads;
pub(crate) mod usage;

pub(crate) use error::ApiError;

use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::Router;
use utoipa::OpenApi;

use crate::AppState;

/// `s` trimmed, or `None` when it is absent or blank.
pub(crate) fn non_blank(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// A download: `body` as `content_type`, saved as `filename`.
pub(crate) fn attachment(
    content_type: &str,
    filename: &str,
    body: impl axum::response::IntoResponse,
) -> axum::response::Response {
    use axum::http::header;
    use axum::response::IntoResponse;
    let disposition = crate::object_view::content_disposition("attachment", filename);
    (
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response()
}

/// A handler family's OpenAPI operations (its `Doc`).
type FamilyDoc = fn() -> utoipa::openapi::OpenApi;

/// Each handler family: the tag its operations carry, and its OpenAPI operations.
const FAMILIES: &[(&str, FamilyDoc)] = &[
    ("auth_config", auth_config::Doc::openapi),
    ("health", health::Doc::openapi),
    ("runs", runs::Doc::openapi),
    ("tasks", tasks::Doc::openapi),
    ("experiments", experiments::Doc::openapi),
    ("outputs", outputs::Doc::openapi),
    ("reports", reports::Doc::openapi),
    ("provenance", provenance::Doc::openapi),
    ("usage", usage::Doc::openapi),
    ("qc", qc::Doc::openapi),
    ("data", data::Doc::openapi),
    ("projects", projects::Doc::openapi),
    ("uploads", uploads::Doc::openapi),
    ("samples", samples::Doc::openapi),
    ("references", references::Doc::openapi),
    ("events", events::Doc::openapi),
    ("register", register::Doc::openapi),
    ("submit", submit::Doc::openapi),
    ("settings", settings::Doc::openapi),
    ("me", me::Doc::openapi),
];

/// The OpenAPI document: the operations of every family in [`FAMILIES`], each tagged with its
/// family's name, and every response described ([`ResponseDescriptions`]).
pub struct ApiDoc;

impl OpenApi for ApiDoc {
    fn openapi() -> utoipa::openapi::OpenApi {
        let mut doc = DocInfo::openapi();
        for (tag, family) in FAMILIES {
            doc.merge(tagged(family(), tag));
        }
        utoipa::Modify::modify(&ResponseDescriptions, &mut doc);
        doc
    }
}

/// The document's own fields (its `info`, from the crate), which the families' operations join.
#[derive(OpenApi)]
struct DocInfo;

/// `doc` with `tag` on each of its operations that has none.
fn tagged(mut doc: utoipa::openapi::OpenApi, tag: &str) -> utoipa::openapi::OpenApi {
    for item in doc.paths.paths.values_mut() {
        for operation in operations(item) {
            let tags = operation.tags.get_or_insert_with(Vec::new);
            if tags.is_empty() {
                tags.push(tag.to_string());
            }
        }
    }
    doc
}

/// Every operation of a path item.
fn operations(
    item: &mut utoipa::openapi::PathItem,
) -> impl Iterator<Item = &mut utoipa::openapi::path::Operation> {
    [
        &mut item.get,
        &mut item.put,
        &mut item.post,
        &mut item.delete,
        &mut item.options,
        &mut item.head,
        &mut item.patch,
        &mut item.trace,
        &mut item.query,
    ]
    .into_iter()
    .flatten()
    .chain(item.additional_operations.values_mut())
}

/// Names each response the handlers leave undescribed by its status's reason phrase (`OK`).
///
/// OpenAPI 3.1 requires a response's `description`, but utoipa 6 omits an empty one rather than
/// writing `""`, which would leave the document invalid.
struct ResponseDescriptions;

impl utoipa::Modify for ResponseDescriptions {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::RefOr;
        for item in openapi.paths.paths.values_mut() {
            for operation in operations(item) {
                for (status, response) in &mut operation.responses.responses {
                    if let RefOr::T(response) = response {
                        if response.description.is_empty() {
                            response.description = StatusCode::from_bytes(status.as_bytes())
                                .ok()
                                .and_then(|code| code.canonical_reason())
                                .unwrap_or("Response")
                                .to_string();
                        }
                    }
                }
            }
        }
    }
}

/// The HTTP API: every family's routes ([`runs::routes`], …), behind the Cognito middleware unless
/// open ([`health::open_routes`], …).
pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .merge(runs::routes())
        .merge(submit::routes())
        .merge(tasks::routes())
        .merge(experiments::routes())
        .merge(outputs::routes())
        .merge(reports::routes())
        .merge(provenance::routes())
        .merge(qc::routes())
        .merge(usage::routes())
        .merge(data::routes())
        .merge(projects::routes())
        .merge(uploads::routes())
        .merge(samples::routes())
        .merge(references::routes())
        .merge(register::routes())
        .merge(health::routes())
        .merge(settings::routes())
        .merge(me::routes())
        // Inner to the auth check (a route layer added later runs first): only an authenticated
        // request has its body read for a NUL.
        .route_layer(axum::middleware::from_fn(nul::reject_nul))
        .route_layer(from_fn_with_state(state.clone(), actor::require_auth));

    // OAuth bootstrap and health endpoints contain no tenant data and must be reachable before a
    // browser has a token (or before an infrastructure health probe can authenticate).
    //
    // `/events` is here too, but is NOT unauthenticated: the coordinator is a machine with no user
    // identity, so it authenticates with its per-run HMAC capability token, which the handler
    // verifies itself. It must bypass the Cognito middleware, not the authorization check.
    let open = Router::new()
        .merge(auth_config::open_routes())
        .merge(health::open_routes())
        .merge(events::open_routes());

    Router::new().merge(protected).merge(open).with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_response_has_a_description() {
        // OpenAPI 3.1 requires one; see `ResponseDescriptions`.
        let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
        let mut undescribed = Vec::new();
        for (path, item) in doc["paths"].as_object().unwrap() {
            for (method, operation) in item.as_object().unwrap() {
                for (status, response) in operation["responses"].as_object().unwrap() {
                    if response["description"].as_str().unwrap_or("").is_empty() {
                        undescribed.push(format!("{method} {path} {status}"));
                    }
                }
            }
        }
        assert!(undescribed.is_empty(), "{undescribed:?}");
        assert_eq!(doc["paths"]["/auth/config"]["get"]["responses"]["200"]["description"], "OK");
    }
}
