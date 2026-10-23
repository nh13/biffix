//! `PgSettingsStore`: site settings and user preferences.

use super::*;

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn site_settings_default_and_update_field_by_field() {
    use biffix_backend::pg::settings::PgSettingsStore;
    use biffix_backend::store::settings::{SettingsStore, SiteSettings, SiteSettingsUpdateRequest};
    let store = PgSettingsStore::new(migrated_pool("site_settings").await);
    assert_eq!(store.get_settings().await.unwrap(), SiteSettings::default());
    let support = biffix_api::SupportContact {
        name: "Bioinformatics core".into(),
        email: "core@lab.example.org".into(),
        url: Some("https://lab.example.org/help".into()),
    };
    // The first write (no row yet) sets what it names; the rest stay unset.
    let first = store
        .update_settings(&SiteSettingsUpdateRequest {
            title: Some(Some("Lab".into())),
            support: Some(Some(support.clone())),
            ..Default::default()
        })
        .await
        .unwrap();
    let want = SiteSettings {
        title: Some("Lab".into()),
        logo_data_url: None,
        support: Some(support.clone()),
    };
    assert_eq!(first, want);
    assert_eq!(store.get_settings().await.unwrap(), want);
    // Two partial updates (as two concurrent requests would make): each keeps the other's field.
    store
        .update_settings(&SiteSettingsUpdateRequest {
            logo_data_url: Some(Some("data:image/png;base64,AA==".into())),
            ..Default::default()
        })
        .await
        .unwrap();
    let after = store
        .update_settings(&SiteSettingsUpdateRequest {
            title: Some(Some("Lab 2".into())),
            ..Default::default()
        })
        .await
        .unwrap();
    let want = SiteSettings {
        title: Some("Lab 2".into()),
        logo_data_url: Some("data:image/png;base64,AA==".into()),
        support: Some(support),
    };
    assert_eq!(after, want);
    // `null` clears a field, and only that one.
    let cleared = store
        .update_settings(&SiteSettingsUpdateRequest { support: Some(None), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(cleared, SiteSettings { support: None, ..want });
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn user_prefs_default_per_user_and_update_field_by_field() {
    use biffix_api::ViewMode;
    use biffix_backend::pg::settings::PgSettingsStore;
    use biffix_backend::store::settings::{SettingsStore, UserPrefs, UserPrefsUpdateRequest};
    let pool = migrated_pool("user_prefs").await;
    let store = PgSettingsStore::new(pool.clone());
    assert_eq!(store.get_user_prefs("alice").await.unwrap(), UserPrefs::default());
    let set = store
        .update_user_prefs(
            "alice",
            &UserPrefsUpdateRequest {
                view: Some(ViewMode::Expert),
                pin: Some("rnaseq".into()),
                tour_done: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(set.view, Some(ViewMode::Expert));
    assert_eq!(set.pinned_workflows, ["rnaseq"]);
    assert!(set.tour_done_at.is_some_and(|t| t > 0.0), "{set:?}");
    assert_eq!(store.get_user_prefs("alice").await.unwrap(), set);
    // Another user is untouched.
    assert_eq!(store.get_user_prefs("bob").await.unwrap(), UserPrefs::default());
    // An absent field keeps its stored value; `tour_done: false` clears the tour.
    let view_only = store
        .update_user_prefs(
            "alice",
            &UserPrefsUpdateRequest { view: Some(ViewMode::Lab), ..Default::default() },
        )
        .await
        .unwrap();
    assert_eq!(view_only, UserPrefs { view: Some(ViewMode::Lab), ..set.clone() });
    let cleared = store
        .update_user_prefs(
            "alice",
            &UserPrefsUpdateRequest { tour_done: Some(false), ..Default::default() },
        )
        .await
        .unwrap();
    assert_eq!(cleared.tour_done_at, None);
    assert_eq!(cleared.pinned_workflows, ["rnaseq"]);
    assert_eq!(store.get_user_prefs("alice").await.unwrap(), cleared);
    // A first write that sets only the view stores no pins and no tour.
    let carol = store
        .update_user_prefs(
            "carol",
            &UserPrefsUpdateRequest { view: Some(ViewMode::Lab), ..Default::default() },
        )
        .await
        .unwrap();
    assert_eq!(carol, UserPrefs { view: Some(ViewMode::Lab), ..Default::default() });
    // A malformed stored value is an error, not silently the default.
    sqlx::query("UPDATE user_prefs SET pinned_workflows = '{\"x\": 1}' WHERE user_sub = 'carol'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.get_user_prefs("carol").await,
        Err(biffix_backend::store::StoreError::Serialization(_))
    ));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn user_prefs_pin_and_unpin_one_key_atomically() {
    use biffix_backend::pg::settings::PgSettingsStore;
    use biffix_backend::store::settings::{SettingsStore, UserPrefsUpdateRequest, MAX_PINS};
    use biffix_backend::store::StoreError;
    let store = PgSettingsStore::new(migrated_pool("user_prefs_pins").await);
    let pin = |key: &str| UserPrefsUpdateRequest { pin: Some(key.into()), ..Default::default() };
    let unpin =
        |key: &str| UserPrefsUpdateRequest { unpin: Some(key.into()), ..Default::default() };
    let pins = |p: biffix_backend::store::settings::UserPrefs| p.pinned_workflows;

    // A first write that only unpins stores an empty list; one that pins stores the key.
    assert!(pins(store.update_user_prefs("dan", &unpin("a")).await.unwrap()).is_empty());
    assert_eq!(pins(store.update_user_prefs("eve", &pin("a")).await.unwrap()), ["a"]);
    // Two different pins in sequence are both kept, in order; pinning again is a no-op.
    assert_eq!(pins(store.update_user_prefs("eve", &pin("b")).await.unwrap()), ["a", "b"]);
    assert_eq!(pins(store.update_user_prefs("eve", &pin("a")).await.unwrap()), ["a", "b"]);
    // Unpin removes it; an absent key is a no-op.
    assert_eq!(pins(store.update_user_prefs("eve", &unpin("a")).await.unwrap()), ["b"]);
    assert_eq!(pins(store.update_user_prefs("eve", &unpin("a")).await.unwrap()), ["b"]);
    // The cap holds after the op, and a refused pin writes nothing (not even the view).
    let full: Vec<String> = (0..MAX_PINS).map(|i| format!("wf{i}")).collect();
    for key in &full {
        store.update_user_prefs("fay", &pin(key)).await.unwrap();
    }
    let over =
        UserPrefsUpdateRequest { view: Some(biffix_api::ViewMode::Expert), ..pin("one-more") };
    assert!(matches!(store.update_user_prefs("fay", &over).await, Err(StoreError::Conflict(_))));
    let stored = store.get_user_prefs("fay").await.unwrap();
    assert_eq!(stored.pinned_workflows, full);
    assert_eq!(stored.view, None);
    // Re-pinning a key already in a full list is fine.
    assert_eq!(pins(store.update_user_prefs("fay", &pin("wf0")).await.unwrap()), full);
}
