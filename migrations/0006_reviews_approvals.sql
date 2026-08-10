-- Module 5 — reviews, approvals, and comments. Additive. A review decision
-- upserts the one approval per (story, version, kind); an approval is valid only
-- when its decision is approved or waived. Filing a new version invalidates the
-- prior version's approvals (handled in the domain layer). These rows are what
-- the story READY gate reads.

CREATE TABLE reviews (
    id uuid PRIMARY KEY,
    story_id uuid NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    version_id uuid NOT NULL REFERENCES story_versions(id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN (
        'desk', 'fact_check', 'copy', 'standards', 'legal', 'visuals', 'audience'
    )),
    assigned_to_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    requested_by_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    decision text NOT NULL DEFAULT 'pending' CHECK (decision IN (
        'pending', 'approved', 'changes_requested', 'rejected', 'waived'
    )),
    notes text,
    decided_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX reviews_story_idx ON reviews (story_id);
CREATE INDEX reviews_version_idx ON reviews (version_id);
CREATE INDEX reviews_kind_idx ON reviews (kind);
CREATE INDEX reviews_assigned_idx ON reviews (assigned_to_id);
CREATE INDEX reviews_decision_idx ON reviews (decision);

CREATE TABLE approvals (
    id uuid PRIMARY KEY,
    story_id uuid NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    version_id uuid NOT NULL REFERENCES story_versions(id) ON DELETE CASCADE,
    review_id uuid REFERENCES reviews(id) ON DELETE SET NULL,
    kind text NOT NULL CHECK (kind IN (
        'desk', 'fact_check', 'copy', 'standards', 'legal', 'visuals', 'audience'
    )),
    approver_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    decision text NOT NULL CHECK (decision IN (
        'pending', 'approved', 'changes_requested', 'rejected', 'waived'
    )),
    rationale text,
    is_valid boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    invalidated_at timestamptz,
    CONSTRAINT uq_version_approval UNIQUE (story_id, version_id, kind)
);

CREATE INDEX approvals_story_idx ON approvals (story_id);
CREATE INDEX approvals_version_idx ON approvals (version_id);
CREATE INDEX approvals_valid_idx ON approvals (is_valid);

CREATE TABLE comments (
    id uuid PRIMARY KEY,
    story_id uuid NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    version_id uuid REFERENCES story_versions(id) ON DELETE SET NULL,
    author_id uuid NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    body text NOT NULL,
    locator text,
    resolved boolean NOT NULL DEFAULT false,
    resolved_by_id uuid REFERENCES users(id) ON DELETE SET NULL,
    resolved_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX comments_story_idx ON comments (story_id);
CREATE INDEX comments_created_idx ON comments (created_at);

COMMENT ON TABLE reviews IS 'Review requests against a specific story version; a decision upserts the matching approval.';
COMMENT ON TABLE approvals IS 'One approval per (story, version, kind); valid only when approved or waived. The READY gate reads these.';
COMMENT ON TABLE comments IS 'Threaded story comments, optionally anchored to a version and a locator.';
