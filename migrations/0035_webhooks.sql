-- Outbound webhooks & the event fabric (see docs/MARKET-RESEARCH-AND-GAPS.md §6).
--
-- Three tables: subscription endpoints, a transactional outbox of domain events,
-- and per-endpoint delivery attempts. Domain mutations write the event to the
-- outbox in their own transaction (never lost, never leaked on rollback); a
-- background dispatcher fans out to matching endpoints and delivers with retries.

CREATE TABLE meridian.webhook_endpoints (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    url           text NOT NULL,
    -- `whsec_<base64>`; the HMAC key for Standard-Webhooks signing.
    secret        text NOT NULL,
    description   text NOT NULL DEFAULT '',
    -- Event-type patterns: exact (`article.published`), prefix (`article.*`),
    -- suffix (`*.published`), or all (`*`).
    event_types   text[] NOT NULL DEFAULT ARRAY['*']::text[],
    -- Optional desk-slug filter; NULL delivers events from every desk.
    desk_filter   text,
    active        boolean NOT NULL DEFAULT true,
    disabled_at   timestamptz,        -- set when auto-disabled after sustained failure
    failing_since timestamptz,        -- first failure of the current failure streak
    created_by_id uuid REFERENCES meridian.users(id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);

-- Transactional outbox: one row per emitted domain event.
CREATE TABLE meridian.webhook_events (
    id          text PRIMARY KEY,     -- `evt_<uuid>` — the delivered `webhook-id`
    event_type  text NOT NULL,        -- resource.action
    subject     text NOT NULL DEFAULT '',
    desk        text,                 -- desk slug for routing/filtering
    payload     jsonb NOT NULL,       -- the `data.object` snapshot
    actor_id    uuid,
    fanned_out  boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX webhook_events_pending ON meridian.webhook_events(created_at) WHERE NOT fanned_out;

CREATE TABLE meridian.webhook_deliveries (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    event_id        text NOT NULL REFERENCES meridian.webhook_events(id) ON DELETE CASCADE,
    endpoint_id     uuid NOT NULL REFERENCES meridian.webhook_endpoints(id) ON DELETE CASCADE,
    event_type      text NOT NULL,
    attempt         integer NOT NULL DEFAULT 0,
    status          text NOT NULL DEFAULT 'pending',
    status_code     integer,
    response        text,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    delivered_at    timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT webhook_deliveries_status_check
        CHECK (status IN ('pending', 'delivered', 'failed', 'exhausted'))
);
CREATE INDEX webhook_deliveries_due
    ON meridian.webhook_deliveries(next_attempt_at) WHERE status = 'pending';
