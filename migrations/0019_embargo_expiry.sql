-- Embargo and expiry as first-class datetimes (03 Phase 1; 05 §3).
--
-- `releases.scheduled_at` already answers "when does this go out". It cannot
-- answer the two questions a newsroom actually has to answer separately:
--
--   embargo_until — we hold it. The story is *finished and released into the
--                   system*, and must not become public before this instant.
--                   Distinct from scheduling: a scheduled release has not
--                   happened yet, whereas an embargoed one may be published,
--                   distributed to partners, and still not showable.
--   expires_at    — we must take it down. Rights lapse, a court order lifts, an
--                   agency licence ends. WordPress has no native auto-unpublish
--                   and 03 §C names it as our opening; it only exists if the
--                   datetime is first-class rather than a note in a field.
--
-- Both live on `releases`, not `stories`, because they are properties of *this
-- publication of this version* — a corrective re-release can carry different
-- terms from the original, which is exactly the case a story-level column
-- cannot express.
--
-- Enforcement, so the columns are not decorative:
--   * `publication_state` on the story is what readers key off; the scheduled
--     pass flips a release to `expired` and the story back to `not_live` once
--     `expires_at` passes. The pass already runs for `scheduled_at`, so expiry
--     rides the same loop rather than needing a second worker.
--   * The publish gate refuses an expiry that is already past, and an embargo
--     that ends after the expiry — a window that never opens is a mistake, not
--     an instruction.
--
-- Both are nullable: most publishing has neither, and a NOT NULL default would
-- force every release to claim terms it does not have.

SET LOCAL search_path = meridian, public;

ALTER TABLE releases ADD COLUMN embargo_until timestamptz;
ALTER TABLE releases ADD COLUMN expires_at timestamptz;

-- A window that closes before it opens can never be satisfied; reject it at the
-- schema so no code path has to remember to.
ALTER TABLE releases ADD CONSTRAINT releases_embargo_before_expiry
    CHECK (embargo_until IS NULL OR expires_at IS NULL OR embargo_until < expires_at);

-- The scheduled pass sweeps for due expiries the same way it sweeps for due
-- schedules, so the index mirrors releases_scheduled_idx. Partial: only live
-- releases can expire, and they are the minority of rows over time.
CREATE INDEX releases_expiry_idx ON releases (expires_at)
    WHERE expires_at IS NOT NULL AND status = 'published';

CREATE INDEX releases_embargo_idx ON releases (embargo_until)
    WHERE embargo_until IS NOT NULL;

-- 'expired' joins the release lifecycle. There is no CHECK on releases.status in
-- 0009, so this is a comment rather than a constraint change — recorded here so
-- the vocabulary is discoverable from the migration history.
COMMENT ON COLUMN releases.embargo_until IS
  'Hold-until instant: published and distributed, but not showable to readers before this.';
COMMENT ON COLUMN releases.expires_at IS
  'Take-down instant: the scheduled pass unpublishes the story once this passes.';
