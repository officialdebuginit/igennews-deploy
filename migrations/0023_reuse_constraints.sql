-- Structured reuse constraints (ODRL-aligned) — the machine-readable half of
-- per-story rights that gap #3.1 (0022) deliberately left out.
--
-- 0022 gave a story its own copyright holder / notice / usage-terms prose and a
-- licence URL. That covers "who owns it and under what licence", but not the two
-- constraints a syndication/rights desk actually enforces day to day:
--
--   * a *reuse embargo* — a partner may hold the item but not republish it before
--     a set instant (a co-published investigation, an under-embargo wire feed);
--   * a *territory* restriction — reuse permitted only in, or everywhere except,
--     a named set of countries (rights cleared for one market, a legal hold in
--     another).
--
-- These are ODRL constraints (RightsML is an ODRL profile): a temporal `dateTime`
-- constraint and a spatial constraint on the `reproduce` action. We store them as
-- discrete columns rather than a freeform policy blob so the editor can present
-- real controls and the serializer can build a valid ODRL policy deterministically.
--
-- All columns are optional / default to "unrestricted", so a story that sets none
-- emits exactly what it did before 0023 — no reuse policy at all.

SET LOCAL search_path = meridian, public;

ALTER TABLE stories
    -- Reuse (republication) not permitted before this instant. Distinct from the
    -- release-level publication embargo: this travels with the syndicated item and
    -- constrains the *partner's* reuse, not our own go-live. NULL -> no embargo.
    ADD COLUMN reuse_embargo_until timestamptz,
    -- How `territories` is read: 'any' ignores it (no restriction), 'allow' permits
    -- reuse only in the listed countries, 'deny' permits everywhere except them.
    ADD COLUMN territory_mode text NOT NULL DEFAULT 'any'
        CHECK (territory_mode IN ('any', 'allow', 'deny')),
    -- Comma-separated ISO 3166-1 alpha-2 country codes the mode applies to.
    -- NULL/blank with mode 'any' means unrestricted. Free text (not a lookup table)
    -- because the set is small, editor-entered, and validated at the UI edge.
    ADD COLUMN territories text;

COMMENT ON COLUMN stories.reuse_embargo_until IS
  'Per-story reuse embargo (ODRL dateTime constraint on reproduce); NULL means no reuse embargo. Distinct from release publication embargo.';
COMMENT ON COLUMN stories.territory_mode IS
  'How territories is applied: any (ignore), allow (only these), deny (all except these).';
COMMENT ON COLUMN stories.territories IS
  'Comma-separated ISO 3166-1 alpha-2 codes for the ODRL spatial constraint; used with territory_mode.';
