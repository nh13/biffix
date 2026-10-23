//! The API served as a Lambda behind API Gateway's HTTP API (`bin/lambda.rs`): each response the
//! router answers becomes the Lambda's ([`LambdaReply`]).

use axum::http::header::{CONTENT_ENCODING, CONTENT_TYPE};
use lambda_http::{Body, IntoResponse};

/// A router response as the Lambda sends it: a text body as a string, anything else as bytes
/// (base64 in the Lambda's answer, which API Gateway decodes), as `lambda_http` sends it, but with a
/// text body sent byte for byte. `lambda_http`'s own conversion decodes a `text/*` body with the
/// charset its content type names, which drops a leading byte-order mark: the one the samples CSV
/// starts with so Excel reads it as UTF-8 (`crate::csv::UTF8_BOM`). biffix writes UTF-8 only, so a
/// text body that is UTF-8 is sent as it is, and one that is not is sent as bytes.
pub struct LambdaReply(pub axum::response::Response);

/// Whether `content_type` names text, by `lambda_http`'s rule (a missing one too): what it would
/// send as a string.
fn is_text(content_type: Option<&str>) -> bool {
    let Some(content_type) = content_type else {
        return true;
    };
    let mime = content_type.split(';').next().unwrap_or_default().trim();
    ["text", "application/json", "application/javascript", "application/xml", "application/yaml"]
        .iter()
        .any(|p| content_type.starts_with(p))
        || ["+xml", "+yaml", "+json"].iter().any(|s| mime.ends_with(s))
}

impl IntoResponse for LambdaReply {
    fn into_response(
        self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = lambda_http::Response<Body>> + Send>>
    {
        Box::pin(async move {
            let (parts, body) = self.0.into_parts();
            let bytes = match axum::body::to_bytes(body, usize::MAX).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::error!(error = %e, "lambda: reading a response body failed");
                    return lambda_http::Response::builder()
                        .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::Empty)
                        .expect("a status alone is a valid response");
                }
            };
            let content_type =
                parts.headers.get(CONTENT_TYPE).map(|v| v.to_str().unwrap_or_default());
            let text = !parts.headers.contains_key(CONTENT_ENCODING) && is_text(content_type);
            let body = match std::str::from_utf8(&bytes) {
                Ok(s) if text => Body::Text(s.to_string()),
                _ => Body::Binary(bytes.to_vec()),
            };
            lambda_http::Response::from_parts(parts, body)
        })
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;

    fn reply(
        content_type: Option<&str>,
        body: impl Into<axum::body::Body>,
    ) -> axum::response::Response {
        let mut res = axum::response::Response::new(body.into());
        if let Some(ct) = content_type {
            res.headers_mut().insert(CONTENT_TYPE, ct.parse().unwrap());
        }
        res
    }

    const CSV: &str = "\u{feff}Sample,Run\r\nCafé,r1\r\n";

    #[tokio::test]
    async fn lambda_http_on_its_own_drops_a_text_body_s_byte_order_mark() {
        // Why LambdaReply exists: lambda_http decodes a text body, and decoding strips the BOM.
        let res = IntoResponse::into_response(reply(Some("text/csv; charset=utf-8"), CSV)).await;
        match res.body() {
            Body::Text(t) => assert_eq!(t, "Sample,Run\r\nCafé,r1\r\n"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_text_body_is_sent_byte_for_byte() {
        let mut csv = reply(Some("text/csv; charset=utf-8"), CSV);
        *csv.status_mut() = StatusCode::CREATED;
        csv.headers_mut().insert("content-disposition", "attachment".parse().unwrap());
        let res = LambdaReply(csv).into_response().await;
        assert_eq!(res.status(), StatusCode::CREATED);
        assert_eq!(res.headers()["content-disposition"], "attachment");
        assert_eq!(res.body(), &Body::Text(CSV.to_string()));
        // JSON, and a body with no content type, are text too.
        let res = LambdaReply(reply(Some("application/json"), "{}")).into_response().await;
        assert_eq!(res.body(), &Body::Text("{}".into()));
        let res = LambdaReply(reply(Some("application/problem+json"), "{}")).into_response().await;
        assert_eq!(res.body(), &Body::Text("{}".into()));
        let res = LambdaReply(reply(None, "ok")).into_response().await;
        assert_eq!(res.body(), &Body::Text("ok".into()));
    }

    #[tokio::test]
    async fn anything_else_is_sent_as_bytes() {
        let zip = vec![b'P', b'K', 3, 4, 0xff];
        let res = LambdaReply(reply(Some("application/zip"), zip.clone())).into_response().await;
        assert_eq!(res.body(), &Body::Binary(zip));
        // An encoded body, and text that is not UTF-8.
        let mut gz = reply(Some("text/plain"), "x");
        gz.headers_mut().insert(CONTENT_ENCODING, "gzip".parse().unwrap());
        assert_eq!(LambdaReply(gz).into_response().await.body(), &Body::Binary(b"x".to_vec()));
        let latin1 = vec![b'c', b'a', b'f', 0xe9];
        let res = LambdaReply(reply(Some("text/plain"), latin1.clone())).into_response().await;
        assert_eq!(res.body(), &Body::Binary(latin1));
    }
}
