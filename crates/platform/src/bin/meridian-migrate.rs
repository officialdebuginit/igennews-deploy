use std::env;

use meridian_platform::{PlatformError, run_migrations};

#[tokio::main]
async fn main() -> Result<(), PlatformError> {
    let _ = dotenvy::dotenv();
    let database_url = env::var("MIGRATION_DATABASE_URL")
        .map_err(|_| PlatformError::MissingVariable("MIGRATION_DATABASE_URL"))?;
    run_migrations(&database_url).await
}
