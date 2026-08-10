//! The capability registry — the single source of truth for what can be granted.
//!
//! Transcribed verbatim from the legacy `permissions/capabilities.py`. Resolution
//! is deny-by-default (see [`crate::authz`]), so a capability that is not
//! registered here is denied to everyone rather than silently allowed.

use std::sync::LazyLock;

use serde::Serialize;

/// A newsroom role. Mirrors the legacy `Role` enum and the `users.role` CHECK.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Role {
    Admin,
    EditorInChief,
    ManagingEditor,
    SectionEditor,
    Reporter,
    CopyEditor,
    FactChecker,
    Producer,
    StandardsLegal,
    AudienceEditor,
    Contributor,
    Viewer,
}

impl Role {
    /// Every role, in declaration order.
    pub const ALL: [Role; 12] = [
        Role::Admin,
        Role::EditorInChief,
        Role::ManagingEditor,
        Role::SectionEditor,
        Role::Reporter,
        Role::CopyEditor,
        Role::FactChecker,
        Role::Producer,
        Role::StandardsLegal,
        Role::AudienceEditor,
        Role::Contributor,
        Role::Viewer,
    ];

    /// The wire value, matching the legacy `Role` string enum.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::EditorInChief => "editor_in_chief",
            Role::ManagingEditor => "managing_editor",
            Role::SectionEditor => "section_editor",
            Role::Reporter => "reporter",
            Role::CopyEditor => "copy_editor",
            Role::FactChecker => "fact_checker",
            Role::Producer => "producer",
            Role::StandardsLegal => "standards_legal",
            Role::AudienceEditor => "audience_editor",
            Role::Contributor => "contributor",
            Role::Viewer => "viewer",
        }
    }

    /// Resolves a stored role string, or `None` if it is not a known role.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|role| role.as_str() == value)
    }
}

/// Whether a capability is answered org-wide or inside one sector.
///
/// This is what lets the resolver refuse to let a *global* role default answer a
/// *sector* question: a reporter's org-level role says nothing about whether they
/// may write in Politics — only a membership there does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Answered once for the whole org (governance, flags, system health).
    Global,
    /// Answered per sector; a workspace membership is the grant.
    Sector,
}

/// The four roles carried everywhere, which overlay every sector.
///
/// Per [02 · Workspace-Sector Taxonomy §4]; the remaining eight are workspace
/// roles held per membership.
pub const GLOBAL_ROLES: &[Role] = &[
    Role::Admin,
    Role::EditorInChief,
    Role::StandardsLegal,
    Role::AudienceEditor,
];

impl Role {
    /// Whether this role is carried org-wide rather than per membership.
    #[must_use]
    pub fn is_global(self) -> bool {
        GLOBAL_ROLES.contains(&self)
    }
}

/// A registered capability and the roles that hold it by default.
#[derive(Clone, Debug, Serialize)]
pub struct Capability {
    pub key: &'static str,
    pub label: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub scope: Scope,
    pub default_roles: &'static [Role],
}

impl Capability {
    #[must_use]
    pub fn allows_role(&self, role: Role) -> bool {
        self.default_roles.contains(&role)
    }
}

// Role groupings reused across defaults, spelled out per capability so that
// changing one capability's audience never silently moves another's.
const EDITORS: &[Role] = &[
    Role::Admin,
    Role::EditorInChief,
    Role::ManagingEditor,
    Role::SectionEditor,
    Role::StandardsLegal,
];
const ALL_ROLES: &[Role] = &Role::ALL;
const AUDIENCE: &[Role] = &[
    Role::Admin,
    Role::EditorInChief,
    Role::ManagingEditor,
    Role::SectionEditor,
    Role::StandardsLegal,
    Role::Producer,
    Role::AudienceEditor,
];
const ADMIN_ONLY: &[Role] = &[Role::Admin];
/// The legacy `require_publisher` audience, now a registered capability.
const PUBLISHERS: &[Role] = &[
    Role::Admin,
    Role::EditorInChief,
    Role::ManagingEditor,
    Role::SectionEditor,
    Role::Producer,
    Role::StandardsLegal,
];
/// The legacy `require_verifier` audience, now a registered capability.
const VERIFIERS: &[Role] = &[
    Role::Admin,
    Role::EditorInChief,
    Role::ManagingEditor,
    Role::SectionEditor,
    Role::FactChecker,
    Role::CopyEditor,
    Role::StandardsLegal,
];

/// A sector-scoped capability — the common case. Answered inside one workspace,
/// where a membership is the grant.
const fn cap(
    key: &'static str,
    label: &'static str,
    category: &'static str,
    description: &'static str,
    default_roles: &'static [Role],
) -> Capability {
    Capability {
        key,
        label,
        category,
        description,
        scope: Scope::Sector,
        default_roles,
    }
}

/// An org-wide capability: governance and cross-sector reach, answered once for
/// the whole newsroom rather than per workspace.
const fn global_cap(
    key: &'static str,
    label: &'static str,
    category: &'static str,
    description: &'static str,
    default_roles: &'static [Role],
) -> Capability {
    Capability {
        key,
        label,
        category,
        description,
        scope: Scope::Global,
        default_roles,
    }
}

/// The registry, sorted by key at construction so lookups and listings agree.
pub static REGISTRY: LazyLock<Vec<Capability>> = LazyLock::new(|| {
    let mut registry = vec![
        // --- Editorial (defaults transcribed from legacy check_permission) ---
        cap(
            "stories.create",
            "Create stories",
            "editorial",
            "Open a new story record.",
            &[
                Role::Reporter,
                Role::EditorInChief,
                Role::ManagingEditor,
                Role::SectionEditor,
                Role::Producer,
            ],
        ),
        cap(
            "stories.edit",
            "Edit stories",
            "editorial",
            "Change story content and metadata.",
            &[
                Role::Reporter,
                Role::EditorInChief,
                Role::ManagingEditor,
                Role::SectionEditor,
                Role::Producer,
                Role::CopyEditor,
            ],
        ),
        cap(
            "stories.delete",
            "Delete stories",
            "editorial",
            "Permanently remove a story you authored.",
            &[Role::EditorInChief, Role::ManagingEditor, Role::Admin],
        ),
        cap(
            "stories.delete_any",
            "Delete anyone's story",
            "editorial",
            "Delete a story authored by someone else.",
            &[Role::EditorInChief, Role::ManagingEditor, Role::Admin],
        ),
        cap(
            "reviews.approve",
            "Decide reviews",
            "editorial",
            "Approve or reject a review, and publish releases.",
            &[
                Role::EditorInChief,
                Role::ManagingEditor,
                Role::SectionEditor,
                Role::Producer,
                Role::StandardsLegal,
                Role::Admin,
            ],
        ),
        cap(
            "tasks.assign",
            "Assign tasks",
            "editorial",
            "Assign a task to another person.",
            &[
                Role::EditorInChief,
                Role::ManagingEditor,
                Role::SectionEditor,
                Role::Producer,
                Role::StandardsLegal,
                Role::Admin,
            ],
        ),
        // --- Editorial: what the legacy role-group gates guarded ---
        // These were `require_editor` / `require_publisher` / `require_verifier`
        // role tests that bypassed the registry entirely, so they could never be
        // answered per sector. Registering them is what makes "who may publish in
        // Politics?" a query rather than an unanswerable question.
        cap(
            "stories.edit_any",
            "Edit any story",
            "editorial",
            "Edit a story you are not on the team of.",
            EDITORS,
        ),
        cap(
            "workflow.advance",
            "Advance the workflow",
            "editorial",
            "Move a story into verification, copy/standards or ready.",
            EDITORS,
        ),
        cap(
            "workflow.fast_path",
            "Use the publish fast path",
            "editorial",
            "Bypass approval and asset blockers with a recorded reason.",
            EDITORS,
        ),
        cap(
            "releases.publish",
            "Publish releases",
            "editorial",
            "Create, schedule and run releases for a story.",
            PUBLISHERS,
        ),
        cap(
            "claims.decide",
            "Decide claims",
            "editorial",
            "Set a claim's verification status.",
            VERIFIERS,
        ),
        cap(
            "sources.approve",
            "Approve confidential sources",
            "editorial",
            "Grant manager approval for anonymous, background or off-record sources.",
            EDITORS,
        ),
        cap(
            "pitches.decide",
            "Decide pitches",
            "editorial",
            "Commission or reject a pitch.",
            EDITORS,
        ),
        cap(
            "reviews.decide_any",
            "Decide anyone's review",
            "editorial",
            "Decide a review assigned to someone else.",
            EDITORS,
        ),
        cap(
            "reviews.view_all",
            "View all reviews",
            "editorial",
            "See reviews beyond your own assignments.",
            EDITORS,
        ),
        cap(
            "comments.moderate",
            "Moderate comments",
            "editorial",
            "Delete a comment written by someone else.",
            EDITORS,
        ),
        cap(
            "feed.moderate",
            "Moderate the feed",
            "editorial",
            "Delete a feed post or reply written by someone else.",
            EDITORS,
        ),
        cap(
            "tasks.view_all",
            "View all tasks",
            "editorial",
            "See tasks beyond the ones assigned to you.",
            EDITORS,
        ),
        cap(
            "tasks.edit_any",
            "Edit any task",
            "editorial",
            "Change a task you neither own nor created.",
            EDITORS,
        ),
        global_cap(
            "people.view_contact",
            "View contact details",
            "administration",
            "See another person's contact details in the directory.",
            EDITORS,
        ),
        // --- Dashboard ---
        cap(
            "dashboard.view",
            "View dashboard",
            "dashboard",
            "See a personal dashboard.",
            ALL_ROLES,
        ),
        cap(
            "dashboard.view_desk",
            "View desk dashboard",
            "dashboard",
            "Scope the dashboard to a whole desk rather than only personal items.",
            EDITORS,
        ),
        global_cap(
            "dashboard.view_global",
            "View newsroom dashboard",
            "dashboard",
            "Scope the dashboard across every desk.",
            EDITORS,
        ),
        cap(
            "dashboard.customize",
            "Customize dashboard",
            "dashboard",
            "Save personal dashboard views.",
            ALL_ROLES,
        ),
        cap(
            "dashboard.share_view",
            "Share dashboard views",
            "dashboard",
            "Share a saved view with a desk.",
            EDITORS,
        ),
        cap(
            "dashboard.view_workload",
            "View workload",
            "dashboard",
            "See per-person workload detail across the newsroom.",
            EDITORS,
        ),
        global_cap(
            "dashboard.view_audience",
            "View audience analytics",
            "dashboard",
            "See audience and performance metrics.",
            AUDIENCE,
        ),
        global_cap(
            "dashboard.view_system_health",
            "View system health",
            "dashboard",
            "See API, database, scheduler and integration status.",
            ADMIN_ONLY,
        ),
        cap(
            "attention.assign",
            "Assign attention items",
            "dashboard",
            "Assign an alert to someone.",
            EDITORS,
        ),
        cap(
            "attention.escalate",
            "Escalate attention items",
            "dashboard",
            "Escalate an alert.",
            EDITORS,
        ),
        // --- Desks / workspaces ---
        cap(
            "desks.manage",
            "Manage desks",
            "desks",
            "Create, edit, re-parent and archive desks (workspaces).",
            EDITORS,
        ),
        cap(
            "desks.invite",
            "Invite to a desk",
            "desks",
            "Invite people to join a desk.",
            EDITORS,
        ),
        // Org-wide creation/removal of top-level sectors is admin-only. Editing a
        // sector, its sub-sectors and its policy stays on desk-scoped desks.manage,
        // so a delegated sector manager still runs their own sector — but only an
        // admin adds or deletes a sector at the top level (doc §3.3/§3.4).
        global_cap(
            "sectors.admin",
            "Administer sectors",
            "desks",
            "Create and delete top-level sectors (workspaces) across the organisation.",
            ADMIN_ONLY,
        ),
        // --- Administration ---
        global_cap(
            "permissions.manage",
            "Manage permissions",
            "administration",
            "Create, change and delete permission policies.",
            ADMIN_ONLY,
        ),
        global_cap(
            "roles.manage",
            "Manage roles",
            "administration",
            "Create and change custom roles, assignments and delegations.",
            ADMIN_ONLY,
        ),
        global_cap(
            "flags.manage",
            "Manage feature flags",
            "administration",
            "Create, target, roll out and emergency-disable features.",
            ADMIN_ONLY,
        ),
        global_cap(
            "webhooks.manage",
            "Manage webhooks",
            "administration",
            "Register outbound webhook endpoints and read the delivery log.",
            ADMIN_ONLY,
        ),
        global_cap(
            "subscriptions.manage",
            "Manage subscriptions",
            "administration",
            "Grant, list and cancel reader subscriptions (reader-revenue entitlements).",
            ADMIN_ONLY,
        ),
    ];
    registry.sort_by(|a, b| (a.category, a.key).cmp(&(b.category, b.key)));
    registry
});

/// Looks up a capability by key.
#[must_use]
pub fn get(key: &str) -> Option<&'static Capability> {
    REGISTRY.iter().find(|capability| capability.key == key)
}

/// Whether a capability key is registered.
#[must_use]
pub fn exists(key: &str) -> bool {
    get(key).is_some()
}

/// Every registered capability key, sorted.
#[must_use]
pub fn all_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = REGISTRY.iter().map(|capability| capability.key).collect();
    keys.sort_unstable();
    keys
}

#[cfg(test)]
mod tests {
    use super::{Role, all_keys, exists, get};

    #[test]
    fn unregistered_capabilities_are_absent() {
        assert!(!exists("stories.teleport"));
        assert!(get("stories.teleport").is_none());
    }

    #[test]
    fn desk_capabilities_default_to_editors_not_reporters() {
        let manage = get("desks.manage").expect("desks.manage is registered");
        assert!(manage.allows_role(Role::SectionEditor));
        assert!(!manage.allows_role(Role::Reporter));
    }

    #[test]
    fn registry_has_the_full_legacy_capability_set() {
        // The full registry: the legacy capability set + the gates that replaced the
        // `require_editor` / `require_publisher` / `require_verifier` role groups +
        // the governance capabilities (webhooks, subscriptions) + `sectors.admin`
        // (two-level taxonomy). The registry is the single source of truth; this
        // asserts its size so an accidental add/remove is caught.
        assert_eq!(all_keys().len(), 38);
    }

    #[test]
    fn the_retired_role_gates_are_registered_with_their_legacy_audiences() {
        use super::Role as R;
        let publish = get("releases.publish").expect("releases.publish is registered");
        assert!(publish.allows_role(R::Producer), "legacy require_publisher included producer");
        assert!(!publish.allows_role(R::FactChecker));

        let claims = get("claims.decide").expect("claims.decide is registered");
        assert!(claims.allows_role(R::FactChecker), "legacy require_verifier included fact checker");
        assert!(claims.allows_role(R::CopyEditor));
        assert!(!claims.allows_role(R::Producer));

        let advance = get("workflow.advance").expect("workflow.advance is registered");
        assert!(advance.allows_role(R::SectionEditor));
        assert!(!advance.allows_role(R::Reporter), "legacy require_editor excluded reporters");
    }

    #[test]
    fn only_the_four_org_wide_roles_are_global() {
        use super::Role as R;
        for role in [R::Admin, R::EditorInChief, R::StandardsLegal, R::AudienceEditor] {
            assert!(role.is_global(), "{} should be global", role.as_str());
        }
        // Section and managing editor are *workspace* roles: senior inside a
        // sector, silent outside it. This is the split the product model needs.
        for role in [
            R::SectionEditor,
            R::ManagingEditor,
            R::Reporter,
            R::CopyEditor,
            R::FactChecker,
            R::Producer,
            R::Contributor,
            R::Viewer,
        ] {
            assert!(!role.is_global(), "{} should be workspace", role.as_str());
        }
    }

    #[test]
    fn editorial_capabilities_are_sector_scoped_and_governance_is_global() {
        use super::Scope;
        for key in ["stories.edit", "stories.create", "reviews.approve", "tasks.assign"] {
            assert_eq!(get(key).expect(key).scope, Scope::Sector, "{key}");
        }
        for key in ["permissions.manage", "roles.manage", "flags.manage", "dashboard.view_global"] {
            assert_eq!(get(key).expect(key).scope, Scope::Global, "{key}");
        }
    }

    #[test]
    fn role_round_trips_through_its_wire_value() {
        for role in Role::ALL {
            assert_eq!(Role::from_wire(role.as_str()), Some(role));
        }
        assert_eq!(Role::from_wire("nonexistent"), None);
    }
}
