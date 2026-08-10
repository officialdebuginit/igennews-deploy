-- Module 8 (views) — saved dashboard views: a named layout + filters, private
-- to its owner or shared to a desk. Additive.

CREATE TABLE dashboard_views (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    desk_id uuid REFERENCES desks(id) ON DELETE SET NULL,
    name text NOT NULL,
    description text,
    is_default boolean NOT NULL DEFAULT false,
    is_shared boolean NOT NULL DEFAULT false,
    layout_json jsonb NOT NULL DEFAULT '[]'::jsonb,
    filters_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX dashboard_views_user_idx ON dashboard_views (user_id);
CREATE INDEX dashboard_views_desk_idx ON dashboard_views (desk_id);

COMMENT ON TABLE dashboard_views IS 'Saved dashboard views (layout + filters); private to owner or shared to a desk.';
