-- Allow the 'expired' release status.
--
-- `expire_due_releases` sets releases.status = 'expired' when a release's
-- `expires_at` passes. Migration 0009 defined `releases_status_check` WITHOUT
-- 'expired', and 0019 (which introduced expiry) recorded "'expired' joins the
-- release lifecycle" but skipped the CHECK update on the mistaken belief that no
-- CHECK existed on releases.status. Any expiry therefore failed with a
-- check-constraint violation — the auto-takedown never actually worked. Surfaced
-- 2026-08-09 by the first live test to exercise expiry. Add 'expired' to the set.

SET LOCAL search_path = meridian, public;

ALTER TABLE releases DROP CONSTRAINT IF EXISTS releases_status_check;
ALTER TABLE releases ADD CONSTRAINT releases_status_check
    CHECK (status IN (
        'draft', 'scheduled', 'publishing', 'published',
        'partial_failure', 'failed', 'cancelled', 'withdrawn', 'expired'
    ));
