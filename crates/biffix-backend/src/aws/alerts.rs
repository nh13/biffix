//! SNS: the operational alerts topic's email recipients, which the Settings page manages
//! ([`AlertTopic`]).

use async_trait::async_trait;

use super::AwsError;

/// One email subscribed to the operational alerts topic.
#[derive(Clone, Debug, PartialEq)]
pub struct AlertSubscriber {
    pub email: String,
    /// The subscription ARN once the recipient confirmed; `None` while pending confirmation (SNS
    /// cannot unsubscribe a pending subscription; it lapses after 3 days unconfirmed).
    pub subscription_arn: Option<String>,
}

/// The alerts topic's email subscriptions (see the module docs).
#[async_trait]
pub trait AlertTopic: Send + Sync {
    /// The email subscribers of the operational alerts topic (confirmed and pending).
    async fn list_alert_subscribers(&self) -> Result<Vec<AlertSubscriber>, AwsError>;

    /// Subscribe `email` to the alerts topic; AWS emails it a confirmation link.
    async fn add_alert_subscriber(&self, email: &str) -> Result<(), AwsError>;

    /// Remove a confirmed subscription of the alerts topic by its ARN; an ARN that is not a
    /// subscription of that topic is [`AwsError::Forbidden`].
    async fn remove_alert_subscriber(&self, subscription_arn: &str) -> Result<(), AwsError>;
}

/// Refuse to touch a subscription that is not of `topic_arn`: a subscription ARN is
/// `<topic arn>:<subscription id>`. The provider's guard, and the test double's.
pub(crate) fn is_subscription_of(topic_arn: &str, subscription_arn: &str) -> Result<(), AwsError> {
    match subscription_arn.strip_prefix(topic_arn) {
        Some(id) if id.len() > 1 && id.starts_with(':') && !id[1..].contains(':') => Ok(()),
        _ => Err(AwsError::Forbidden(format!(
            "{subscription_arn} is not a subscription of the alerts topic"
        ))),
    }
}
