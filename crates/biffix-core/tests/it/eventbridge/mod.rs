//! The EventBridge decoders and the ingest-queue router (`biffix_core::eventbridge`).

mod decode;
mod describe;
mod route;

use biffix_core::eventbridge::decode::QueueContext;

/// The queue context the tests decode under: one coordinator queue.
fn ctx() -> QueueContext {
    QueueContext { coordinator_queues: vec!["biffix-dev-coordinator".into()] }
}
