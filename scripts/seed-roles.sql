-- =====================================================================
-- iGEN News — system role definitions.
--
-- `role_definitions` is normally projected by the application at boot from the
-- capability registry in `crates/newsroom/src/capabilities.rs` (see
-- `NewsroomService::sync_system_roles`). Neither content seed writes it, so a
-- genuinely empty database used to fail on `seed-igennews.sql`'s
-- `role_assignments` insert until the app had been started once.
--
-- This file is that projection, captured from the application's own output, so
-- the seeds are self-sufficient. It is a BOOTSTRAP, not a source of truth: the
-- app re-projects on every boot using the same ON CONFLICT semantics written
-- here, so a stale copy repairs itself rather than overriding the registry.
--
-- Idempotent. Run BEFORE seed-igennews.sql.
--   psql "$DATABASE_DIRECT_URL" -v ON_ERROR_STOP=1 -f scripts/seed-roles.sql
-- =====================================================================
BEGIN;
SET search_path = meridian, public;

INSERT INTO role_definitions (id, key, name, description, is_system, capabilities, scope)
VALUES
  (gen_random_uuid(), 'admin', 'Admin', 'Built-in Admin role.', true,
   '["flags.manage","ingest.poll","people.view_contact","permissions.manage","roles.manage","subscriptions.manage","webhooks.manage","attention.assign","attention.escalate","dashboard.customize","dashboard.share_view","dashboard.view","dashboard.view_audience","dashboard.view_desk","dashboard.view_global","dashboard.view_system_health","dashboard.view_workload","desks.invite","desks.manage","sectors.admin","claims.decide","comments.moderate","feed.moderate","pitches.decide","releases.publish","reviews.approve","reviews.decide_any","reviews.view_all","sources.approve","stories.delete","stories.delete_any","stories.edit_any","tasks.assign","tasks.edit_any","tasks.view_all","workflow.advance","workflow.fast_path","frontpage.curate"]'::jsonb, 'global'),
  (gen_random_uuid(), 'audience_editor', 'Audience Editor', 'Built-in Audience Editor role.', true,
   '["dashboard.customize","dashboard.view","dashboard.view_audience","frontpage.curate"]'::jsonb, 'global'),
  (gen_random_uuid(), 'contributor', 'Contributor', 'Built-in Contributor role.', true,
   '["dashboard.customize","dashboard.view"]'::jsonb, 'workspace'),
  (gen_random_uuid(), 'copy_editor', 'Copy Editor', 'Built-in Copy Editor role.', true,
   '["dashboard.customize","dashboard.view","claims.decide","stories.edit"]'::jsonb, 'workspace'),
  (gen_random_uuid(), 'editor_in_chief', 'Editor In Chief', 'Built-in Editor In Chief role.', true,
   '["people.view_contact","attention.assign","attention.escalate","dashboard.customize","dashboard.share_view","dashboard.view","dashboard.view_audience","dashboard.view_desk","dashboard.view_global","dashboard.view_workload","desks.invite","desks.manage","claims.decide","comments.moderate","feed.moderate","pitches.decide","releases.publish","reviews.approve","reviews.decide_any","reviews.view_all","sources.approve","stories.create","stories.delete","stories.delete_any","stories.edit","stories.edit_any","tasks.assign","tasks.edit_any","tasks.view_all","workflow.advance","workflow.fast_path","frontpage.curate"]'::jsonb, 'global'),
  (gen_random_uuid(), 'fact_checker', 'Fact Checker', 'Built-in Fact Checker role.', true,
   '["dashboard.customize","dashboard.view","claims.decide"]'::jsonb, 'workspace'),
  (gen_random_uuid(), 'managing_editor', 'Managing Editor', 'Built-in Managing Editor role.', true,
   '["people.view_contact","attention.assign","attention.escalate","dashboard.customize","dashboard.share_view","dashboard.view","dashboard.view_audience","dashboard.view_desk","dashboard.view_global","dashboard.view_workload","desks.invite","desks.manage","claims.decide","comments.moderate","feed.moderate","pitches.decide","releases.publish","reviews.approve","reviews.decide_any","reviews.view_all","sources.approve","stories.create","stories.delete","stories.delete_any","stories.edit","stories.edit_any","tasks.assign","tasks.edit_any","tasks.view_all","workflow.advance","workflow.fast_path"]'::jsonb, 'workspace'),
  (gen_random_uuid(), 'producer', 'Producer', 'Built-in Producer role.', true,
   '["dashboard.customize","dashboard.view","dashboard.view_audience","releases.publish","reviews.approve","stories.create","stories.edit","tasks.assign"]'::jsonb, 'workspace'),
  (gen_random_uuid(), 'reporter', 'Reporter', 'Built-in Reporter role.', true,
   '["dashboard.customize","dashboard.view","stories.create","stories.edit"]'::jsonb, 'workspace'),
  (gen_random_uuid(), 'section_editor', 'Section Editor', 'Built-in Section Editor role.', true,
   '["people.view_contact","attention.assign","attention.escalate","dashboard.customize","dashboard.share_view","dashboard.view","dashboard.view_audience","dashboard.view_desk","dashboard.view_workload","desks.invite","desks.manage","claims.decide","comments.moderate","feed.moderate","pitches.decide","releases.publish","reviews.approve","reviews.decide_any","reviews.view_all","sources.approve","stories.create","stories.edit","stories.edit_any","tasks.assign","tasks.edit_any","tasks.view_all","workflow.advance","workflow.fast_path"]'::jsonb, 'workspace'),
  (gen_random_uuid(), 'standards_legal', 'Standards Legal', 'Built-in Standards Legal role.', true,
   '["people.view_contact","attention.assign","attention.escalate","dashboard.customize","dashboard.share_view","dashboard.view","dashboard.view_audience","dashboard.view_desk","dashboard.view_global","dashboard.view_workload","desks.invite","desks.manage","claims.decide","comments.moderate","feed.moderate","pitches.decide","releases.publish","reviews.approve","reviews.decide_any","reviews.view_all","sources.approve","stories.edit_any","tasks.assign","tasks.edit_any","tasks.view_all","workflow.advance","workflow.fast_path"]'::jsonb, 'global'),
  (gen_random_uuid(), 'viewer', 'Viewer', 'Built-in Viewer role.', true,
   '["dashboard.customize","dashboard.view"]'::jsonb, 'workspace')
ON CONFLICT (key) DO UPDATE SET
  capabilities = EXCLUDED.capabilities,
  scope        = EXCLUDED.scope,
  updated_at   = now()
WHERE role_definitions.is_system
  AND (role_definitions.capabilities <> EXCLUDED.capabilities
       OR role_definitions.scope <> EXCLUDED.scope);

COMMIT;
