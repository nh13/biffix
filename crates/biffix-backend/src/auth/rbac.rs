//! Control-plane RBAC — pure, unit-tested.
//!
//! Coarse, legible roles for biologists/PIs (not fine-grained ABAC). A user's role comes from
//! their **IdP group membership**, mapped here — the only identity config biffix holds. A biffix
//! RBAC check is **defense-in-depth**, not the enforcement boundary (that is IAM).
//!
//! Visibility is flat/open for now: the check answers role-only. All decisions route through the
//! one [`Rbac::can`] choke point so project-scoped visibility is a later policy change here, not a
//! rewrite of call sites.

use std::collections::BTreeMap;
use std::str::FromStr;

/// Coarse RBAC roles, ordered by privilege (each includes those below it). The variants are
/// declared least- to most-privileged, so the derived `Ord` *is* the privilege order — a capability
/// check is a direct `>=` comparison, with no parallel hand-rolled rank table to keep in sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Operator,
    Developer,
    Admin,
}

impl Role {
    /// Every role, least- to most-privileged.
    pub(crate) const ALL: [Role; 4] = [Role::Viewer, Role::Operator, Role::Developer, Role::Admin];

    /// The config spelling of this role. This vocabulary is a contract with the infra, which
    /// writes it into `BIFFIX_GROUP_ROLE_MAP` and `BIFFIX_AUTHENTICATED_DEFAULT_ROLE`
    /// (`infra/biffix_infra/rbac.py`).
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Developer => "developer",
            Role::Admin => "admin",
        }
    }
}

/// A role name that is not the spelling of any [`Role`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown RBAC role {0:?} (expected one of viewer, operator, developer, admin)")]
pub struct UnknownRole(pub String);

impl FromStr for Role {
    type Err = UnknownRole;

    /// Parse the exact lowercase config spelling ([`Role::as_str`]).
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Role::ALL
            .into_iter()
            .find(|role| role.as_str() == raw)
            .ok_or_else(|| UnknownRole(raw.to_string()))
    }
}

/// Parse `BIFFIX_GROUP_ROLE_MAP`: a JSON object of IdP group name to role name. The error names
/// the variable and, for a bad role, the group it was mapped from, so a failed cold start says
/// exactly what to fix.
pub fn parse_group_role_map(raw: &str) -> Result<BTreeMap<String, Role>, RoleMapError> {
    let map: BTreeMap<String, String> = serde_json::from_str(raw).map_err(RoleMapError::NotJson)?;
    map.into_iter()
        .map(|(group, role)| match role.parse::<Role>() {
            Ok(role) => Ok((group, role)),
            Err(source) => Err(RoleMapError::UnknownRole { group, source }),
        })
        .collect()
}

/// A `BIFFIX_GROUP_ROLE_MAP` the backend cannot use.
#[derive(Debug, thiserror::Error)]
pub enum RoleMapError {
    #[error("BIFFIX_GROUP_ROLE_MAP is not a JSON object of group name to role: {0}")]
    NotJson(#[source] serde_json::Error),
    #[error("BIFFIX_GROUP_ROLE_MAP: group {group:?} maps to an {source}")]
    UnknownRole {
        group: String,
        #[source]
        source: UnknownRole,
    },
}

/// Capabilities gated by a minimum role. Some are hard-floored to admin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    ViewRuns,
    LaunchWorkflow,
    RegisterWorkflow,
    /// Add, edit and delete reference bundles (Settings → References).
    ManageReferenceBundles,
    /// Set or clear the site's support contact (`PUT /settings` with `support`), which every Lab
    /// user's *Ask* button mails.
    ManageSupportContact,
}

impl Capability {
    /// The minimum role that satisfies this capability.
    fn min_role(self) -> Role {
        match self {
            Capability::ViewRuns => Role::Viewer,
            Capability::LaunchWorkflow => Role::Operator,
            Capability::RegisterWorkflow => Role::Developer,
            Capability::ManageReferenceBundles | Capability::ManageSupportContact => Role::Admin,
        }
    }
}

/// The IdP-group → role mapping (the only identity config biffix stores).
#[derive(Clone, Debug, Default)]
pub struct Rbac {
    group_to_role: BTreeMap<String, Role>,
    authenticated_default: Option<Role>,
}

impl Rbac {
    pub fn new(group_to_role: BTreeMap<String, Role>) -> Self {
        Self { group_to_role, authenticated_default: None }
    }

    /// Set the explicit role granted to every identity admitted by this tenant's Cognito pool.
    /// For a pool that admits only the deploy's own organization (self-signup disabled, an
    /// organization-internal IdP application), pool admission itself grants read-only Viewer.
    pub fn with_authenticated_default(mut self, role: Role) -> Self {
        self.authenticated_default = Some(role);
        self
    }

    /// The effective role is the highest mapped IdP group. Only when *none* of the caller's groups
    /// is mapped (no `cognito:groups` at all, as with Google federation, or only groups biffix
    /// does not know) does it fall back to the deployment's explicit authenticated-user role. The
    /// default is a fallback, not a floor: a member of a mapped group gets that group's role even
    /// when the default is higher, so an explicit group assignment is always what decides.
    pub fn role_for(&self, groups: &[String]) -> Option<Role> {
        groups
            .iter()
            .filter_map(|g| self.group_to_role.get(g).copied())
            .max()
            .or(self.authenticated_default)
    }

    /// The one authorization choke point. Today: role-only. Later: + project/ACL args here.
    pub fn can(&self, groups: &[String], cap: Capability) -> bool {
        match self.role_for(groups) {
            Some(role) => role >= cap.min_role(),
            None => false,
        }
    }
}
