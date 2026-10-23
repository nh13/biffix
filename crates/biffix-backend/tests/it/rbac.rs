//! RBAC mapping + capability checks — pure.

use std::collections::BTreeMap;

use biffix_backend::auth::rbac::{parse_group_role_map, Capability, Rbac, Role};

fn rbac() -> Rbac {
    let mut m = BTreeMap::new();
    m.insert("biffix-viewers".to_string(), Role::Viewer);
    m.insert("biffix-operators".to_string(), Role::Operator);
    m.insert("biffix-admins".to_string(), Role::Admin);
    Rbac::new(m)
}

#[test]
fn highest_mapped_role_wins() {
    let r = rbac();
    let groups = vec!["biffix-viewers".to_string(), "biffix-admins".to_string()];
    assert_eq!(r.role_for(&groups), Some(Role::Admin));
}

#[test]
fn unmapped_groups_have_no_role() {
    assert_eq!(rbac().role_for(&["random-group".to_string()]), None);
}

#[test]
fn viewer_can_view_but_not_launch() {
    let r = rbac();
    let g = vec!["biffix-viewers".to_string()];
    assert!(r.can(&g, Capability::ViewRuns));
    assert!(!r.can(&g, Capability::LaunchWorkflow));
    assert!(!r.can(&g, Capability::RegisterWorkflow));
}

#[test]
fn operator_can_launch() {
    let r = rbac();
    let g = vec!["biffix-operators".to_string()];
    assert!(r.can(&g, Capability::LaunchWorkflow));
    assert!(!r.can(&g, Capability::RegisterWorkflow));
}

#[test]
fn admin_can_manage_reference_bundles() {
    let r = rbac();
    let g = vec!["biffix-admins".to_string()];
    assert!(r.can(&g, Capability::ManageReferenceBundles));
    assert!(r.can(&g, Capability::RegisterWorkflow)); // includes lower privileges
}

#[test]
fn no_groups_means_no_access() {
    assert!(!rbac().can(&[], Capability::ViewRuns));
}

#[test]
fn authenticated_default_allows_group_less_google_user_to_view_only() {
    let r = Rbac::new(BTreeMap::new()).with_authenticated_default(Role::Viewer);
    assert!(r.can(&[], Capability::ViewRuns));
    assert!(!r.can(&[], Capability::LaunchWorkflow));
}

fn groups(names: &[&str]) -> Vec<String> {
    names.iter().map(std::string::ToString::to_string).collect()
}

#[test]
fn developer_can_register_but_not_administer() {
    let mut m = BTreeMap::new();
    m.insert("biffix-developers".to_string(), Role::Developer);
    let r = Rbac::new(m);
    let g = groups(&["biffix-developers"]);
    assert!(r.can(&g, Capability::LaunchWorkflow));
    assert!(r.can(&g, Capability::RegisterWorkflow));
    assert!(!r.can(&g, Capability::ManageReferenceBundles));
    assert!(!r.can(&g, Capability::ManageSupportContact));
}

#[test]
fn authenticated_default_applies_when_no_group_is_mapped() {
    // Groups biffix does not know (e.g. a Workspace's "Domain Users") fall back to the default,
    // exactly like a token with no `cognito:groups` at all.
    let r = rbac().with_authenticated_default(Role::Viewer);
    let unmapped = groups(&["Domain Users"]);
    assert_eq!(r.role_for(&unmapped), Some(Role::Viewer));
    assert!(r.can(&unmapped, Capability::ViewRuns));
    assert!(!r.can(&unmapped, Capability::LaunchWorkflow));
}

#[test]
fn authenticated_default_is_a_fallback_not_a_floor() {
    // A mapped group decides even when the default is higher: an explicit viewer stays a viewer.
    let r = rbac().with_authenticated_default(Role::Operator);
    let viewer = groups(&["biffix-viewers"]);
    assert_eq!(r.role_for(&viewer), Some(Role::Viewer));
    assert!(!r.can(&viewer, Capability::LaunchWorkflow));
    assert_eq!(r.role_for(&[]), Some(Role::Operator));
}

#[test]
fn role_names_round_trip_the_infra_vocabulary() {
    // The same four spellings `infra/biffix_infra/rbac.py` writes into the backend env.
    for (name, role) in [
        ("viewer", Role::Viewer),
        ("operator", Role::Operator),
        ("developer", Role::Developer),
        ("admin", Role::Admin),
    ] {
        assert_eq!(name.parse::<Role>(), Ok(role));
        assert_eq!(role.as_str(), name);
    }
    let err = "Admin".parse::<Role>().unwrap_err().to_string();
    assert!(err.contains("\"Admin\""), "{err}");
    assert!(err.contains("viewer, operator, developer, admin"), "{err}");
}

#[test]
fn group_role_map_parses_and_names_the_bad_group() {
    let map =
        parse_group_role_map(r#"{"biffix-admins":"admin","biffix-viewers":"viewer"}"#).unwrap();
    assert_eq!(map["biffix-admins"], Role::Admin);
    assert_eq!(map["biffix-viewers"], Role::Viewer);

    let err = parse_group_role_map(r#"{"biffix-admins":"amdin"}"#).unwrap_err().to_string();
    assert!(err.contains("BIFFIX_GROUP_ROLE_MAP"), "{err}");
    assert!(err.contains("\"biffix-admins\""), "{err}");
    assert!(err.contains("\"amdin\""), "{err}");

    let err = parse_group_role_map("[1, 2]").unwrap_err().to_string();
    assert!(err.contains("BIFFIX_GROUP_ROLE_MAP"), "{err}");
}
