//! In-platform legal documents: multi-party, in-app signed agreements.
//!
//! A document is markdown bound to a SHA-256 digest. Its signing parties are
//! **registered platform users only** — there is no external/email party. Each
//! party signs in-app; the platform executes an ML-DSA-65 signature over the
//! canonical party statement in [`crate::legal_signing`]. When every party has
//! signed, the document is `executed`. Parties are notified in-app when invited
//! and when the document completes.
//!
//! Signatures + the verifying public key are stored so a downloaded copy can be
//! re-verified offline: recompute the digest from the body, re-check each party.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::awareness::{notify, NotifyInput};
use crate::legal_signing::{content_digest, LegalSigner, PartySignature};
use crate::{Actor, NewsroomError, NewsroomService};

/// A legal document, with the creator's name and (in list views) party/signed
/// counts joined on.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct LegalDocument {
    pub id: Uuid,
    pub title: String,
    pub body_md: String,
    pub content_sha256: String,
    pub status: String,
    pub created_by: Uuid,
    #[sqlx(default)]
    pub created_by_name: Option<String>,
    #[sqlx(default)]
    pub party_count: i64,
    #[sqlx(default)]
    pub signed_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// One party (a registered user) on a document, with their signature once signed.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct LegalParty {
    pub id: Uuid,
    pub document_id: Uuid,
    pub user_id: Uuid,
    pub party_role: String,
    #[sqlx(default)]
    pub party_kind: String,
    #[sqlx(default)]
    pub sign_order: Option<i32>,
    #[serde(with = "time::serde::rfc3339")]
    pub invited_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub signed_at: Option<OffsetDateTime>,
    pub signature: Option<String>,
    pub public_key: Option<String>,
    pub algorithm: Option<String>,
    /// The party's display name at the instant they signed, snapshotted so the
    /// signature verifies regardless of any later profile rename. NULL until signed
    /// (and for signatures predating this column).
    #[sqlx(default)]
    pub signed_name: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub declined_at: Option<OffsetDateTime>,
    pub decline_reason: Option<String>,
    #[sqlx(default)]
    pub display_name: Option<String>,
    #[sqlx(default)]
    pub email: Option<String>,
}

/// One entry in a document's activity trail (its audit-event history).
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct LegalActivity {
    pub action: String,
    #[sqlx(default)]
    pub actor_name: Option<String>,
    pub after: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The outcome of verifying every party signature against the document body.
#[derive(Clone, Debug, Serialize)]
pub struct LegalVerification {
    pub document_id: Uuid,
    pub title: String,
    pub content_sha256: String,
    pub fully_executed: bool,
    pub parties: Vec<LegalPartyVerdict>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LegalPartyVerdict {
    pub party_name: String,
    pub role: String,
    pub signed_at: Option<String>,
    pub signed: bool,
    pub valid: bool,
}

impl NewsroomService {
    /// Resolves a signing party from an email or handle to a registered user.
    /// Signing is platform-only, so an unknown identifier is a hard error rather
    /// than an invitation to some external address.
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`] if no such user; database failures.
    pub async fn resolve_party(
        &self,
        identifier: &str,
    ) -> Result<(Uuid, String, String), NewsroomError> {
        let ident = identifier.trim();
        let row: Option<(Uuid, String, String)> = sqlx::query_as(
            "SELECT id, display_name, email FROM meridian.users \
             WHERE lower(email) = lower($1) OR lower(handle) = lower($1) LIMIT 1",
        )
        .bind(ident)
        .fetch_optional(self.pool())
        .await?;
        row.ok_or_else(|| {
            NewsroomError::Unprocessable(format!(
                "No platform user matches '{ident}'. Parties must be registered users."
            ))
        })
    }

    /// Creates a legal document from markdown, binding its content digest.
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`] for an empty title/body; database failures.
    pub async fn create_legal_document(
        &self,
        actor: &Actor,
        title: &str,
        body_md: &str,
    ) -> Result<LegalDocument, NewsroomError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(NewsroomError::Unprocessable("A document needs a title".to_owned()));
        }
        if body_md.trim().is_empty() {
            return Err(NewsroomError::Unprocessable("A document needs a body".to_owned()));
        }
        let digest = content_digest(body_md);
        let id = Uuid::now_v7();
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO meridian.legal_documents \
             (id, title, body_md, content_sha256, status, created_by) \
             VALUES ($1, $2, $3, $4, 'pending', $5)",
        )
        .bind(id)
        .bind(title)
        .bind(body_md)
        .bind(&digest)
        .bind(actor.id)
        .execute(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "legaldoc.created",
            "legal_document",
            &id.to_string(),
            None,
            Some(serde_json::json!({ "title": title, "content_sha256": digest })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "legal.created",
            &id.to_string(),
            None,
            serde_json::json!({ "document_id": id, "title": title }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        self.get_legal_document_row(id).await
    }

    /// Adds a registered user as a signing party and notifies them in-app.
    /// Only the document creator may add parties, and only while it is not yet
    /// executed.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] if not the creator; [`NewsroomError::NotFound`];
    /// [`NewsroomError::Conflict`] if already executed or the user is already a
    /// party; database failures.
    pub async fn add_legal_party(
        &self,
        actor: &Actor,
        document_id: Uuid,
        identifier: &str,
        role: &str,
        kind: &str,
        sign_order: Option<i32>,
    ) -> Result<LegalParty, NewsroomError> {
        let doc = self.get_legal_document_row(document_id).await?;
        if doc.created_by != actor.id {
            return Err(NewsroomError::Forbidden {
                capability: "legaldoc.owner".to_owned(),
                reason: "Only the document owner can add parties".to_owned(),
            });
        }
        if doc.status == "executed" {
            return Err(NewsroomError::Conflict(
                "This document is already fully executed".to_owned(),
            ));
        }
        if doc.status == "void" {
            return Err(NewsroomError::Conflict("This document has been voided".to_owned()));
        }
        // Adding a party after someone has signed would change the agreement the
        // earlier signers committed to, so it is blocked once any signature exists.
        let signed_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM meridian.legal_document_parties \
             WHERE document_id = $1 AND signed_at IS NOT NULL)",
        )
        .bind(document_id)
        .fetch_one(self.pool())
        .await?;
        if signed_exists {
            return Err(NewsroomError::Conflict(
                "Parties cannot be added after a signature has been collected".to_owned(),
            ));
        }
        let (user_id, display_name, _email) = self.resolve_party(identifier).await?;
        let role = if role.trim().is_empty() { "signatory" } else { role.trim() };
        // A "cc" party is a viewer — never asked to sign, never blocks execution.
        let kind = if kind.trim() == "cc" { "cc" } else { "signatory" };
        let sign_order = sign_order.filter(|_| kind == "signatory");
        let party_id = Uuid::now_v7();
        let mut tx = self.pool().begin().await?;
        let inserted = sqlx::query(
            "INSERT INTO meridian.legal_document_parties \
             (id, document_id, user_id, party_role, party_kind, sign_order) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (document_id, user_id) DO NOTHING",
        )
        .bind(party_id)
        .bind(document_id)
        .bind(user_id)
        .bind(role)
        .bind(kind)
        .bind(sign_order)
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() == 0 {
            return Err(NewsroomError::Conflict(format!(
                "{display_name} is already a party to this document"
            )));
        }
        // In-app notification. A cc viewer is shared the document; a signatory is
        // asked to sign.
        let (n_title, n_body): (String, &str) = if kind == "cc" {
            (format!("Shared with you: {}", doc.title), "A legal document was shared with you to review.")
        } else {
            (format!("Signature requested: {}", doc.title), "A legal document is awaiting your signature.")
        };
        notify(
            &mut tx,
            &NotifyInput {
                user_id,
                kind: if kind == "cc" { "legal.shared" } else { "legal.sign_request" },
                title: &n_title,
                body: Some(n_body),
                entity_type: "legal_document",
                entity_id: &document_id.to_string(),
                priority: if kind == "cc" { "normal" } else { "high" },
                group_key: None,
            },
        )
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "legaldoc.party_added",
            "legal_document",
            &document_id.to_string(),
            None,
            Some(serde_json::json!({ "user_id": user_id, "role": role })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "legal.party_added",
            &document_id.to_string(),
            None,
            serde_json::json!({ "document_id": document_id, "user_id": user_id, "role": role }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        self.get_party(party_id).await
    }

    /// Executes the acting user's signature on a document they are a party to.
    /// When it completes the set, the document becomes `executed` and the creator
    /// is notified.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] if the actor is not a party;
    /// [`NewsroomError::Conflict`] if already signed; [`NewsroomError::NotFound`];
    /// [`NewsroomError::Unprocessable`] if signing fails; database failures.
    pub async fn sign_legal_document(
        &self,
        actor: &Actor,
        document_id: Uuid,
    ) -> Result<LegalParty, NewsroomError> {
        let doc = self.get_legal_document_row(document_id).await?;
        let party: Option<LegalParty> = sqlx::query_as::<_, LegalParty>(
            "SELECT p.*, u.display_name, u.email FROM meridian.legal_document_parties p \
             LEFT JOIN meridian.users u ON u.id = p.user_id \
             WHERE p.document_id = $1 AND p.user_id = $2",
        )
        .bind(document_id)
        .bind(actor.id)
        .fetch_optional(self.pool())
        .await?;
        let party = party.ok_or(NewsroomError::Forbidden {
            capability: "legaldoc.party".to_owned(),
            reason: "You are not a party to this document".to_owned(),
        })?;
        if doc.status == "void" {
            return Err(NewsroomError::Conflict(
                "This document has been voided and can no longer be signed".to_owned(),
            ));
        }
        if party.party_kind == "cc" {
            return Err(NewsroomError::Conflict(
                "You are a viewer on this document and are not asked to sign".to_owned(),
            ));
        }
        if party.declined_at.is_some() {
            return Err(NewsroomError::Conflict("You have declined this document".to_owned()));
        }
        if party.signed_at.is_some() {
            return Err(NewsroomError::Conflict("You have already signed".to_owned()));
        }
        // Sequential signing: if this party has an order, every signatory ranked
        // before it must have signed first.
        if let Some(order) = party.sign_order {
            let ahead: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM meridian.legal_document_parties \
                 WHERE document_id = $1 AND party_kind = 'signatory' \
                   AND sign_order IS NOT NULL AND sign_order < $2 AND signed_at IS NULL",
            )
            .bind(document_id)
            .bind(order)
            .fetch_one(self.pool())
            .await?;
            if ahead > 0 {
                return Err(NewsroomError::Conflict(
                    "It is not your turn yet — earlier signatories must sign first".to_owned(),
                ));
            }
        }

        let signer = LegalSigner::from_env();
        // Truncate to microseconds so the exact instant we sign round-trips through
        // Postgres `timestamptz` (µs precision) unchanged — otherwise the signed
        // statement and the stored timestamp diverge and verification fails.
        let now = OffsetDateTime::now_utc();
        let now = now.replace_nanosecond((now.nanosecond() / 1000) * 1000).unwrap_or(now);
        let signed_at = crate::rfc3339(now);
        let party_name = party.display_name.clone().unwrap_or_else(|| actor.id.to_string());
        let sig: PartySignature = signer
            .sign_party(
                &document_id.to_string(),
                &doc.title,
                &doc.content_sha256,
                &actor.id.to_string(),
                &party_name,
                &party.party_role,
                &signed_at,
            )
            .ok_or_else(|| NewsroomError::Unprocessable("Signing failed".to_owned()))?;
        let public_key = signer.public_key_b64();

        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "UPDATE meridian.legal_document_parties \
             SET signed_at = $6, signature = $3, public_key = $4, algorithm = $5, signed_name = $7 \
             WHERE document_id = $1 AND user_id = $2",
        )
        .bind(document_id)
        .bind(actor.id)
        .bind(&sig.signature)
        .bind(&public_key)
        .bind(&sig.algorithm)
        .bind(now)
        .bind(&party_name)
        .execute(&mut *tx)
        .await?;

        // Executed once every *signatory* has signed; cc viewers never block it.
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meridian.legal_document_parties \
             WHERE document_id = $1 AND party_kind = 'signatory' AND signed_at IS NULL",
        )
        .bind(document_id)
        .fetch_one(&mut *tx)
        .await?;
        if remaining == 0 {
            sqlx::query(
                "UPDATE meridian.legal_documents SET status = 'executed', updated_at = now() \
                 WHERE id = $1",
            )
            .bind(document_id)
            .execute(&mut *tx)
            .await?;
            notify(
                &mut tx,
                &NotifyInput {
                    user_id: doc.created_by,
                    kind: "legal.executed",
                    title: &format!("Fully executed: {}", doc.title),
                    body: Some("All parties have signed this document."),
                    entity_type: "legal_document",
                    entity_id: &document_id.to_string(),
                    priority: "normal",
                    group_key: None,
                },
            )
            .await?;
        } else if doc.created_by != actor.id {
            // Progress: tell the owner someone signed (unless the owner is the signer).
            let signer_name = party.display_name.clone().unwrap_or_default();
            notify(
                &mut tx,
                &NotifyInput {
                    user_id: doc.created_by,
                    kind: "legal.party_signed",
                    title: &format!("{signer_name} signed: {}", doc.title),
                    body: Some("A party signed; the document is awaiting the remaining signatures."),
                    entity_type: "legal_document",
                    entity_id: &document_id.to_string(),
                    priority: "normal",
                    group_key: None,
                },
            )
            .await?;
        }
        crate::audit(
            &mut *tx,
            actor.id,
            "legaldoc.signed",
            "legal_document",
            &document_id.to_string(),
            None,
            Some(serde_json::json!({ "signed_at": signed_at })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "legal.party_signed",
            &document_id.to_string(),
            None,
            serde_json::json!({ "document_id": document_id, "signed_at": signed_at }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        self.get_party(party.id).await
    }

    /// Documents the actor created or is a party to, newest first, with counts.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_legal_documents(
        &self,
        actor: &Actor,
    ) -> Result<Vec<LegalDocument>, NewsroomError> {
        Ok(sqlx::query_as::<_, LegalDocument>(
            "SELECT d.*, u.display_name AS created_by_name, \
                    (SELECT count(*) FROM meridian.legal_document_parties p WHERE p.document_id = d.id AND p.party_kind = 'signatory') AS party_count, \
                    (SELECT count(*) FROM meridian.legal_document_parties p WHERE p.document_id = d.id AND p.party_kind = 'signatory' AND p.signed_at IS NOT NULL) AS signed_count \
             FROM meridian.legal_documents d \
             LEFT JOIN meridian.users u ON u.id = d.created_by \
             WHERE d.created_by = $1 \
                OR EXISTS (SELECT 1 FROM meridian.legal_document_parties p \
                           WHERE p.document_id = d.id AND p.user_id = $1) \
             ORDER BY d.created_at DESC",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?)
    }

    /// A document with its parties. Access is limited to the creator and parties.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] if the actor is neither; [`NewsroomError::NotFound`];
    /// database failures.
    pub async fn get_legal_document(
        &self,
        actor: &Actor,
        document_id: Uuid,
    ) -> Result<(LegalDocument, Vec<LegalParty>), NewsroomError> {
        let doc = self.get_legal_document_row(document_id).await?;
        let parties = self.list_parties(document_id).await?;
        let is_party = parties.iter().any(|p| p.user_id == actor.id);
        if doc.created_by != actor.id && !is_party {
            return Err(NewsroomError::Forbidden {
                capability: "legaldoc.access".to_owned(),
                reason: "This document is not shared with you".to_owned(),
            });
        }
        Ok((doc, parties))
    }

    /// Verifies every party signature against the document's current body.
    ///
    /// Access is limited to the creator and parties, like every other per-document
    /// endpoint — the verdict carries the title and the full signatory roster, so it
    /// must not be readable by an unrelated logged-in user who merely holds the id.
    /// (The public, stateless `POST /legal/verify` is the un-authenticated path,
    /// and it only echoes caller-supplied data.)
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] if the actor is neither creator nor party;
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn verify_legal_document(
        &self,
        actor: &Actor,
        document_id: Uuid,
    ) -> Result<LegalVerification, NewsroomError> {
        let doc = self.get_legal_document_row(document_id).await?;
        let parties = self.list_parties(document_id).await?;
        if doc.created_by != actor.id && !parties.iter().any(|p| p.user_id == actor.id) {
            return Err(NewsroomError::Forbidden {
                capability: "legaldoc.access".to_owned(),
                reason: "This document is not shared with you".to_owned(),
            });
        }
        // Recompute the digest from the stored body: a tampered body invalidates
        // every signature bound to the original.
        let digest = content_digest(&doc.body_md);
        let signer = LegalSigner::from_env();
        // Only signatories are verified — cc viewers never sign, so they neither
        // appear in the verdict nor affect whether the document is fully executed.
        let signatories: Vec<&LegalParty> = parties.iter().filter(|p| p.party_kind != "cc").collect();
        let mut verdicts = Vec::with_capacity(signatories.len());
        let mut all_valid = !signatories.is_empty();
        for p in &signatories {
            let signed = p.signed_at.is_some();
            let valid = match (&p.signature, signed) {
                (Some(signature), true) => {
                    let party = PartySignature {
                        party_id: p.user_id.to_string(),
                        party_name: p.signed_name.clone().or_else(|| p.display_name.clone()).unwrap_or_default(),
                        party_role: p.party_role.clone(),
                        signed_at: p.signed_at.map(crate::rfc3339).unwrap_or_default(),
                        signature: signature.clone(),
                        algorithm: p.algorithm.clone().unwrap_or_default(),
                    };
                    signer.verify_party(&document_id.to_string(), &doc.title, &digest, &party)
                }
                _ => false,
            };
            if !valid {
                all_valid = false;
            }
            verdicts.push(LegalPartyVerdict {
                party_name: p.signed_name.clone().or_else(|| p.display_name.clone()).unwrap_or_default(),
                role: p.party_role.clone(),
                signed_at: p.signed_at.map(crate::rfc3339),
                signed,
                valid,
            });
        }
        Ok(LegalVerification {
            document_id,
            title: doc.title,
            content_sha256: digest,
            fully_executed: all_valid && doc.status == "executed",
            parties: verdicts,
        })
    }

    /// Voids a document — the owner cancels it. A voided document can no longer be
    /// signed; all parties are notified.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] if not the owner; [`NewsroomError::Conflict`] if
    /// already executed or voided; [`NewsroomError::NotFound`]; database failures.
    pub async fn void_legal_document(
        &self,
        actor: &Actor,
        document_id: Uuid,
    ) -> Result<LegalDocument, NewsroomError> {
        let doc = self.get_legal_document_row(document_id).await?;
        if doc.created_by != actor.id {
            return Err(NewsroomError::Forbidden {
                capability: "legaldoc.owner".to_owned(),
                reason: "Only the document owner can void it".to_owned(),
            });
        }
        if doc.status == "executed" {
            return Err(NewsroomError::Conflict(
                "A fully executed document cannot be voided".to_owned(),
            ));
        }
        if doc.status == "void" {
            return Err(NewsroomError::Conflict("This document is already void".to_owned()));
        }
        let parties = self.list_parties(document_id).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "UPDATE meridian.legal_documents SET status = 'void', updated_at = now() WHERE id = $1",
        )
        .bind(document_id)
        .execute(&mut *tx)
        .await?;
        for p in &parties {
            notify(
                &mut tx,
                &NotifyInput {
                    user_id: p.user_id,
                    kind: "legal.voided",
                    title: &format!("Voided: {}", doc.title),
                    body: Some("The owner cancelled this document; no further action is needed."),
                    entity_type: "legal_document",
                    entity_id: &document_id.to_string(),
                    priority: "normal",
                    group_key: None,
                },
            )
            .await?;
        }
        crate::audit(
            &mut *tx,
            actor.id,
            "legaldoc.voided",
            "legal_document",
            &document_id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "legal.voided",
            &document_id.to_string(),
            None,
            serde_json::json!({ "document_id": document_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        self.get_legal_document_row(document_id).await
    }

    /// A party declines to sign, with a reason. Declining voids the document (a
    /// rejected agreement cannot execute); the owner and other parties are notified.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] if not a party; [`NewsroomError::Conflict`] if
    /// already signed/declined or the document is executed; database failures.
    pub async fn decline_legal_document(
        &self,
        actor: &Actor,
        document_id: Uuid,
        reason: &str,
    ) -> Result<LegalDocument, NewsroomError> {
        let doc = self.get_legal_document_row(document_id).await?;
        if doc.status == "executed" {
            return Err(NewsroomError::Conflict(
                "This document is already fully executed".to_owned(),
            ));
        }
        let party = self.party_for(document_id, actor.id).await?.ok_or(NewsroomError::Forbidden {
            capability: "legaldoc.party".to_owned(),
            reason: "You are not a party to this document".to_owned(),
        })?;
        if party.signed_at.is_some() {
            return Err(NewsroomError::Conflict("You have already signed".to_owned()));
        }
        if party.declined_at.is_some() {
            return Err(NewsroomError::Conflict("You have already declined".to_owned()));
        }
        let reason = reason.trim();
        let parties = self.list_parties(document_id).await?;
        let decliner = party.display_name.clone().unwrap_or_default();
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "UPDATE meridian.legal_document_parties \
             SET declined_at = now(), decline_reason = $3 WHERE document_id = $1 AND user_id = $2",
        )
        .bind(document_id)
        .bind(actor.id)
        .bind(if reason.is_empty() { None } else { Some(reason) })
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE meridian.legal_documents SET status = 'void', updated_at = now() WHERE id = $1",
        )
        .bind(document_id)
        .execute(&mut *tx)
        .await?;
        // Notify the owner and the other parties.
        for uid in parties.iter().map(|p| p.user_id).chain(std::iter::once(doc.created_by)) {
            if uid == actor.id {
                continue;
            }
            notify(
                &mut tx,
                &NotifyInput {
                    user_id: uid,
                    kind: "legal.declined",
                    title: &format!("{decliner} declined: {}", doc.title),
                    body: Some("A party declined to sign, so the document is now void."),
                    entity_type: "legal_document",
                    entity_id: &document_id.to_string(),
                    priority: "high",
                    group_key: None,
                },
            )
            .await?;
        }
        crate::audit(
            &mut *tx,
            actor.id,
            "legaldoc.declined",
            "legal_document",
            &document_id.to_string(),
            None,
            Some(serde_json::json!({ "reason": reason })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "legal.declined",
            &document_id.to_string(),
            None,
            serde_json::json!({ "document_id": document_id, "reason": reason }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        self.get_legal_document_row(document_id).await
    }

    /// Removes a party from a document (owner only, before that party has signed).
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] if not the owner; [`NewsroomError::Conflict`] if
    /// the party already signed or the document is executed; [`NewsroomError::NotFound`].
    pub async fn remove_legal_party(
        &self,
        actor: &Actor,
        document_id: Uuid,
        party_id: Uuid,
    ) -> Result<(), NewsroomError> {
        let doc = self.get_legal_document_row(document_id).await?;
        if doc.created_by != actor.id {
            return Err(NewsroomError::Forbidden {
                capability: "legaldoc.owner".to_owned(),
                reason: "Only the document owner can remove parties".to_owned(),
            });
        }
        if doc.status == "executed" {
            return Err(NewsroomError::Conflict(
                "Parties cannot be removed from an executed document".to_owned(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let deleted = sqlx::query(
            "DELETE FROM meridian.legal_document_parties \
             WHERE id = $1 AND document_id = $2 AND signed_at IS NULL",
        )
        .bind(party_id)
        .bind(document_id)
        .execute(&mut *tx)
        .await?;
        if deleted.rows_affected() == 0 {
            return Err(NewsroomError::Conflict(
                "That party has already signed and cannot be removed".to_owned(),
            ));
        }
        crate::audit(
            &mut *tx,
            actor.id,
            "legaldoc.party_removed",
            "legal_document",
            &document_id.to_string(),
            None,
            Some(serde_json::json!({ "party_id": party_id })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "legal.party_removed",
            &document_id.to_string(),
            None,
            serde_json::json!({ "document_id": document_id, "party_id": party_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The document's activity trail (audit-event history), oldest first. Access is
    /// limited to the creator and parties.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] if neither; database failures.
    pub async fn legal_document_activity(
        &self,
        actor: &Actor,
        document_id: Uuid,
    ) -> Result<Vec<LegalActivity>, NewsroomError> {
        // Gate access exactly like get_legal_document.
        self.get_legal_document(actor, document_id).await?;
        Ok(sqlx::query_as::<_, LegalActivity>(
            "SELECT e.action, u.display_name AS actor_name, e.after, e.created_at \
             FROM meridian.audit_events e \
             LEFT JOIN meridian.users u ON u.id = e.actor_id \
             WHERE e.entity_type = 'legal_document' AND e.entity_id = $1 \
             ORDER BY e.created_at",
        )
        .bind(document_id.to_string())
        .fetch_all(self.pool())
        .await?)
    }

    /// Edits a document's title and/or body (owner only, and only while no party
    /// has signed — editing changes the content digest, which would invalidate any
    /// existing signature). Recomputes the content digest.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] if not the owner; [`NewsroomError::Conflict`] if
    /// a signature exists or the document is executed/void;
    /// [`NewsroomError::Unprocessable`] for empty fields; database failures.
    pub async fn update_legal_document(
        &self,
        actor: &Actor,
        document_id: Uuid,
        title: &str,
        body_md: &str,
    ) -> Result<LegalDocument, NewsroomError> {
        let doc = self.get_legal_document_row(document_id).await?;
        if doc.created_by != actor.id {
            return Err(NewsroomError::Forbidden {
                capability: "legaldoc.owner".to_owned(),
                reason: "Only the document owner can edit it".to_owned(),
            });
        }
        if doc.status == "executed" || doc.status == "void" {
            return Err(NewsroomError::Conflict(format!(
                "A {} document cannot be edited",
                doc.status
            )));
        }
        let signed_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM meridian.legal_document_parties \
             WHERE document_id = $1 AND signed_at IS NOT NULL)",
        )
        .bind(document_id)
        .fetch_one(self.pool())
        .await?;
        if signed_exists {
            return Err(NewsroomError::Conflict(
                "The document cannot be edited after a signature has been collected".to_owned(),
            ));
        }
        let title = title.trim();
        if title.is_empty() {
            return Err(NewsroomError::Unprocessable("A document needs a title".to_owned()));
        }
        if body_md.trim().is_empty() {
            return Err(NewsroomError::Unprocessable("A document needs a body".to_owned()));
        }
        let digest = content_digest(body_md);
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "UPDATE meridian.legal_documents \
             SET title = $2, body_md = $3, content_sha256 = $4, updated_at = now() WHERE id = $1",
        )
        .bind(document_id)
        .bind(title)
        .bind(body_md)
        .bind(&digest)
        .execute(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "legaldoc.edited",
            "legal_document",
            &document_id.to_string(),
            None,
            Some(serde_json::json!({ "title": title, "content_sha256": digest })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "legal.edited",
            &document_id.to_string(),
            None,
            serde_json::json!({ "document_id": document_id, "title": title }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        self.get_legal_document_row(document_id).await
    }

    /// Deletes a document (owner only). A fully executed document is a permanent
    /// record and cannot be deleted — void it instead. Parties cascade away.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] if not the owner; [`NewsroomError::Conflict`] if
    /// executed; [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_legal_document(
        &self,
        actor: &Actor,
        document_id: Uuid,
    ) -> Result<(), NewsroomError> {
        let doc = self.get_legal_document_row(document_id).await?;
        if doc.created_by != actor.id {
            return Err(NewsroomError::Forbidden {
                capability: "legaldoc.owner".to_owned(),
                reason: "Only the document owner can delete it".to_owned(),
            });
        }
        if doc.status == "executed" {
            return Err(NewsroomError::Conflict(
                "A fully executed document is a permanent record and cannot be deleted".to_owned(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM meridian.legal_documents WHERE id = $1")
            .bind(document_id)
            .execute(&mut *tx)
            .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "legaldoc.deleted",
            "legal_document",
            &document_id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "legal.deleted",
            &document_id.to_string(),
            None,
            serde_json::json!({ "document_id": document_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    // --- internal helpers ---

    async fn party_for(
        &self,
        document_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<LegalParty>, NewsroomError> {
        Ok(sqlx::query_as::<_, LegalParty>(
            "SELECT p.*, u.display_name, u.email FROM meridian.legal_document_parties p \
             LEFT JOIN meridian.users u ON u.id = p.user_id \
             WHERE p.document_id = $1 AND p.user_id = $2",
        )
        .bind(document_id)
        .bind(user_id)
        .fetch_optional(self.pool())
        .await?)
    }

    async fn get_legal_document_row(
        &self,
        document_id: Uuid,
    ) -> Result<LegalDocument, NewsroomError> {
        sqlx::query_as::<_, LegalDocument>(
            "SELECT d.*, u.display_name AS created_by_name FROM meridian.legal_documents d \
             LEFT JOIN meridian.users u ON u.id = d.created_by WHERE d.id = $1",
        )
        .bind(document_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("Legal document"))
    }

    async fn list_parties(&self, document_id: Uuid) -> Result<Vec<LegalParty>, NewsroomError> {
        Ok(sqlx::query_as::<_, LegalParty>(
            "SELECT p.*, u.display_name, u.email FROM meridian.legal_document_parties p \
             LEFT JOIN meridian.users u ON u.id = p.user_id \
             WHERE p.document_id = $1 ORDER BY p.invited_at",
        )
        .bind(document_id)
        .fetch_all(self.pool())
        .await?)
    }

    async fn get_party(&self, party_id: Uuid) -> Result<LegalParty, NewsroomError> {
        sqlx::query_as::<_, LegalParty>(
            "SELECT p.*, u.display_name, u.email FROM meridian.legal_document_parties p \
             LEFT JOIN meridian.users u ON u.id = p.user_id WHERE p.id = $1",
        )
        .bind(party_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("Party"))
    }
}
