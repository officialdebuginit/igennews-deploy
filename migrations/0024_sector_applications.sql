-- Sector applications (Phase 3 of the sectors plan).
--
-- Membership in a sector is granted, never self-served. Until now the only path in
-- was an admin/lead inviting you (desk_invitations) or granting you directly. This
-- adds the other direction: a user *applies* to a sector they can see in the
-- registry, attaches KYC documents, and an admin reviews and approves — at which
-- point a desk_membership row is created (the same row an invite would have made).
--
-- KYC documents are stored as a jsonb array of {label, url} references rather than
-- as blobs: the files live in the media object store (S3), and the application only
-- needs to point the reviewer at them.
--
-- A partial unique index allows exactly one *pending* application per (desk, user):
-- you can re-apply after a rejection, but you cannot stack duplicate open requests.

SET LOCAL search_path = meridian, public;

CREATE TABLE sector_applications (
    id uuid PRIMARY KEY,
    desk_id uuid NOT NULL REFERENCES desks(id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- The workspace role the applicant requests, and (on approval) is granted. Must
    -- satisfy the same set the desk_memberships role check enforces.
    role text NOT NULL DEFAULT 'reporter',
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected', 'withdrawn')),
    -- The applicant's cover note.
    message text,
    -- [{label, url}] — pointers to KYC documents the reviewer inspects.
    kyc_documents jsonb NOT NULL DEFAULT '[]'::jsonb,
    -- The reviewer's decision note (a rejection reason, an approval remark).
    decision_note text,
    reviewed_by uuid REFERENCES users(id) ON DELETE SET NULL,
    reviewed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- One open request per person per sector; history (approved/rejected) is retained.
CREATE UNIQUE INDEX sector_applications_one_pending
    ON sector_applications (desk_id, user_id) WHERE status = 'pending';
CREATE INDEX sector_applications_status_idx ON sector_applications (status);
CREATE INDEX sector_applications_user_idx ON sector_applications (user_id);

COMMENT ON TABLE sector_applications IS
  'User-initiated requests to join a sector (desk), with KYC document references; an admin approves to create the desk_membership.';
