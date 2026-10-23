//! The EC2 wire event (`source: aws-ec2`).
//!
//! It carries only the spot interruption warning. Attribution instance→task is a
//! projection concern, not this event's job — it stays a bare fact.

use serde::{Deserialize, Serialize};

use crate::ids::InstanceId;
use crate::time::Timestamp;

/// An EC2 spot instance interruption warning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ec2Event {
    pub ts: Timestamp,
    /// `detail.instance-id`.
    pub instance_id: InstanceId,
    #[serde(rename = "kind")]
    pub kind: Ec2EventKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ec2EventKind {
    SpotInterruptionWarning,
    #[serde(untagged)]
    Unknown(String),
}
