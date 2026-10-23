//! A run's life outside the HTTP handlers: reading one for a caller ([`access`]), [`submit`] and [`launch`] (the submit pipeline and its
//! tail), [`cancel`], how the API shows a run ([`detail`]), its logs and attempts ([`logs`]) and why
//! it failed ([`failure`]), the launch form's [`estimate`], the provenance crate ([`provenance`]),
//! the task page ([`task_page`]), spend over a period ([`usage`]), and the experiments runs are
//! grouped into ([`experiments`]) with their samples download ([`samples_csv`]).

pub(crate) mod access;
pub(crate) mod cancel;
pub(crate) mod detail;
pub mod estimate;
pub mod experiments;
pub(crate) mod failure;
pub mod launch;
pub(crate) mod logs;
pub mod output_prefix;
pub(crate) mod outputs;
pub mod provenance;
pub(crate) mod samples_csv;
pub mod submit;
pub mod task_page;
pub mod usage;
