//! CloudWatch Logs: a page of a Batch job's log stream.

use async_trait::async_trait;

use super::errors::OrClassify;
use super::AwsCloudProvider;
use crate::aws::logs::{CloudLogEvent, CloudLogPage, LogDirection, LogReader, BATCH_LOG_GROUP};
use crate::aws::AwsError;

#[async_trait]
impl LogReader for AwsCloudProvider {
    async fn get_log_events(
        &self,
        log_stream: &str,
        cursor: Option<&str>,
        direction: LogDirection,
        limit: i32,
    ) -> Result<CloudLogPage, AwsError> {
        self.get_log_events_in(BATCH_LOG_GROUP, log_stream, cursor, direction, limit).await
    }

    async fn get_log_events_in(
        &self,
        log_group: &str,
        log_stream: &str,
        cursor: Option<&str>,
        direction: LogDirection,
        limit: i32,
    ) -> Result<CloudLogPage, AwsError> {
        let mut req = self
            .logs
            .get_log_events()
            .log_group_name(log_group)
            .log_stream_name(log_stream)
            .limit(limit)
            // Forward reads oldest-first (live-tail toward newer); Backward starts at the tail.
            .start_from_head(matches!(direction, LogDirection::Forward));
        if let Some(token) = cursor {
            req = req.next_token(token);
        }
        // A stream that expired, rotated or was never created, or a group that does not exist
        // (ResourceNotFoundException), is `NotFound`: a 404, not a server error.
        let out = req.send().await.or_classify(AwsError::Logs)?;
        let events = out
            .events()
            .iter()
            .map(|e| CloudLogEvent {
                timestamp_ms: e.timestamp().unwrap_or_default(),
                message: e.message().unwrap_or_default().to_string(),
            })
            .collect();
        Ok(CloudLogPage {
            events,
            next_forward_token: out.next_forward_token().map(String::from),
            next_backward_token: out.next_backward_token().map(String::from),
        })
    }
}
