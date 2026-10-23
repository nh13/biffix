//! The host-lifecycle step: resolve spot vs on-demand for the hosts that lack it, from EC2.

use std::time::Duration;

use biffix_core::clock::since_epoch;
use lambda_runtime::Error;

use super::{aws_error, micros_before, note, State};

/// How long after a host is first seen to keep trying to resolve its lifecycle. EC2 keeps
/// describing an instance for about an hour after it terminates; past that the lookup cannot
/// succeed, and readers fall back to the job queue's configured capacity.
pub(super) const HOST_LIFECYCLE_WINDOW: Duration = Duration::from_secs(2 * 60 * 60);

/// Resolve spot vs on-demand for hosts that lack it, with batched EC2 DescribeInstances (by
/// `instance-id` filter, so an instance EC2 no longer knows is simply absent rather than failing
/// the call). A failed chunk is logged and the rest still run; returns how many hosts were
/// resolved, or the first error once every chunk was tried.
pub(super) async fn resolve_host_lifecycles(state: &State) -> Result<usize, Error> {
    let since_micros = micros_before(since_epoch(), HOST_LIFECYCLE_WINDOW);
    let ec2_ids =
        crate::store::hosts::hosts_missing_lifecycle(&state.pool, since_micros, 1_000).await?;
    let mut resolved = 0;
    let mut first_error: Option<Error> = None;
    // The instance-id filter takes at most 200 values.
    for chunk in ec2_ids.chunks(200) {
        let filter = aws_sdk_ec2::types::Filter::builder()
            .name("instance-id")
            .set_values(Some(chunk.to_vec()))
            .build();
        let mut pages = state.ec2.describe_instances().filters(filter).into_paginator().send();
        while let Some(page) = pages.next().await {
            let page = match page {
                Ok(page) => page,
                Err(error) => {
                    let error = aws_error("DescribeInstances", error);
                    tracing::warn!(error = %error, "host lifecycle chunk failed (continuing)");
                    note(&mut first_error, error);
                    break;
                }
            };
            for instance in
                page.reservations().iter().flat_map(aws_sdk_ec2::types::Reservation::instances)
            {
                let Some(id) = instance.instance_id() else {
                    continue;
                };
                let lifecycle = crate::decode::host_lifecycle(
                    instance
                        .instance_lifecycle()
                        .map(aws_sdk_ec2::types::InstanceLifecycleType::as_str),
                );
                match crate::store::hosts::record_host_lifecycle(&state.pool, id, &lifecycle).await
                {
                    Ok(()) => resolved += 1,
                    Err(error) => note(&mut first_error, error.into()),
                }
            }
        }
    }
    first_error.map_or(Ok(resolved), Err)
}
