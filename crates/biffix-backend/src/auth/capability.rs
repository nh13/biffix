//! Capability-token minting and verification for this deployment.
//!
//! Wraps the shared HMAC scheme in [`biffix_core::capability`] with this process's secret and clock.
//! The minter (submit) and the verifier (`POST /events`) are deliberately built from the SAME secret
//! and the same code, so a token this control plane issues is exactly what it will accept.

use std::sync::Arc;

use biffix_core::capability::{mint, verify, CapabilityError};
use biffix_core::ids::RunId;

/// The attempts of a coordinator job and each attempt's timeout, as its job definition sets them
/// (`infra/biffix_infra/constructs/coordinator.py`; `infra/tests/synth/test_api.py` fails when
/// they differ). Batch retries a lost host under the same job id and environment, so every attempt
/// runs on the token minted at launch.
const COORDINATOR_ATTEMPTS: u64 = 2;
const COORDINATOR_TIMEOUT_HOURS: u64 = 24;

/// Time a coordinator may wait in its queue, before its first attempt and before a retry. Batch
/// does not bound it; this is a margin for a busy or scaling-up coordinator compute environment.
const QUEUE_MARGIN_HOURS: u64 = 24;

/// How long a run's ingest capability stays valid: every attempt of the coordinator for its whole
/// timeout, plus [`QUEUE_MARGIN_HOURS`]. A token that expires mid-run silently stops tracking
/// while the run keeps going (the plugins drop a batch the route answers 401).
pub(crate) const DEFAULT_TTL_SECS: u64 =
    (COORDINATOR_ATTEMPTS * COORDINATOR_TIMEOUT_HOURS + QUEUE_MARGIN_HOURS) * 3600;

/// Holds the signing secret for this deployment. Shared through an `Arc` (the app state and the
/// provider's [`Self::minter`] hold the same one), so the secret itself is not.
pub struct CapabilityVerifier {
    secret: Vec<u8>,
}

impl CapabilityVerifier {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self { secret: secret.into() }
    }

    /// Mint the token handed to a run's coordinator, valid for [`DEFAULT_TTL_SECS`].
    pub fn mint_for(&self, run_id: &RunId) -> String {
        mint(&self.secret, run_id, biffix_core::clock::epoch_secs_u64() + DEFAULT_TTL_SECS)
    }

    /// Verify an inbound token and return the run it authorizes.
    pub(crate) fn verify(&self, token: &str) -> Result<RunId, CapabilityError> {
        verify(&self.secret, token, biffix_core::clock::epoch_secs_u64())
    }

    /// A minter closure for the AWS provider, which owns token creation at submit.
    pub(crate) fn minter(self: &Arc<Self>) -> Box<dyn Fn(&RunId) -> String + Send + Sync> {
        let this = Arc::clone(self);
        Box::new(move |run: &RunId| this.mint_for(run))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_outlives_a_retried_coordinator_that_waited_in_its_queue() {
        // Both attempts ran to their timeout, after most of a day queued.
        let cap = CapabilityVerifier::new(b"secret".to_vec());
        let run = RunId::new("run-1");
        let token = cap.mint_for(&run);
        let minted = biffix_core::clock::epoch_secs_u64();
        let late = minted + 2 * 24 * 3600 + 23 * 3600;
        assert_eq!(verify(b"secret", &token, late), Ok(run));
        // It does expire: a token is not forever.
        let after = minted + DEFAULT_TTL_SECS + 60;
        assert!(verify(b"secret", &token, after).is_err());
    }
}
