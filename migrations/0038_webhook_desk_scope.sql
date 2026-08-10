-- Sector-scoped webhook ownership. `desk_id` is the OWNING sector (NULL = org-level).
-- Admins (webhooks.manage) manage every endpoint; a desk lead (desks.manage on the
-- desk) manages that desk's endpoints. Distinct from `desk_filter`, which only filters
-- which events an endpoint hears.
ALTER TABLE meridian.webhook_endpoints
    ADD COLUMN desk_id uuid REFERENCES meridian.desks(id) ON DELETE CASCADE;
CREATE INDEX webhook_endpoints_desk ON meridian.webhook_endpoints(desk_id);
