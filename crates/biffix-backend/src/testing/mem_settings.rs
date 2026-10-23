//! [`MemSettings`]: the in-memory [`SettingsStore`].

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::store::settings::{SettingsStore, SiteSettings, SiteSettingsUpdateRequest};
use crate::store::StoreError;
use async_trait::async_trait;

#[derive(Default)]
pub struct MemSettings {
    pub inner: Mutex<SiteSettings>,
    pub user_prefs: Mutex<BTreeMap<String, biffix_api::UserPrefs>>,
}

#[async_trait]
impl SettingsStore for MemSettings {
    async fn get_settings(&self) -> Result<SiteSettings, StoreError> {
        Ok(self.inner.lock().unwrap().clone())
    }
    async fn update_settings(
        &self,
        u: &SiteSettingsUpdateRequest,
    ) -> Result<SiteSettings, StoreError> {
        // As the Pg store does it in one statement: under the lock, an absent field keeps its value.
        let mut s = self.inner.lock().unwrap();
        if let Some(title) = &u.title {
            s.title.clone_from(title);
        }
        if let Some(logo) = &u.logo_data_url {
            s.logo_data_url.clone_from(logo);
        }
        if let Some(support) = &u.support {
            s.support.clone_from(support);
        }
        Ok(s.clone())
    }
    async fn get_user_prefs(&self, user: &str) -> Result<biffix_api::UserPrefs, StoreError> {
        Ok(self.user_prefs.lock().unwrap().get(user).cloned().unwrap_or_default())
    }
    async fn update_user_prefs(
        &self,
        user: &str,
        u: &biffix_api::UserPrefsUpdateRequest,
    ) -> Result<biffix_api::UserPrefs, StoreError> {
        // As the Pg store does it in one statement: under the lock, an absent field keeps its value,
        // and a pin past the cap writes nothing.
        let mut all = self.user_prefs.lock().unwrap();
        let mut pins = all.get(user).map(|p| p.pinned_workflows.clone()).unwrap_or_default();
        if let Some(key) = &u.pin {
            if !pins.contains(key) {
                pins.push(key.clone());
            }
            if pins.len() > crate::store::settings::MAX_PINS {
                return Err(StoreError::Conflict(format!(
                    "at most {} pinned pipelines",
                    crate::store::settings::MAX_PINS
                )));
            }
        } else if let Some(key) = &u.unpin {
            pins.retain(|p| p != key);
        }
        let p = all.entry(user.to_string()).or_default();
        p.pinned_workflows = pins;
        if let Some(view) = u.view {
            p.view = Some(view);
        }
        match u.tour_done {
            Some(true) => p.tour_done_at = Some(biffix_core::clock::epoch_secs()),
            Some(false) => p.tour_done_at = None,
            None => {}
        }
        Ok(p.clone())
    }
}
