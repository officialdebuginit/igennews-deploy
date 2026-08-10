//! Module 10 — administration: the permissions/roles management API and the
//! audit-log viewer. Ported from the legacy `permissions` package
//! (`service.upsert_policy`, `roles.py`) and the `/audit` endpoint. The tables
//! were created in migration 0003; this is the management surface over them.
//!
//! The two escalation guards are the point of this module: nobody may grant a
//! capability they do not themselves hold, and built-in (system) roles cannot be
//! edited or deleted.

use serde::Serialize;
use sqlx::FromRow;
use sqlx::types::Json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::authz::{self, Actor};
use crate::capabilities::{self, Role};
use crate::{DecisionView, NewsroomError, NewsroomService};

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct PermissionPolicy {
    pub id: Uuid,
    pub subject_type: String,
    pub subject_id: Uuid,
    pub capability: String,
    pub allow: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    pub reason: Option<String>,
    pub granted_by_id: Option<Uuid>,
    pub version: i32,
    // Not part of the legacy permission-policy contract.
    #[serde(skip_serializing)]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct RoleDefinition {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub is_system: bool,
    pub capabilities: Json<Vec<String>>,
    // Whether the role is carried org-wide or per workspace membership. Legacy's
    // roles are flat and its contract has no such field, so it is read but not
    // serialized — the sector-aware admin UI consumes it directly.
    #[serde(skip_serializing)]
    pub scope: String,
    // Legacy's role contract carries neither timestamp.
    #[serde(skip_serializing)]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct RoleAssignment {
    pub id: Uuid,
    pub user_id: Uuid,
    pub role_key: String,
    pub scope_type: String,
    pub scope_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub starts_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub ends_at: Option<OffsetDateTime>,
    pub granted_by_id: Option<Uuid>,
    pub reason: Option<String>,
    // Not part of the legacy role-assignment contract.
    #[serde(skip_serializing)]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Delegation {
    pub id: Uuid,
    pub from_user_id: Uuid,
    pub to_user_id: Uuid,
    pub capabilities: Json<Vec<String>>,
    #[serde(with = "time::serde::rfc3339")]
    pub starts_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub ends_at: OffsetDateTime,
    pub reason: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
    // Not part of the legacy delegation contract.
    #[serde(skip_serializing)]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct AuditRecord {
    pub id: Uuid,
    pub actor_id: Option<Uuid>,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
    pub context: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A capability policy to write.
#[derive(Clone, Debug)]
pub struct PolicyInput {
    pub subject_type: String,
    pub subject_id: Uuid,
    pub capability: String,
    pub allow: bool,
    pub expires_at: Option<OffsetDateTime>,
    pub reason: Option<String>,
}

/// A custom role to create.
#[derive(Clone, Debug)]
pub struct RoleInput {
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub capabilities: Vec<String>,
}

/// A role assignment to create.
#[derive(Clone, Debug)]
pub struct AssignmentInput {
    pub role_key: String,
    pub scope_type: String,
    pub scope_id: Option<Uuid>,
    pub ends_at: Option<OffsetDateTime>,
    pub reason: Option<String>,
}

/// A delegation to create.
#[derive(Clone, Debug)]
pub struct DelegationInput {
    pub to_user_id: Uuid,
    pub capabilities: Vec<String>,
    pub ends_at: OffsetDateTime,
    pub reason: Option<String>,
}

/// Filters for the audit log.
#[derive(Clone, Debug, Default)]
pub struct AuditFilter {
    pub entity_type: Option<String>,
    pub action: Option<String>,
    pub actor_id: Option<Uuid>,
}

impl NewsroomService {
    /// Loads the capability [`Actor`] for a user, or `None` if unknown.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn actor_for_user(&self, user_id: Uuid) -> Result<Option<Actor>, NewsroomError> {
        let row: Option<(String, bool)> =
            sqlx::query_as("SELECT role, is_admin FROM meridian.users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(self.pool())
                .await?;
        Ok(row.and_then(|(role, is_admin)| {
            Role::from_wire(&role).map(|role| Actor {
                id: user_id,
                role,
                is_admin,
            })
        }))
    }

    /// The full capability registry, for the permissions admin screen.
    #[must_use]
    pub fn list_capabilities(&self) -> &'static [capabilities::Capability] {
        &capabilities::REGISTRY
    }

    /// Resolves one capability for a target user (the simulation view).
    ///
    /// Simulating **another** user reveals whether they hold a capability — the same
    /// privilege-map disclosure as [`Self::effective_permissions`], so it is gated
    /// the same way: self is always allowed, anyone else requires `permissions.manage`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] when probing another user without
    /// `permissions.manage`; [`NewsroomError::NotFound`] for an unknown user;
    /// database failures.
    pub async fn simulate(
        &self,
        viewer: &Actor,
        user_id: Uuid,
        capability: &str,
        desk_id: Option<Uuid>,
    ) -> Result<DecisionView, NewsroomError> {
        if viewer.id != user_id {
            authz::require(self.pool(), viewer, "permissions.manage", None).await?;
        }
        let actor = self
            .actor_for_user(user_id)
            .await?
            .ok_or(NewsroomError::NotFound("User"))?;
        Ok(authz::resolve(self.pool(), &actor, capability, desk_id).await?.into())
    }

    /// Every capability resolved for a target user (the effective-permissions view).
    ///
    /// `desk_id` scopes the answer to one sector; `None` answers at org level.
    ///
    /// A viewer may always resolve **their own** capabilities — the UI relies on this
    /// to stop offering doors that would 403. Resolving *another* user's set is the
    /// "why can this person do that?" diagnostic and is privileged: it exposes the
    /// full privilege map of an account, so it requires `permissions.manage`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] when inspecting another user without
    /// `permissions.manage`; [`NewsroomError::NotFound`] for an unknown user;
    /// database failures.
    pub async fn effective_permissions(
        &self,
        viewer: &Actor,
        user_id: Uuid,
        desk_id: Option<Uuid>,
    ) -> Result<Vec<DecisionView>, NewsroomError> {
        if viewer.id != user_id {
            authz::require(self.pool(), viewer, "permissions.manage", None).await?;
        }
        let actor = self
            .actor_for_user(user_id)
            .await?
            .ok_or(NewsroomError::NotFound("User"))?;
        Ok(authz::effective(self.pool(), &actor, desk_id)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Every permission policy.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn list_policies(
        &self,
        actor: &Actor,
    ) -> Result<Vec<PermissionPolicy>, NewsroomError> {
        authz::require(self.pool(), actor, "permissions.manage", None).await?;
        Ok(sqlx::query_as::<_, PermissionPolicy>(
            "SELECT * FROM meridian.permission_policies ORDER BY created_at DESC",
        )
        .fetch_all(self.pool())
        .await?)
    }

    /// Creates or updates a policy. Requires `permissions.manage`, and — the
    /// escalation guard — the actor must themselves hold a capability they grant.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::Unprocessable`] for an
    /// unknown capability or subject type; [`NewsroomError::NotFound`]; database
    /// failures.
    pub async fn upsert_policy(
        &self,
        actor: &Actor,
        input: &PolicyInput,
    ) -> Result<PermissionPolicy, NewsroomError> {
        authz::require(self.pool(), actor, "permissions.manage", None).await?;
        if !matches!(input.subject_type.as_str(), "user" | "group") {
            return Err(NewsroomError::Unprocessable(
                "subject_type must be 'user' or 'group'".to_owned(),
            ));
        }
        if !capabilities::exists(&input.capability) {
            return Err(NewsroomError::Unprocessable(format!(
                "'{}' is not a registered capability",
                input.capability
            )));
        }
        // Nobody may hand out a capability they do not hold themselves — and for a
        // group (desk) policy, they must hold it *in that desk*, otherwise a lead
        // of one sector could grant capabilities into another.
        let grant_scope = (input.subject_type == "group").then_some(input.subject_id);
        if input.allow && !authz::has(self.pool(), actor, &input.capability, grant_scope).await? {
            return Err(NewsroomError::Forbidden {
                capability: input.capability.clone(),
                reason: format!(
                    "You cannot grant '{}' because you do not hold it yourself",
                    input.capability
                ),
            });
        }
        let mut tx = self.pool().begin().await?;
        let policy = sqlx::query_as::<_, PermissionPolicy>(
            "INSERT INTO meridian.permission_policies \
             (id, subject_type, subject_id, capability, allow, expires_at, reason, granted_by_id, version) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,1) \
             ON CONFLICT (subject_type, subject_id, capability) DO UPDATE SET \
               allow = EXCLUDED.allow, expires_at = EXCLUDED.expires_at, reason = EXCLUDED.reason, \
               granted_by_id = EXCLUDED.granted_by_id, \
               version = meridian.permission_policies.version + 1, updated_at = now() \
             RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(&input.subject_type)
        .bind(input.subject_id)
        .bind(&input.capability)
        .bind(input.allow)
        .bind(input.expires_at)
        .bind(&input.reason)
        .bind(actor.id)
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "permission.upserted",
            "permission",
            &policy.id.to_string(),
            None,
            Some(serde_json::json!({ "capability": policy.capability, "allow": policy.allow })),
        )
        .await?;
        tx.commit().await?;
        Ok(policy)
    }

    /// Deletes a policy; requires `permissions.manage`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_policy(&self, actor: &Actor, policy_id: Uuid) -> Result<(), NewsroomError> {
        authz::require(self.pool(), actor, "permissions.manage", None).await?;
        let mut tx = self.pool().begin().await?;
        let deleted = sqlx::query("DELETE FROM meridian.permission_policies WHERE id = $1")
            .bind(policy_id)
            .execute(&mut *tx)
            .await?;
        if deleted.rows_affected() == 0 {
            return Err(NewsroomError::NotFound("Policy"));
        }
        crate::audit(
            &mut *tx,
            actor.id,
            "permission.deleted",
            "permission",
            &policy_id.to_string(),
            None,
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Mirrors the built-in roles into `role_definitions` from the registry.
    /// Idempotent; safe to call on boot. Returns how many rows changed.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn sync_system_roles(&self) -> Result<u64, NewsroomError> {
        let mut changed = 0;
        for role in Role::ALL {
            let expected: Vec<String> = capabilities::REGISTRY
                .iter()
                .filter(|capability| capability.allows_role(role))
                .map(|capability| capability.key.to_owned())
                .collect();
            let name = titleize(role.as_str());
            let scope = if role.is_global() { "global" } else { "workspace" };
            let result = sqlx::query(
                "INSERT INTO meridian.role_definitions (id, key, name, description, is_system, capabilities, scope) \
                 VALUES ($1, $2, $3, $4, true, $5, $6) \
                 ON CONFLICT (key) DO UPDATE SET capabilities = EXCLUDED.capabilities, \
                   scope = EXCLUDED.scope, updated_at = now() \
                 WHERE meridian.role_definitions.is_system \
                   AND (meridian.role_definitions.capabilities <> EXCLUDED.capabilities \
                        OR meridian.role_definitions.scope <> EXCLUDED.scope)",
            )
            .bind(Uuid::now_v7())
            .bind(role.as_str())
            .bind(&name)
            .bind(format!("Built-in {name} role."))
            .bind(Json(&expected))
            .bind(scope)
            .execute(self.pool())
            .await?;
            changed += result.rows_affected();
        }
        Ok(changed)
    }

    /// Every role definition.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_roles(&self) -> Result<Vec<RoleDefinition>, NewsroomError> {
        Ok(
            sqlx::query_as::<_, RoleDefinition>("SELECT * FROM meridian.role_definitions ORDER BY key")
                .fetch_all(self.pool())
                .await?,
        )
    }

    /// Creates a custom role. Requires `roles.manage`, all capabilities must be
    /// registered, and the actor must hold every capability they package.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::Unprocessable`];
    /// [`NewsroomError::Conflict`]; database failures.
    pub async fn create_role(
        &self,
        actor: &Actor,
        input: &RoleInput,
    ) -> Result<RoleDefinition, NewsroomError> {
        authz::require(self.pool(), actor, "roles.manage", None).await?;
        self.check_can_package(actor, &input.capabilities).await?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM meridian.role_definitions WHERE key = $1)")
                .bind(&input.key)
                .fetch_one(self.pool())
                .await?;
        if exists {
            return Err(NewsroomError::Conflict(format!(
                "Role '{}' already exists",
                input.key
            )));
        }
        let mut tx = self.pool().begin().await?;
        let role = sqlx::query_as::<_, RoleDefinition>(
            "INSERT INTO meridian.role_definitions (id, key, name, description, is_system, capabilities) \
             VALUES ($1, $2, $3, $4, false, $5) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(&input.key)
        .bind(&input.name)
        .bind(&input.description)
        .bind(Json(&input.capabilities))
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "role.created",
            "role",
            &role.id.to_string(),
            None,
            Some(serde_json::json!({ "key": role.key })),
        )
        .await?;
        tx.commit().await?;
        Ok(role)
    }

    /// One role definition by key.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn get_role(&self, key: &str) -> Result<RoleDefinition, NewsroomError> {
        sqlx::query_as::<_, RoleDefinition>("SELECT * FROM meridian.role_definitions WHERE key = $1")
            .bind(key)
            .fetch_optional(self.pool())
            .await?
            .ok_or(NewsroomError::NotFound("Role"))
    }

    /// Updates a custom role. Built-in roles are derived from the registry and
    /// cannot be edited; the actor must hold every capability they package.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::Conflict`] for a system
    /// role; [`NewsroomError::NotFound`]; database failures.
    pub async fn update_role(
        &self,
        actor: &Actor,
        key: &str,
        name: Option<&str>,
        description: Option<&str>,
        capabilities_list: Option<&[String]>,
    ) -> Result<RoleDefinition, NewsroomError> {
        authz::require(self.pool(), actor, "roles.manage", None).await?;
        let existing = self.get_role(key).await?;
        if existing.is_system {
            return Err(NewsroomError::Conflict(format!(
                "'{key}' is a built-in role. Use a permission policy to adjust it, or create a custom role."
            )));
        }
        if let Some(caps) = capabilities_list {
            self.check_can_package(actor, caps).await?;
        }
        let mut tx = self.pool().begin().await?;
        let role = sqlx::query_as::<_, RoleDefinition>(
            "UPDATE meridian.role_definitions SET \
               name = COALESCE($2, name), description = COALESCE($3, description), \
               capabilities = COALESCE($4, capabilities), updated_at = now() \
             WHERE key = $1 RETURNING *",
        )
        .bind(key)
        .bind(name)
        .bind(description)
        .bind(capabilities_list.map(Json))
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "role.updated",
            "role",
            &role.id.to_string(),
            None,
            Some(serde_json::json!({ "key": key })),
        )
        .await?;
        tx.commit().await?;
        Ok(role)
    }

    /// Deletes a custom role; a built-in (system) role cannot be deleted.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::Conflict`] for a system
    /// role; [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_role(&self, actor: &Actor, key: &str) -> Result<(), NewsroomError> {
        authz::require(self.pool(), actor, "roles.manage", None).await?;
        let is_system: Option<bool> =
            sqlx::query_scalar("SELECT is_system FROM meridian.role_definitions WHERE key = $1")
                .bind(key)
                .fetch_optional(self.pool())
                .await?;
        match is_system {
            None => return Err(NewsroomError::NotFound("Role")),
            Some(true) => {
                return Err(NewsroomError::Conflict(format!(
                    "'{key}' is a built-in role and cannot be deleted"
                )));
            }
            Some(false) => {}
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM meridian.role_definitions WHERE key = $1")
            .bind(key)
            .execute(&mut *tx)
            .await?;
        crate::audit(&mut *tx, actor.id, "role.deleted", "role", key, None, None).await?;
        tx.commit().await?;
        Ok(())
    }

    /// A user's role assignments.
    ///
    /// Another user's assignments expose their granted roles, scopes, the granting
    /// admin and the reason — a governance disclosure — so this is gated like the
    /// rest of that surface: self is always allowed, anyone else needs
    /// `permissions.manage`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] when reading another user's assignments without
    /// `permissions.manage`; propagates database failures.
    pub async fn list_assignments(
        &self,
        viewer: &Actor,
        user_id: Uuid,
    ) -> Result<Vec<RoleAssignment>, NewsroomError> {
        if viewer.id != user_id {
            authz::require(self.pool(), viewer, "permissions.manage", None).await?;
        }
        Ok(sqlx::query_as::<_, RoleAssignment>(
            "SELECT * FROM meridian.role_assignments WHERE user_id = $1 ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Assigns a role to a user. Requires `roles.manage` and that the actor holds
    /// every capability the role grants.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::Unprocessable`];
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn assign_role(
        &self,
        actor: &Actor,
        user_id: Uuid,
        input: &AssignmentInput,
    ) -> Result<RoleAssignment, NewsroomError> {
        authz::require(self.pool(), actor, "roles.manage", None).await?;
        let definition_caps: Option<Json<Vec<String>>> =
            sqlx::query_scalar("SELECT capabilities FROM meridian.role_definitions WHERE key = $1")
                .bind(&input.role_key)
                .fetch_optional(self.pool())
                .await?;
        let caps = definition_caps.ok_or(NewsroomError::NotFound("Role"))?.0;
        self.check_can_package(actor, &caps).await?;
        if input.scope_type == "desk" && input.scope_id.is_none() {
            return Err(NewsroomError::Unprocessable(
                "A desk-scoped assignment needs a scope_id".to_owned(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let assignment = sqlx::query_as::<_, RoleAssignment>(
            "INSERT INTO meridian.role_assignments \
             (id, user_id, role_key, scope_type, scope_id, ends_at, granted_by_id, reason) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(&input.role_key)
        .bind(&input.scope_type)
        .bind(input.scope_id)
        .bind(input.ends_at)
        .bind(actor.id)
        .bind(&input.reason)
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "role.assigned",
            "user",
            &user_id.to_string(),
            None,
            Some(serde_json::json!({ "role_key": input.role_key })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "role.assigned",
            &user_id.to_string(),
            None,
            serde_json::json!({ "user_id": user_id, "role_key": input.role_key }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(assignment)
    }

    /// Revokes a role assignment; requires `roles.manage`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn revoke_assignment(
        &self,
        actor: &Actor,
        assignment_id: Uuid,
    ) -> Result<(), NewsroomError> {
        authz::require(self.pool(), actor, "roles.manage", None).await?;
        let mut tx = self.pool().begin().await?;
        let deleted = sqlx::query("DELETE FROM meridian.role_assignments WHERE id = $1")
            .bind(assignment_id)
            .execute(&mut *tx)
            .await?;
        if deleted.rows_affected() == 0 {
            return Err(NewsroomError::NotFound("Assignment"));
        }
        crate::audit(
            &mut *tx,
            actor.id,
            "role.unassigned",
            "role_assignment",
            &assignment_id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "role.unassigned",
            &assignment_id.to_string(),
            None,
            serde_json::json!({ "assignment_id": assignment_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Delegates the actor's own capabilities to another user for a window.
    /// Self-service: giving away authority you hold is not acquiring any.
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`]; [`NewsroomError::Forbidden`]; database failures.
    pub async fn create_delegation(
        &self,
        actor: &Actor,
        input: &DelegationInput,
    ) -> Result<Delegation, NewsroomError> {
        if input.to_user_id == actor.id {
            return Err(NewsroomError::Unprocessable(
                "You cannot delegate to yourself".to_owned(),
            ));
        }
        self.check_can_package(actor, &input.capabilities).await?;
        let mut tx = self.pool().begin().await?;
        let delegation = sqlx::query_as::<_, Delegation>(
            "INSERT INTO meridian.delegations (id, from_user_id, to_user_id, capabilities, ends_at, reason) \
             VALUES ($1,$2,$3,$4,$5,$6) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(actor.id)
        .bind(input.to_user_id)
        .bind(Json(&input.capabilities))
        .bind(input.ends_at)
        .bind(&input.reason)
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "delegation.created",
            "user",
            &input.to_user_id.to_string(),
            None,
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(delegation)
    }

    /// Delegations involving the actor (granted by or to them), newest first.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_delegations(&self, actor: &Actor) -> Result<Vec<Delegation>, NewsroomError> {
        Ok(sqlx::query_as::<_, Delegation>(
            "SELECT * FROM meridian.delegations \
             WHERE from_user_id = $1 OR to_user_id = $1 ORDER BY created_at DESC",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Revokes a delegation; the delegator or a role manager may revoke it.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn revoke_delegation(
        &self,
        actor: &Actor,
        delegation_id: Uuid,
    ) -> Result<(), NewsroomError> {
        let from: Option<Uuid> =
            sqlx::query_scalar("SELECT from_user_id FROM meridian.delegations WHERE id = $1")
                .bind(delegation_id)
                .fetch_optional(self.pool())
                .await?;
        let from = from.ok_or(NewsroomError::NotFound("Delegation"))?;
        if from != actor.id && !authz::has(self.pool(), actor, "roles.manage", None).await? {
            return Err(NewsroomError::Forbidden {
                capability: "roles.manage".to_owned(),
                reason: "Only the delegator or a role manager can revoke this".to_owned(),
            });
        }
        // The revoke and its audit row commit together: a delegation that was
        // lifted with no trace is exactly the event an audit log exists for.
        // Every other mutation in this module already does this.
        let mut tx = self.pool().begin().await?;
        sqlx::query("UPDATE meridian.delegations SET revoked_at = now() WHERE id = $1")
            .bind(delegation_id)
            .execute(&mut *tx)
            .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "delegation.revoked",
            "delegation",
            &delegation_id.to_string(),
            None,
            Some(serde_json::json!({ "from_user_id": from })),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The audit log, newest first, with optional filters.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn list_audit(
        &self,
        actor: &Actor,
        filter: &AuditFilter,
        limit: i64,
    ) -> Result<Vec<AuditRecord>, NewsroomError> {
        authz::require(self.pool(), actor, "dashboard.view_system_health", None).await?;
        Ok(sqlx::query_as::<_, AuditRecord>(
            "SELECT * FROM meridian.audit_events \
             WHERE ($1::text IS NULL OR entity_type = $1) \
               AND ($2::text IS NULL OR action = $2) \
               AND ($3::uuid IS NULL OR actor_id = $3) \
             ORDER BY created_at DESC LIMIT $4",
        )
        .bind(&filter.entity_type)
        .bind(&filter.action)
        .bind(filter.actor_id)
        .bind(limit.clamp(1, 500))
        .fetch_all(self.pool())
        .await?)
    }

    /// The escalation guard: the actor must hold every capability being packaged,
    /// and each must be registered.
    async fn check_can_package(
        &self,
        actor: &Actor,
        capabilities_list: &[String],
    ) -> Result<(), NewsroomError> {
        for key in capabilities_list {
            if !capabilities::exists(key) {
                return Err(NewsroomError::Unprocessable(format!(
                    "'{key}' is not a registered capability"
                )));
            }
            if !authz::has(self.pool(), actor, key, None).await? {
                return Err(NewsroomError::Forbidden {
                    capability: key.clone(),
                    reason: format!("You cannot grant '{key}' because you do not hold it yourself"),
                });
            }
        }
        Ok(())
    }
}

/// Turns a `snake_case` role key into a display name (`editor_in_chief` →
/// "Editor In Chief").
fn titleize(value: &str) -> String {
    value
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::titleize;

    #[test]
    fn titleize_matches_legacy_naming() {
        assert_eq!(titleize("editor_in_chief"), "Editor In Chief");
        assert_eq!(titleize("reporter"), "Reporter");
    }
}
