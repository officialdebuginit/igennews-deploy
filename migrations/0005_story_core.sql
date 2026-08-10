-- Module 4 — story core: pitches, stories, immutable versions, sources,
-- evidence, claims, and the workflow-state history that the SLA and pipeline
-- analytics read. Additive only; uuid ids, jsonb blobs, timestamptz, matching
-- the earlier modules.
--
-- event_id references coverage_events (Module 6) which does not exist yet, so it
-- is stored as a plain nullable uuid here; the foreign key is added with Module
-- 6. Enum domains are expressed as CHECK constraints, transcribed from the
-- legacy StrEnum values.

CREATE TABLE pitches (
    id uuid PRIMARY KEY,
    headline text NOT NULL,
    summary text NOT NULL,
    angle text,
    desk_id uuid REFERENCES desks(id) ON DELETE SET NULL,
    event_id uuid,
    created_by_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    assignee_id uuid REFERENCES users(id) ON DELETE SET NULL,
    editor_id uuid REFERENCES users(id) ON DELETE SET NULL,
    status text NOT NULL DEFAULT 'proposed' CHECK (status IN (
        'proposed', 'needs_detail', 'commissioned', 'parked', 'declined', 'merged'
    )),
    priority text NOT NULL DEFAULT 'medium' CHECK (priority IN ('low', 'medium', 'high', 'urgent')),
    target_audience text,
    expected_format text,
    key_questions jsonb NOT NULL DEFAULT '[]'::jsonb,
    likely_sources jsonb NOT NULL DEFAULT '[]'::jsonb,
    risks jsonb NOT NULL DEFAULT '[]'::jsonb,
    target_at timestamptz,
    decision_note text,
    decided_by_id uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX pitches_headline_idx ON pitches (headline);
CREATE INDEX pitches_desk_idx ON pitches (desk_id);
CREATE INDEX pitches_status_idx ON pitches (status);
CREATE INDEX pitches_target_idx ON pitches (target_at);

CREATE TABLE stories (
    id uuid PRIMARY KEY,
    slug text NOT NULL UNIQUE,
    title text NOT NULL DEFAULT 'Untitled Story',
    dek text NOT NULL DEFAULT '',
    body jsonb NOT NULL DEFAULT '[]'::jsonb,
    category text NOT NULL DEFAULT 'General',
    tags jsonb NOT NULL DEFAULT '[]'::jsonb,
    story_type text NOT NULL DEFAULT 'article',
    workflow_state text NOT NULL DEFAULT 'assigned' CHECK (workflow_state IN (
        'intake', 'proposed', 'assigned', 'reporting', 'drafting',
        'desk_review', 'verification', 'copy_standards', 'ready',
        'parked', 'archived'
    )),
    publication_state text NOT NULL DEFAULT 'not_live' CHECK (publication_state IN (
        'not_live', 'scheduled', 'published', 'partially_published',
        'updating', 'withdrawn', 'archived'
    )),
    validity_state text NOT NULL DEFAULT 'current' CHECK (validity_state IN (
        'current', 'under_review', 'corrected', 'superseded', 'disputed', 'retracted'
    )),
    priority text NOT NULL DEFAULT 'medium' CHECK (priority IN ('low', 'medium', 'high', 'urgent')),
    desk_id uuid REFERENCES desks(id) ON DELETE SET NULL,
    event_id uuid,
    pitch_id uuid UNIQUE REFERENCES pitches(id) ON DELETE SET NULL,
    author_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    editor_id uuid REFERENCES users(id) ON DELETE SET NULL,
    fact_checker_id uuid REFERENCES users(id) ON DELETE SET NULL,
    copy_editor_id uuid REFERENCES users(id) ON DELETE SET NULL,
    due_at timestamptz,
    scheduled_at timestamptz,
    published_at timestamptz,
    version_number integer NOT NULL DEFAULT 0,
    fast_path boolean NOT NULL DEFAULT false,
    fast_path_reason text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX stories_slug_idx ON stories (slug);
CREATE INDEX stories_title_idx ON stories (title);
CREATE INDEX stories_category_idx ON stories (category);
CREATE INDEX stories_workflow_idx ON stories (workflow_state);
CREATE INDEX stories_publication_idx ON stories (publication_state);
CREATE INDEX stories_validity_idx ON stories (validity_state);
CREATE INDEX stories_priority_idx ON stories (priority);
CREATE INDEX stories_desk_idx ON stories (desk_id);
CREATE INDEX stories_author_idx ON stories (author_id);
CREATE INDEX stories_editor_idx ON stories (editor_id);
CREATE INDEX stories_due_idx ON stories (due_at);
CREATE INDEX stories_scheduled_idx ON stories (scheduled_at);
CREATE INDEX stories_published_idx ON stories (published_at);

-- Immutable snapshots: rows are appended, never updated. One number per story.
CREATE TABLE story_versions (
    id uuid PRIMARY KEY,
    story_id uuid NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    number integer NOT NULL,
    title text NOT NULL,
    dek text NOT NULL,
    body jsonb NOT NULL DEFAULT '[]'::jsonb,
    category text NOT NULL,
    tags jsonb NOT NULL DEFAULT '[]'::jsonb,
    change_summary text,
    created_by_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    filed boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_story_version UNIQUE (story_id, number)
);

CREATE INDEX story_versions_story_idx ON story_versions (story_id);
CREATE INDEX story_versions_created_idx ON story_versions (created_at);

CREATE TABLE sources (
    id uuid PRIMARY KEY,
    story_id uuid NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    identity text NOT NULL,
    publishable_attribution text NOT NULL,
    ground_rule text NOT NULL DEFAULT 'on_record' CHECK (ground_rule IN (
        'on_record', 'background', 'deep_background', 'off_record'
    )),
    description text,
    motive text,
    reliability_notes text,
    access_level text NOT NULL DEFAULT 'story_team',
    manager_approved_by_id uuid REFERENCES users(id) ON DELETE SET NULL,
    approval_rationale text,
    created_by_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX sources_story_idx ON sources (story_id);

CREATE TABLE source_access_grants (
    source_id uuid NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    granted_by_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    granted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, user_id)
);

CREATE TABLE evidence (
    id uuid PRIMARY KEY,
    story_id uuid NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    source_id uuid REFERENCES sources(id) ON DELETE SET NULL,
    kind text NOT NULL,
    title text NOT NULL,
    uri text,
    notes text,
    checksum text,
    acquired_at timestamptz,
    verification_status text NOT NULL DEFAULT 'unverified' CHECK (verification_status IN (
        'unverified', 'in_progress', 'verified', 'disputed', 'rejected'
    )),
    chain_of_custody jsonb NOT NULL DEFAULT '[]'::jsonb,
    created_by_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX evidence_story_idx ON evidence (story_id);
CREATE INDEX evidence_kind_idx ON evidence (kind);

CREATE TABLE claims (
    id uuid PRIMARY KEY,
    story_id uuid NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    version_id uuid REFERENCES story_versions(id) ON DELETE SET NULL,
    text text NOT NULL,
    locator text,
    status text NOT NULL DEFAULT 'unverified' CHECK (status IN (
        'unverified', 'in_progress', 'verified', 'disputed', 'rejected'
    )),
    evidence_ids jsonb NOT NULL DEFAULT '[]'::jsonb,
    reviewed_by_id uuid REFERENCES users(id) ON DELETE SET NULL,
    decision_note text,
    reviewed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX claims_story_idx ON claims (story_id);
CREATE INDEX claims_status_idx ON claims (status);

-- One open row (exited_at IS NULL) per story marks the current dwell; closing it
-- and opening the next is how time-in-state is measured for SLAs.
CREATE TABLE workflow_state_history (
    id uuid PRIMARY KEY,
    story_id uuid NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    version_id uuid REFERENCES story_versions(id) ON DELETE SET NULL,
    from_state text NOT NULL,
    to_state text NOT NULL,
    actor_id uuid REFERENCES users(id) ON DELETE SET NULL,
    entered_at timestamptz NOT NULL DEFAULT now(),
    exited_at timestamptz,
    reason text,
    fast_path boolean NOT NULL DEFAULT false
);

CREATE INDEX workflow_state_history_story_idx ON workflow_state_history (story_id);
CREATE INDEX workflow_state_history_to_idx ON workflow_state_history (to_state);
CREATE INDEX workflow_state_history_entered_idx ON workflow_state_history (entered_at);
-- Enforces at most one open dwell row per story.
CREATE UNIQUE INDEX workflow_state_history_open_uq ON workflow_state_history (story_id)
    WHERE exited_at IS NULL;

COMMENT ON TABLE pitches IS 'Story pitches; a commissioned pitch becomes a story (one-to-one via stories.pitch_id).';
COMMENT ON TABLE stories IS 'Editorial stories; body is an ordered block list. Never seeded with demo data.';
COMMENT ON TABLE story_versions IS 'Immutable, append-only story snapshots; one number per story.';
COMMENT ON TABLE sources IS 'Story sources with ground-rule attribution and manager-approval trail.';
COMMENT ON TABLE evidence IS 'Evidence items with verification status and chain of custody.';
COMMENT ON TABLE claims IS 'Verifiable claims linked to evidence and an optional version.';
COMMENT ON TABLE workflow_state_history IS 'Open/closed dwell rows per story; SLA and pipeline analytics read time-in-state from here.';
