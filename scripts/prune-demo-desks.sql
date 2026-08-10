-- Prune the demo newsroom's sectors so the sector list is exactly the 50 India
-- Global Expo sectors from seed-sectors.sql.
--
-- Removes every desk that is NOT one of the 50 seeded IGE sectors (identified by
-- settings.source). Their memberships/applications cascade away; their stories are
-- de-sectored (stories.desk_id → NULL via the existing ON DELETE SET NULL) rather
-- than deleted, so no editorial content is lost — the stories simply drop to
-- org-level. Idempotent: re-running is a no-op once only the 50 remain.

SET search_path = meridian, public;
BEGIN;

DELETE FROM desks
WHERE settings->>'source' IS DISTINCT FROM 'India Global Expo — Master 50 Sectors';

COMMIT;
