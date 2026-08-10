//! Post-quantum KYC verification certificates (FIPS 204 ML-DSA).
//!
//! When an admin approves a sector application, we sign a canonical statement over
//! the application — who, which sector, the document set, the decision and time —
//! with **ML-DSA-65** (the RustCrypto `ml-dsa` crate). The result is a detached,
//! post-quantum-secure signature anyone can verify against the published public
//! key: a tamper-evident record that *this* KYC was verified by *this* newsroom,
//! sound even against a quantum adversary.
//!
//! The keypair is derived deterministically from a 32-byte seed (FIPS 204 §5's ξ),
//! so the public key is stable across restarts and thus publishable. The seed comes
//! from `MERIDIAN_KYC_SIGNING_SEED` (base64, 32 bytes) or a fixed development seed
//! when unset, so the flow works out of the box.

use base64::Engine as _;
use ml_dsa::{
    B32, EncodedSignature, EncodedVerifyingKey, Keypair, MlDsa65, Signature, Signer, SigningKey,
    Verifier, VerifyingKey,
};

/// The algorithm label stored alongside each signature.
pub const KYC_SIGNING_ALG: &str = "ML-DSA-65";

/// Signs and verifies KYC verification certificates with a seed-derived ML-DSA-65
/// keypair. Holds only the 32-byte seed, so it is trivially `Clone`/`Send`/`Sync`;
/// the (cheap, rarely-run) key expansion happens per operation.
#[derive(Clone)]
pub struct KycSigner {
    seed: [u8; 32],
}

impl KycSigner {
    /// Builds the signer from `MERIDIAN_KYC_SIGNING_SEED` (base64 32 bytes) or a
    /// fixed development seed. Deterministic: same seed → same public key.
    #[must_use]
    pub fn from_env() -> Self {
        let seed = std::env::var("MERIDIAN_KYC_SIGNING_SEED")
            .ok()
            .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok())
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .unwrap_or(*b"meridian-newsroom-kyc-dev-seed!!");
        Self { seed }
    }

    fn signing_key(&self) -> SigningKey<MlDsa65> {
        SigningKey::<MlDsa65>::from_seed(&B32::from(self.seed))
    }

    /// The base64 public (verifying) key, published so a certificate can be checked.
    #[must_use]
    pub fn public_key_b64(&self) -> String {
        let vk: VerifyingKey<MlDsa65> = self.signing_key().verifying_key();
        base64::engine::general_purpose::STANDARD.encode(vk.encode())
    }

    /// Signs `message`, returning the base64 signature (or `None` on the internal
    /// error path).
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
}

/// Verifies a certificate against an explicit base64 public key — the standalone
/// path a third party uses, without the newsroom's seed.
#[must_use]
pub fn verify_with_public_key(public_key_b64: &str, message: &[u8], signature_b64: &str) -> bool {
    let (Ok(vk_raw), Ok(sig_raw)) = (
        base64::engine::general_purpose::STANDARD.decode(public_key_b64),
        base64::engine::general_purpose::STANDARD.decode(signature_b64),
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
    VerifyingKey::<MlDsa65>::decode(&vk_enc).verify(message, &signature).is_ok()
}

/// The canonical statement signed for a KYC verification certificate. Kept stable
/// and unambiguous so a verifier can reconstruct exactly what was attested.
#[must_use]
pub fn certificate_message(
    application_id: &str,
    user_id: &str,
    desk_id: &str,
    document_count: usize,
    decision: &str,
    issued_at: &str,
) -> Vec<u8> {
    format!(
        "meridian-kyc-v1\napplication={application_id}\nuser={user_id}\nsector={desk_id}\n\
         documents={document_count}\ndecision={decision}\nissued={issued_at}"
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_roundtrips_and_rejects_tampering() {
        let signer = KycSigner::from_env();
        let msg = certificate_message("app-1", "user-1", "desk-1", 2, "approved", "2026-08-01T00:00:00Z");
        let sig = signer.sign_b64(&msg).expect("signs");
        assert!(signer.verify_b64(&msg, &sig), "a genuine signature must verify");
        // The standalone path, given only the published public key, also verifies.
        assert!(verify_with_public_key(&signer.public_key_b64(), &msg, &sig));

        // A different statement (one more document) must not verify against it.
        let tampered = certificate_message("app-1", "user-1", "desk-1", 3, "approved", "2026-08-01T00:00:00Z");
        assert!(!signer.verify_b64(&tampered, &sig), "tampered message must fail");
        assert!(!signer.verify_b64(&msg, "not-base64!!"), "garbage signature must fail");
    }

    #[test]
    fn public_key_is_stable_for_the_same_seed() {
        assert_eq!(KycSigner::from_env().public_key_b64(), KycSigner::from_env().public_key_b64());
    }

    /// Golden test: the signed statement's byte layout is a wire format every
    /// stored certificate is bound to. If a field is reordered, renamed, or the
    /// separators change, every existing certificate silently stops verifying —
    /// so pin the exact bytes here. Changing this string is a breaking change.
    #[test]
    fn certificate_message_format_is_stable() {
        let msg = certificate_message(
            "app-1", "user-1", "desk-1", 2, "approved", "2026-08-01T00:00:00Z",
        );
        let expected = "meridian-kyc-v1\n\
             application=app-1\n\
             user=user-1\n\
             sector=desk-1\n\
             documents=2\n\
             decision=approved\n\
             issued=2026-08-01T00:00:00Z";
        assert_eq!(String::from_utf8(msg).unwrap(), expected);
    }

    /// The verification path reconstructs the issue time by reformatting the
    /// stored `timestamptz`, which only keeps microseconds. `review_application`
    /// therefore truncates to microseconds *before* signing so the reconstructed
    /// string is byte-identical. This locks that invariant: a microsecond-precise
    /// instant round-trips through RFC3339 unchanged (and a signature over it
    /// re-verifies), guarding against the nanosecond-mismatch regression.
    #[test]
    fn microsecond_timestamp_roundtrips_and_stays_verifiable() {
        use time::OffsetDateTime;
        use time::format_description::well_known::Rfc3339;

        // A microsecond-truncated instant (nanos are a whole number of micros).
        let base = OffsetDateTime::from_unix_timestamp(1_785_000_000)
            .unwrap()
            .replace_nanosecond(934_207_000)
            .unwrap();
        let signed_str = base.format(&Rfc3339).unwrap();

        // Simulate the Postgres round trip: format → parse → format again.
        let reconstructed = OffsetDateTime::parse(&signed_str, &Rfc3339)
            .unwrap()
            .format(&Rfc3339)
            .unwrap();
        assert_eq!(signed_str, reconstructed, "microsecond time must round-trip");

        // And a certificate signed over it re-verifies after that round trip.
        let signer = KycSigner::from_env();
        let msg = certificate_message("a", "u", "d", 1, "approved", &signed_str);
        let sig = signer.sign_b64(&msg).expect("signs");
        let msg2 = certificate_message("a", "u", "d", 1, "approved", &reconstructed);
        assert!(signer.verify_b64(&msg2, &sig), "reconstructed cert must verify");
    }
}
