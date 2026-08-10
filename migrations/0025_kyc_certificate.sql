-- Post-quantum KYC verification certificate (FIPS 204 ML-DSA).
--
-- When an admin approves a sector application, the newsroom signs a canonical
-- statement over it — applicant, sector, document count, decision, timestamp —
-- with ML-DSA-65. These columns hold that detached signature, its algorithm, and
-- the public key it verifies against, so the approval is tamper-evident and
-- checkable by anyone, and stays sound against a quantum adversary.
--
-- All nullable: only approved applications carry a certificate; rejected/pending
-- ones leave them null.

SET LOCAL search_path = meridian, public;

ALTER TABLE sector_applications
    ADD COLUMN verification_signature text,
    ADD COLUMN verification_alg text,
    ADD COLUMN verification_pubkey text;

COMMENT ON COLUMN sector_applications.verification_signature IS
  'Base64 ML-DSA-65 signature over the KYC verification certificate; set on approval.';
COMMENT ON COLUMN sector_applications.verification_alg IS
  'Signature algorithm label, e.g. ML-DSA-65 (FIPS 204).';
COMMENT ON COLUMN sector_applications.verification_pubkey IS
  'Base64 ML-DSA public key the signature verifies against.';
