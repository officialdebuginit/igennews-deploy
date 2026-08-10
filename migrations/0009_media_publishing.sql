-- Module 9 — media and publishing: asset metadata (RustFS objects or external
-- URIs), release records, and corrections. Additive.
--
-- An asset is EITHER externally referenced (uri set, object_key null) OR stored
-- by us (object_key set). The scheduled-publish attempt/retry machinery is the
-- worker's concern and is not in this migration.

CREATE TABLE assets (
    id uuid PRIMARY KEY,
    story_id uuid REFERENCES stories(id) ON DELETE SET NULL,
    kind text NOT NULL,
    uri text NOT NULL,
    bucket text,
    object_key text,
    filename text,
    byte_size bigint,
    width integer,
    height integer,
    checksum text,
    creator text,
    source text,
    rights text,
    rights_expires_at timestamptz,
    credit text,
    caption text,
    alt_text text,
    verification_status text NOT NULL DEFAULT 'unverified' CHECK (verification_status IN (
        'unverified', 'in_progress', 'verified', 'disputed', 'rejected'
    )),
    c2pa_data jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_by_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX assets_story_idx ON assets (story_id);
CREATE INDEX assets_kind_idx ON assets (kind);
CREATE INDEX assets_object_key_idx ON assets (object_key);
CREATE INDEX assets_checksum_idx ON assets (checksum);

CREATE TABLE releases (
    id uuid PRIMARY KEY,
    story_id uuid NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    version_id uuid NOT NULL REFERENCES story_versions(id) ON DELETE RESTRICT,
    release_type text NOT NULL DEFAULT 'publish',
    status text NOT NULL DEFAULT 'draft' CHECK (status IN (
        'draft', 'scheduled', 'publishing', 'published',
        'partial_failure', 'failed', 'cancelled', 'withdrawn'
    )),
    channels jsonb NOT NULL DEFAULT '[]'::jsonb,
    receipts jsonb NOT NULL DEFAULT '[]'::jsonb,
    scheduled_at timestamptz,
    published_at timestamptz,
    correction_of_id uuid REFERENCES releases(id) ON DELETE SET NULL,
    created_by_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX releases_story_idx ON releases (story_id);
CREATE INDEX releases_version_idx ON releases (version_id);
CREATE INDEX releases_status_idx ON releases (status);
CREATE INDEX releases_scheduled_idx ON releases (scheduled_at);
CREATE INDEX releases_published_idx ON releases (published_at);

CREATE TABLE corrections (
    id uuid PRIMARY KEY,
    story_id uuid NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    release_id uuid NOT NULL REFERENCES releases(id) ON DELETE RESTRICT,
    reported_by_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    owner_id uuid REFERENCES users(id) ON DELETE SET NULL,
    classification text NOT NULL CHECK (classification IN (
        'presentation_fix', 'clarification', 'correction', 'retraction', 'legal_privacy_removal'
    )),
    status text NOT NULL DEFAULT 'open',
    description text NOT NULL,
    public_note text,
    resolved_release_id uuid REFERENCES releases(id) ON DELETE SET NULL,
    resolved_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX corrections_story_idx ON corrections (story_id);
CREATE INDEX corrections_release_idx ON corrections (release_id);
CREATE INDEX corrections_owner_idx ON corrections (owner_id);
CREATE INDEX corrections_status_idx ON corrections (status);

COMMENT ON TABLE assets IS 'Media asset metadata; object_key set for RustFS-stored bytes, uri only for external references.';
COMMENT ON TABLE releases IS 'Publish records against an immutable story version; correction_of_id links a re-publish to its origin.';
COMMENT ON TABLE corrections IS 'Post-publication corrections with an editorial classification and resolution trail.';
