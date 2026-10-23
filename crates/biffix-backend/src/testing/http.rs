//! Request helpers: send one request to a router and read its status and JSON body.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use serde_json::Value;
use tower::ServiceExt;

/// A response's status and body: its JSON, or its text as a JSON string when it is not JSON (an
/// error message), so a test can print or match either. The body is read whole.
pub async fn response_json(res: Response) -> (StatusCode, Value) {
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, v)
}

/// Send `req` to `app` and read the answer ([`response_json`]).
pub async fn send(app: axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    response_json(app.oneshot(req).await.unwrap()).await
}

/// Send one `method` request to `uri` (with `body` as JSON when given) and read the answer
/// ([`response_json`]). `method` is a [`Method`] or its name.
pub async fn call<M>(
    app: axum::Router,
    method: M,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value)
where
    Method: TryFrom<M>,
    <Method as TryFrom<M>>::Error: Into<axum::http::Error>,
{
    call_as(app, method, uri, body, None).await
}

/// [`call`], with `bearer` as the `Authorization: Bearer` token when given.
pub async fn call_as<M>(
    app: axum::Router,
    method: M,
    uri: &str,
    body: Option<Value>,
    bearer: Option<&str>,
) -> (StatusCode, Value)
where
    Method: TryFrom<M>,
    <Method as TryFrom<M>>::Error: Into<axum::http::Error>,
{
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(bearer) = bearer {
        req = req.header("authorization", format!("Bearer {bearer}"));
    }
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    send(app, req.body(body).unwrap()).await
}

/// `GET uri` ([`call`]).
pub async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    call(app, Method::GET, uri, None).await
}

/// `POST uri` with `body` as JSON ([`call`]).
pub async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    call(app, Method::POST, uri, Some(body)).await
}
