-- Module 6 — tasks and coverage events (planning/calendar). Additive.
-- Also backfills the event_id foreign keys that Module 4 deferred: pitches and
-- stories referenced coverage_events before that table existed, so their
-- event_id was a plain uuid; now the constraint can be added.

CREATE TABLE coverage_events (
    id uuid PRIMARY KEY,
    title text NOT NULL,
    description text,
    desk_id uuid REFERENCES desks(id) ON DELETE SET NULL,
    owner_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    starts_at timestamptz,
    ends_at timestamptz,
    status text NOT NULL DEFAULT 'planned',
    location text,
    metadata_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX coverage_events_title_idx ON coverage_events (title);
CREATE INDEX coverage_events_desk_idx ON coverage_events (desk_id);
CREATE INDEX coverage_events_starts_idx ON coverage_events (starts_at);
CREATE INDEX coverage_events_status_idx ON coverage_events (status);

CREATE TABLE tasks (
    id uuid PRIMARY KEY,
    story_id uuid REFERENCES stories(id) ON DELETE CASCADE,
    desk_id uuid REFERENCES desks(id) ON DELETE CASCADE,
    pitch_id uuid REFERENCES pitches(id) ON DELETE CASCADE,
    event_id uuid REFERENCES coverage_events(id) ON DELETE CASCADE,
    title text NOT NULL,
    description text,
    status text NOT NULL DEFAULT 'todo' CHECK (status IN (
        'todo', 'in_progress', 'blocked', 'in_review', 'done', 'cancelled'
    )),
    priority text NOT NULL DEFAULT 'medium' CHECK (priority IN ('low', 'medium', 'high', 'urgent')),
    assigned_to_id uuid REFERENCES users(id) ON DELETE SET NULL,
    created_by_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    due_at timestamptz,
    completed_at timestamptz,
    blocker text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX tasks_story_idx ON tasks (story_id);
CREATE INDEX tasks_desk_idx ON tasks (desk_id);
CREATE INDEX tasks_title_idx ON tasks (title);
CREATE INDEX tasks_status_idx ON tasks (status);
CREATE INDEX tasks_assigned_idx ON tasks (assigned_to_id);
CREATE INDEX tasks_due_idx ON tasks (due_at);

-- Attach the coverage_events foreign key that Module 4 left as a bare uuid.
ALTER TABLE pitches
    ADD CONSTRAINT pitches_event_id_fkey
    FOREIGN KEY (event_id) REFERENCES coverage_events(id) ON DELETE SET NULL;
ALTER TABLE stories
    ADD CONSTRAINT stories_event_id_fkey
    FOREIGN KEY (event_id) REFERENCES coverage_events(id) ON DELETE SET NULL;

COMMENT ON TABLE coverage_events IS 'Planning/calendar events a desk covers; owns pitches, stories, and tasks by event_id.';
COMMENT ON TABLE tasks IS 'Assignable work items (kanban); completed_at is set when status becomes done.';
