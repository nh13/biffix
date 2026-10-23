//! SNS: the operational alerts topic's email subscriptions.

use async_trait::async_trait;

use super::errors::{classify_configured, OrClassify};
use super::AwsCloudProvider;
use crate::aws::alerts::{is_subscription_of, AlertSubscriber, AlertTopic};
use crate::aws::AwsError;

impl AwsCloudProvider {
    fn alert_topic(&self) -> Result<&str, AwsError> {
        self.cfg.alert_topic_arn.as_deref().ok_or(AwsError::AlertsNotConfigured)
    }
}

#[async_trait]
impl AlertTopic for AwsCloudProvider {
    async fn list_alert_subscribers(&self) -> Result<Vec<AlertSubscriber>, AwsError> {
        let topic = self.alert_topic()?;
        let mut out = Vec::new();
        let mut pages =
            self.sns.list_subscriptions_by_topic().topic_arn(topic).into_paginator().send();
        while let Some(page) = pages.next().await {
            let page =
                page.map_err(|e| classify_configured(AwsError::Sns, "the alerts topic", &e))?;
            for sub in page.subscriptions() {
                if sub.protocol() != Some("email") {
                    continue;
                }
                let Some(email) = sub.endpoint() else {
                    continue;
                };
                // A pending subscription's ARN is the literal "PendingConfirmation".
                let arn =
                    sub.subscription_arn().filter(|a| a.starts_with("arn:")).map(str::to_owned);
                out.push(AlertSubscriber { email: email.to_owned(), subscription_arn: arn });
            }
        }
        Ok(out)
    }

    async fn add_alert_subscriber(&self, email: &str) -> Result<(), AwsError> {
        self.sns
            .subscribe()
            .topic_arn(self.alert_topic()?)
            .protocol("email")
            .endpoint(email)
            .send()
            .await
            .map_err(|e| classify_configured(AwsError::Sns, "the alerts topic", &e))?;
        Ok(())
    }

    async fn remove_alert_subscriber(&self, subscription_arn: &str) -> Result<(), AwsError> {
        is_subscription_of(self.alert_topic()?, subscription_arn)?;
        self.sns
            .unsubscribe()
            .subscription_arn(subscription_arn)
            .send()
            .await
            .or_classify(AwsError::Sns)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_subscription_of_the_alerts_topic_may_be_removed() {
        let topic = "arn:aws:sns:us-east-1:123:biffix-alerts";
        assert!(is_subscription_of(topic, &format!("{topic}:0b1c-4d")).is_ok());
        for other in [
            "arn:aws:sns:us-east-1:123:other:0b1c-4d",
            "arn:aws:sns:us-east-1:123:biffix-alerts-2:0b1c-4d",
            "arn:aws:sns:us-east-1:123:biffix-alerts",
            "arn:aws:sns:us-east-1:123:biffix-alerts:",
            "arn:aws:sns:us-east-1:123:biffix-alerts:a:b",
        ] {
            assert!(
                matches!(is_subscription_of(topic, other), Err(AwsError::Forbidden(_))),
                "{other}"
            );
        }
    }
}
