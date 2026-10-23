//! [`MemReferenceStore`]: the in-memory [`ReferenceStore`].

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use biffix_api::ReferenceBundle;

use crate::store::references::ReferenceStore;
use crate::store::StoreError;

#[derive(Default)]
pub struct MemReferenceStore {
    bundles: Mutex<BTreeMap<String, ReferenceBundle>>,
}

#[async_trait]
impl ReferenceStore for MemReferenceStore {
    async fn list_bundles(&self) -> Result<Vec<ReferenceBundle>, StoreError> {
        Ok(self.bundles.lock().unwrap().values().cloned().collect())
    }
    async fn get_bundle(&self, id: &str) -> Result<ReferenceBundle, StoreError> {
        self.bundles.lock().unwrap().get(id).cloned().ok_or(StoreError::NotFound)
    }
    async fn create_bundle(&self, bundle: &ReferenceBundle) -> Result<(), StoreError> {
        let mut bundles = self.bundles.lock().unwrap();
        if bundles.contains_key(&bundle.id) {
            return Err(StoreError::Conflict(format!(
                "reference bundle {} already exists",
                bundle.id
            )));
        }
        bundles.insert(bundle.id.clone(), bundle.clone());
        Ok(())
    }
    async fn update_bundle(&self, bundle: &ReferenceBundle) -> Result<ReferenceBundle, StoreError> {
        let mut bundles = self.bundles.lock().unwrap();
        let existing = bundles.get_mut(&bundle.id).ok_or(StoreError::NotFound)?;
        *existing = ReferenceBundle {
            created_at_micros: existing.created_at_micros,
            created_by: existing.created_by.clone(),
            ..bundle.clone()
        };
        Ok(existing.clone())
    }
    async fn delete_bundle(&self, id: &str) -> Result<(), StoreError> {
        self.bundles.lock().unwrap().remove(id).map(|_| ()).ok_or(StoreError::NotFound)
    }
}
