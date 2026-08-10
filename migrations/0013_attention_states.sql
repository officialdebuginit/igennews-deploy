-- Module 8 (attention) — per-person response to a computed attention item.
-- Additive. The items themselves are computed from current state (never stored),
-- so the queue cannot go stale; only the acknowledgement/snooze response is
-- persisted here, keyed by the item's stable fingerprint and the user.
-- There is deliberately no permanent dismissal — snooze is time-bounded.

CREATE TABLE attention_states (
    fingerprint text NOT NULL,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    entity_type text,
    entity_id text,
    attention_type text,
    first_detected_at timestamptz NOT NULL DEFAULT now(),
    last_detected_at timestamptz NOT NULL DEFAULT now(),
    acknowledged_at timestamptz,
    snoozed_until timestamptz,
    escalated_at timestamptz,
    metadata_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    PRIMARY KEY (fingerprint, user_id)
);

CREATE INDEX attention_states_snoozed_idx ON attention_states (snoozed_until);

COMMENT ON TABLE attention_states IS 'Per-user acknowledgement/snooze of computed attention items; items are never stored, only the response.';
