//! Requests carrying a NUL character ([`reject_nul`]): Postgres stores no NUL in text, so one bound
//! as a parameter (a search, a filter, a name, a path id) fails the query as a data error, which
//! must be the caller's 400, never a retryable 503.

use axum::extract::Request;
use axum::http::header::CONTENT_TYPE;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::ApiError;

/// The largest JSON body read whole to look for an escaped NUL: past any route's own body limit and
/// the 6 MB a Lambda request can carry, so no body a route would accept is refused for its size.
const MAX_SCANNED_BODY: usize = 8 * 1024 * 1024;

/// The 400 for a request carrying a NUL.
fn nul_refused(where_: &str) -> Response {
    ApiError::BadRequest(format!(
        "the request's {where_} holds a NUL character (`\\u0000`), which biffix never stores"
    ))
    .into_response()
}

/// Whether `bytes` (a JSON text) holds an escaped NUL (`\u0000`, its backslash not itself escaped)
/// or a raw one.
fn json_has_nul(bytes: &[u8]) -> bool {
    // One forward pass: an escaped backslash (`\\`) is stepped over whole, so a backslash met
    // here is never itself escaped.
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            0 => return true,
            b'\\' if bytes[i + 1..].starts_with(b"u0000") => return true,
            b'\\' if bytes.get(i + 1) == Some(&b'\\') => i += 2,
            _ => i += 1,
        }
    }
    false
}

/// Refuse (400) a request whose path or query holds a NUL (`%00`), or whose JSON body does. Runs
/// after authentication, so only a caller who may use the API has a body read here; the body is
/// handed on whole to the route, whose own body limit still applies.
pub(super) async fn reject_nul(request: Request, next: Next) -> Response {
    let uri = request.uri();
    let target = uri.path_and_query().map_or(uri.path(), |p| p.as_str());
    if target.to_ascii_lowercase().contains("%00") {
        return nul_refused("path or query");
    }
    let json = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("json"));
    if !json {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_SCANNED_BODY).await else {
        return ApiError::PayloadTooLarge(format!(
            "the request body is larger than {} MiB",
            MAX_SCANNED_BODY / (1024 * 1024)
        ))
        .into_response();
    };
    if json_has_nul(&bytes) {
        return nul_refused("body");
    }
    next.run(Request::from_parts(parts, bytes.into())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_escaped_or_raw_nul_is_found_and_an_escaped_backslash_is_not_one() {
        assert!(json_has_nul(br#"{"q":"a\u0000b"}"#));
        assert!(json_has_nul(b"{\"q\":\"a\0b\"}"));
        assert!(json_has_nul(br#"{"q":"a\\\u0000"}"#));
        // `\\u0000` is a backslash, then the text `u0000`.
        assert!(!json_has_nul(br#"{"q":"a\\u0000"}"#));
        assert!(!json_has_nul(br#"{"q":"u0000"}"#));
        assert!(!json_has_nul(br#"{"q":"\u0001"}"#));
    }
}
