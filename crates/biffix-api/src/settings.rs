//! `settings` — the site chrome (title + logo) and the operational-alert recipients.

use serde::{Deserialize, Deserializer, Serialize};

/// `GET /settings` — the customizable site chrome. Both fields optional so an unset deployment
/// renders defaults. Also the stored row (`SettingsStore`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SiteSettings {
    /// Display title shown in the header/tab (falls back to "biffix" when unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The logo as a raster-image `data:` URL (kept small; checked at the API boundary). `None` =
    /// wordmark only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_data_url: Option<String>,
    /// Who Lab users ask when an analysis needs attention (a pipeline's own `catalog.contact` wins
    /// for its runs). `None` = no *Ask* button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support: Option<SupportContact>,
}

/// A person (or team) to ask for help: the site's support contact, or a pipeline's own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SupportContact {
    /// Shown as "Ask `<name>`".
    pub name: String,
    /// Where the *Ask* button's `mailto:` goes.
    pub email: String,
    /// A help page (a ticket form, a chat channel), shown beside the email when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl From<biffix_workflow::formschema::Contact> for SupportContact {
    fn from(c: biffix_workflow::formschema::Contact) -> Self {
        Self { name: c.name, email: c.email, url: c.url }
    }
}

impl From<SupportContact> for biffix_workflow::formschema::Contact {
    fn from(c: SupportContact) -> Self {
        Self { name: c.name, email: c.email, url: c.url }
    }
}

/// `PUT /settings` — a partial update. Per field: absent keeps the stored value; `null` or an empty
/// (or, for the title, blank) string clears it; any other value replaces it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SiteSettingsUpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, nullable))]
    pub title: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, nullable))]
    pub logo_data_url: Option<Option<String>>,
    /// `null` removes the support contact. Setting or clearing it needs an Admin.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<SupportContact>, nullable))]
    pub support: Option<Option<SupportContact>>,
}

/// A field that is present in the body, `null` or not: `Some(None)` for `null`. An absent field
/// never reaches this (serde's `default` makes it `None`), which is what tells "keep" from "clear".
pub(crate) fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// The run view a user picks: the biologist-facing Lab view or the full Expert view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    Lab,
    Expert,
}

impl ViewMode {
    /// The wire (and stored) spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ViewMode::Lab => "lab",
            ViewMode::Expert => "expert",
        }
    }

    /// The view `s` spells, if any.
    pub fn parse(s: &str) -> Option<ViewMode> {
        [ViewMode::Lab, ViewMode::Expert].into_iter().find(|v| v.as_str() == s)
    }
}

/// `GET /me/prefs` — the caller's own display preferences. Also the stored row (`SettingsStore`).
/// A user with no stored row gets the defaults: no view chosen (the UI shows Lab), nothing pinned.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UserPrefs {
    /// `None` until the user picks one (the UI's default is `lab`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ViewMode>,
    /// Pinned pipelines, in the order the user pinned them. Each is an opaque key the client
    /// chooses (the UI stores `JSON.stringify([project, name])`), not a workflow name: the server
    /// only trims, de-duplicates and caps them.
    #[serde(default)]
    pub pinned_workflows: Vec<String>,
    /// When the user finished or skipped the first-visit tour (epoch seconds); `None` = not yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tour_done_at: Option<f64>,
}

/// `PUT /me/prefs` — a partial update. Per field: absent keeps the stored value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UserPrefsUpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ViewMode>,
    /// Pin one key (opaque to the server, see [`UserPrefs::pinned_workflows`]), atomically against
    /// the stored list: appended when absent (the order kept), a no-op when already pinned.
    /// Trimmed; 1 to 512 characters. A pin past the cap of 50 is a 400 and stores nothing. At most
    /// one of this and `unpin` per update (400 otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// Unpin one key, atomically against the stored list; a no-op when it is not pinned. Trimmed;
    /// 1 to 512 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unpin: Option<String>,
    /// `true` records the tour as done now; `false` clears it (replay the tour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tour_done: Option<bool>,
}

/// One email that receives operational alerts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlertRecipient {
    pub email: String,
    /// `confirmed`, or `pending` until the recipient clicks the link in AWS's confirmation email.
    pub status: String,
}

/// `GET /settings/alerts` — the alert recipients. `configured` is false when the deploy has no
/// alerts topic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlertRecipientList {
    pub configured: bool,
    pub recipients: Vec<AlertRecipient>,
}

/// `POST /settings/alerts` — add a recipient.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlertRecipientAddRequest {
    pub email: String,
}

/// `POST /settings/alerts/remove` — remove a recipient. The address travels in the body, not the
/// request URL, so access logs don't keep it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlertRecipientRemoveRequest {
    pub email: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_field_keeps_and_a_null_one_clears() {
        let absent: SiteSettingsUpdateRequest = serde_json::from_str(r#"{"title":"Lab"}"#).unwrap();
        assert_eq!(absent.title, Some(Some("Lab".into())));
        assert_eq!(absent.logo_data_url, None);
        let null: SiteSettingsUpdateRequest =
            serde_json::from_str(r#"{"logo_data_url":null}"#).unwrap();
        assert_eq!(null.title, None);
        assert_eq!(null.logo_data_url, Some(None));
    }
}
