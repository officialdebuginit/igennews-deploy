-- Party kinds and signing order for legal documents.
--
-- `party_kind` distinguishes a **signatory** (must sign for the document to
-- execute) from a **cc** viewer (can open and read the document but is never
-- asked to sign and never blocks execution).
--
-- `sign_order` is an optional 1-based rank. When set on the signatories, a party
-- may only sign once every signatory with a lower order has signed (sequential
-- signing). Left NULL, signing is order-independent (the existing behaviour).

SET LOCAL search_path = meridian, public;

ALTER TABLE legal_document_parties
    ADD COLUMN IF NOT EXISTS party_kind text NOT NULL DEFAULT 'signatory',
    ADD COLUMN IF NOT EXISTS sign_order int;

ALTER TABLE legal_document_parties
    DROP CONSTRAINT IF EXISTS legal_party_kind_check;
ALTER TABLE legal_document_parties
    ADD CONSTRAINT legal_party_kind_check CHECK (party_kind IN ('signatory', 'cc'));
