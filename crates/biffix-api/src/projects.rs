//! The deploy's projects: each its own isolation boundary (bucket, roles, data sources), and where
//! a workflow version is registered and its runs launch.

use serde::{Deserialize, Serialize};

/// `GET /projects`: one of the deploy's projects (each has its own bucket, roles and data
/// sources; a workflow version runs in its own).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Project {
    /// The project id (`--project` at registration).
    pub id: String,
    /// The project's S3 bucket.
    pub bucket: String,
    /// Browsers may upload into the project bucket.
    pub uploads: bool,
    /// The project bucket allows the alignment viewer's cross-origin reads.
    pub viewer_cors: bool,
    /// The deploy's default project (where a request naming none goes).
    pub default: bool,
}
