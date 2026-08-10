-- Module 7 — awareness: in-app notifications with preferences, grouping,
-- digest, and escalation, plus the social primitives (follows, favourites,
-- recents, saved searches) the composite navigation reads. Additive.
-- Search and the feed are separate, larger pieces and are not in this migration.

CREATE TABLE notifications (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind text NOT NULL,
    title text NOT NULL,
    body text,
    entity_type text,
    entity_id text,
    read_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    priority text NOT NULL DEFAULT 'normal' CHECK (priority IN ('low', 'normal', 'high', 'critical')),
    group_key text,
    count integer NOT NULL DEFAULT 1,
    deliver_after timestamptz,
    escalated_at timestamptz,
    escalated_to_id uuid REFERENCES users(id) ON DELETE SET NULL
);

CREATE INDEX notifications_user_idx ON notifications (user_id);
CREATE INDEX notifications_kind_idx ON notifications (kind);
CREATE INDEX notifications_unread_idx ON notifications (user_id, created_at DESC) WHERE read_at IS NULL;
CREATE INDEX notifications_group_idx ON notifications (group_key);
CREATE INDEX notifications_deliver_idx ON notifications (deliver_after);

CREATE TABLE notification_preferences (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind text NOT NULL,
    in_app boolean NOT NULL DEFAULT true,
    digest text NOT NULL DEFAULT 'immediate' CHECK (digest IN ('immediate', 'hourly', 'daily', 'off')),
    min_priority text NOT NULL DEFAULT 'low' CHECK (min_priority IN ('low', 'normal', 'high', 'critical')),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, kind)
);

CREATE TABLE follows (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    entity_type text NOT NULL,
    entity_id text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, entity_type, entity_id)
);

CREATE TABLE favorites (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    entity_type text NOT NULL,
    entity_id text NOT NULL,
    label text NOT NULL,
    position integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, entity_type, entity_id)
);

CREATE TABLE recent_items (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    entity_type text NOT NULL,
    entity_id text NOT NULL,
    title text NOT NULL,
    visited_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, entity_type, entity_id)
);

CREATE INDEX recent_items_user_visited_idx ON recent_items (user_id, visited_at DESC);

CREATE TABLE saved_searches (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name text NOT NULL,
    query text NOT NULL,
    filters_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    is_shared boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX saved_searches_user_idx ON saved_searches (user_id);

COMMENT ON TABLE notifications IS 'In-app notifications; group_key folds repeats, deliver_after holds digested rows, escalation lifts stale high/critical items.';
COMMENT ON TABLE notification_preferences IS 'Per-kind delivery preference; a missing row means use the default.';
COMMENT ON TABLE follows IS 'Interest in an entity ("tell me when this changes"), distinct from a favourite.';
COMMENT ON TABLE favorites IS 'Pinned shortcuts ("put this where I can reach it"), distinct from a follow.';
COMMENT ON TABLE recent_items IS 'The last things a person opened; trimmed to a per-user cap on write.';
COMMENT ON TABLE saved_searches IS 'A named, optionally shared search query with filters.';
