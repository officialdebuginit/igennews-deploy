-- Two-level sector taxonomy: sub-sectors (industries) within a sector (desk).
-- Implements docs/SECTOR-TAXONOMY-AND-GOVERNANCE.md §2.3.
--
-- A sub_sector is a *classification within a sector*, NOT a child desk: the desk
-- stays the authorization boundary for every editorial action, and the industry is
-- data carried on the story. Modelling industries as their own table (rather than
-- child desks) is deliberate — membership does not inherit down the desk parent_id
-- tree, so a reporter who belongs to a sector can file under any of its industries
-- without a separate membership per industry.
--
-- Slugs are unique *per parent sector* (the same industry slug, e.g. `steel`, may
-- legitimately recur across sectors), so any industry URL must be sector-qualified
-- (e.g. /s/economy/i/steel), never a bare global /industry/steel.

SET LOCAL search_path = meridian, public;

CREATE TABLE sub_sectors (
    id          uuid PRIMARY KEY,
    desk_id     uuid NOT NULL REFERENCES desks(id) ON DELETE CASCADE,  -- parent Sector
    name        text NOT NULL,
    slug        text NOT NULL,
    description text,
    position    int  NOT NULL DEFAULT 0,             -- display order within the sector
    is_archived boolean NOT NULL DEFAULT false,
    settings    jsonb NOT NULL DEFAULT '{}'::jsonb,   -- optional per-industry policy override
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT sub_sectors_name_length CHECK (length(name) BETWEEN 1 AND 160),
    CONSTRAINT sub_sectors_slug_length CHECK (length(slug) BETWEEN 1 AND 160),
    UNIQUE (desk_id, slug),
    UNIQUE (desk_id, name)
);

CREATE INDEX sub_sectors_desk_idx     ON sub_sectors (desk_id);
CREATE INDEX sub_sectors_archived_idx ON sub_sectors (is_archived);

-- A story keeps its desk_id (the Sector / authz boundary, unchanged) and may also
-- record the industry it is filed under. Nullable + ON DELETE SET NULL keeps the
-- rollout non-breaking: existing rows, desk_id, category and all authz are untouched.
-- The invariant "the industry belongs to the story's sector" (sub_sector.desk_id =
-- story.desk_id) is enforced in the service layer (create_story / update_story).
ALTER TABLE stories
    ADD COLUMN sub_sector_id uuid REFERENCES sub_sectors(id) ON DELETE SET NULL;

CREATE INDEX stories_sub_sector_idx ON stories (sub_sector_id);
