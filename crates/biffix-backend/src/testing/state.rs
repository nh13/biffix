//! [`TestState`]: an [`AppState`] over fresh in-memory doubles, overridden one part at a time.

use std::sync::Arc;

use crate::auth::capability::CapabilityVerifier;
use crate::auth::identity::NoopIdentityResolver;
use crate::aws::CloudProvider;
use crate::config::projects::ProjectsHandle;
use crate::store::experiments::ExperimentStore;
use crate::store::references::ReferenceStore;
use crate::store::registry::RegistryStore;
use crate::store::settings::SettingsStore;
use crate::store::{EventSink, Store};
use crate::{AppState, AuthContext, DeployConfig};

use super::{FakeCloud, MemEventSink, MemRegistry, MemSettings, MemStore};

/// Builds an [`AppState`] for a test. [`TestState::new`] is fresh in-memory doubles, no auth, a
/// capability verifier over [`TEST_CAPABILITY_SECRET`] (a deploy that can launch runs and ingest
/// their events), no projects and the default deploy config; each `with_*` replaces one part
/// and [`TestState::build`] hands the state over
/// (`TestState::new().with_store(store.clone()).with_cloud(cloud.clone()).build()`).
///
/// A test that already holds a state overrides it the same way (`TestState::from(state)`), so a
/// new [`AppState`] field means one edit here rather than one per test.
#[derive(Clone)]
pub struct TestState(AppState);

/// The capability signing secret of a [`TestState::new`] state.
pub const TEST_CAPABILITY_SECRET: &[u8; 32] = &[7u8; 32];

impl Default for TestState {
    fn default() -> Self {
        Self::new()
    }
}

impl From<AppState> for TestState {
    fn from(state: AppState) -> Self {
        Self(state)
    }
}

impl TestState {
    /// Fresh in-memory doubles over no projects, with auth off and a capability verifier over
    /// [`TEST_CAPABILITY_SECRET`].
    pub fn new() -> Self {
        Self(AppState {
            store: Arc::new(MemStore::new()),
            registry: Arc::new(MemRegistry::new()),
            events: Arc::new(MemEventSink::new()),
            cloud: Arc::new(FakeCloud::new()),
            auth: None,
            capability: Some(Arc::new(CapabilityVerifier::new(TEST_CAPABILITY_SECRET.to_vec()))),
            deploy: DeployConfig::default(),
            projects: Default::default(),
            settings: Arc::new(MemSettings::default()),
            identity: Arc::new(NoopIdentityResolver),
            references: Arc::new(super::MemReferenceStore::default()),
            experiments: Arc::new(super::MemExperimentStore::default()),
        })
    }

    /// The state built so far.
    pub fn build(self) -> AppState {
        self.0
    }

    pub fn with_store(mut self, store: Arc<dyn Store>) -> Self {
        self.0.store = store;
        self
    }

    pub fn with_registry(mut self, registry: Arc<dyn RegistryStore>) -> Self {
        self.0.registry = registry;
        self
    }

    pub fn with_events(mut self, events: Arc<dyn EventSink>) -> Self {
        self.0.events = events;
        self
    }

    pub fn with_cloud(mut self, cloud: Arc<dyn CloudProvider>) -> Self {
        self.0.cloud = cloud;
        self
    }

    /// Authenticate every request with `auth` (tokens it does not verify are refused).
    pub fn with_auth(mut self, auth: impl Into<Arc<AuthContext>>) -> Self {
        self.0.auth = Some(auth.into());
        self
    }

    /// Accept `POST /events` appends carrying a capability token `verifier` verifies.
    pub fn with_capability(mut self, verifier: impl Into<Arc<CapabilityVerifier>>) -> Self {
        self.0.capability = Some(verifier.into());
        self
    }

    /// No capability secret: engine ingest is off, and no run launches.
    pub fn without_capability(mut self) -> Self {
        self.0.capability = None;
        self
    }

    pub fn with_deploy(mut self, deploy: DeployConfig) -> Self {
        self.0.deploy = deploy;
        self
    }

    pub fn with_projects(mut self, projects: Arc<ProjectsHandle>) -> Self {
        self.0.projects = projects;
        self
    }

    pub fn with_settings(mut self, settings: Arc<dyn SettingsStore>) -> Self {
        self.0.settings = settings;
        self
    }

    pub fn with_references(mut self, references: Arc<dyn ReferenceStore>) -> Self {
        self.0.references = references;
        self
    }

    pub fn with_experiments(mut self, experiments: Arc<dyn ExperimentStore>) -> Self {
        self.0.experiments = experiments;
        self
    }
}
