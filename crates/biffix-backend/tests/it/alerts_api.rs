//! `/settings/alerts`: list, add and remove operational-alert recipients (the email subscribers of
//! the deploy's alerts topic), with an in-memory SNS double.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{json, Value};

use biffix_backend::api;
use biffix_backend::aws::alerts::AlertSubscriber;
use biffix_backend::testing::{call, FakeCloud, TestState};

fn app(subscribers: Option<Vec<AlertSubscriber>>) -> (axum::Router, Arc<FakeCloud>) {
    let cloud = Arc::new(FakeCloud::new());
    *cloud.alert_subscribers.lock().unwrap() = subscribers;
    let state = TestState::new().with_cloud(cloud.clone()).build();
    (api::router(state), cloud)
}

fn confirmed(email: &str) -> AlertSubscriber {
    AlertSubscriber {
        email: email.into(),
        subscription_arn: Some(format!("arn:aws:sns:us-east-1:1:alerts:{email}")),
    }
}

fn pending(email: &str) -> AlertSubscriber {
    AlertSubscriber { email: email.into(), subscription_arn: None }
}

#[tokio::test]
async fn lists_recipients_with_their_confirmation_status() {
    let (app, _) = app(Some(vec![confirmed("b@example.org"), pending("a@example.org")]));
    let (status, v) = call(app, "GET", "/settings/alerts", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v,
        json!({"configured": true, "recipients": [
            {"email": "a@example.org", "status": "pending"},
            {"email": "b@example.org", "status": "confirmed"},
        ]})
    );
}

#[tokio::test]
async fn a_deploy_without_an_alerts_topic_reports_unconfigured() {
    let (app, _) = app(None);
    let (status, v) = call(app.clone(), "GET", "/settings/alerts", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v, json!({"configured": false, "recipients": []}));
    let (status, _) =
        call(app, "POST", "/settings/alerts", Some(json!({"email": "ops@example.org"}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn adding_subscribes_once_and_rejects_a_malformed_address() {
    let (app, cloud) = app(Some(vec![]));
    let (status, v) =
        call(app.clone(), "POST", "/settings/alerts", Some(json!({"email": " ops@example.org "})))
            .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["recipients"][0], json!({"email": "ops@example.org", "status": "pending"}));
    // Re-adding (in any case) is a success that does not subscribe twice.
    let (status, v) =
        call(app.clone(), "POST", "/settings/alerts", Some(json!({"email": "OPS@example.org"})))
            .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["recipients"], json!([{"email": "ops@example.org", "status": "pending"}]));
    assert_eq!(
        cloud.alert_subscribers.lock().unwrap().clone().unwrap(),
        vec![pending("ops@example.org")]
    );

    let (status, _) =
        call(app, "POST", "/settings/alerts", Some(json!({"email": "not an email"}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn removing_a_confirmed_recipient_unsubscribes_it() {
    let (app, cloud) = app(Some(vec![confirmed("ops@example.org"), confirmed("x@example.org")]));
    let (status, _) = remove(app, "OPS@example.org").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let left = cloud.alert_subscribers.lock().unwrap().clone().unwrap();
    assert_eq!(left, vec![confirmed("x@example.org")]);
}

#[tokio::test]
async fn a_pending_or_unknown_recipient_cannot_be_removed() {
    let (app, cloud) = app(Some(vec![pending("new@example.org")]));
    let (status, v) = remove(app.clone(), "new@example.org").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(v.as_str().unwrap().contains("not confirmed"), "{v}");
    let (status, _) = remove(app, "nobody@example.org").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Neither refusal unsubscribed anyone.
    assert_eq!(
        cloud.alert_subscribers.lock().unwrap().clone().unwrap(),
        vec![pending("new@example.org")]
    );
}

/// Remove `email` (`POST /settings/alerts/remove`: the address in the body, not the request URL).
async fn remove(app: axum::Router, email: &str) -> (StatusCode, Value) {
    call(app, "POST", "/settings/alerts/remove", Some(json!({ "email": email }))).await
}
