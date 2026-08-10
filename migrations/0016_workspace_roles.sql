-- Phase 0: make workspace membership a first-class role carrier.
--
-- Until now `desk_memberships.role` was free text defaulting to 'member', because
-- membership was an organizational grouping and never an authorization input. The
-- resolver now reads it as the role that decides capability inside a sector, so
-- the column has to carry one of the eight workspace roles.
--
-- Two backfills protect an already-populated newsroom:
--   1. any role outside the workspace set becomes 'reporter' — the permissive
--      choice, preserving today's access rather than locking people out; an admin
--      review pass narrows it afterwards.
--   2. every desk lead gets a 'section_editor' membership in the desk they lead.
--      `require_desk_admin` short-circuits on lead-ness, so leads keep desk
--      administration either way — but without this they would lose the editorial
--      capabilities (stories.edit_any, workflow.advance, releases.publish) inside
--      their own desk, which is resolved through membership.

SET LOCAL search_path = meridian, public;

-- 1. Normalise existing roles.
UPDATE desk_memberships
   SET role = 'reporter'
 WHERE role NOT IN (
    'section_editor', 'managing_editor', 'reporter', 'copy_editor',
    'fact_checker', 'producer', 'contributor', 'viewer'
 );

-- 2. Desk leads hold a section_editor membership in the desk they lead.
INSERT INTO desk_memberships (desk_id, user_id, role)
SELECT d.id, d.lead_user_id, 'section_editor'
  FROM desks d
 WHERE d.lead_user_id IS NOT NULL
ON CONFLICT (desk_id, user_id) DO UPDATE SET role = 'section_editor';

-- 3. Constrain the column, and change the default away from the now-invalid
--    'member'. A new membership with no explicit role is a viewer: read-only is
--    the safe default for a grant whose intent was not stated.
ALTER TABLE desk_memberships ALTER COLUMN role SET DEFAULT 'viewer';

-- Guarded so re-running the migration is a clean no-op, matching the project's
-- "re-run is a clean no-op" contract for the ledger.
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conname = 'desk_memberships_role_check'
       AND conrelid = 'meridian.desk_memberships'::regclass
  ) THEN
    ALTER TABLE desk_memberships
      ADD CONSTRAINT desk_memberships_role_check CHECK (role IN (
        'section_editor', 'managing_editor', 'reporter', 'copy_editor',
        'fact_checker', 'producer', 'contributor', 'viewer'
      ));
  END IF;
END $$;

COMMENT ON COLUMN desk_memberships.role IS
  'Workspace role, the capability carrier inside this sector. Distinct from users.role, which is the org-wide role.';

-- 4. Role definitions record whether they are org-wide or per-workspace, so the
--    capability-matrix admin can present the two tiers separately. Values are
--    written by `sync_system_roles` from the capability registry.
ALTER TABLE role_definitions
  ADD COLUMN IF NOT EXISTS scope text NOT NULL DEFAULT 'workspace'
  CHECK (scope IN ('global', 'workspace'));

COMMENT ON COLUMN role_definitions.scope IS
  'global = carried org-wide and overlaying every sector; workspace = held per desk membership.';
