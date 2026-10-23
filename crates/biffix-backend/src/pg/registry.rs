//! Postgres-backed [`RegistryStore`] (behind `feature = "aws"`).
//!
//! Persists frozen `WorkflowVersion`s in the shared `workflow_versions` table
//! (`biffix_db::migrate`). The registry is reference data, not the event log — a plain keyed
//! table whose primary key is the immutable version id, so a duplicate insert is a conflict.

use async_trait::async_trait;
use biffix_core::builds::{BuildProjection, BuildState};
use biffix_core::time::Timestamp;
use sqlx::postgres::PgPool;
// `AssertSqlSafe` marks query text assembled with `format!`: every interpolated piece is a code
// constant (column lists, enum-derived literals, `$n` placeholders), never a runtime value; values
// are always bound.
use sqlx::{AssertSqlSafe, Row};

use crate::register::workflow::{WorkflowSource, WorkflowVersion};
use crate::store::registry::{
    CoordinatorVariant, ImageArtifact, RegisteredVersion, RegistryStore, SourceBuild, VersionImage,
};
use crate::store::StoreError;
use biffix_workflow::engines::Engine;

/// Postgres-backed [`RegistryStore`] (the registered workflows, their versions and images).
pub struct PgRegistryStore {
    pool: PgPool,
}

impl PgRegistryStore {
    /// A store over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Declared images by version (of `version` only, or of every version), sorted by name.
    async fn images_of(
        &self,
        version: Option<&str>,
    ) -> Result<std::collections::HashMap<String, Vec<VersionImage>>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM workflow_version_images
             WHERE $1::text IS NULL OR version_id = $1 ORDER BY version_id COLLATE \"C\", name COLLATE \"C\"",
        )
        .bind(version)
        .fetch_all(&self.pool)
        .await?;
        let mut by_version: std::collections::HashMap<String, Vec<VersionImage>> =
            std::collections::HashMap::new();
        for r in rows {
            by_version.entry(r.get("version_id")).or_default().push(VersionImage {
                name: r.get("name"),
                declared_ref: r.get("declared_ref"),
                source_ref: r.get("source_ref"),
                content_key: r.get("content_key"),
                own_runtime: r.get("own_runtime"),
                build: r
                    .get::<Option<serde_json::Value>, _>("recipe")
                    .map(serde_json::from_value)
                    .transpose()?,
            });
        }
        Ok(by_version)
    }
}

/// A column read as `T`, a type error named by its column.
fn required<T>(r: &sqlx::postgres::PgRow, col: &str) -> Result<T, StoreError>
where
    for<'a> T: sqlx::Decode<'a, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    r.try_get::<T, _>(col).map_err(|e| StoreError::Serialization(format!("{col}: {e}")))
}

/// A stored build number: never negative (it was a `u32` when written).
fn build_of(r: &sqlx::postgres::PgRow, col: &str) -> Result<u32, StoreError> {
    let stored: i32 = r.get(col);
    u32::try_from(stored).map_err(|_| StoreError::Serialization(format!("{col} {stored}")))
}

/// The `workflow_listing` columns a [`biffix_api::WorkflowListing`] is read from.
const LISTING_COLUMNS: &str = "project, name, hidden, updated_by, \
     (EXTRACT(EPOCH FROM updated_at) * 1000000)::bigint AS updated_at_micros";

fn listing_entry(r: &sqlx::postgres::PgRow) -> biffix_api::WorkflowListing {
    biffix_api::WorkflowListing {
        project: r.get("project"),
        name: r.get("name"),
        hidden: r.get("hidden"),
        updated_at_micros: r.get("updated_at_micros"),
        updated_by: r.get("updated_by"),
    }
}

/// The `workflow_version_releases` columns a [`biffix_api::WorkflowReleaseChange`] is read from.
const RELEASE_COLUMNS: &str = "released, actor, actor_label, \
     (EXTRACT(EPOCH FROM at) * 1000000)::bigint AS at_micros";

fn release_change(r: &sqlx::postgres::PgRow) -> biffix_api::WorkflowReleaseChange {
    biffix_api::WorkflowReleaseChange {
        released: r.get("released"),
        at_micros: r.get("at_micros"),
        by: r.get("actor"),
        by_label: r.get("actor_label"),
    }
}

fn row_to_record(r: &sqlx::postgres::PgRow) -> Result<RegisteredVersion, StoreError> {
    let source: serde_json::Value = r.get("source");
    let source: WorkflowSource = serde_json::from_value(source)?;
    // Native Postgres text[] ↔ Vec<String> directly — no JSON re-parse per row.
    let queue_classes: Vec<String> = r.get("queue_classes");
    let registration_inputs = serde_json::from_value(required(r, "registration_inputs")?)?;
    let version = WorkflowVersion {
        id: r.get("id"),
        build: build_of(r, "build")?,
        git_sha: r.get("git_sha"),
        source_content_hash: r.get("source_content_hash"),
        image_digest: r.get("image_digest"),
        image_repository: r.get("image_repository"),
        image_registry_id: r.get("image_registry_id"),
        image_region: r.get("image_region"),
        engine: r
            .get::<String, _>("engine")
            .parse::<Engine>()
            .map_err(|e| StoreError::Serialization(e.to_string()))?,
        engine_version: r.get("engine_version"),
        entrypoint: r.get("entrypoint"),
        target: r.get("target"),
        form_schema: r.get("form_schema"),
        queue_classes,
        about_markdown: r.get("about_markdown"),
        runtime_key: required(r, "runtime_key")?,
        runtime_recipe: required(r, "runtime_recipe")?,
    };
    Ok(RegisteredVersion {
        name: r.get("name"),
        project: r.get("project"),
        source,
        version,
        created_by: r.get("created_by"),
        created_by_label: r.get("created_by_label"),
        created_at_micros: r.get("created_at"),
        registration_inputs,
        registration_inputs_hash: required(r, "registration_inputs_hash")?,
        images: Vec::new(),
    })
}

#[async_trait]
impl RegistryStore for PgRegistryStore {
    async fn put_version(&self, rec: &RegisteredVersion) -> Result<(), StoreError> {
        let source = serde_json::to_value(&rec.source)?;
        let registration_inputs = serde_json::to_value(&rec.registration_inputs)?;
        let build = i32::try_from(rec.version.build).map_err(|_| {
            StoreError::Serialization(format!("build {} is out of range", rec.version.build))
        })?;
        let recipes = rec
            .images
            .iter()
            .map(|i| i.build.as_ref().map(serde_json::to_value).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        // A repeated id, or a second version claiming the same build of one source (two concurrent
        // registrations), is a conflict: a version is immutable. The primary key and the unique
        // (git_sha, source_content_hash, build) index enforce it; ON CONFLICT DO NOTHING lets us
        // detect it via rows_affected.
        let mut tx = self.pool.begin().await?;
        let res = sqlx::query(
            r"INSERT INTO workflow_versions
                 (id, name, project, git_sha, source_content_hash, image_digest, engine,
                  engine_version, entrypoint, target, source, form_schema, queue_classes,
                  created_by, created_at, image_repository, image_registry_id, image_region,
                  created_by_label, about_markdown, build, registration_inputs,
                  registration_inputs_hash, runtime_key, runtime_recipe)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,
                       $21,$22,$23,$24,$25)
               ON CONFLICT DO NOTHING",
        )
        .bind(&rec.version.id)
        .bind(&rec.name)
        .bind(&rec.project)
        .bind(&rec.version.git_sha)
        .bind(&rec.version.source_content_hash)
        .bind(&rec.version.image_digest)
        .bind(rec.version.engine.as_str())
        .bind(&rec.version.engine_version)
        .bind(&rec.version.entrypoint)
        .bind(&rec.version.target)
        .bind(&source)
        .bind(&rec.version.form_schema)
        .bind(&rec.version.queue_classes)
        .bind(&rec.created_by)
        .bind(rec.created_at_micros)
        .bind(&rec.version.image_repository)
        .bind(&rec.version.image_registry_id)
        .bind(&rec.version.image_region)
        .bind(&rec.created_by_label)
        .bind(&rec.version.about_markdown)
        .bind(build)
        .bind(&registration_inputs)
        .bind(&rec.registration_inputs_hash)
        .bind(&rec.version.runtime_key)
        .bind(&rec.version.runtime_recipe)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::Conflict(format!(
                "workflow version {} already exists",
                rec.version.id
            )));
        }
        // Every declared image in one statement.
        if !rec.images.is_empty() {
            let col =
                |f: fn(&VersionImage) -> &str| -> Vec<&str> { rec.images.iter().map(f).collect() };
            sqlx::query(
                "INSERT INTO workflow_version_images
                   (version_id, name, declared_ref, source_ref, content_key, own_runtime, kind,
                    recipe)
                 SELECT $1, * FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[],
                                          $6::bool[], $7::text[], $8::jsonb[])",
            )
            .bind(&rec.version.id)
            .bind(col(|i| &i.name))
            .bind(col(|i| &i.declared_ref))
            .bind(col(|i| &i.source_ref))
            .bind(col(|i| &i.content_key))
            .bind(rec.images.iter().map(|i| i.own_runtime).collect::<Vec<_>>())
            .bind(col(VersionImage::kind))
            .bind(&recipes)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn get_version(&self, id: &str) -> Result<RegisteredVersion, StoreError> {
        let row = sqlx::query("SELECT * FROM workflow_versions WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        let mut rec = row_to_record(&row)?;
        rec.images = self.images_of(Some(id)).await?.remove(id).unwrap_or_default();
        Ok(rec)
    }

    async fn list_versions(&self) -> Result<Vec<RegisteredVersion>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM workflow_versions ORDER BY created_at DESC, id COLLATE \"C\"",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut images = self.images_of(None).await?;
        rows.iter()
            .map(|row| {
                let mut rec = row_to_record(row)?;
                rec.images = images.remove(&rec.version.id).unwrap_or_default();
                Ok(rec)
            })
            .collect()
    }

    async fn has_project(&self, project: &str) -> Result<bool, StoreError> {
        Ok(sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM workflow_versions WHERE project = $1)")
            .bind(project)
            .fetch_one(&self.pool)
            .await?)
    }

    async fn hidden_workflows(
        &self,
    ) -> Result<std::collections::HashSet<(String, String)>, StoreError> {
        let rows = sqlx::query("SELECT project, name FROM workflow_listing WHERE hidden")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| (r.get("project"), r.get("name"))).collect())
    }

    async fn workflow_listing(
        &self,
        project: &str,
        name: &str,
    ) -> Result<Option<biffix_api::WorkflowListing>, StoreError> {
        let row = sqlx::query(AssertSqlSafe(format!(
            "SELECT {LISTING_COLUMNS} FROM workflow_listing WHERE project = $1 AND name = $2"
        )))
        .bind(project)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(listing_entry))
    }

    async fn put_workflow_listing(
        &self,
        project: &str,
        name: &str,
        hidden: bool,
        updated_by: Option<&str>,
    ) -> Result<biffix_api::WorkflowListing, StoreError> {
        // One statement: the existence check and the upsert cannot be split by a concurrent
        // registration or a second curator.
        let row = sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO workflow_listing (project, name, hidden, updated_at, updated_by) \
             SELECT $1, $2, $3, now(), $4 \
             WHERE EXISTS (SELECT 1 FROM workflow_versions WHERE project = $1 AND name = $2) \
             ON CONFLICT (project, name) DO UPDATE \
             SET hidden = EXCLUDED.hidden, updated_at = EXCLUDED.updated_at, \
                 updated_by = EXCLUDED.updated_by \
             RETURNING {LISTING_COLUMNS}"
        )))
        .bind(project)
        .bind(name)
        .bind(hidden)
        .bind(updated_by)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        Ok(listing_entry(&row))
    }

    async fn released_versions(
        &self,
        version_ids: &[String],
    ) -> Result<std::collections::HashMap<String, biffix_api::WorkflowReleaseChange>, StoreError>
    {
        // Each named version's newest change (one probe of the `(version_id, seq DESC)` index per
        // version), kept when it released the version.
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT v.id AS version_id, {RELEASE_COLUMNS} \
             FROM unnest($1::text[]) AS v(id) \
             CROSS JOIN LATERAL ( \
                 SELECT released, actor, actor_label, at FROM workflow_version_releases r \
                 WHERE r.version_id = v.id ORDER BY r.seq DESC LIMIT 1 \
             ) newest WHERE released"
        )))
        .bind(version_ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| (r.get("version_id"), release_change(r))).collect())
    }

    async fn release_history(
        &self,
        version_id: &str,
    ) -> Result<Vec<biffix_api::WorkflowReleaseChange>, StoreError> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {RELEASE_COLUMNS} FROM workflow_version_releases \
             WHERE version_id = $1 ORDER BY seq DESC"
        )))
        .bind(version_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(release_change).collect())
    }

    async fn set_version_released(
        &self,
        version_id: &str,
        released: bool,
        actor: Option<&str>,
        actor_label: Option<&str>,
    ) -> Result<Vec<biffix_api::WorkflowReleaseChange>, StoreError> {
        let mut tx = self.pool.begin().await?;
        // One change of one version at a time, so two curators at once cannot both record the
        // same change (each reads the state the other left).
        // The repo's salted 64-bit per-key lock, in the same key space as the launch and
        // output-prefix locks (`pg_store`).
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('biffix:workflow-release:' || $1, 0))",
        )
        .bind(version_id)
        .execute(&mut *tx)
        .await?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM workflow_versions WHERE id = $1)")
                .bind(version_id)
                .fetch_one(&mut *tx)
                .await?;
        if !exists {
            return Err(StoreError::NotFound);
        }
        // Absent = never released.
        let current: Option<bool> = sqlx::query_scalar(
            "SELECT released FROM workflow_version_releases \
             WHERE version_id = $1 ORDER BY seq DESC LIMIT 1",
        )
        .bind(version_id)
        .fetch_optional(&mut *tx)
        .await?;
        if current.unwrap_or(false) != released {
            sqlx::query(
                "INSERT INTO workflow_version_releases (version_id, released, actor, actor_label) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(version_id)
            .bind(released)
            .bind(actor)
            .bind(actor_label)
            .execute(&mut *tx)
            .await?;
        }
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {RELEASE_COLUMNS} FROM workflow_version_releases \
             WHERE version_id = $1 ORDER BY seq DESC"
        )))
        .bind(version_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows.iter().map(release_change).collect())
    }

    async fn source_builds(
        &self,
        git_sha: &str,
        content_hash: &str,
    ) -> Result<Vec<SourceBuild>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, build, registration_inputs_hash FROM workflow_versions
             WHERE git_sha = $1 AND source_content_hash = $2 ORDER BY build",
        )
        .bind(git_sha)
        .bind(content_hash)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(SourceBuild {
                    id: r.get("id"),
                    build: build_of(r, "build")?,
                    registration_inputs_hash: required(r, "registration_inputs_hash")?,
                })
            })
            .collect()
    }

    async fn get_build(&self, subject: &str) -> Result<Option<BuildProjection>, StoreError> {
        let Some(r) = sqlx::query("SELECT * FROM build_projection WHERE subject = $1")
            .bind(subject)
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(None);
        };
        let state: String = r.get("state");
        let outputs: serde_json::Value = r.get("outputs");
        Ok(Some(BuildProjection {
            subject: r.get("subject"),
            state: state
                .parse::<BuildState>()
                .map_err(|e| StoreError::Serialization(format!("build {e}")))?,
            build_arn: r.get("build_arn"),
            outputs: serde_json::from_value(outputs)?,
            reason: r.get("reason"),
            logs_url: r.get("logs_url"),
            request: r.get::<Option<serde_json::Value>, _>("request").unwrap_or_default(),
            builds: build_of(&r, "builds")?,
            updated: Timestamp::from_micros(r.get("updated_at")),
        }))
    }

    async fn coordinator_variants(
        &self,
        runtime_key: &str,
    ) -> Result<Vec<CoordinatorVariant>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM coordinator_variants WHERE runtime_key = $1
             ORDER BY created_at DESC, base_digest COLLATE \"C\"",
        )
        .bind(runtime_key)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| CoordinatorVariant {
                runtime_key: r.get("runtime_key"),
                base_digest: r.get("base_digest"),
                image: r.get("image"),
                job_definition_arn: r.get("job_definition_arn"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    async fn get_image_artifact(
        &self,
        content_key: &str,
    ) -> Result<Option<ImageArtifact>, StoreError> {
        let Some(r) = sqlx::query("SELECT * FROM image_artifacts WHERE content_key = $1")
            .bind(content_key)
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(ImageArtifact {
            content_key: r.get("content_key"),
            source_ref: r.get("source_ref"),
            image: r.get("image"),
            digest: r.get("digest"),
            archs: serde_json::from_value(r.get("archs"))?,
            capabilities: serde_json::from_value(r.get("capabilities"))?,
            keep_tag: r.get("keep_tag"),
            archived: r.get("archived"),
            archive_bytes: r.get("archive_bytes"),
            created_at: r.get("created_at"),
        }))
    }

    async fn put_image_artifact(&self, a: &ImageArtifact) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO image_artifacts
               (content_key, source_ref, image, digest, archs, capabilities, keep_tag, archived,
                archive_bytes, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT DO NOTHING",
        )
        .bind(&a.content_key)
        .bind(&a.source_ref)
        .bind(&a.image)
        .bind(&a.digest)
        .bind(serde_json::to_value(&a.archs)?)
        .bind(serde_json::to_value(&a.capabilities)?)
        .bind(&a.keep_tag)
        .bind(a.archived)
        .bind(a.archive_bytes)
        .bind(a.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn replace_image_artifact(
        &self,
        stale_image: &str,
        a: &ImageArtifact,
    ) -> Result<(), StoreError> {
        // Compare-and-set on the recorded image: a concurrent replacement that got there first
        // leaves this one a no-op.
        sqlx::query(
            "UPDATE image_artifacts
                SET source_ref = $2, image = $3, digest = $4, archs = $5, capabilities = $6,
                    keep_tag = $7, archived = $8, archive_bytes = $9, created_at = $10
              WHERE content_key = $1 AND image = $11",
        )
        .bind(&a.content_key)
        .bind(&a.source_ref)
        .bind(&a.image)
        .bind(&a.digest)
        .bind(serde_json::to_value(&a.archs)?)
        .bind(serde_json::to_value(&a.capabilities)?)
        .bind(&a.keep_tag)
        .bind(a.archived)
        .bind(a.archive_bytes)
        .bind(a.created_at)
        .bind(stale_image)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn put_coordinator_variant(&self, v: &CoordinatorVariant) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO coordinator_variants
               (runtime_key, base_digest, image, job_definition_arn, created_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (runtime_key, base_digest) DO UPDATE
               SET job_definition_arn = EXCLUDED.job_definition_arn
               WHERE coordinator_variants.image = EXCLUDED.image",
        )
        .bind(&v.runtime_key)
        .bind(&v.base_digest)
        .bind(&v.image)
        .bind(&v.job_definition_arn)
        .bind(v.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
