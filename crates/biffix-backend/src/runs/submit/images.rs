//! The images a submit names (`images`): checked with the request, then pinned, authorized and
//! their imports (and derives) moved forward once every other check has passed.

use std::collections::{BTreeMap, BTreeSet};

use crate::auth::actor::{authorize, AuthedActor};
use crate::auth::rbac::Capability;
use crate::error::ServiceError;
use crate::runs::launch::MAX_SUBMIT_IMAGES;
use crate::store::registry::RegisteredVersion;
use crate::AppState;

/// The images a submit names, checked before any AWS call: trimmed, blanks dropped, a bare
/// `sha256:` digest read as naming the deploy's workflow repository, each spelling once, at most
/// [`MAX_SUBMIT_IMAGES`], and each a reference. Each as `(spelling, reference)`: the spelling as
/// given (after the digest expansion) is what tasks name it by.
pub(super) fn submit_image_refs(
    state: &AppState,
    given: Vec<String>,
) -> Result<Vec<(String, biffix_workflow::images::ImageRef)>, ServiceError> {
    let mut seen = BTreeSet::new();
    let mut spellings = Vec::new();
    for r in given.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
        let expanded = crate::register::pipeline::expand_bare_digest(state, r)?;
        if seen.insert(expanded.clone()) {
            spellings.push(expanded);
        }
    }
    if spellings.len() > MAX_SUBMIT_IMAGES {
        return Err(ServiceError::Invalid(format!(
            "at most {MAX_SUBMIT_IMAGES} submit-time images"
        )));
    }
    spellings
        .into_iter()
        .map(|r| match biffix_workflow::images::ImageRef::parse(&r) {
            Ok(reference) => Ok((r, reference)),
            Err(e) => Err(ServiceError::Invalid(format!("image `{r}`: {e}"))),
        })
        .collect()
}

/// Pin each submit-time image: each distinct reference once (concurrently), every spelling of it
/// sharing that digest. In `refs` order.
pub(super) async fn pin_submit_images(
    state: &AppState,
    refs: Vec<(String, biffix_workflow::images::ImageRef)>,
) -> Result<Vec<biffix_core::event::SubmitImage>, ServiceError> {
    let mut distinct: BTreeMap<String, (&str, &biffix_workflow::images::ImageRef)> =
        BTreeMap::new();
    for (spelling, reference) in &refs {
        distinct.entry(reference.to_string()).or_insert((spelling, reference));
    }
    let digests: BTreeMap<String, String> = futures::future::try_join_all(
        distinct.into_iter().map(|(normalized, (spelling, reference))| async move {
            let digest =
                crate::register::pipeline::pin_reference(state, spelling, reference).await?;
            Ok::<_, ServiceError>((normalized, digest))
        }),
    )
    .await?
    .into_iter()
    .collect();
    Ok(refs
        .into_iter()
        .map(|(spelling, reference)| {
            let digest = &digests[&reference.to_string()];
            biffix_core::event::SubmitImage {
                declared_ref: spelling,
                source_ref: reference.pinned(digest).to_string(),
                content_key: biffix_workflow::images::content_key(digest),
            }
        })
        .collect())
}

/// Who may name submit-time images: anyone who may launch, as long as each is already imported
/// into the tenant (a Developer imported it: registering it, or naming it at an earlier submit),
/// so re-running or resuming with a prior run's images needs no more than launching. Bringing a
/// new image in is what registration does, so it needs `RegisterWorkflow` (Developer).
pub(super) async fn authorize_submit_images(
    state: &AppState,
    actor: &AuthedActor,
    images: &[biffix_core::event::SubmitImage],
) -> Result<(), ServiceError> {
    for image in images {
        let imported = state
            .registry
            .get_image_artifact(&image.content_key)
            .await
            .map_err(|e| {
                ServiceError::reworded(e, |e| {
                    format!("reading image `{}`: {e}", image.declared_ref)
                })
            })?
            .is_some();
        if !imported {
            return authorize(state, actor, Capability::RegisterWorkflow).map(|_| ()).map_err(
                |e| {
                    e.with_message(format!(
                        "image `{}` is not yet imported into this deploy; importing a \
                         submit-time image needs the Developer role",
                        image.declared_ref
                    ))
                },
            );
        }
    }
    Ok(())
}

/// Move each submit-time image's import (and derive) one step forward; a submit's face of
/// [`crate::runs::launch::ensure_submit_images`].
pub(super) async fn prepare_submit_images(
    state: &AppState,
    rec: &RegisteredVersion,
    images: &[biffix_core::event::SubmitImage],
) -> Result<Vec<crate::images::ImageStatus>, ServiceError> {
    if images.is_empty() {
        return Ok(Vec::new());
    }
    let variant_err = |e: crate::images::build_follow::BuildError| {
        tracing::warn!(error = %e, "submit: preparing the submit-time images failed");
        ServiceError::from(e)
    };
    let ctx = crate::images::VersionContext::of(state, &rec.version).await.map_err(variant_err)?;
    crate::runs::launch::ensure_submit_images(state, &ctx, images).await.map_err(variant_err)
}
