-- Module 3 (feature flags) — a named toggle with targeting and a kill switch.
-- Additive. emergency_disabled is deliberately separate from enabled and is
-- checked first: a kill switch a rollout or targeting rule could override is not
-- a kill switch. Target lists hold id strings, matching the legacy JSON columns.

CREATE TABLE feature_flags (
    key text PRIMARY KEY,
    description text,
    enabled boolean NOT NULL DEFAULT false,
    rollout_percent integer NOT NULL DEFAULT 0 CHECK (rollout_percent BETWEEN 0 AND 100),
    target_user_ids jsonb NOT NULL DEFAULT '[]'::jsonb,
    target_desk_ids jsonb NOT NULL DEFAULT '[]'::jsonb,
    emergency_disabled boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT feature_flags_key_length CHECK (length(key) BETWEEN 1 AND 80),
    CONSTRAINT feature_flags_targets_are_arrays CHECK (
        jsonb_typeof(target_user_ids) = 'array' AND jsonb_typeof(target_desk_ids) = 'array'
    )
);

COMMENT ON TABLE feature_flags IS 'Feature toggles with per-user/per-desk targeting, percentage rollout, and a first-checked emergency kill switch.';
