-- Reader subscriptions / entitlements — the reader-revenue foundation.
--
-- This is the entitlement record, decoupled from any payment provider: a manual
-- ("comp") grant, or a row a future Stripe integration would upsert from Stripe's own
-- webhooks (`external_ref` holds the provider id). Subscription lifecycle changes emit
-- `subscription.started` / `subscription.canceled` on the outbound webhook fabric.

CREATE TABLE meridian.subscriptions (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    subscriber_email   text NOT NULL,
    subscriber_name    text,
    plan               text NOT NULL DEFAULT 'standard',
    status             text NOT NULL DEFAULT 'active',
    source             text NOT NULL DEFAULT 'manual',   -- manual | comp | stripe
    external_ref       text,                             -- provider subscription id
    started_at         timestamptz NOT NULL DEFAULT now(),
    current_period_end timestamptz,
    canceled_at        timestamptz,
    created_by_id      uuid REFERENCES meridian.users(id) ON DELETE SET NULL,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT subscriptions_status_check
        CHECK (status IN ('active', 'trialing', 'canceled', 'past_due'))
);

-- One live subscription per email (case-insensitive); canceled/past_due don't count.
CREATE UNIQUE INDEX subscriptions_one_active_per_email
    ON meridian.subscriptions (lower(subscriber_email))
    WHERE status IN ('active', 'trialing');
