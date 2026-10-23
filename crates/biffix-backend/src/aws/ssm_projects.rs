//! SSM: the deploy's project contexts, one parameter per project (the [`ProjectsLoader`] a
//! deployed backend reads its projects through).

use async_trait::async_trait;

use crate::config::projects::{parse_parameters, Projects, ProjectsError, ProjectsLoader};

/// Reads one [`crate::config::projects::ProjectContext`] per SSM parameter directly under a path (non-recursive, every
/// page): the deploy writes `<path>/<project id>`.
pub(crate) struct SsmProjectsLoader {
    client: aws_sdk_ssm::Client,
    /// The parameters' path with no trailing `/` (`BIFFIX_PROJECTS_PATH` trimmed): what
    /// `GetParametersByPath` is sent, and the resource the deploy grants it on
    /// (`parameter/biffix/<tenant>/projects`).
    path: String,
    /// The default project's id (`BIFFIX_DEFAULT_PROJECT`).
    default_id: String,
}

impl SsmProjectsLoader {
    pub(crate) fn new(sdk_config: &aws_config::SdkConfig, path: &str, default_id: &str) -> Self {
        Self {
            client: aws_sdk_ssm::Client::new(sdk_config),
            path: path.trim_end_matches('/').to_string(),
            default_id: default_id.to_string(),
        }
    }
}

#[async_trait]
impl ProjectsLoader for SsmProjectsLoader {
    async fn load(&self) -> Result<Projects, ProjectsError> {
        use aws_sdk_ssm::error::DisplayErrorContext;
        let mut params = Vec::new();
        let mut pages = self
            .client
            .get_parameters_by_path()
            .path(&self.path)
            .recursive(false)
            .into_paginator()
            .send();
        while let Some(page) = pages.next().await {
            let page = page.map_err(|e| ProjectsError::Read {
                path: self.path.clone(),
                reason: DisplayErrorContext(&e).to_string(),
            })?;
            params.extend(page.parameters().iter().map(|p| {
                (
                    p.name().unwrap_or_default().to_string(),
                    p.value().unwrap_or_default().to_string(),
                )
            }));
        }
        parse_parameters(&format!("{}/", self.path), &params, &self.default_id)
    }
}
