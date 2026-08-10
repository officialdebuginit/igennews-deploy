-- Snapshot the exact party name each signature was made over.
--
-- A party's signed statement binds their display name at signing time, but the
-- name was previously reconstructed from the live `users.display_name` at verify
-- time. A later profile rename therefore changed the reconstructed statement and
-- made a genuine, fully-executed signature verify as invalid (and a signer with no
-- display name signed over their UUID while verify used an empty string). Persist
-- the exact name that was signed so verification is stable across renames.
--
-- NULL for parties that have not signed, and for signatures predating this column
-- (verification falls back to the live display name for those, unchanged).

SET LOCAL search_path = meridian, public;

ALTER TABLE legal_document_parties
    ADD COLUMN IF NOT EXISTS signed_name text;
