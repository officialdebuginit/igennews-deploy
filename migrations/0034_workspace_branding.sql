-- Per-workspace branding for the multi-tenant publishing platform.
--
-- Each desk (workspace) may carry its own brand — masthead name, logo, accent
-- colour, ink tint, typography preset. A single row with `desk_id = NULL` is the
-- org / platform default. Resolution at render time: desk brand → org default →
-- the built-in wire-service tokens.
--
-- Colours are stored as opaque CSS colour strings (e.g. "oklch(0.53 0.205 27)" or
-- "#c8102e") and injected at runtime as an override of the `--accent-brand` /
-- `--brand-ink` design tokens, so a brand never needs a rebuild.

SET LOCAL search_path = meridian, public;

CREATE TABLE IF NOT EXISTS workspace_branding (
    id            uuid PRIMARY KEY,
    desk_id       uuid REFERENCES desks(id) ON DELETE CASCADE,   -- NULL = org default
    brand_name    text,
    logo_asset_id uuid REFERENCES assets(id) ON DELETE SET NULL,
    logo_url      text,
    accent_color  text,        -- CSS colour → --accent-brand
    ink_color     text,        -- CSS colour → --brand-ink (masthead), optional
    font_preset   text,        -- data-font key (wire/broadsheet/…), optional
    updated_by    uuid REFERENCES users(id) ON DELETE SET NULL,
    updated_at    timestamptz NOT NULL DEFAULT now()
);

-- At most one brand row per desk.
CREATE UNIQUE INDEX IF NOT EXISTS workspace_branding_per_desk
    ON workspace_branding (desk_id) WHERE desk_id IS NOT NULL;

-- At most one org-default row (desk_id IS NULL).
CREATE UNIQUE INDEX IF NOT EXISTS workspace_branding_org_default
    ON workspace_branding ((true)) WHERE desk_id IS NULL;
