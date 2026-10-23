//! Postgres [`SettingsStore`](crate::store::settings::SettingsStore) (behind `feature = "aws"`).

use async_trait::async_trait;
use sqlx::postgres::PgPool;
// `AssertSqlSafe` marks query text assembled with `format!`: every interpolated piece is a code
// constant (column lists, enum-derived literals, `$n` placeholders), never a runtime value; values
// are always bound.
use sqlx::{AssertSqlSafe, Row};

use crate::store::StoreError;

/// The single-row settings store: the `site_settings` row keyed 'default'.
pub struct PgSettingsStore {
    pool: PgPool,
}

impl PgSettingsStore {
    /// A store over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl crate::store::settings::SettingsStore for PgSettingsStore {
    async fn get_settings(&self) -> Result<crate::store::settings::SiteSettings, StoreError> {
        let row = sqlx::query(
            "SELECT title, logo_data_url, support FROM site_settings WHERE id = 'default'",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => crate::store::settings::SiteSettings {
                title: r.get("title"),
                logo_data_url: r.get("logo_data_url"),
                // A malformed stored value is a serialization error, never silently none.
                support: r
                    .get::<Option<serde_json::Value>, _>("support")
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|e| {
                        StoreError::Serialization(format!("site_settings.support: {e}"))
                    })?,
            },
            None => crate::store::settings::SiteSettings::default(),
        })
    }

    async fn update_settings(
        &self,
        u: &crate::store::settings::SiteSettingsUpdateRequest,
    ) -> Result<crate::store::settings::SiteSettings, StoreError> {
        // One statement, so two concurrent partial updates cannot lose each other's fields: each
        // field is set (to its value, NULL clearing it) only when its flag is, else kept.
        sqlx::query(
            "INSERT INTO site_settings (id, title, logo_data_url, support, updated_at) \
             VALUES ('default', $2::text, $4::text, $6::jsonb, now()) \
             ON CONFLICT (id) DO UPDATE SET \
               title = CASE WHEN $1::bool THEN $2::text ELSE site_settings.title END, \
               logo_data_url = CASE WHEN $3::bool THEN $4::text \
                                 ELSE site_settings.logo_data_url END, \
               support = CASE WHEN $5::bool THEN $6::jsonb ELSE site_settings.support END, \
               updated_at = now()",
        )
        .bind(u.title.is_some())
        .bind(u.title.as_ref().and_then(Option::as_deref))
        .bind(u.logo_data_url.is_some())
        .bind(u.logo_data_url.as_ref().and_then(Option::as_deref))
        .bind(u.support.is_some())
        .bind(u.support.as_ref().and_then(Option::as_ref).map(|c| serde_json::json!(c)))
        .execute(&self.pool)
        .await?;
        // Read back in the same shape `get_settings` reads (a malformed stored value an error).
        self.get_settings().await
    }

    async fn get_user_prefs(
        &self,
        user: &str,
    ) -> Result<crate::store::settings::UserPrefs, StoreError> {
        let row = sqlx::query(AssertSqlSafe(format!(
            "SELECT {USER_PREFS_COLS} FROM user_prefs WHERE user_sub = $1"
        )))
        .bind(user)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map_or(Ok(crate::store::settings::UserPrefs::default()), user_prefs_row)
    }

    async fn update_user_prefs(
        &self,
        user: &str,
        update: &crate::store::settings::UserPrefsUpdateRequest,
    ) -> Result<crate::store::settings::UserPrefs, StoreError> {
        // One statement, so two concurrent partial updates cannot lose each other's fields: NULL
        // keeps the stored value; $3 is the tour's `set` (now) / `clear` (NULL) / NULL (keep).
        // The pins are the stored list with $4 appended when absent or with $5 removed, else the
        // stored list. A pin past the cap ($6) fails the update's WHERE (no row is written or
        // returned): a first write's list is never past it.
        let tour = update.tour_done.map(|done| if done { "set" } else { "clear" });
        let row = sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO user_prefs (user_sub, view, pinned_workflows, tour_done_at, updated_at) \
             VALUES ($1, $2::text, {inserted}, \
                     CASE WHEN $3::text = 'set' THEN now() END, now()) \
             ON CONFLICT (user_sub) DO UPDATE SET \
               view = COALESCE($2::text, user_prefs.view), \
               pinned_workflows = {updated}, \
               tour_done_at = CASE $3::text WHEN 'set' THEN now() WHEN 'clear' THEN NULL \
                                ELSE user_prefs.tour_done_at END, \
               updated_at = now() \
             WHERE $4::text IS NULL OR jsonb_array_length({updated}) <= $6::int \
             RETURNING {USER_PREFS_COLS}",
            inserted = pins_after("'[]'::jsonb"),
            updated = pins_after("user_prefs.pinned_workflows"),
        )))
        .bind(user)
        .bind(update.view.map(biffix_api::ViewMode::as_str))
        .bind(tour)
        .bind(update.pin.as_deref())
        .bind(update.unpin.as_deref())
        .bind(i32::try_from(crate::store::settings::MAX_PINS).unwrap_or(i32::MAX))
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            StoreError::Conflict(format!(
                "at most {} pinned pipelines",
                crate::store::settings::MAX_PINS
            ))
        })?;
        user_prefs_row(&row)
    }
}

/// The pin list an update leaves, from the stored list `stored` (SQL): `stored` with the key `$4`
/// appended when absent (the order kept) or with `$5` removed, else `stored` as it is.
fn pins_after(stored: &str) -> String {
    format!(
        "CASE WHEN $4::text IS NOT NULL THEN \
                CASE WHEN {stored} @> jsonb_build_array($4::text) THEN {stored} \
                     ELSE {stored} || jsonb_build_array($4::text) END \
              WHEN $5::text IS NOT NULL THEN {stored} - $5::text \
              ELSE {stored} END"
    )
}

/// The `user_prefs` columns [`user_prefs_row`] reads.
const USER_PREFS_COLS: &str =
    "view, pinned_workflows, EXTRACT(EPOCH FROM tour_done_at)::float8 AS tour_done_at";

/// A `user_prefs` row ([`USER_PREFS_COLS`]). A malformed stored value is a serialization error,
/// never silently read as the default.
fn user_prefs_row(
    r: &sqlx::postgres::PgRow,
) -> Result<crate::store::settings::UserPrefs, StoreError> {
    let view = r
        .get::<Option<String>, _>("view")
        .map(|v| {
            biffix_api::ViewMode::parse(&v)
                .ok_or_else(|| StoreError::Serialization(format!("user_prefs.view: `{v}`")))
        })
        .transpose()?;
    let pinned_workflows = serde_json::from_value(r.get("pinned_workflows"))
        .map_err(|e| StoreError::Serialization(format!("user_prefs.pinned_workflows: {e}")))?;
    Ok(crate::store::settings::UserPrefs {
        view,
        pinned_workflows,
        tour_done_at: r.get("tour_done_at"),
    })
}
