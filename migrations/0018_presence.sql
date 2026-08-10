-- Presence (gap 03 §A.3 E5).
--
-- The SSE hub tells a browser that *something* changed. It cannot say who else is
-- looking at the story you are editing, which is the question presence answers and
-- the reason two people silently overwrite each other's work.
--
-- **The decision this table encodes**, stated because nothing constrained it:
-- presence here means **"has this entity open right now"** — not "is typing", not
-- "is editing". Viewing is the weakest claim of the three and the only one this
-- data can actually support: a heartbeat proves a page is open, nothing more.
-- Claiming "editing" from the same evidence would be the kind of inference this
-- project's data policy exists to prevent. Field-level editing presence is a
-- larger feature (03 §A.7, lockless co-editing) and is deliberately not this.
--
-- **Why a table rather than in-process state.** The `RealtimeHub` broadcast
-- channel is per-process, so an in-memory registry would silently show a partial
-- newsroom the moment a second instance runs — and be *correct in dev, wrong in
-- production*, the worst failure shape. A row is visible to every instance.
--
-- **Why no expiry job.** Liveness is decided by the read (`last_seen_at > now() -
-- window`), not by deleting rows, so a crashed browser ages out on its own with no
-- sweeper to get stuck. The table is naturally bounded: the primary key is one row
-- per (entity, person), so it grows with distinct pairs, not with heartbeats — a
-- person re-reading the same story forever writes the same row.

SET LOCAL search_path = meridian, public;

CREATE TABLE presence (
    entity_type text NOT NULL,
    entity_id text NOT NULL,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (entity_type, entity_id, user_id)
);

-- The read is always "who is on this entity, recently", so the index leads with
-- the entity and carries the recency it filters on.
CREATE INDEX presence_entity_idx ON presence (entity_type, entity_id, last_seen_at DESC);

COMMENT ON TABLE presence IS
  'Who currently has an entity open. Liveness is decided by last_seen_at against a read-time window, never by deletion.';
