-- Legal documents: in-platform, multi-party signed agreements.
--
-- A legal document is markdown authored in the app, bound to a SHA-256 content
-- digest. Signing parties are **registered platform users only** (a foreign key
-- to users) — there is no external/email-only party. Each party signs in-app; the
-- platform executes an ML-DSA-65 signature over a canonical statement binding the
-- document id, title, content digest, party, role, and timestamp
-- (see crates/newsroom/src/legal_signing.rs). When every party has signed the
-- document becomes `executed`.
--
-- The signature + public key are stored so a downloaded copy can be re-verified
-- offline: recompute the digest from the body, re-check each party statement.

SET LOCAL search_path = meridian, public;

CREATE TABLE legal_documents (
    id             uuid PRIMARY KEY,
    title          text NOT NULL,
    body_md        text NOT NULL,
    content_sha256 text NOT NULL,
    status         text NOT NULL DEFAULT 'pending'
                        CHECK (status IN ('draft', 'pending', 'executed', 'void')),
    created_by     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE legal_document_parties (
    id          uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES legal_documents(id) ON DELETE CASCADE,
    -- Parties are registered platform users; no signing outside the platform.
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    party_role  text NOT NULL DEFAULT 'signatory',
    invited_at  timestamptz NOT NULL DEFAULT now(),
    signed_at   timestamptz,
    -- Base64 ML-DSA-65 signature over the canonical party statement, the platform
    -- public key it verifies against, and the algorithm id. NULL until signed.
    signature   text,
    public_key  text,
    algorithm   text,
    UNIQUE (document_id, user_id)
);

CREATE INDEX idx_legal_docs_creator ON legal_documents (created_by);
CREATE INDEX idx_legal_parties_doc ON legal_document_parties (document_id);
CREATE INDEX idx_legal_parties_user ON legal_document_parties (user_id);

COMMENT ON TABLE legal_documents IS
  'In-platform multi-party signed agreements; markdown body bound to a SHA-256 digest.';
COMMENT ON TABLE legal_document_parties IS
  'Registered-user signatories of a legal document, with their ML-DSA signature once signed.';
