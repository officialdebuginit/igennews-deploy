-- Front-page curation (01 Area 2, `/org/frontpage`; 03 Phase 1).
--
-- The publishing layer can put a story live. It cannot say *where* on the front
-- page it goes, which is the audience/EIC decision that follows publication and
-- has had no representation at all — the homepage was implicitly "whatever the
-- story list returns", i.e. chronological, i.e. not curated.
--
-- The model is **fixed slots, assigned stories**, following Guardian Facia and
-- Arc PageBuilder: the page has a shape (a lead, some secondaries, a river), and
-- curation is deciding what occupies each position. The alternative — an ordered
-- list of stories — cannot express "the lead slot is empty", which is precisely
-- the state an editor needs to see.
--
-- `story_id` is nullable because an empty slot is a real, visible state, not a
-- missing row. ON DELETE SET NULL rather than CASCADE: deleting a story must
-- empty its slot, never silently delete the slot and reshape the page.
--
-- Only a *published* story may occupy a slot — enforced in the domain rather than
-- by a constraint, because `publication_state` lives on `stories` and a CHECK
-- cannot reach across the foreign key. The domain check is the real one; expiry
-- and unpublish clear slots through the same path.
--
-- Slots are rows, not an enum, so a newsroom can reshape its front page without a
-- migration. `position` orders them and is unique so two slots cannot claim the
-- same place.

SET LOCAL search_path = meridian, public;

CREATE TABLE front_page_slots (
    id uuid PRIMARY KEY,
    -- Ordering and identity of the position itself. Unique: a page cannot have
    -- two "position 1"s.
    position integer NOT NULL UNIQUE,
    label text NOT NULL,
    -- What is in the slot right now. NULL is an empty slot, which is a state the
    -- editor must be able to see rather than infer.
    story_id uuid REFERENCES stories(id) ON DELETE SET NULL,
    -- A pinned slot is held deliberately and should survive routine re-curation;
    -- it is advisory to the UI, not an access rule.
    is_pinned boolean NOT NULL DEFAULT false,
    updated_by_id uuid REFERENCES users(id) ON DELETE SET NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX front_page_slots_story_idx ON front_page_slots (story_id)
    WHERE story_id IS NOT NULL;

COMMENT ON TABLE front_page_slots IS
  'Fixed front-page positions and the story occupying each. An empty slot is a real state; only published stories may be placed.';

-- A default shape so the screen has something to curate on first run. Positions
-- and labels are editable; these are a starting page, not a fixed structure.
INSERT INTO front_page_slots (id, position, label) VALUES
    (gen_random_uuid(), 1, 'Lead'),
    (gen_random_uuid(), 2, 'Secondary'),
    (gen_random_uuid(), 3, 'Secondary'),
    (gen_random_uuid(), 4, 'River'),
    (gen_random_uuid(), 5, 'River'),
    (gen_random_uuid(), 6, 'River')
ON CONFLICT (position) DO NOTHING;
