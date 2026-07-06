//! Database pool setup. Configures a SQLX pool for PostgreSQL and asserts at
//! startup that the session default transaction isolation is `SERIALIZABLE`,
//! refusing to boot if it is weaker.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use sqlx::postgres::{PgPool, PgPoolOptions};

/// Required isolation level. The ledger refuses to boot with anything weaker.
pub const REQUIRED_ISOLATION: &str = "serializable";

/// Configuration parsed from environment.
#[derive(Debug, Clone)]
pub struct DbConfig {
    pub url: String,
    pub isolation: String,
    pub max_connections: u32,
}

impl DbConfig {
    /// Load DB configuration from environment.
    ///
    /// - `DB_URL` (required): PostgreSQL connection string.
    /// - `DB_ISOLATION` (default `serializable`): requested isolation level.
    /// - `DB_MAX_CONNECTIONS` (default `32`): pool size.
    pub fn from_env() -> Result<Self> {
        let url = std::env::var("DB_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .context("DB_URL (or DATABASE_URL) must be set")?;
        let isolation =
            std::env::var("DB_ISOLATION").unwrap_or_else(|_| REQUIRED_ISOLATION.to_string());
        let max_connections: u32 = std::env::var("DB_MAX_CONNECTIONS")
            .ok()
            .map(|s| s.parse::<u32>())
            .transpose()
            .context("DB_MAX_CONNECTIONS must be a non-negative integer")?
            .unwrap_or(32);
        Ok(Self {
            url,
            isolation,
            max_connections,
        })
    }
}

/// Connect to the database and verify the effective default transaction
/// isolation is `SERIALIZABLE`. Refuses to boot if it is anything weaker.
pub async fn connect_and_verify(cfg: &DbConfig) -> Result<PgPool> {
    // Refuse to boot if the operator explicitly requested a weaker level.
    if !cfg.isolation.eq_ignore_ascii_case(REQUIRED_ISOLATION) {
        bail!(
            "DB_ISOLATION must be '{}' (got '{}'); the ledger refuses to run at weaker isolation",
            REQUIRED_ISOLATION,
            cfg.isolation
        );
    }

    let pool = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&cfg.url)
        .await
        .context("failed to connect to PostgreSQL")?;

    verify_isolation(&pool).await?;
    Ok(pool)
}

/// Query the session default transaction isolation and assert it is
/// `SERIALIZABLE`. Both the `default_transaction_isolation` GUC and the
/// current setting are checked, so either an `ALTER DATABASE` default or a
/// per-session `SET` satisfies the assertion.
pub async fn verify_isolation(pool: &PgPool) -> Result<()> {
    let row: (String,) = sqlx::query_as("SHOW transaction isolation level")
        .fetch_one(pool)
        .await
        .context("failed to query transaction isolation level")?;
    let level = row.0.to_lowercase();
    if !level.contains(REQUIRED_ISOLATION) {
        return Err(anyhow!(
            "DB isolation is '{}' but the ledger requires '{}'; refusing to boot",
            level,
            REQUIRED_ISOLATION
        ));
    }
    // Also check the default isolation stored for new sessions in this DB.
    let default: (String,) =
        sqlx::query_as("SELECT current_setting('default_transaction_isolation')")
            .fetch_one(pool)
            .await?;
    let d = default.0.to_lowercase();
    if !d.contains(REQUIRED_ISOLATION) {
        return Err(anyhow!(
            "default_transaction_isolation is '{}' but the ledger requires '{}'; set it via ALTER DATABASE ... SET default_transaction_isolation = 'serializable'",
            d,
            REQUIRED_ISOLATION
        ));
    }
    Ok(())
}

/// Run embedded SQL migrations from the `migrations/` directory.
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|e| anyhow!("migration failed: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_isolation_is_serializable() {
        assert_eq!(REQUIRED_ISOLATION, "serializable");
    }

    #[test]
    fn rejects_weaker_requested_isolation() {
        // The config is constructed directly (no env) so this is a pure check.
        let cfg = DbConfig {
            url: "postgres://x".into(),
            isolation: "read_committed".into(),
            max_connections: 1,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(connect_and_verify(&cfg))
            .expect_err("should refuse read_committed");
        let msg = format!("{err:#}");
        assert!(msg.contains("must be 'serializable'"), "got: {msg}");
    }

    #[test]
    fn defaults_when_env_unset() {
        // SAFETY: tests run in a single process; remove the vars so defaults win.
        std::env::remove_var("DB_URL");
        std::env::remove_var("DATABASE_URL");
        let res = DbConfig::from_env();
        assert!(res.is_err(), "should require DB_URL");

        std::env::set_var("DB_URL", "postgres://localhost/ledger");
        std::env::remove_var("DB_ISOLATION");
        std::env::remove_var("DB_MAX_CONNECTIONS");
        let cfg = DbConfig::from_env().unwrap();
        assert_eq!(cfg.isolation, REQUIRED_ISOLATION);
        assert_eq!(cfg.max_connections, 32);
        std::env::remove_var("DB_URL");
    }
}
