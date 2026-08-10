-- Advanced webhook controls: per-endpoint custom headers, sent with every delivery
-- (e.g. an Authorization token a receiver expects alongside the HMAC signature).
ALTER TABLE meridian.webhook_endpoints
    ADD COLUMN headers jsonb NOT NULL DEFAULT '{}'::jsonb;
