-- Per-channel scheduling: a map of channel key -> RFC-3339 instant. A channel
-- absent from the map goes out at the release's own `scheduled_at`; a channel
-- present goes out at its own time. An empty map ('{}') is the default and
-- reproduces the old atomic all-channels-at-once behaviour exactly.
ALTER TABLE meridian.releases
    ADD COLUMN IF NOT EXISTS channel_schedule jsonb NOT NULL DEFAULT '{}'::jsonb;
