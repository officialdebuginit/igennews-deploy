-- Distribution channels (01 Area 5, `/org/publishing/channels`).
--
-- `releases.channels` is a free jsonb array. Every channel name in the system is
-- therefore whatever a caller happened to type: nothing enumerates the newsroom's
-- channels, nothing validates a target, and "newsletter" and "Newsletter" are two
-- different destinations. That is also why publish receipts could only ever be
-- synthesised — there was no channel *record* to hold a delivery configuration.
--
-- This is the registry. `releases.channels` stays as it is: it is the historical
-- record of what a given release targeted, and rewriting it would rewrite the past
-- for releases whose targets have since been renamed or retired.
--
-- `kind` is constrained because it decides how delivery would work, and an
-- unknown kind is not deliverable by anything. `config` is deliberately open —
-- an ESP list id and a webhook signing secret have nothing in common, and forcing
-- them into shared columns would model neither.
--
-- No credentials belong in `config`. There is no secrets manager wired up yet
-- (MIGRATION-STATUS Module 1, deferred), so a channel records *where* to deliver
-- and the operator supplies *how* to authenticate out of band. Storing a token
-- here would put it in every pg_dump and every audit `after` payload.

SET LOCAL search_path = meridian, public;

CREATE TABLE channels (
    id uuid PRIMARY KEY,
    -- Stable machine name; this is what a release records in its jsonb array, so
    -- it must not drift once anything has targeted it.
    key text NOT NULL UNIQUE,
    name text NOT NULL,
    kind text NOT NULL CHECK (kind IN ('web', 'app_push', 'newsletter', 'social', 'syndication')),
    description text,
    -- A retired channel keeps its rows and its history; it simply stops being
    -- offered as a target. Deleting would orphan the receipts that reference it.
    is_active boolean NOT NULL DEFAULT true,
    -- Delivery destination, shape depending on `kind`. Never credentials.
    config jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX channels_active_idx ON channels (key) WHERE is_active;

COMMENT ON TABLE channels IS
  'The newsroom''s distribution channels. releases.channels records what a release targeted, by key; this table says what those keys mean.';
COMMENT ON COLUMN channels.config IS
  'Delivery destination for this channel kind. Never credentials — no secrets manager is wired up, so tokens are supplied out of band.';

-- Seed the four the publishing console has been offering as hardcoded strings, so
-- existing releases' recorded targets resolve to real rows rather than dangling.
INSERT INTO channels (id, key, name, kind, description) VALUES
    (gen_random_uuid(), 'web',         'Website',     'web',         'The public site'),
    (gen_random_uuid(), 'app',         'Mobile app',  'app_push',    'Push to the mobile app'),
    (gen_random_uuid(), 'newsletter',  'Newsletter',  'newsletter',  'Email editions'),
    (gen_random_uuid(), 'syndication', 'Syndication', 'syndication', 'Wire and partner feeds')
ON CONFLICT (key) DO NOTHING;
