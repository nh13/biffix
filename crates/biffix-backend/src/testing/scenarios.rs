//! Scenarios run against both a double and the real store, so the double behaves as the store
//! does.

use crate::store::registry::{RegisteredVersion, RegistryStore};
use crate::store::StoreError;

/// One registry scenario, run against [`MemRegistry`] (registration.rs) and the Postgres registry
/// (pg/registry.rs), so the double's order and conflicts are the real store's rather than a
/// description of them. `version(id, build, created_at_micros)` builds a version of one source
/// (the same git sha and content hash whatever the id), so its builds group together.
///
/// The ids tied on registration time sort one way bytewise (`"ab:c" < "abb"`) and the other way
/// under a linguistic collation such as the database's `en_US`, which ignores the colon.
///
/// [`MemRegistry`]: crate::testing::MemRegistry
pub async fn registry_orders_and_conflicts(
    registry: &impl RegistryStore,
    version: impl Fn(&str, u32, i64) -> RegisteredVersion,
) {
    let source = version("probe", 0, 0).version;
    let ids = || async {
        registry
            .list_versions()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.version.id)
            .collect::<Vec<String>>()
    };
    let builds = || async {
        registry
            .source_builds(&source.git_sha, &source.source_content_hash)
            .await
            .unwrap()
            .into_iter()
            .map(|b| (b.id, b.build))
            .collect::<Vec<(String, u32)>>()
    };
    for (id, build, at) in [("abb", 2, 10), ("v-old", 1, 5), ("v-new", 3, 20), ("ab:c", 0, 10)] {
        registry.put_version(&version(id, build, at)).await.unwrap();
    }
    // Newest first, ties by id, bytewise.
    let newest_first = ["v-new", "ab:c", "abb", "v-old"];
    assert_eq!(ids().await, newest_first);
    // A source's builds by build number, whatever the ids or registration times.
    let by_build = [("ab:c", 0), ("v-old", 1), ("abb", 2), ("v-new", 3)]
        .map(|(id, build)| (id.to_string(), build));
    assert_eq!(builds().await, by_build);
    // A second version claiming a build the source already has is a conflict, as is a repeated
    // id; neither is stored.
    for (id, build) in [("v-racer", 2), ("ab:c", 9)] {
        let err = registry.put_version(&version(id, build, 30)).await.unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "{id}: {err:?}");
    }
    assert_eq!(ids().await, newest_first);
    assert_eq!(builds().await, by_build);
    assert_eq!(registry.get_version("ab:c").await.unwrap().version.build, 0);
}
