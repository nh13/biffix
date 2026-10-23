//! The registration pipeline behind `POST /workflows` and `POST /workflows/{id}/reregister`:
//! validate a request and pin what it names ([`prepare`]), then freeze and record the version and
//! start its runtime and image builds ([`commit`]); and what a re-registration would do
//! ([`plan_of`]). The HTTP handlers (`crate::api::register`) authorize, call these and shape the
//! response; an error is the status and message the handler returns.

use biffix_api::{RegisterRequest, ReregisterPlan};
use futures::{StreamExt, TryStreamExt};

use crate::error::ServiceError;
use crate::register::version::{freeze_version, RegisterError, RegisterInput};
use crate::register::workflow::WorkflowSource;
use crate::store::registry::RegisteredVersion;
use crate::store::StoreError;
use crate::AppState;
use biffix_workflow::digest::is_sha256_hex;
use biffix_workflow::engines::Engine;
use biffix_workflow::formschema::FormSchema;
use biffix_workflow::images::{ecr_coords_from_repo_uri, EcrCoords};
use serde::Deserialize;

/// How many times a registration freezes its version again after losing its build number to a
/// concurrent registration of the same source ([`commit`]).
const MAX_BUILD_RACES: usize = 3;

/// How many of a registration's images are pinned, or started, at once.
const ENSURE_CONCURRENCY: usize = 8;

/// The most images (the default included) one registration may declare: each is a registry lookup
/// at registration and an import build after, so an unbounded list is a cheap way to fan out
/// thousands of outbound requests and builds. Well above what a real pipeline declares (an
/// nf-core pipeline's few dozen containers).
pub(crate) const MAX_REGISTER_IMAGES: usize = 256;

/// The deploy's workflow (worker-image) repository as ECR coordinates, where a version's default
/// image lives: recorded on a version at registration, and the deep link of a version registered
/// before that was recorded.
pub(crate) fn workflow_repo_coords(state: &AppState) -> Option<EcrCoords> {
    ecr_coords_from_repo_uri(&state.deploy.worker_image_repo)
}

/// Validate a `kind:s3` source uri points at the version's project bucket under the sanctioned
/// [`crate::aws::objects::SOURCE_PREFIX`] staging prefix. A `kind:git` source is unaffected. This is a
/// security gate: register records the uri the coordinator later fetches and executes, so a caller
/// must not be able to aim a version at an arbitrary object. A tarball names its `sha256` (which
/// the coordinator checks the staged object against), and it must be a sha256.
fn validate_s3_source_location(
    source: &WorkflowSource,
    project_bucket: &str,
) -> Result<(), String> {
    let WorkflowSource::S3 { uri, sha256 } = source else {
        return Ok(());
    };
    match sha256 {
        Some(sha256) if !is_sha256_hex(sha256) => {
            return Err(format!(
                "the s3 source's sha256 must be 64 lowercase hex digits: {sha256}"
            ));
        }
        None if uri.ends_with(".tar.gz") || uri.ends_with(".tgz") => {
            return Err(format!("the s3 source {uri} is a tarball, so it must name its sha256"));
        }
        _ => {}
    }
    staged_key(uri, project_bucket).map(|_| ())
}

/// The key of `uri` when it names an object in the staging area (the project bucket, under
/// [`crate::aws::objects::SOURCE_PREFIX`]); why not otherwise. The one rule for both staged sources and
/// staged build contexts. No `..`: the presigner never stages a key with one, and the
/// coordinator's and the build's fetch must not be able to resolve out of the staging area.
fn staged_key<'a>(uri: &'a str, project_bucket: &str) -> Result<&'a str, String> {
    if project_bucket.is_empty() {
        return Err(
            "s3-staged sources are not configured on this deploy (no project bucket)".into()
        );
    }
    if !uri.starts_with("s3://") {
        return Err(format!("s3 source uri must start with s3://: {uri}"));
    }
    let (bucket, key) = biffix_core::s3_layout::split_s3_uri(uri)
        .ok_or_else(|| format!("s3 source uri has no key: {uri}"))?;
    if bucket != project_bucket {
        return Err(format!(
            "s3 source must be in the project bucket ({project_bucket}), not {bucket}"
        ));
    }
    if !key.starts_with(crate::aws::objects::SOURCE_PREFIX) || key.contains("..") {
        return Err(format!(
            "s3 source key must be under the {} prefix, with no `..`: {uri}",
            crate::aws::objects::SOURCE_PREFIX
        ));
    }
    Ok(key)
}

/// Why version `rec`'s staged files are out of reach of a run in `project`: its `kind:s3` source
/// or an image's build context is staged outside the project's bucket (a version registered on a
/// single-project deploy whose label a later deploy configured as another project). `None` when
/// they are in it, or the project has no bucket to compare with. Submit and launch refuse such a
/// version: the project's coordinator could not fetch them.
pub(crate) fn staged_outside_project(
    rec: &crate::store::registry::RegisteredVersion,
    project: &crate::config::projects::ProjectContext,
) -> Option<String> {
    if project.bucket.is_empty() {
        return None;
    }
    let source = match &rec.source {
        WorkflowSource::S3 { uri, .. } => Some(uri.as_str()),
        WorkflowSource::Git { .. } => None,
    };
    let contexts =
        rec.images.iter().filter_map(|i| i.build.as_ref().map(|b| b.context_uri.as_str()));
    let outside =
        source.into_iter().chain(contexts).find(|uri| staged_key(uri, &project.bucket).is_err())?;
    Some(format!(
        "this version's staged files ({outside}) are outside project `{id}`'s bucket \
         s3://{bucket}/ (it was registered before `{id}` was a project of this deploy); \
         re-register the workflow into project `{id}` from its checkout \
         (`biffix workflows register --project {id}`)",
        id = project.id,
        bucket = project.bucket
    ))
}

/// A registration validated, its runtime chosen and its images pinned: everything up to freezing
/// the version, with nothing written yet.
pub(crate) struct Prepared {
    source: WorkflowSource,
    recipe: biffix_workflow::runtime::RuntimeRecipe,
    runtime_key: String,
    images: Vec<crate::store::registry::VersionImage>,
    input: RegisterInput,
    prior: Vec<crate::store::registry::SourceBuild>,
    registration_inputs_hash: String,
}

impl Prepared {
    /// Freeze the version: the next build of its source, or [`RegisterError::IdenticalBuild`]
    /// naming the build registered with the same inputs.
    fn freeze(
        &self,
        state: &AppState,
    ) -> Result<crate::register::workflow::WorkflowVersion, RegisterError> {
        freeze_version(
            &self.input,
            &state.deploy.queue_classes,
            &self.prior,
            &self.registration_inputs_hash,
        )
    }
}

/// Validate a registration and pin what it names; nothing is written.
pub(crate) async fn prepare(
    state: &AppState,
    req: &RegisterRequest,
) -> Result<Prepared, ServiceError> {
    let source = WorkflowSource::deserialize(&req.source)
        .map_err(|e| ServiceError::Invalid(format!("invalid source: {e}")))?;
    // The version runs in its project: on a deploy of several, one it does not configure is
    // refused now rather than at every submit.
    let project = state
        .projects
        .current_for(Some(&req.project))
        .await
        .for_version(&req.project)
        .map_err(|e| {
            ServiceError::Invalid(format!("{e}; register it into one of them with --project"))
        })?
        .clone();
    // A `kind:s3` source must point inside the version's project bucket under the staging prefix.
    // The coordinator fetches and EXECUTES whatever this uri names, so an arbitrary bucket/key
    // here would be a code-execution surface; reject anything outside the sanctioned staging area.
    // (A re-registration keeps its source, so one staged in another project's bucket before
    // per-project routing is refused here: register from the checkout instead.)
    validate_s3_source_location(&source, &project.bucket).map_err(ServiceError::Invalid)?;
    let engine: Engine = req.engine.as_deref().unwrap_or("snakemake").parse().map_err(
        |e: biffix_workflow::engines::UnknownEngine| ServiceError::Invalid(e.to_string()),
    )?;
    // The target is one word of the engine's command line: it must read as a target.
    if let Some(problem) = req
        .target
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .and_then(|t| engine.target_problem(t))
    {
        return Err(ServiceError::Invalid(format!("target: {problem}")));
    }
    // The version pins one exact engine release biffix supports (the CLI checks this too, but the
    // server is the authority).
    let pinned =
        biffix_workflow::engines::check_engine_version(engine.as_str(), &req.engine_version)
            .map_err(|e| ServiceError::Invalid(e.to_string()))?;
    // The engine runtime that release is built into: frozen onto the version with every pin.
    let recipe =
        biffix_workflow::runtime::recipe_for(engine.as_str(), pinned.version).ok_or_else(|| {
            ServiceError::Invalid(format!(
                "no engine runtime for {} {}",
                engine.as_str(),
                pinned.canonical
            ))
        })?;
    // The workflow's own Nextflow plugins are built into that runtime (the coordinator runs
    // Nextflow offline), so they are part of its key; refused here as the CLI refuses them.
    let recipe = recipe
        .with_workflow_plugins(req.nextflow_plugins.iter().map(Into::into).collect())
        .map_err(|e| ServiceError::Invalid(e.to_string()))?;
    let runtime_key = biffix_workflow::runtime::runtime_key(&recipe);
    let mut form_schema = FormSchema::deserialize(&req.form_schema)
        .map_err(|e| ServiceError::Invalid(format!("invalid form_schema: {e}")))?;
    // The catalog is held to the CLI's limits here too, so a direct API registration cannot
    // freeze a thousand tags or a multi-KB description (idempotent for a CLI-derived form).
    if let Some(catalog) = form_schema.catalog.take() {
        form_schema.catalog =
            biffix_workflow::formschema::normalize_catalog(&catalog, &mut form_schema.warnings);
    }
    // Likewise the reports: a per-sample path without `{sample}`, an unknown kind or an unsafe path
    // is caught as the CLI catches it.
    let reports = std::mem::take(&mut form_schema.reports);
    form_schema.reports =
        biffix_workflow::formschema::normalize_reports(&reports, &mut form_schema.warnings);
    if req.about_markdown.as_ref().is_some_and(|a| a.len() > biffix_api::MAX_ABOUT_BYTES) {
        return Err(ServiceError::Invalid(format!(
            "about_markdown is larger than {} KiB",
            biffix_api::MAX_ABOUT_BYTES / 1024
        )));
    }

    // The workflow's images: the default (`image`) and any named ones (`images`), each pinned to
    // a digest now and imported into the tenant workflow repository by an import build.
    let images = pin_images(state, req, engine, &project.bucket).await?;
    // The version's image digest is its imported default's. A built default has none yet (its
    // `source_ref` is the recipe's path, not a pinned reference): submit uses the built image's.
    let image_digest = images
        .iter()
        .find(|i| i.name == "default" && i.build.is_none())
        .map(|i| digest_of(&i.source_ref).to_string())
        .unwrap_or_default();
    // Imported images live in the deploy's workflow repository, under their source digest.
    let coords = (!image_digest.is_empty()).then(|| workflow_repo_coords(state)).flatten();

    let prior = state.registry.source_builds(&req.git_sha, &req.source_content_hash).await?;
    let input = RegisterInput {
        git_sha: req.git_sha.clone(),
        content_hash: req.source_content_hash.clone(),
        image_digest,
        image_repository: coords.as_ref().map(|c| c.repository.clone()),
        image_registry_id: coords.as_ref().map(|c| c.registry_id.clone()),
        image_region: coords.as_ref().map(|c| c.region.clone()),
        engine,
        // The canonical spelling (`9.03.0` is `9.3.0`; Nextflow's `25.4.0` is `25.04.0`), so one
        // release is one string wherever it is compared.
        engine_version: pinned.canonical.clone(),
        entrypoint: req.entrypoint.clone(),
        target: req.target.clone(),
        form_schema,
        queue_classes: req.queue_classes.clone().unwrap_or_default(),
        about_markdown: req.about_markdown.clone().filter(|a| !a.trim().is_empty()),
        runtime_key: runtime_key.clone(),
        runtime_recipe: serde_json::to_value(&recipe)
            .expect("a recipe of strings always serializes"),
    };
    // What the version freezes, for "identical to build N".
    let registration_inputs_hash = crate::register::version::inputs_hash(
        &req.name,
        &req.project,
        &source,
        &req.registration_inputs.source_mode,
        &runtime_key,
        &images
            .iter()
            .map(|i| match &i.build {
                None => format!("{}={}", i.name, i.source_ref),
                // A built image is identified by what it is built from, not its path.
                Some(_) => format!("{}={}#{}", i.name, i.source_ref, i.content_key),
            })
            .collect::<Vec<_>>(),
        &input,
    );
    Ok(Prepared { source, recipe, runtime_key, images, input, prior, registration_inputs_hash })
}

/// Freeze and record a prepared registration, then start its runtime and image builds. Returns the
/// version recorded and its readiness (`ready` when it can run already; `None` when that could not
/// be read).
pub(crate) async fn commit(
    state: &AppState,
    who: String,
    created_by_label: Option<String>,
    req: &RegisterRequest,
    mut prepared: Prepared,
) -> Result<(RegisteredVersion, Option<crate::images::ReadinessState>), ServiceError> {
    // A registration of the same source with other inputs, recorded since `prepare` read the
    // source's builds (a CI matrix registering one commit twice), takes the build number this one
    // froze: freeze again from the builds as they are now, a few times, before answering 409. An
    // identical one recorded meanwhile makes this one `IdenticalBuild`, as it would have before.
    let mut lost_races = 0;
    let rec = loop {
        let version = prepared.freeze(state)?;
        let rec = RegisteredVersion {
            name: req.name.clone(),
            project: req.project.clone(),
            source: prepared.source.clone(),
            version,
            created_by: Some(who.clone()),
            created_by_label: created_by_label.clone(),
            created_at_micros: biffix_core::clock::epoch_micros(),
            registration_inputs: req.registration_inputs.clone(),
            registration_inputs_hash: prepared.registration_inputs_hash.clone(),
            images: prepared.images.clone(),
        };
        // Route the store error through the shared taxonomy: a genuine duplicate id is a 409, but
        // a transient store outage must surface as 503 (retryable), not a misleading 409.
        match state.registry.put_version(&rec).await {
            Ok(()) => break rec,
            Err(StoreError::Conflict(_)) if lost_races < MAX_BUILD_RACES => {
                lost_races += 1;
                prepared.prior =
                    state.registry.source_builds(&req.git_sha, &req.source_content_hash).await?;
            }
            Err(e) => return Err(ServiceError::from(e)),
        }
    };
    let Prepared { recipe, runtime_key, .. } = prepared;

    // Build the version's coordinator variant now (a runtime another version already built is
    // reused). Best-effort: the version is registered either way, and the sweep retries.
    match state.cloud.coordinator_base().await {
        Ok(base) => {
            if let Err(e) =
                crate::images::variants::ensure(state, &runtime_key, &recipe, &base, true).await
            {
                tracing::warn!(
                    version = rec.version.id.as_str(),
                    error = %e,
                    "register: could not start the coordinator variant; the sweep will retry"
                );
            }
        }
        Err(e) => tracing::warn!(error = %e, "register: no base coordinator image"),
    }
    // Import its images (an image another version already imported is reused). Best-effort, like
    // the runtime: the sweep retries. Concurrently (a workflow may name dozens), but one import at
    // a time: `ensure` reads then starts a build, so two images that are one import (the same
    // digest or recipe) run one after the other, or both would start it.
    let mut imports: std::collections::BTreeMap<&str, Vec<&crate::store::registry::VersionImage>> =
        std::collections::BTreeMap::new();
    for image in &rec.images {
        imports.entry(&image.content_key).or_default().push(image);
    }
    match crate::images::VersionContext::of(state, &rec.version).await {
        Ok(ctx) => {
            let (ctx, version_id) = (&ctx, rec.version.id.as_str());
            futures::stream::iter(imports.into_values())
                .for_each_concurrent(ENSURE_CONCURRENCY, |images| async move {
                    for image in images {
                        if let Err(e) = crate::images::ensure(state, ctx, image, true).await {
                            tracing::warn!(
                                version = version_id,
                                image = image.source_ref,
                                error = %e,
                                "register: could not start an image import; the sweep will retry"
                            );
                        }
                    }
                })
                .await;
        }
        Err(e) => tracing::warn!(
            version = rec.version.id.as_str(),
            error = %e,
            "register: could not start the image imports; the sweep will retry"
        ),
    }

    // Whether it can run already (everything was built before), or is still being built.
    let readiness = crate::images::readiness(state, &rec).await.ok().map(|r| r.state);
    Ok((rec, readiness))
}

/// The registration a version would be given to register it again: everything it recorded (form,
/// About page, source, images as declared and their build recipes), on `engine_version` when given.
/// Its images are pinned again when the request is prepared. A version whose runtime recipe cannot
/// be read is refused (500): its Nextflow plugins live only there, and a re-registration without
/// them would fail every run offline.
pub fn reregister_request(
    rec: &RegisteredVersion,
    engine_version: Option<&str>,
) -> Result<RegisterRequest, ServiceError> {
    let v = &rec.version;
    let images: std::collections::BTreeMap<String, biffix_api::ImageRequest> = rec
        .images
        .iter()
        .map(|i| {
            let request = biffix_api::ImageRequest {
                reference: i.build.is_none().then(|| i.declared_ref.clone()),
                build: i.build.as_ref().map(|b| biffix_api::ImageBuildRequest {
                    kind: b.kind.clone(),
                    context: b.context.clone(),
                    dockerfile: b.dockerfile.clone(),
                    context_uri: b.context_uri.clone(),
                    context_sha256: b.context_sha256.clone(),
                    tree_hash: b.tree_hash.clone(),
                    archs: b.archs.clone(),
                }),
                runtime: i.own_runtime.then(|| "own".to_string()),
            };
            (i.name.clone(), request)
        })
        .collect();
    let mut inputs = rec.registration_inputs.clone();
    if let Some(engine_version) = engine_version {
        inputs.overrides.engine_version = Some(engine_version.to_string());
    }
    inputs.reregistered_from = Some(v.id.as_str().into());
    // The workflow's own Nextflow plugins, as its runtime recipe pinned them.
    let (_, recipe) = crate::images::variants::runtime_of(v).map_err(|e| {
        ServiceError::Internal(format!(
            "version {}'s runtime recipe cannot be read, so its Nextflow plugins cannot be \
                 carried over: {e}",
            v.id
        ))
    })?;
    let nextflow_plugins: Vec<biffix_api::NextflowPluginRequest> =
        recipe.workflow_plugins().iter().map(Into::into).collect();
    Ok(RegisterRequest {
        name: rec.name.clone(),
        project: rec.project.clone(),
        source: serde_json::to_value(&rec.source).expect("a source of strings always serializes"),
        git_sha: v.git_sha.clone(),
        source_content_hash: v.source_content_hash.clone(),
        image: None,
        engine: Some(v.engine.as_str().to_string()),
        engine_version: engine_version.unwrap_or(&v.engine_version).to_string(),
        entrypoint: v.entrypoint.clone(),
        target: v.target.clone(),
        form_schema: v.form_schema.clone(),
        queue_classes: (!v.queue_classes.is_empty()).then(|| v.queue_classes.clone()),
        about_markdown: v.about_markdown.clone(),
        registration_inputs: inputs,
        images,
        nextflow_plugins,
    })
}

/// What registering `prepared` (re-registered from `from`) would do.
pub(crate) async fn plan_of(
    state: &AppState,
    from: &RegisteredVersion,
    prepared: &Prepared,
) -> Result<ReregisterPlan, ServiceError> {
    let (id, build, identical_to) = match prepared.freeze(state) {
        Ok(version) => (version.id, version.build, None),
        Err(RegisterError::IdenticalBuild { id, build }) => (id.clone(), build, Some(id)),
        Err(e) => return Err(e.into()),
    };
    let runtime_cached =
        !state.registry.coordinator_variants(&prepared.runtime_key).await?.is_empty();
    // A Snakemake image (not `runtime: own`) runs derived with the runtime: reused only when that
    // derive is built too.
    let derives = prepared.input.engine == Engine::Snakemake;
    let mut images = Vec::with_capacity(prepared.images.len());
    for image in &prepared.images {
        let artifact = crate::images::usable_artifact(state, image).await?;
        let cached = match artifact {
            Some(a) if derives && !image.own_runtime => state
                .registry
                .get_image_artifact(&biffix_workflow::images::derived_key(
                    &a.digest,
                    &prepared.runtime_key,
                ))
                .await?
                .is_some(),
            artifact => artifact.is_some(),
        };
        let previous = from
            .images
            .iter()
            .find(|i| i.name == image.name)
            .map(|i| i.source_ref.clone())
            .or_else(|| {
                (from.images.is_empty() && image.name == "default")
                    .then(|| from.version.image_digest.clone())
                    .filter(|d| !d.is_empty() && !image.source_ref.ends_with(d.as_str()))
            });
        images.push(biffix_api::ReregisterImagePlan {
            name: image.name.clone(),
            kind: image.kind().to_string(),
            source_ref: image.source_ref.clone(),
            previous_source_ref: previous.filter(|p| *p != image.source_ref),
            cached,
        });
    }
    Ok(ReregisterPlan {
        from: from.version.id.as_str().into(),
        id: id.into(),
        build,
        identical_to: identical_to.map(Into::into),
        engine_version: prepared.input.engine_version.clone(),
        runtime_key: prepared.runtime_key.clone(),
        runtime_cached,
        images,
    })
}

/// The digest of a pinned reference (`…@sha256:…`).
pub(crate) fn digest_of(pinned: &str) -> &str {
    pinned.rsplit_once('@').map_or("", |(_, d)| d)
}

/// Validate a registration's images and pin each to a digest, concurrently (a workflow may name
/// dozens). A bad or missing image is the caller's error (400); a registry fault is a 502.
async fn pin_images(
    state: &AppState,
    req: &RegisterRequest,
    engine: Engine,
    project_bucket: &str,
) -> Result<Vec<crate::store::registry::VersionImage>, ServiceError> {
    let default_ref = req.image.as_deref().filter(|s| !s.is_empty());
    let count = req.images.len() + usize::from(default_ref.is_some());
    if count > MAX_REGISTER_IMAGES {
        return Err(ServiceError::Invalid(format!(
            "a workflow version declares {count} images; at most {MAX_REGISTER_IMAGES}"
        )));
    }
    // Each named image is either imported (`ref`) or built from a recipe (`build`).
    let mut named = std::collections::BTreeMap::new();
    let mut built = Vec::new();
    for (name, image) in req.images.clone() {
        match (image.reference, image.build) {
            (Some(reference), None) => {
                let decl = biffix_workflow::images::ImageDecl {
                    reference: Some(reference),
                    runtime: image.runtime,
                    ..Default::default()
                };
                named.insert(name, decl);
            }
            (None, Some(build)) => {
                built.push(build_image(&name, &build, image.runtime.as_deref(), project_bucket)?);
            }
            _ => {
                return Err(ServiceError::Invalid(format!(
                    "image `{name}` must set exactly one of `ref` and `build`"
                )))
            }
        }
    }
    if default_ref.is_some() && built.iter().any(|b| b.name == "default") {
        return Err(ServiceError::Invalid(
            biffix_workflow::images::ImageDeclError::DuplicateDefault.to_string(),
        ));
    }
    let default = default_ref.map(|i| expand_bare_digest(state, i)).transpose()?;
    let declared = biffix_workflow::images::declared_images(&named, default.as_deref())
        .map_err(|e| ServiceError::Invalid(e.to_string()))?;
    // Only references reach `declared_images` here (builds were taken out above).
    let declared: Vec<(
        &biffix_workflow::images::DeclaredImage,
        &biffix_workflow::images::ImageRef,
    )> = declared.iter().filter_map(|d| d.reference().map(|r| (d, r))).collect();
    // Built or imported, only a Snakemake image can carry its own Snakemake.
    let own = built
        .iter()
        .filter(|b| b.own_runtime)
        .map(|b| &b.name)
        .chain(declared.iter().filter(|(d, _)| d.own_runtime).map(|(d, _)| &d.name))
        .next();
    if let (Some(own), true) = (own, engine != Engine::Snakemake) {
        return Err(ServiceError::Invalid(format!(
            "image `{own}`: `runtime: own` is for Snakemake images only"
        )));
    }
    let declared_refs: std::collections::BTreeMap<&str, String> = req
        .images
        .iter()
        .filter_map(|(name, image)| Some((name.as_str(), image.reference.clone()?)))
        .chain(default.as_deref().map(|d| ("default", d.to_string())))
        .collect();
    // In order, a few at a time, like the imports `commit` starts.
    let pins: Vec<_> = declared
        .iter()
        .map(|&(d, reference)| {
            Box::pin(async move {
                let digest = pin_reference(state, &d.name, reference).await?;
                Ok::<_, ServiceError>((d, reference, digest))
            })
        })
        .collect();
    let pinned: Vec<_> =
        futures::stream::iter(pins).buffered(ENSURE_CONCURRENCY).try_collect().await?;
    Ok(pinned
        .into_iter()
        .map(|(d, reference, digest)| crate::store::registry::VersionImage {
            name: d.name.clone(),
            declared_ref: declared_refs
                .get(d.name.as_str())
                .cloned()
                .unwrap_or_else(|| reference.to_string()),
            source_ref: reference.pinned(&digest).to_string(),
            content_key: biffix_workflow::images::content_key(&digest),
            own_runtime: d.own_runtime,
            build: None,
        })
        .chain(built)
        .map(|i| (i.name.clone(), i))
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_values()
        .collect())
}

/// A reference as given, with a bare `sha256:` digest read as naming an image in the deploy's
/// workflow repository. A bare digest where the deploy configures no such repository is a 400.
pub(crate) fn expand_bare_digest(
    state: &AppState,
    reference: &str,
) -> Result<String, ServiceError> {
    match reference.strip_prefix("sha256:") {
        Some(_) if !state.deploy.worker_image_repo.is_empty() => {
            Ok(format!("{}@{reference}", state.deploy.worker_image_repo))
        }
        Some(_) => Err(ServiceError::Invalid(
            "a bare sha256 digest names an image in the deploy's worker-image repository, which \
             this deploy does not configure; give the image's full reference"
                .into(),
        )),
        None => Ok(reference.to_string()),
    }
}

/// The digest `reference` (the image `name`) resolves to now. An image its registry does not have
/// is the caller's error (400); a registry outage is a 503, any other failure a 502.
pub(crate) async fn pin_reference(
    state: &AppState,
    name: &str,
    reference: &biffix_workflow::images::ImageRef,
) -> Result<String, ServiceError> {
    state.cloud.resolve_image_digest(reference).await.map_err(|e| match e {
        crate::aws::AwsError::Image(message) => ServiceError::Invalid(message),
        e @ crate::aws::AwsError::Unavailable(_) => {
            ServiceError::Unavailable(format!("resolving image `{name}` ({reference}) failed: {e}"))
        }
        e => ServiceError::Upstream(format!("resolving image `{name}` ({reference}) failed: {e}")),
    })
}

/// A named image built from a recipe the workflow ships: validated, keyed by what it is built from
/// (with this server's recipe versions), and recorded with what its builds need. Its context must
/// be staged in the version's `project_bucket`.
fn build_image(
    name: &str,
    req: &biffix_api::ImageBuildRequest,
    runtime: Option<&str>,
    project_bucket: &str,
) -> Result<crate::store::registry::VersionImage, ServiceError> {
    use biffix_workflow::images::{build_content_key, Arch, BuildKind};
    let bad = |m: String| ServiceError::Invalid(format!("image `{name}`: {m}"));
    // The name and `runtime` follow the same rules as an imported image's (the CLI's too).
    let decl_error =
        |e: biffix_workflow::images::ImageDeclError| ServiceError::Invalid(e.to_string());
    biffix_workflow::images::check_image_name(name).map_err(decl_error)?;
    let own_runtime = biffix_workflow::images::own_runtime(name, runtime).map_err(decl_error)?;
    let kind: BuildKind = req.kind.parse().map_err(|e| bad(format!("{e}")))?;
    // The paths reach the build as-is (`$WORK/context/$BIFFIX_DOCKERFILE`) and spell its key, so
    // they are checked and cleaned here as the CLI does: relative, no `..`, one spelling
    // (`./Dockerfile` is `Dockerfile`, so it is the same build).
    let clean = |what: &str, path: &str| {
        biffix_workflow::images::clean_path(path).ok_or_else(|| {
            bad(format!(
                "the {what} `{path}` must be a relative path that stays inside the workflow"
            ))
        })
    };
    let context = clean("context", &req.context)?;
    let dockerfile = req.dockerfile.as_deref().map(|d| clean("Dockerfile", d)).transpose()?;
    match (kind, &dockerfile) {
        (BuildKind::Dockerfile, None) => {
            return Err(bad("a Dockerfile build names its Dockerfile".into()))
        }
        (BuildKind::Dockerfile, Some(d)) if d.is_empty() => {
            return Err(bad("a Dockerfile build names its Dockerfile".into()))
        }
        (BuildKind::Pixi, Some(_)) => return Err(bad("a pixi build has no Dockerfile".into())),
        _ => {}
    }
    let mut archs = req
        .archs
        .iter()
        .map(|a| Arch::parse(a).ok_or_else(|| bad(format!("unknown architecture `{a}`"))))
        .collect::<Result<Vec<_>, _>>()?;
    archs.sort();
    archs.dedup();
    if archs.is_empty() {
        return Err(bad("no architecture to build for".into()));
    }
    if !is_sha256_hex(&req.tree_hash) || !is_sha256_hex(&req.context_sha256) {
        return Err(bad("the context's hashes must be sha256 hex".into()));
    }
    // The context is what the image is built from: it must be the tarball the CLI staged, in the
    // project bucket's staging area, named by its tree hash.
    let staged_suffix = format!("/contexts/{}.tar.gz", req.tree_hash);
    let in_staging =
        staged_key(&req.context_uri, project_bucket).is_ok_and(|key| key.ends_with(&staged_suffix));
    if !in_staging {
        return Err(bad(format!(
            "the build context must be staged in the project bucket under {}: {}",
            crate::aws::objects::SOURCE_PREFIX,
            req.context_uri
        )));
    }
    // Keyed by the tarball's sha256, which the build checks before it builds, not the tree hash
    // the client claims: the tenant-wide key names only what was really built from it.
    let content_key = build_content_key(kind, &req.context_sha256, dockerfile.as_deref(), &archs);
    let path = match &dockerfile {
        Some(dockerfile) if context.is_empty() => dockerfile.clone(),
        Some(dockerfile) => format!("{context}/{dockerfile}"),
        None => context.clone(),
    };
    Ok(crate::store::registry::VersionImage {
        name: name.to_string(),
        declared_ref: name.to_string(),
        source_ref: format!("{}:{path}", kind.as_str()),
        content_key,
        own_runtime,
        build: Some(crate::store::registry::BuildRecipe {
            kind: kind.as_str().to_string(),
            context,
            context_uri: req.context_uri.clone(),
            context_sha256: req.context_sha256.clone(),
            tree_hash: req.tree_hash.clone(),
            dockerfile,
            dockerfile_text: (kind == BuildKind::Pixi).then(biffix_workflow::pixi::dockerfile),
            archs: archs.iter().map(|a| a.as_str().to_string()).collect(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_registration_that_loses_its_build_number_to_a_concurrent_one_takes_the_next() {
        use std::sync::Arc;
        let registry = Arc::new(crate::testing::MemRegistry::new());
        let state = crate::testing::TestState::new()
            .with_registry(registry.clone())
            .with_deploy(crate::DeployConfig {
                coordinator_queue: "coordinator".into(),
                queue_classes: vec!["cpu-spot".into()],
                worker_image_repo: crate::testing::FAKE_WORKFLOW_REPO.into(),
                ..Default::default()
            })
            .with_projects(crate::testing::bucket_project("test-bucket"))
            .build();
        let body = |entrypoint: &str| -> RegisterRequest {
            serde_json::from_value(serde_json::json!({
                "name": "nf",
                "project": "p",
                "source": {"kind": "git", "repo": "github.com/org/nf", "git_ref": "main"},
                "git_sha": "abc",
                "source_content_hash": "hash",
                "registration_inputs": {"source_mode": "git", "cli_version": "0.0.0"},
                "engine": "nextflow",
                "engine_version": "25.10.4",
                "entrypoint": entrypoint,
                "image": "ubuntu:24.04",
                "form_schema": {"fields": [], "warnings": []},
                "queue_classes": ["cpu-spot"]
            }))
            .unwrap()
        };
        let (mine, theirs) = (body("main.nf"), body("other.nf"));
        let prepared = prepare(&state, &mine).await.unwrap();
        // Another registration of the same commit, with other inputs, lands first and takes the
        // build this one froze from what `prepare` read.
        let other = prepare(&state, &theirs).await.unwrap();
        let (first, _) = commit(&state, "u".into(), None, &theirs, other).await.unwrap();
        assert_eq!(first.version.build, 0);
        let (rec, _) = commit(&state, "u".into(), None, &mine, prepared).await.unwrap();
        assert_eq!(rec.version.build, 1);
        assert_eq!(registry.versions.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_tarball_source_names_a_sha256() {
        let source = |sha256: Option<&str>| WorkflowSource::S3 {
            uri: "s3://b/workflow-sources/abc:hash/source.tar.gz".into(),
            sha256: sha256.map(str::to_string),
        };
        assert!(validate_s3_source_location(&source(None), "b").is_err());
        assert!(validate_s3_source_location(&source(Some(&"a".repeat(64))), "b").is_ok());
        // A prefix has no one object to check.
        let prefix = WorkflowSource::S3 {
            uri: "s3://b/workflow-sources/abc:hash/src/".into(),
            sha256: None,
        };
        assert!(validate_s3_source_location(&prefix, "b").is_ok());
        for bad in ["abc", &"A".repeat(64), &"g".repeat(64)] {
            assert!(validate_s3_source_location(&source(Some(bad)), "b").is_err(), "{bad}");
        }
    }

    #[test]
    fn a_staged_key_is_in_the_project_bucket_under_the_prefix_with_no_dot_dot() {
        let ok = "s3://b/workflow-sources/abc:hash/source.tar.gz";
        assert_eq!(staged_key(ok, "b"), Ok("workflow-sources/abc:hash/source.tar.gz"));
        for (uri, bucket) in [
            (ok, ""),
            (ok, "other"),
            ("https://b/workflow-sources/x", "b"),
            ("s3://b", "b"),
            ("s3://b/elsewhere/x.tar.gz", "b"),
            ("s3://b/workflow-sources/../elsewhere/x.tar.gz", "b"),
        ] {
            assert!(staged_key(uri, bucket).is_err(), "{uri} in {bucket:?}");
        }
    }
}
