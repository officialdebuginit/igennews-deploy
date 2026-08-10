-- Module 3 — Newsroom structure, plus the authorization tables its access
-- checks require. Additive only. Ids are uuid and JSON blobs are jsonb, matching
-- the Module 2 identity schema; the legacy String(36) ids hold UUIDs already, so
-- no value is lost. Notification *delivery* stays a Module 7 concern and is not
-- introduced here; audit_events is shared infrastructure because gate #2 requires
-- every desk mutation to preserve its audit trail.

-- --- Shared audit trail ------------------------------------------------------

CREATE TABLE audit_events (
    id uuid PRIMARY KEY,
    actor_id uuid REFERENCES users(id) ON DELETE SET NULL,
    action text NOT NULL,
    entity_type text NOT NULL,
    entity_id text NOT NULL,
    before jsonb,
    after jsonb,
    context jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX audit_events_actor_idx ON audit_events (actor_id);
CREATE INDEX audit_events_action_idx ON audit_events (action);
CREATE INDEX audit_events_entity_idx ON audit_events (entity_type, entity_id);
CREATE INDEX audit_events_created_idx ON audit_events (created_at);

-- --- Authorization -----------------------------------------------------------

CREATE TABLE permission_policies (
    id uuid PRIMARY KEY,
    subject_type text NOT NULL CHECK (subject_type IN ('user', 'group')),
    subject_id uuid NOT NULL,
    capability text NOT NULL,
    allow boolean NOT NULL DEFAULT true,
    expires_at timestamptz,
    reason text,
    granted_by_id uuid REFERENCES users(id) ON DELETE SET NULL,
    version integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_policy_subject_capability UNIQUE (subject_type, subject_id, capability)
);

CREATE INDEX permission_policies_subject_idx ON permission_policies (subject_id);
CREATE INDEX permission_policies_capability_idx ON permission_policies (capability);

CREATE TABLE role_definitions (
    id uuid PRIMARY KEY,
    key text NOT NULL UNIQUE,
    name text NOT NULL,
    description text,
    is_system boolean NOT NULL DEFAULT false,
    capabilities jsonb NOT NULL DEFAULT '[]'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE role_assignments (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role_key text NOT NULL REFERENCES role_definitions(key) ON DELETE CASCADE,
    scope_type text NOT NULL DEFAULT 'global' CHECK (scope_type IN ('global', 'desk')),
    scope_id uuid,
    starts_at timestamptz NOT NULL DEFAULT now(),
    ends_at timestamptz,
    granted_by_id uuid REFERENCES users(id) ON DELETE SET NULL,
    reason text,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT role_assignments_desk_scope_requires_id CHECK (
        scope_type <> 'desk' OR scope_id IS NOT NULL
    )
);

CREATE INDEX role_assignments_user_idx ON role_assignments (user_id);
CREATE INDEX role_assignments_role_idx ON role_assignments (role_key);
CREATE INDEX role_assignments_scope_idx ON role_assignments (scope_id);

CREATE TABLE delegations (
    id uuid PRIMARY KEY,
    from_user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    to_user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    capabilities jsonb NOT NULL DEFAULT '[]'::jsonb,
    starts_at timestamptz NOT NULL DEFAULT now(),
    ends_at timestamptz NOT NULL,
    reason text,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX delegations_from_idx ON delegations (from_user_id);
CREATE INDEX delegations_to_idx ON delegations (to_user_id);

-- --- Desk structure ----------------------------------------------------------

CREATE TABLE desks (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    slug text NOT NULL UNIQUE,
    description text,
    lead_user_id uuid REFERENCES users(id) ON DELETE SET NULL,
    parent_id uuid REFERENCES desks(id) ON DELETE SET NULL,
    settings jsonb NOT NULL DEFAULT '{}'::jsonb,
    is_archived boolean NOT NULL DEFAULT false,
    archived_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT desks_name_length CHECK (length(name) BETWEEN 1 AND 120),
    CONSTRAINT desks_slug_length CHECK (length(slug) BETWEEN 1 AND 120)
);

CREATE INDEX desks_slug_idx ON desks (slug);
CREATE INDEX desks_parent_idx ON desks (parent_id);
CREATE INDEX desks_archived_idx ON desks (is_archived);

CREATE TABLE desk_memberships (
    desk_id uuid NOT NULL REFERENCES desks(id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role text NOT NULL DEFAULT 'member',
    joined_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (desk_id, user_id)
);

CREATE INDEX desk_memberships_user_idx ON desk_memberships (user_id);

CREATE TABLE desk_slas (
    id uuid PRIMARY KEY,
    desk_id uuid NOT NULL REFERENCES desks(id) ON DELETE CASCADE,
    workflow_state text NOT NULL CHECK (workflow_state IN (
        'intake', 'proposed', 'assigned', 'reporting', 'drafting',
        'desk_review', 'verification', 'copy_standards', 'ready',
        'parked', 'archived'
    )),
    target_hours double precision NOT NULL CHECK (target_hours > 0),
    warn_at_percent integer NOT NULL DEFAULT 80 CHECK (warn_at_percent BETWEEN 1 AND 100),
    is_active boolean NOT NULL DEFAULT true,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_desk_sla_state UNIQUE (desk_id, workflow_state)
);

CREATE INDEX desk_slas_desk_idx ON desk_slas (desk_id);

CREATE TABLE desk_schedules (
    desk_id uuid PRIMARY KEY REFERENCES desks(id) ON DELETE CASCADE,
    timezone text NOT NULL DEFAULT 'UTC',
    hours jsonb NOT NULL DEFAULT '{}'::jsonb,
    on_call_user_id uuid REFERENCES users(id) ON DELETE SET NULL,
    notes text,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE desk_invitations (
    id uuid PRIMARY KEY,
    desk_id uuid NOT NULL REFERENCES desks(id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    invited_by_id uuid REFERENCES users(id) ON DELETE SET NULL,
    desk_role text NOT NULL DEFAULT 'member',
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'accepted', 'declined')),
    message text,
    responded_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX desk_invitations_desk_idx ON desk_invitations (desk_id);
CREATE INDEX desk_invitations_user_idx ON desk_invitations (user_id);
CREATE INDEX desk_invitations_status_idx ON desk_invitations (status);
-- At most one live invitation per (desk, user); resolved rows do not collide.
CREATE UNIQUE INDEX desk_invitations_pending_uq ON desk_invitations (desk_id, user_id)
    WHERE status = 'pending';

COMMENT ON TABLE audit_events IS 'Append-only audit trail shared across modules; desk mutations write here (gate #2).';
COMMENT ON TABLE permission_policies IS 'Per-user or per-desk capability overrides resolved ahead of role defaults.';
COMMENT ON TABLE role_definitions IS 'Named capability bundles; system rows mirror the legacy Role enum.';
COMMENT ON TABLE role_assignments IS 'Additive, optionally desk-scoped and time-boxed role grants.';
COMMENT ON TABLE delegations IS 'Time-bound lending of specific capabilities from one user to another.';
COMMENT ON TABLE desks IS 'Newsroom desks (the UI "workspace"); reporting hierarchy via parent_id, never seeded with demo data.';
COMMENT ON TABLE desk_slas IS 'Per-desk time target for one workflow state; a state with no row has no SLA and cannot breach.';
COMMENT ON TABLE desk_schedules IS 'Coverage windows and on-call for a desk; hours maps weekday to [open, close].';
COMMENT ON TABLE desk_invitations IS 'Desk membership invitations; accepting one creates a desk_memberships row.';
