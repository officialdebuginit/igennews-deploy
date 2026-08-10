//! Live-database coverage for the legal-document signing flow (finding I /
//! migration 0032): a party's signature must stay verifiable after the signer
//! renames their profile. The signed name is snapshotted at signing time, so
//! verification must NOT re-derive it from the live `users.display_name`.
//!
//! Seeds two throwaway users and tears them down by scope; leaves no residue.

use meridian_newsroom::{Actor, NewsroomService, Role};
use sqlx::PgPool;
use uuid::Uuid;

/// Direct connection: these tests prepare statements, which the PgBouncer
/// transaction pool on `DATABASE_URL` does not support.
async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_DIRECT_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .expect("DATABASE_DIRECT_URL for the live legal-signing test");
    PgPool::connect(&url).await.expect("database reachable")
}

async fn insert_user(pool: &PgPool, display_name: &str) -> (Uuid, String) {
    let id = Uuid::now_v7();
    let email = format!("legal-{id}@probe.invalid");
    sqlx::query(
        "INSERT INTO meridian.users (id, email, handle, display_name, password_hash, role, is_active, is_admin) \
         VALUES ($1, $2, $3, $4, '', 'reporter', true, true)",
    )
    .bind(id)
    .bind(&email)
    .bind(format!("legal-{id}"))
    .bind(display_name)
    .execute(pool)
    .await
    .expect("probe user inserted");
    (id, email)
}

fn actor(id: Uuid) -> Actor {
    Actor { id, role: Role::Reporter, is_admin: true }
}

async fn tear_down(pool: &PgPool, ids: &[Uuid]) {
    let _ = sqlx::query("DELETE FROM meridian.legal_document_parties WHERE user_id = ANY($1)").bind(ids).execute(pool).await;
    let _ = sqlx::query("DELETE FROM meridian.legal_documents WHERE created_by = ANY($1)").bind(ids).execute(pool).await;
    let _ = sqlx::query("DELETE FROM meridian.notifications WHERE user_id = ANY($1)").bind(ids).execute(pool).await;
    let _ = sqlx::query("DELETE FROM meridian.audit_events WHERE actor_id = ANY($1)").bind(ids).execute(pool).await;
    let _ = sqlx::query("DELETE FROM meridian.users WHERE id = ANY($1)").bind(ids).execute(pool).await;
}

#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn signature_survives_a_signer_rename() {
    let pool = pool().await;
    let service = NewsroomService::new(pool.clone());
    let (owner_id, _owner_email) = insert_user(&pool, "Doc Owner").await;
    let (signer_id, signer_email) = insert_user(&pool, "Original Name").await;
    let owner = actor(owner_id);
    let signer = actor(signer_id);

    let doc = service
        .create_legal_document(&owner, "Probe Agreement", "# Terms\n\nSign here.")
        .await
        .expect("create document");
    service
        .add_legal_party(&owner, doc.id, &signer_email, "signatory", "signatory", None)
        .await
        .expect("add signatory party");
    service.sign_legal_document(&signer, doc.id).await.expect("party signs");

    // Verified by the creator (also exercises the per-document access gate).
    let before = service.verify_legal_document(&owner, doc.id).await.expect("verify");
    let executed_before = before.fully_executed;
    let all_valid_before = before.parties.iter().all(|p| p.valid);

    // The signer renames their profile. The snapshot must keep the signature valid.
    sqlx::query("UPDATE meridian.users SET display_name = $2 WHERE id = $1")
        .bind(signer_id)
        .bind("Renamed Person")
        .execute(&pool)
        .await
        .expect("rename signer");

    let after = service.verify_legal_document(&owner, doc.id).await.expect("verify after rename");
    let executed_after = after.fully_executed;
    let all_valid_after = after.parties.iter().all(|p| p.valid);

    tear_down(&pool, &[owner_id, signer_id]).await;

    assert!(executed_before, "a freshly signed document is fully executed");
    assert!(all_valid_before, "every signature verifies before the rename");
    assert!(executed_after, "a signer's profile rename must NOT un-execute the document");
    assert!(all_valid_after, "every signature still verifies after the rename (name is snapshotted)");
}
