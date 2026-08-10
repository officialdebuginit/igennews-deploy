//! Capability resolution, ported from the legacy `permissions/resolver.py`.
//!
//! One resolver serves enforcement, simulation and the effective-permission
//! viewer, so an explanation shown to an administrator can never disagree with
//! the decision the API actually makes. Resolution is deny-by-default and
//! first-match-wins in the documented order.

use sqlx::PgPool;
use uuid::Uuid;

use crate::{NewsroomError, capabilities};
use crate::capabilities::Role;

/// The minimum an access check needs to know about the acting user. Built from
/// the identity crate's `User` at the web boundary so this crate stays testable
/// without pulling in authentication.
#[derive(Clone, Copy, Debug)]
pub struct Actor {
    pub id: Uuid,
    pub role: Role,
    pub is_admin: bool,
}

/// The outcome of resolving one capability, with the rule that decided it.
#[derive(Clone, Debug)]
pub struct Decision {
    pub capability: String,
    pub allowed: bool,
    pub source: &'static str,
    pub reason: String,
}

impl Decision {
    fn new(
        capability: &str,
        allowed: bool,
        source: &'static str,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            capability: capability.to_owned(),
            allowed,
            source,
            reason: reason.into(),
        }
    }
}

/// Answers whether `actor` holds `capability`, recording why.
///
/// Order (first match wins): unregistered → deny; admin → allow; user policy →
/// as written; active delegation → allow; active role assignment → allow; desk
/// policy (deny beats allow); global role default; workspace membership role;
/// else deny.
///
/// `desk_id` is the sector the action targets. When it is present, a *sector*
/// capability is answered by the actor's membership role in that sector — a
/// global role default cannot reach across the boundary unless the role is one of
/// the four org-wide roles ([`capabilities::GLOBAL_ROLES`]). When it is absent,
/// the org-level answer stands and callers are expected to narrow their queries
/// to the sectors the viewer can see.
///
/// # Errors
///
/// Returns [`NewsroomError::Database`] if any resolution query fails.
// The ordered rule chain is one algorithm; splitting it across helpers would
// scatter the first-match-wins contract that makes it auditable.
#[allow(clippy::too_many_lines)]
pub async fn resolve(
    pool: &PgPool,
    actor: &Actor,
    capability: &str,
    desk_id: Option<Uuid>,
) -> Result<Decision, NewsroomError> {
    let Some(declared) = capabilities::get(capability) else {
        // Deny-by-default: an unregistered capability is granted to no one.
        return Ok(Decision::new(
            capability,
            false,
            "unregistered",
            format!("'{capability}' is not a registered capability"),
        ));
    };

    if actor.is_admin {
        return Ok(Decision::new(
            capability,
            true,
            "admin",
            "User has the admin flag",
        ));
    }

    // 3. User policy, as written (an expired policy does not match and falls
    //    through to the grant-only rules below).
    if let Some(allow) = sqlx::query_scalar::<_, bool>(
        "SELECT allow FROM meridian.permission_policies \
         WHERE subject_type = 'user' AND subject_id = $1 AND capability = $2 \
           AND (expires_at IS NULL OR expires_at > now()) LIMIT 1",
    )
    .bind(actor.id)
    .bind(capability)
    .fetch_optional(pool)
    .await?
    {
        return Ok(Decision::new(
            capability,
            allow,
            "user_policy",
            format!(
                "User-level policy explicitly says {}",
                if allow { "allow" } else { "deny" }
            ),
        ));
    }

    // 4. Active delegation (grant-only): a live, unrevoked delegation to this
    //    user that lists the capability.
    let delegated = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS ( \
           SELECT 1 FROM meridian.delegations \
           WHERE to_user_id = $1 AND revoked_at IS NULL \
             AND starts_at <= now() AND ends_at > now() \
             AND capabilities ? $2 )",
    )
    .bind(actor.id)
    .bind(capability)
    .fetch_one(pool)
    .await?;
    if delegated {
        return Ok(Decision::new(
            capability,
            true,
            "delegation",
            "An active delegation grants this",
        ));
    }

    // 5. Active role assignment (grant-only). Desk-scoped assignments apply only
    //    when they match the targeted desk.
    let assignments = sqlx::query_as::<_, (String, Option<Uuid>)>(
        "SELECT ra.scope_type, ra.scope_id \
         FROM meridian.role_assignments ra \
         JOIN meridian.role_definitions rd ON rd.key = ra.role_key \
         WHERE ra.user_id = $1 AND ra.starts_at <= now() \
           AND (ra.ends_at IS NULL OR ra.ends_at > now()) \
           AND rd.capabilities ? $2",
    )
    .bind(actor.id)
    .bind(capability)
    .fetch_all(pool)
    .await?;
    for (scope_type, scope_id) in assignments {
        if scope_type == "desk" && scope_id != desk_id {
            continue;
        }
        let scope = if scope_type == "global" {
            "globally".to_owned()
        } else {
            format!("on desk {}", scope_id.map(|id| id.to_string()).unwrap_or_default())
        };
        return Ok(Decision::new(
            capability,
            true,
            "role_assignment",
            format!("An assigned role grants this {scope}"),
        ));
    }

    // 6. Desk policy on any desk the user belongs to. Deny beats allow.
    let desk_policy = sqlx::query_as::<_, (Option<bool>, Option<bool>)>(
        "SELECT bool_or(NOT allow) AS any_deny, bool_or(allow) AS any_allow \
         FROM meridian.permission_policies \
         WHERE subject_type = 'group' AND capability = $2 \
           AND (expires_at IS NULL OR expires_at > now()) \
           AND subject_id IN ( \
             SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $1 )",
    )
    .bind(actor.id)
    .bind(capability)
    .fetch_one(pool)
    .await?;
    if desk_policy.0 == Some(true) {
        return Ok(Decision::new(
            capability,
            false,
            "desk_policy",
            "A desk policy denies this; deny beats allow at desk level",
        ));
    }
    if desk_policy.1 == Some(true) {
        return Ok(Decision::new(
            capability,
            true,
            "desk_policy",
            "A desk policy allows this",
        ));
    }

    // 7. Global role default from the registry. A global role overlays every
    //    sector, so it answers regardless of scope. A workspace role's org-level
    //    value answers only org-wide capabilities, or a sector capability asked
    //    without a sector — the unscoped list case, where the *query* narrows the
    //    rows rather than the resolver denying outright.
    let global_answer =
        declared.scope == capabilities::Scope::Global || actor.role.is_global() || desk_id.is_none();
    if global_answer && declared.allows_role(actor.role) {
        return Ok(Decision::new(
            capability,
            true,
            "role_default",
            format!("Role '{}' holds this by default", actor.role.as_str()),
        ));
    }

    // 8. Workspace membership. Inside a sector, the role carried by the actor's
    //    membership *in that sector* decides — this is what makes "who may
    //    publish in Politics?" answerable, and what makes a sector a boundary
    //    rather than a label.
    //
    //    Only for *sector* capabilities. A global capability asked with a desk
    //    attached was already answered by step 7; letting it fall through here
    //    would deny it with the reason "no membership in this sector", which is
    //    not why — `roles.manage` is not withheld for want of a desk. The trace
    //    powers the admin's own explanation, so a right answer with a wrong reason
    //    is still a defect.
    if let (Some(desk), capabilities::Scope::Sector) = (desk_id, declared.scope) {
        let membership_role: Option<String> = sqlx::query_scalar(
            "SELECT role FROM meridian.desk_memberships WHERE user_id = $1 AND desk_id = $2",
        )
        .bind(actor.id)
        .bind(desk)
        .fetch_optional(pool)
        .await?;
        if let Some(workspace_role) = membership_role.as_deref().and_then(Role::from_wire) {
            let held = declared.allows_role(workspace_role);
            let verb = if held { "holds" } else { "does not hold" };
            return Ok(Decision::new(
                capability,
                held,
                "workspace_membership",
                format!(
                    "Workspace role '{}' in this sector {verb} this",
                    workspace_role.as_str()
                ),
            ));
        }
        return Ok(Decision::new(
            capability,
            false,
            "no_membership",
            "No membership in this sector grants this".to_owned(),
        ));
    }

    // 9. Fallback deny.
    Ok(Decision::new(
        capability,
        false,
        "default_deny",
        format!(
            "Role '{}' does not hold this by default; nothing else granted it",
            actor.role.as_str()
        ),
    ))
}

/// Whether `actor` holds `capability` for the given optional desk scope.
///
/// # Errors
///
/// Returns [`NewsroomError::Database`] on query failure.
pub async fn has(
    pool: &PgPool,
    actor: &Actor,
    capability: &str,
    desk_id: Option<Uuid>,
) -> Result<bool, NewsroomError> {
    Ok(resolve(pool, actor, capability, desk_id).await?.allowed)
}

/// Enforces a capability, returning [`NewsroomError::Forbidden`] with the reason
/// it was denied — the legacy 403 body's `message`/`reason` pair.
///
/// `desk_id` is the sector the action targets. Pass `Some(desk)` for anything
/// that happens inside a workspace and `None` only for genuinely org-wide
/// capabilities; the parameter is mandatory so that every call site records an
/// explicit scope decision rather than silently defaulting to unscoped.
///
/// # Errors
///
/// Returns [`NewsroomError::Forbidden`] when denied and
/// [`NewsroomError::Database`] on query failure.
pub async fn require(
    pool: &PgPool,
    actor: &Actor,
    capability: &str,
    desk_id: Option<Uuid>,
) -> Result<(), NewsroomError> {
    let decision = resolve(pool, actor, capability, desk_id).await?;
    if decision.allowed {
        return Ok(());
    }
    Err(NewsroomError::Forbidden {
        capability: capability.to_owned(),
        reason: decision.reason,
    })
}

/// Resolves every registered capability for `actor`, for the effective-permission
/// viewer.
///
/// `desk_id` scopes the answer to one sector: without it, sector capabilities
/// resolve against the actor's global standing only, which is what the org-level
/// viewer wants. The per-sector viewer passes the desk.
///
/// # Errors
///
/// Returns [`NewsroomError::Database`] on query failure.
pub async fn effective(
    pool: &PgPool,
    actor: &Actor,
    desk_id: Option<Uuid>,
) -> Result<Vec<Decision>, NewsroomError> {
    let mut decisions = Vec::new();
    for key in capabilities::all_keys() {
        decisions.push(resolve(pool, actor, key, desk_id).await?);
    }
    Ok(decisions)
}
