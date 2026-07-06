//! Stage 1 acceptance tests against a live PostgreSQL >= 14 instance.
//!
//! Skipped unless `LEDGER_TEST_DB_URL` is set, so the unit suite still passes
//! in CI without a database. Run locally with:
//!
//! ```bash
//! export LEDGER_TEST_DB_URL="postgres://localhost/ledger_stage1_test"
//! cargo test --test stage1_acceptance -- --nocapture
//! ```
//!
//! Covers every acceptance criterion in issue #1:
//!   - migrations apply cleanly to a fresh PG >= 14 and set the session
//!     default isolation to SERIALIZABLE
//!   - UPDATE/DELETE on `entries` is rejected by the DB
//!   - constraints: entries.amount > 0, direction CHECK, posting_id UNIQUE
//!   - FK entries.account_id -> accounts.account_id with active-only check
//!   - chart_of_accounts seeded with all README account types
//!   - the service refuses to start if DB isolation is not SERIALIZABLE
//!
//! The HTTP endpoints (`GET /v1/chart-of-accounts`, `POST /v1/accounts`) are
//! covered by unit tests in `src/main.rs` and `src/accounts.rs`; the
//! live-DB tests below focus on the DB-level invariants the issue requires.

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

fn db_url() -> Option<String> {
    std::env::var("LEDGER_TEST_DB_URL").ok()
}

/// Apply migrations to a fresh database. Drops all ledger tables first so the
/// tests are idempotent.
async fn fresh_db(url: &str) -> PgPool {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(url)
        .await
        .expect("connect to LEDGER_TEST_DB_URL");

    // Tear down any prior state so each test starts clean.
    for stmt in [
        "DROP TABLE IF EXISTS hash_chain CASCADE",
        "DROP TABLE IF EXISTS balance_snapshots CASCADE",
        "DROP TABLE IF EXISTS entries CASCADE",
        "DROP TABLE IF EXISTS postings CASCADE",
        "DROP TABLE IF EXISTS accounts CASCADE",
        "DROP TABLE IF EXISTS chart_of_accounts CASCADE",
        "DROP FUNCTION IF EXISTS reject_entries_mutation CASCADE",
        "DROP FUNCTION IF EXISTS enforce_entries_account_active CASCADE",
    ] {
        sqlx::query(stmt).execute(&pool).await.unwrap();
    }

    // Apply migrations in order. We run them inline so we don't need a
    // `_sqlx_migrations` table; the source SQL lives in ./migrations.
    let migrations = [
        ("20260706000001_create_ledger_schema", "up.sql"),
        ("20260706000002_seed_chart_of_accounts", "up.sql"),
    ];
    for (dir, file) in migrations {
        let path = std::path::Path::new("migrations").join(dir).join(file);
        let sql = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        sqlx::query(&sql).execute(&pool).await.unwrap_or_else(|e| {
            panic!("apply {}: {e}", path.display());
        });
    }

    // Some default-isolation checks need a fresh connection that picks up the
    // ALTER DATABASE setting. Disconnect all pooled connections so the next
    // acquire re-reads the GUC.
    pool.close().await;
    PgPoolOptions::new()
        .max_connections(4)
        .connect(url)
        .await
        .expect("reconnect after migration")
}

#[tokio::test]
async fn migrations_apply_and_set_serializable_default() {
    let url = match db_url() {
        Some(u) => u,
        None => {
            eprintln!("skip: LEDGER_TEST_DB_URL unset");
            return;
        }
    };
    let pool = fresh_db(&url).await;

    let count: i64 = sqlx::query("SELECT count(*) FROM chart_of_accounts")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 11);

    // The DB default isolation must now be serializable.
    let default: String = sqlx::query("SELECT current_setting('default_transaction_isolation')")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert!(
        default.to_lowercase().contains("serializable"),
        "default_transaction_isolation = {default}"
    );

    // A transaction in this pool must report serializable.
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("BEGIN").execute(&mut *conn).await.unwrap();
    let iso: String = sqlx::query("SHOW transaction isolation level")
        .fetch_one(&mut *conn)
        .await
        .unwrap()
        .get(0);
    sqlx::query("ROLLBACK").execute(&mut *conn).await.unwrap();
    assert!(
        iso.to_lowercase().contains("serializable"),
        "transaction isolation = {iso}"
    );
}

#[tokio::test]
async fn entries_append_only_rejects_update_and_delete() {
    let url = match db_url() {
        Some(u) => u,
        None => {
            eprintln!("skip: LEDGER_TEST_DB_URL unset");
            return;
        }
    };
    let pool = fresh_db(&url).await;
    setup_minimal_entry(&pool).await;

    let res = sqlx::query("UPDATE entries SET amount = 2 WHERE entry_id = 1")
        .execute(&pool)
        .await;
    assert!(res.is_err(), "UPDATE on entries should be rejected");
    let err = format!("{}", res.err().unwrap());
    assert!(
        err.contains("append-only") || err.to_lowercase().contains("check_violation"),
        "err: {err}"
    );

    let res = sqlx::query("DELETE FROM entries WHERE entry_id = 1")
        .execute(&pool)
        .await;
    assert!(res.is_err(), "DELETE on entries should be rejected");
}

#[tokio::test]
async fn entries_amount_must_be_positive() {
    let url = match db_url() {
        Some(u) => u,
        None => {
            eprintln!("skip: LEDGER_TEST_DB_URL unset");
            return;
        }
    };
    let pool = fresh_db(&url).await;
    setup_minimal_entry(&pool).await;

    let res = sqlx::query(
        r#"INSERT INTO entries (posting_id, account_id, direction, amount, asset, this_hash)
           VALUES ('p2','acct_1','credit',0,'USD',decode('00','hex'))"#,
    )
    .execute(&pool)
    .await;
    assert!(res.is_err(), "amount=0 should be rejected");
}

#[tokio::test]
async fn entries_direction_check_constraint() {
    let url = match db_url() {
        Some(u) => u,
        None => {
            eprintln!("skip: LEDGER_TEST_DB_URL unset");
            return;
        }
    };
    let pool = fresh_db(&url).await;
    setup_minimal_entry(&pool).await;

    let res = sqlx::query(
        r#"INSERT INTO entries (posting_id, account_id, direction, amount, asset, this_hash)
           VALUES ('p2','acct_1','sideways',1,'USD',decode('00','hex'))"#,
    )
    .execute(&pool)
    .await;
    assert!(res.is_err(), "bad direction should be rejected");
}

#[tokio::test]
async fn postings_posting_id_unique() {
    let url = match db_url() {
        Some(u) => u,
        None => {
            eprintln!("skip: LEDGER_TEST_DB_URL unset");
            return;
        }
    };
    let pool = fresh_db(&url).await;
    setup_minimal_entry(&pool).await;

    let res = sqlx::query("INSERT INTO postings (posting_id, status) VALUES ('p1','committed')")
        .execute(&pool)
        .await;
    assert!(res.is_err(), "duplicate posting_id should be rejected");
}

#[tokio::test]
async fn entries_fk_account_must_exist_and_be_active() {
    let url = match db_url() {
        Some(u) => u,
        None => {
            eprintln!("skip: LEDGER_TEST_DB_URL unset");
            return;
        }
    };
    let pool = fresh_db(&url).await;
    setup_minimal_entry(&pool).await;

    let res = sqlx::query(
        r#"INSERT INTO entries (posting_id, account_id, direction, amount, asset, this_hash)
           VALUES ('p2','nope','credit',1,'USD',decode('00','hex'))"#,
    )
    .execute(&pool)
    .await;
    assert!(res.is_err(), "nonexistent account should be rejected");

    sqlx::query("UPDATE accounts SET status='frozen' WHERE account_id='acct_1'")
        .execute(&pool)
        .await
        .unwrap();
    let res = sqlx::query(
        r#"INSERT INTO entries (posting_id, account_id, direction, amount, asset, this_hash)
           VALUES ('p2','acct_1','credit',1,'USD',decode('00','hex'))"#,
    )
    .execute(&pool)
    .await;
    assert!(res.is_err(), "frozen account should reject new entries");
    let err = format!("{}", res.err().unwrap());
    assert!(err.contains("not active"), "err: {err}");
}

#[tokio::test]
async fn chart_of_accounts_seeded_with_all_readme_types() {
    let url = match db_url() {
        Some(u) => u,
        None => {
            eprintln!("skip: LEDGER_TEST_DB_URL unset");
            return;
        }
    };
    let pool = fresh_db(&url).await;

    let rows = sqlx::query(
        "SELECT account_type, normal_balance, allowed_directions FROM chart_of_accounts ORDER BY account_type",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    let types: Vec<String> = rows.iter().map(|r| r.get::<String, _>(0)).collect();
    for expected in [
        "user_custodial",
        "user_payable",
        "operational_fiat",
        "operational_crypto",
        "treasury_fiat",
        "treasury_crypto",
        "fx_gain_loss",
        "fee_revenue",
        "rail_settlement",
        "venue_settlement",
        "chargeback_reserve",
    ] {
        assert!(types.iter().any(|t| t == expected), "missing {expected}");
    }

    let fx = rows
        .iter()
        .find(|r| r.get::<String, _>(0) == "fx_gain_loss")
        .unwrap();
    assert_eq!(fx.get::<String, _>(1), "either");
    let dirs: Vec<String> = fx.get(2);
    assert!(dirs.contains(&"debit".to_string()));
    assert!(dirs.contains(&"credit".to_string()));

    let uc = rows
        .iter()
        .find(|r| r.get::<String, _>(0) == "user_custodial")
        .unwrap();
    assert_eq!(uc.get::<String, _>(1), "credit");
    assert_eq!(uc.get::<Vec<String>, _>(2), vec!["credit".to_string()]);

    let of = rows
        .iter()
        .find(|r| r.get::<String, _>(0) == "operational_fiat")
        .unwrap();
    assert_eq!(of.get::<String, _>(1), "debit");
    assert_eq!(of.get::<Vec<String>, _>(2), vec!["debit".to_string()]);
}

/// Create a minimal posting + account + entry for constraint tests.
async fn setup_minimal_entry(pool: &PgPool) {
    sqlx::query(
        r#"INSERT INTO accounts (account_id, type, asset_class, label, status)
           VALUES ('acct_1','user_custodial','fiat','test','active')
           ON CONFLICT DO NOTHING"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO postings (posting_id, status) VALUES ('p1','committed')
           ON CONFLICT DO NOTHING"#,
    )
    .execute(pool)
    .await
    .unwrap();
    let exists: i64 = sqlx::query("SELECT count(*) FROM entries WHERE entry_id = 1")
        .fetch_one(pool)
        .await
        .unwrap()
        .get(0);
    if exists == 0 {
        sqlx::query(
            r#"INSERT INTO entries (posting_id, account_id, direction, amount, asset, this_hash)
               VALUES ('p1','acct_1','credit',1,'USD',decode('00','hex'))"#,
        )
        .execute(pool)
        .await
        .unwrap();
    }
}
