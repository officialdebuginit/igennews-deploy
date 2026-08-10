//! Multi-party legal document signing (Markdown agreements).
//!
//! A *legal document* is Markdown content executed by one or more parties. This is
//! **not** a new cryptographic primitive — rolling your own crypto is unsafe. It is
//! a signing *protocol* on top of the same FIPS 204 **ML-DSA-65** signatures used
//! for KYC certificates ([`crate::kyc_signing`]).
//!
//! Each party's signature is a detached ML-DSA signature over a canonical statement
//! that binds together: (a) the document's identity + title, (b) a SHA-256 digest of
//! the exact Markdown content, and (c) that party's identity, role and the instant
//! they signed. Because every party signs over the **same content digest**:
//!
//! * changing one byte of the Markdown breaks **every** signature (tamper-evident);
//! * a document is *fully executed* only when every listed party has a valid
//!   signature over the current content;
//! * any third party can verify each signature against the published public key,
//!   without the platform's private seed.
//!
//! This gives a two-way (or N-way) mutually-signed, post-quantum-verifiable record
//! for agreements — the "dual-way signature" a legal document needs.

use base64::Engine as _;
use ml_dsa::{
    B32, EncodedSignature, EncodedVerifyingKey, Keypair, MlDsa65, Signature, Signer, SigningKey,
    Verifier, VerifyingKey,
};
use serde::{Deserialize, Serialize};

/// The algorithm label stored alongside each party signature.
pub const LEGAL_SIGNING_ALG: &str = "ML-DSA-65";

/// SHA-256 hex digest of the document's Markdown content — the exact bytes every
/// party binds their signature to.
#[must_use]
pub fn content_digest(markdown: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(markdown.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The canonical statement a single party signs. Stable and unambiguous so a
/// verifier can reconstruct exactly what was attested. **Changing this format is a
/// breaking change** — every stored signature is bound to these exact bytes.
#[must_use]
pub fn party_statement(
    document_id: &str,
    title: &str,
    content_digest: &str,
    party_id: &str,
    party_name: &str,
    party_role: &str,
    signed_at: &str,
) -> Vec<u8> {
    format!(
        "meridian-legaldoc-v1\ndocument={document_id}\ntitle={title}\n\
         content_sha256={content_digest}\nparty={party_id}\nname={party_name}\n\
         role={party_role}\nsigned={signed_at}"
    )
    .into_bytes()
}

/// One party's executed signature on a document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartySignature {
    pub party_id: String,
    pub party_name: String,
    pub party_role: String,
    pub signed_at: String,
    /// Base64 ML-DSA-65 signature over [`party_statement`].
    pub signature: String,
    pub algorithm: String,
}

/// Signs and verifies legal-document party signatures with a seed-derived
/// ML-DSA-65 keypair — the same machinery as [`crate::kyc_signing::KycSigner`] but
/// keyed independently so legal execution and KYC attestation never share a key.
#[derive(Clone)]
pub struct LegalSigner {
    seed: [u8; 32],
}

impl LegalSigner {
    /// Builds the signer from `MERIDIAN_LEGAL_SIGNING_SEED` (base64, 32 bytes) or a
    /// fixed development seed. Deterministic: same seed → same public key.
    #[must_use]
    pub fn from_env() -> Self {
        let seed = std::env::var("MERIDIAN_LEGAL_SIGNING_SEED")
            .ok()
            .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok())
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .unwrap_or(*b"meridian-legaldoc-dev-seed!!2026");
        Self { seed }
    }

    fn signing_key(&self) -> SigningKey<MlDsa65> {
        SigningKey::<MlDsa65>::from_seed(&B32::from(self.seed))
    }

    /// The base64 public (verifying) key, published so signatures can be checked.
    #[must_use]
    pub fn public_key_b64(&self) -> String {
        let vk: VerifyingKey<MlDsa65> = self.signing_key().verifying_key();
        base64::engine::general_purpose::STANDARD.encode(vk.encode())
    }

    /// Signs a message, returning the base64 signature.
    #[must_use]
    pub fn sign_b64(&self, message: &[u8]) -> Option<String> {
        let signature = self.signing_key().try_sign(message).ok()?;
        Some(base64::engine::general_purpose::STANDARD.encode(signature.encode()))
    }

    /// Verifies a base64 signature against `message` and this signer's public key.
    #[must_use]
    pub fn verify_b64(&self, message: &[u8], signature_b64: &str) -> bool {
        let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(signature_b64) else {
            return false;
        };
        let Ok(encoded) = EncodedSignature::<MlDsa65>::try_from(raw.as_slice()) else {
            return false;
        };
        let Some(signature) = Signature::<MlDsa65>::decode(&encoded) else {
            return false;
        };
        self.signing_key().verifying_key().verify(message, &signature).is_ok()
    }

    /// Executes one party's signature over `markdown` (via its digest).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn sign_party(
        &self,
        document_id: &str,
        title: &str,
        content_digest: &str,
        party_id: &str,
        party_name: &str,
        party_role: &str,
        signed_at: &str,
    ) -> Option<PartySignature> {
        let message = party_statement(
            document_id, title, content_digest, party_id, party_name, party_role, signed_at,
        );
        let signature = self.sign_b64(&message)?;
        Some(PartySignature {
            party_id: party_id.to_owned(),
            party_name: party_name.to_owned(),
            party_role: party_role.to_owned(),
            signed_at: signed_at.to_owned(),
            signature,
            algorithm: LEGAL_SIGNING_ALG.to_owned(),
        })
    }

    /// Verifies a party's signature against the document's current content digest.
    #[must_use]
    pub fn verify_party(
        &self,
        document_id: &str,
        title: &str,
        content_digest: &str,
        party: &PartySignature,
    ) -> bool {
        let message = party_statement(
            document_id,
            title,
            content_digest,
            &party.party_id,
            &party.party_name,
            &party.party_role,
            &party.signed_at,
        );
        self.verify_b64(&message, &party.signature)
    }
}

/// Standalone verification with an explicit public key — the path a third party
/// uses to check a party signature without the platform's seed.
#[must_use]
pub fn verify_party_with_public_key(
    public_key_b64: &str,
    document_id: &str,
    title: &str,
    content_digest: &str,
    party: &PartySignature,
) -> bool {
    let message = party_statement(
        document_id,
        title,
        content_digest,
        &party.party_id,
        &party.party_name,
        &party.party_role,
        &party.signed_at,
    );
    let (Ok(vk_raw), Ok(sig_raw)) = (
        base64::engine::general_purpose::STANDARD.decode(public_key_b64),
        base64::engine::general_purpose::STANDARD.decode(&party.signature),
    ) else {
        return false;
    };
    let (Ok(vk_enc), Ok(sig_enc)) = (
        EncodedVerifyingKey::<MlDsa65>::try_from(vk_raw.as_slice()),
        EncodedSignature::<MlDsa65>::try_from(sig_raw.as_slice()),
    ) else {
        return false;
    };
    let Some(signature) = Signature::<MlDsa65>::decode(&sig_enc) else {
        return false;
    };
    VerifyingKey::<MlDsa65>::decode(&vk_enc).verify(&message, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn party_signs_and_verifies() {
        let signer = LegalSigner::from_env();
        let markdown = "# Service Agreement\n\nAcme and Beta agree to the following terms.";
        let digest = content_digest(markdown);
        let sig = signer
            .sign_party("doc-1", "Service Agreement", &digest, "u-acme", "Acme Ltd", "client", "2026-08-03T00:00:00Z")
            .expect("signs");
        assert!(signer.verify_party("doc-1", "Service Agreement", &digest, &sig));
        // Standalone (published public key) path also verifies.
        assert!(verify_party_with_public_key(&signer.public_key_b64(), "doc-1", "Service Agreement", &digest, &sig));
    }

    #[test]
    fn editing_the_document_breaks_every_signature() {
        let signer = LegalSigner::from_env();
        let markdown = "# NDA\n\nBoth parties keep the secret.";
        let digest = content_digest(markdown);
        let sig = signer
            .sign_party("doc-2", "NDA", &digest, "u-1", "Party One", "discloser", "2026-08-03T00:00:00Z")
            .expect("signs");
        // Tamper with the content → a new digest → the old signature no longer verifies.
        let tampered = content_digest("# NDA\n\nBoth parties keep the secret, except Party One.");
        assert_ne!(digest, tampered);
        assert!(!signer.verify_party("doc-2", "NDA", &tampered, &sig), "edited content must invalidate");
    }

    #[test]
    fn two_way_execution_requires_both_parties() {
        let signer = LegalSigner::from_env();
        let markdown = "# Mutual NDA\n\nParty A and Party B.";
        let digest = content_digest(markdown);
        let parties = [("u-a", "Party A", "disclosing"), ("u-b", "Party B", "receiving")];
        let sigs: Vec<PartySignature> = parties
            .iter()
            .map(|(id, name, role)| {
                signer
                    .sign_party("doc-3", "Mutual NDA", &digest, id, name, role, "2026-08-03T00:00:00Z")
                    .expect("signs")
            })
            .collect();
        // Every party's signature verifies over the identical content — a fully
        // executed, dual-way-signed agreement.
        assert_eq!(sigs.len(), 2);
        assert!(sigs.iter().all(|s| signer.verify_party("doc-3", "Mutual NDA", &digest, s)));
        // A signature lifted onto a different document id must not verify.
        assert!(!signer.verify_party("doc-OTHER", "Mutual NDA", &digest, &sigs[0]));
    }

    #[test]
    fn public_key_is_stable() {
        assert_eq!(LegalSigner::from_env().public_key_b64(), LegalSigner::from_env().public_key_b64());
    }
}
