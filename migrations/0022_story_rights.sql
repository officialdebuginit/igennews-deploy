-- Per-story rights (RightsML/ODRL overrides).
--
-- Until now the only rights statement a syndicated item carried was the outlet
-- blanket: the `USAGE_TERMS`/`COPYRIGHT_NOTICE` constants baked into the ninjs
-- and NewsML-G2 serializers. That is correct for the common case (staff copy,
-- "© the outlet, licensed partners may reuse") but wrong for the cases that
-- actually matter to a rights desk: wire copy the outlet may not re-license, a
-- guest column whose author retains copyright, a piece published under Creative
-- Commons, an image-heavy explainer with a bespoke embargo note.
--
-- IPTC's model for this is `rightsInfo` — a per-item copyright holder, copyright
-- notice, and usage terms — with a machine-readable licence link (ODRL/RightsML
-- or a plain CC URL). We store the four fields a `rightsInfo` needs directly on
-- the story. Every column is nullable, and NULL means "fall back to the outlet
-- default" — so existing stories keep emitting exactly what they emitted before,
-- and a story only overrides the fields a desk deliberately sets.
--
-- These are editorial metadata, not a new lifecycle: they are patched like any
-- other story field and surfaced on export (ninjs `rightsinfo`, NewsML
-- `<rightsInfo>`) and on the public reader (schema.org copyrightHolder / license).

SET LOCAL search_path = meridian, public;

ALTER TABLE stories
    -- The copyright owner, when it is not the outlet (a syndication partner, a
    -- guest author who retains rights). NULL -> the outlet.
    ADD COLUMN rights_holder text,
    -- The "© …" line. NULL -> the outlet's standard notice.
    ADD COLUMN rights_notice text,
    -- Human-readable usage terms — the reuse conditions a licensing partner reads.
    -- NULL -> the outlet's standard usage terms.
    ADD COLUMN usage_terms text,
    -- A machine-readable licence link (a Creative Commons deed URL, or an ODRL /
    -- RightsML policy document). NULL -> no explicit licence, i.e. all-rights-reserved.
    ADD COLUMN license_url text;

COMMENT ON COLUMN stories.rights_holder IS
  'Per-story copyright holder override; NULL falls back to the outlet default at serialization time.';
COMMENT ON COLUMN stories.rights_notice IS
  'Per-story copyright notice override; NULL falls back to the outlet default.';
COMMENT ON COLUMN stories.usage_terms IS
  'Per-story usage terms (RightsML/ODRL human-readable); NULL falls back to the outlet default.';
COMMENT ON COLUMN stories.license_url IS
  'Per-story machine-readable licence URL (e.g. a Creative Commons deed); NULL means no explicit licence.';
