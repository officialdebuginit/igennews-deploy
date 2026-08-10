use std::env;

use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use meridian_identity::{IdentityError, IdentityService};
use sqlx::PgPool;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires provisioned PostgreSQL; run make test-live"]
async fn login_rotation_and_replay_revocation_match_the_legacy_contract() {
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect through PgBouncer");
    let user_id = Uuid::now_v7();
    let marker = user_id.simple().to_string();
    let password = format!("test-password-{marker}");
    let salt = SaltString::encode_b64(&user_id.as_bytes()[..16]).expect("test salt");
    let password_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("hash test password")
        .to_string();

    sqlx::query(
        "INSERT INTO meridian.users (id,email,handle,display_name,password_hash,role) \
         VALUES ($1,$2,$3,'Ephemeral identity contract',$4,'reporter')",
    )
    .bind(user_id)
    .bind(format!("{marker}@contract.invalid"))
    .bind(format!("contract-{marker}"))
    .bind(password_hash)
    .execute(&pool)
    .await
    .expect("insert ephemeral user");

    let service = IdentityService::new(pool.clone(), "x".repeat(64), 15, 30);
    async {
        let user = service
            .authenticate(&format!("contract-{marker}"), &password)
            .await
            .expect("authenticate Argon2 legacy-compatible hash");
        let (first, first_refresh) = service
            .create_session(&user, Some("contract-test"), None, None)
            .await
            .expect("create session");
        let first_access = service
            .issue_access_token(&user, first.id)
            .expect("access token");
        service
            .resolve_access_token(&first_access)
            .await
            .expect("live access token");

        let (second, _second_refresh, rotated_user) = service
            .rotate_session(&first_refresh)
            .await
            .expect("one-time refresh rotation");
        assert_eq!(first.family_id, second.family_id);
        let second_access = service
            .issue_access_token(&rotated_user, second.id)
            .expect("replacement access token");
        service
            .resolve_access_token(&second_access)
            .await
            .expect("replacement session live");

        assert!(matches!(
            service.rotate_session(&first_refresh).await,
            Err(IdentityError::RefreshTokenReuse)
        ));
        assert!(matches!(
            service.resolve_access_token(&second_access).await,
            Err(IdentityError::InvalidAccessToken)
        ));
    }
    .await;

    sqlx::query("DELETE FROM meridian.users WHERE id=$1")
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("clean ephemeral user and cascading sessions");
    pool.close().await;
}
