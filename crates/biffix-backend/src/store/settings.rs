//! Site settings (ui customization): a single-row store for the site title and logo, editable by
//! developers via `PUT /settings` and read by every page via `GET /settings`. Also each user's own
//! display preferences (`/me/prefs`): the other small, non-event UI state.

use async_trait::async_trait;

use crate::store::StoreError;

/// The stored row is the wire shape `GET /settings` serves (biffix-api owns it).
pub use biffix_api::{SiteSettings, SiteSettingsUpdateRequest, UserPrefs, UserPrefsUpdateRequest};

/// The most pipelines a user may pin: a short list of favourites, not a catalog copy.
pub const MAX_PINS: usize = 50;
/// The longest pin key, in characters. Keys are opaque client keys (the UI's is
/// `JSON.stringify([project, name])`), so this bounds two names plus the JSON around them.
pub(crate) const MAX_PIN_CHARS: usize = 512;

/// Read/write the single site-settings row.
#[async_trait]
pub trait SettingsStore: Send + Sync {
    async fn get_settings(&self) -> Result<SiteSettings, StoreError>;
    /// Apply a partial update to the site settings atomically (no lost update between two
    /// concurrent writers) and return the result: a field `update` leaves out (`None`) keeps its
    /// stored value, `Some(None)` clears it and `Some(Some(v))` sets it. The caller has checked and
    /// normalized the values.
    async fn update_settings(
        &self,
        update: &SiteSettingsUpdateRequest,
    ) -> Result<SiteSettings, StoreError>;
    /// One user's preferences; the defaults when they have none stored.
    async fn get_user_prefs(&self, user: &str) -> Result<UserPrefs, StoreError>;
    /// Apply a partial update to one user's preferences atomically (no lost update between two
    /// concurrent writers) and return the result: a field `update` leaves out keeps its stored
    /// value; `tour_done` stamps now (`true`) or clears (`false`). A whole pin list is stored as
    /// given (the caller trims and de-duplicates it); `pin` appends a key when absent and `unpin`
    /// removes one, against the stored list in the same atomic write (the caller gives at most one
    /// of the three). A `pin` that would leave more than [`MAX_PINS`] is
    /// [`StoreError::Conflict`] and writes nothing.
    async fn update_user_prefs(
        &self,
        user: &str,
        update: &UserPrefsUpdateRequest,
    ) -> Result<UserPrefs, StoreError>;
}
