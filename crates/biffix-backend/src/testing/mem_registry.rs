//! [`MemRegistry`]: the in-memory [`RegistryStore`], and the fake image artifacts and coordinator
//! variants it hands out.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::store::registry::{
    CoordinatorVariant, ImageArtifact, RegisteredVersion, RegistryStore, SourceBuild, VersionImage,
};
use crate::store::StoreError;
use async_trait::async_trait;

use super::fake_variant_job_definition;

/// In-memory [`RegistryStore`] double. Builds and coordinator variants are set by tests; a
/// version put with a runtime gets a ready variant on [`FAKE_BASE_DIGEST`] unless the test turns
/// that off (`auto_variants`), so an ordinary submit test launches like a registered version whose
/// runtime was built.
pub struct MemRegistry {
    pub versions: Mutex<BTreeMap<String, RegisteredVersion>>,
    pub builds: Mutex<BTreeMap<String, biffix_core::builds::BuildProjection>>,
    pub variants: Mutex<Vec<CoordinatorVariant>>,
    pub artifacts: Mutex<BTreeMap<String, ImageArtifact>>,
    pub auto_variants: std::sync::atomic::AtomicBool,
    /// Whether a registered version's images count as imported (and, for a Snakemake version,
    /// derived) at once, like `auto_variants`.
    pub auto_images: std::sync::atomic::AtomicBool,
    /// `workflow_listing`, keyed by (project, name).
    pub listing: Mutex<BTreeMap<(String, String), biffix_api::WorkflowListing>>,
    /// `workflow_version_releases`: every change, oldest first, with its version id.
    pub releases: Mutex<Vec<(String, biffix_api::WorkflowReleaseChange)>>,
    /// Make every release read (`released_versions`, `release_history`) fail as unavailable.
    pub fail_releases: std::sync::atomic::AtomicBool,
}

impl Default for MemRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MemRegistry {
    /// A release read, failing while `fail_releases` is set.
    fn release_read(&self) -> Result<(), StoreError> {
        if self.fail_releases.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::Unavailable("releases read failed (test)".into()));
        }
        Ok(())
    }

    /// One version's recorded changes, newest first.
    fn history_of(&self, version_id: &str) -> Vec<biffix_api::WorkflowReleaseChange> {
        self.releases
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|(id, _)| id == version_id)
            .map(|(_, c)| c.clone())
            .collect()
    }

    pub fn new() -> Self {
        Self {
            versions: Mutex::new(BTreeMap::new()),
            builds: Mutex::new(BTreeMap::new()),
            variants: Mutex::new(Vec::new()),
            artifacts: Mutex::new(BTreeMap::new()),
            auto_variants: std::sync::atomic::AtomicBool::new(true),
            auto_images: std::sync::atomic::AtomicBool::new(true),
            listing: Mutex::new(BTreeMap::new()),
            releases: Mutex::new(Vec::new()),
            fail_releases: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

/// The imported image a declared image gets (bash, glibc and a Snakemake 9, so it suits any engine).
pub fn fake_artifact(image: &VersionImage) -> ImageArtifact {
    let digest = crate::register::pipeline::digest_of(&image.source_ref).to_string();
    ImageArtifact {
        content_key: image.content_key.clone(),
        source_ref: image.source_ref.clone(),
        image: format!("{FAKE_WORKFLOW_REPO}@{digest}"),
        digest,
        archs: vec!["amd64".into(), "arm64".into()],
        capabilities: biffix_workflow::images::Capabilities::parse(
            "bash=1,libc=glibc,snakemake=9.27.0",
        ),
        keep_tag: Some("biffix-keep-x".into()),
        archived: true,
        archive_bytes: Some(1),
        created_at: 1,
    }
}

/// The derived image a Snakemake version's `image` gets with the runtime `runtime_key` (under the
/// derive's key, as `crate::images::derived` records it).
pub fn fake_derived_artifact(image: &VersionImage, runtime_key: &str) -> ImageArtifact {
    let imported = fake_artifact(image);
    let key = biffix_workflow::images::derived_key(&imported.digest, runtime_key);
    ImageArtifact {
        content_key: key.clone(),
        source_ref: imported.image.clone(),
        image: format!("{FAKE_WORKFLOW_REPO}@sha256:{key}"),
        digest: format!("sha256:{key}"),
        ..imported
    }
}

/// The tenant workflow repository images are imported into.
pub const FAKE_WORKFLOW_REPO: &str = "111122223333.dkr.ecr.us-east-1.amazonaws.com/biffix-workers";

/// The base coordinator image the fake cloud reports as deployed.
pub const FAKE_BASE_DIGEST: &str = "sha256:base";
pub const FAKE_COORDINATOR_REPO: &str = "123.dkr.ecr.us-east-1.amazonaws.com/coord";

/// The ready variant a version with `runtime_key` gets on `base_digest` (its image and job
/// definition both name the base, so variants of different bases differ; template revision 1).
pub fn fake_variant(runtime_key: &str, base_digest: &str) -> CoordinatorVariant {
    let base = base_digest.strip_prefix("sha256:").unwrap_or(base_digest);
    CoordinatorVariant {
        runtime_key: runtime_key.to_string(),
        base_digest: base_digest.to_string(),
        image: format!("{FAKE_COORDINATOR_REPO}@sha256:variant-{runtime_key}-{base}"),
        job_definition_arn: fake_variant_job_definition(runtime_key, base_digest, 1),
        created_at: 1,
    }
}

/// A version's runtime fields for a pinned `engine` release, as registration freezes them.
pub fn runtime_fields(engine: &str, version: &str) -> (String, serde_json::Value) {
    let parsed = biffix_workflow::engines::EngineVersion::parse(version).expect("exact version");
    let recipe = biffix_workflow::runtime::recipe_for(engine, parsed).expect("supported release");
    (biffix_workflow::runtime::runtime_key(&recipe), serde_json::to_value(&recipe).unwrap())
}

#[async_trait]
impl RegistryStore for MemRegistry {
    async fn put_version(&self, rec: &RegisteredVersion) -> Result<(), StoreError> {
        let mut map = self.versions.lock().unwrap();
        // Like Postgres: a repeated id, or a second version claiming the same build of one source
        // (the primary key and the unique (git_sha, source_content_hash, build) index).
        let same_build = |r: &RegisteredVersion| {
            r.version.git_sha == rec.version.git_sha
                && r.version.source_content_hash == rec.version.source_content_hash
                && r.version.build == rec.version.build
        };
        if map.contains_key(&rec.version.id) || map.values().any(same_build) {
            return Err(StoreError::Conflict(format!(
                "workflow version {} already exists",
                rec.version.id
            )));
        }
        map.insert(rec.version.id.clone(), rec.clone());
        let key = &rec.version.runtime_key;
        if self.auto_variants.load(std::sync::atomic::Ordering::SeqCst) {
            let mut variants = self.variants.lock().unwrap();
            // A variant is one (runtime, base): one on another base is not this one.
            if !variants.iter().any(|v| &v.runtime_key == key && v.base_digest == FAKE_BASE_DIGEST)
            {
                variants.push(fake_variant(key, FAKE_BASE_DIGEST));
            }
        }
        if self.auto_images.load(std::sync::atomic::Ordering::SeqCst) {
            let mut artifacts = self.artifacts.lock().unwrap();
            for image in &rec.images {
                artifacts.entry(image.content_key.clone()).or_insert_with(|| fake_artifact(image));
                if let (biffix_workflow::engines::Engine::Snakemake, false) =
                    (rec.version.engine, image.own_runtime)
                {
                    let derived = fake_derived_artifact(image, key);
                    artifacts.entry(derived.content_key.clone()).or_insert(derived);
                }
            }
        }
        Ok(())
    }
    async fn get_build(
        &self,
        subject: &str,
    ) -> Result<Option<biffix_core::builds::BuildProjection>, StoreError> {
        Ok(self.builds.lock().unwrap().get(subject).cloned())
    }
    async fn coordinator_variants(
        &self,
        runtime_key: &str,
    ) -> Result<Vec<CoordinatorVariant>, StoreError> {
        let mut found: Vec<CoordinatorVariant> = self
            .variants
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.runtime_key == runtime_key)
            .cloned()
            .collect();
        // Like Postgres: `ORDER BY created_at DESC, base_digest`.
        found.sort_by(|a, b| {
            b.created_at.cmp(&a.created_at).then_with(|| a.base_digest.cmp(&b.base_digest))
        });
        Ok(found)
    }
    async fn get_image_artifact(
        &self,
        content_key: &str,
    ) -> Result<Option<ImageArtifact>, StoreError> {
        Ok(self.artifacts.lock().unwrap().get(content_key).cloned())
    }
    async fn put_image_artifact(&self, artifact: &ImageArtifact) -> Result<(), StoreError> {
        self.artifacts
            .lock()
            .unwrap()
            .entry(artifact.content_key.clone())
            .or_insert_with(|| artifact.clone());
        Ok(())
    }
    async fn replace_image_artifact(
        &self,
        stale_image: &str,
        artifact: &ImageArtifact,
    ) -> Result<(), StoreError> {
        if let Some(recorded) = self
            .artifacts
            .lock()
            .unwrap()
            .get_mut(&artifact.content_key)
            .filter(|recorded| recorded.image == stale_image)
        {
            *recorded = artifact.clone();
        }
        Ok(())
    }
    async fn put_coordinator_variant(
        &self,
        variant: &CoordinatorVariant,
    ) -> Result<(), StoreError> {
        let mut variants = self.variants.lock().unwrap();
        match variants
            .iter_mut()
            .find(|v| v.runtime_key == variant.runtime_key && v.base_digest == variant.base_digest)
        {
            // Like the store: the first image stays; the same image takes a new job definition.
            Some(v) if v.image == variant.image => {
                v.job_definition_arn = variant.job_definition_arn.clone();
            }
            Some(_) => {}
            None => variants.push(variant.clone()),
        }
        Ok(())
    }
    async fn hidden_workflows(
        &self,
    ) -> Result<std::collections::HashSet<(String, String)>, StoreError> {
        Ok(self
            .listing
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e)| e.hidden)
            .map(|(k, _)| k.clone())
            .collect())
    }
    async fn workflow_listing(
        &self,
        project: &str,
        name: &str,
    ) -> Result<Option<biffix_api::WorkflowListing>, StoreError> {
        Ok(self.listing.lock().unwrap().get(&(project.to_string(), name.to_string())).cloned())
    }
    async fn put_workflow_listing(
        &self,
        project: &str,
        name: &str,
        hidden: bool,
        updated_by: Option<&str>,
    ) -> Result<biffix_api::WorkflowListing, StoreError> {
        // Like Postgres: only a workflow with a registered version can be curated.
        let exists =
            self.versions.lock().unwrap().values().any(|r| r.project == project && r.name == name);
        if !exists {
            return Err(StoreError::NotFound);
        }
        let entry = biffix_api::WorkflowListing {
            project: project.to_string(),
            name: name.to_string(),
            hidden,
            updated_at_micros: biffix_core::clock::epoch_micros(),
            updated_by: updated_by.map(str::to_string),
        };
        self.listing.lock().unwrap().insert((project.to_string(), name.to_string()), entry.clone());
        Ok(entry)
    }
    async fn released_versions(
        &self,
        version_ids: &[String],
    ) -> Result<std::collections::HashMap<String, biffix_api::WorkflowReleaseChange>, StoreError>
    {
        self.release_read()?;
        // The newest change per named version (later entries win), kept when it released it.
        let mut newest = std::collections::HashMap::new();
        for (id, change) in self.releases.lock().unwrap().iter() {
            if version_ids.contains(id) {
                newest.insert(id.clone(), change.clone());
            }
        }
        newest.retain(|_, c: &mut biffix_api::WorkflowReleaseChange| c.released);
        Ok(newest)
    }
    async fn release_history(
        &self,
        version_id: &str,
    ) -> Result<Vec<biffix_api::WorkflowReleaseChange>, StoreError> {
        self.release_read()?;
        Ok(self.history_of(version_id))
    }
    async fn set_version_released(
        &self,
        version_id: &str,
        released: bool,
        actor: Option<&str>,
        actor_label: Option<&str>,
    ) -> Result<Vec<biffix_api::WorkflowReleaseChange>, StoreError> {
        // Like Postgres: only a registered version, and only a change is recorded.
        if !self.versions.lock().unwrap().contains_key(version_id) {
            return Err(StoreError::NotFound);
        }
        let current = self.history_of(version_id).first().cloned();
        if current.is_some_and(|c| c.released) != released {
            self.releases.lock().unwrap().push((
                version_id.to_string(),
                biffix_api::WorkflowReleaseChange {
                    released,
                    at_micros: biffix_core::clock::epoch_micros(),
                    by: actor.map(str::to_string),
                    by_label: actor_label.map(str::to_string),
                },
            ));
        }
        Ok(self.history_of(version_id))
    }
    async fn get_version(&self, id: &str) -> Result<RegisteredVersion, StoreError> {
        self.versions.lock().unwrap().get(id).cloned().ok_or(StoreError::NotFound)
    }
    async fn list_versions(&self) -> Result<Vec<RegisteredVersion>, StoreError> {
        // Newest first, ties by id bytewise (Postgres: ORDER BY created_at DESC, id COLLATE "C").
        let mut versions: Vec<RegisteredVersion> =
            self.versions.lock().unwrap().values().cloned().collect();
        versions.sort_by(|a, b| {
            b.created_at_micros
                .cmp(&a.created_at_micros)
                .then_with(|| a.version.id.cmp(&b.version.id))
        });
        Ok(versions)
    }
    async fn source_builds(
        &self,
        git_sha: &str,
        content_hash: &str,
    ) -> Result<Vec<SourceBuild>, StoreError> {
        let mut builds: Vec<SourceBuild> = self
            .versions
            .lock()
            .unwrap()
            .values()
            .filter(|r| {
                r.version.git_sha == git_sha && r.version.source_content_hash == content_hash
            })
            .map(SourceBuild::from)
            .collect();
        // By build, as Postgres orders them.
        builds.sort_by_key(|b| b.build);
        Ok(builds)
    }
}
