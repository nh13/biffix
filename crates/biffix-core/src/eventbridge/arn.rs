//! The names AWS ARNs carry, which every reader of a Batch job takes alike: the decoders
//! ([`super::decode`]), the execution builder ([`super::execution`]) and the ingest reconciler.

/// A Batch job queue's name from its ARN (`arn:aws:batch:…:job-queue/<name>`); a bare name is
/// returned unchanged.
pub fn queue_name(queue_arn: &str) -> &str {
    queue_arn.rsplit([':', '/']).next().unwrap_or(queue_arn)
}

/// The container-instance id in a `containerInstanceArn`: its last `/` segment (the whole string
/// when it has none). Batch job events and ECS host facts are keyed by it, so they join.
pub fn container_instance_id(container_instance_arn: &str) -> &str {
    container_instance_arn.rsplit('/').next().unwrap_or(container_instance_arn)
}
