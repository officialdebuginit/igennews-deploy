-- Decline-to-sign for legal-document parties.
--
-- A party may refuse to sign, with a reason. Declining voids the whole document
-- (an agreement one party rejected cannot be executed), so the owner and the
-- other parties are notified. These columns are NULL until a party declines.

SET LOCAL search_path = meridian, public;

ALTER TABLE legal_document_parties
    ADD COLUMN IF NOT EXISTS declined_at    timestamptz,
    ADD COLUMN IF NOT EXISTS decline_reason text;
