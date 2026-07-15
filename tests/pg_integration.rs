use ledger_accounting::posting::PostingRequest;
use ledger_accounting::store::Store;
use serde_json::json;
use std::sync::OnceLock;
use tokio::sync::Mutex;

fn pg_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn db_url() -> Option<String> {
    match std::env::var("PG_TEST_DB_URL").ok() {
        Some(u) if !u.is_empty() => Some(u),
        _ => None,
    }
}

fn create_account_body(id: &str, type_name: &str, asset_class: &str) -> serde_json::Value {
    json!({
        "account_id": id,
        "type": type_name,
        "asset_class": asset_class,
        "label": format!("{}-{}", type_name, id),
    })
}

fn balanced_posting(posting_id: &str) -> serde_json::Value {
    json!({
        "posting_id": posting_id,
        "memo": "test",
        "ref_tx_id": "tx1",
        "entries": [
            { "account_id": "uc", "direction": "debit", "amount": 100, "asset": "USD" },
            { "account_id": "op", "direction": "credit", "amount": 100, "asset": "USD" }
        ]
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_roundtrip_and_persistence() {
    let url = match db_url() {
        Some(u) => u,
        None => {
            eprintln!("[pg_integration] PG_TEST_DB_URL not set; skipping");
            return;
        }
    };
    let _guard = pg_lock().lock().await;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect");
    sqlx::query("DROP TABLE IF EXISTS balance_snapshots CASCADE;")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP TABLE IF EXISTS hash_chain CASCADE;")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP TABLE IF EXISTS entries CASCADE;")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP TABLE IF EXISTS postings CASCADE;")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP TABLE IF EXISTS accounts CASCADE;")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP TABLE IF EXISTS chart_of_accounts CASCADE;")
        .execute(&pool)
        .await
        .unwrap();
    drop(pool);

    let store = Store::connect(&url).await.expect("connect store");
    store.run_migrations().await.expect("migrations");
    store.hydrate().await.expect("hydrate empty");

    store
        .create_account(
            serde_json::from_value(create_account_body("uc", "user_custodial", "both")).unwrap(),
        )
        .unwrap();
    store
        .create_account(
            serde_json::from_value(create_account_body("op", "operational_fiat", "fiat")).unwrap(),
        )
        .unwrap();

    let req: PostingRequest = serde_json::from_value(balanced_posting("pg1")).unwrap();
    let (resp, replay) = store.post(req).unwrap();
    assert!(!replay);
    assert_eq!(store.balance("uc", "USD"), Some(100));
    assert_eq!(store.balance("op", "USD"), Some(-100));
    assert!(store.verify_chain().is_ok());
    assert_eq!(store.global_chain_head(), resp.hash_head);

    let (resp2, replay2) = store
        .post(serde_json::from_value(balanced_posting("pg1")).unwrap())
        .unwrap();
    assert!(replay2);
    assert_eq!(resp.entry_ids, resp2.entry_ids);
    assert_eq!(store.entry_count(), 2);

    let store2 = Store::connect(&url).await.expect("reconnect");
    store2.run_migrations().await.expect("migrations 2");
    store2.hydrate().await.expect("hydrate 2");
    assert_eq!(store2.entry_count(), 2);
    assert_eq!(store2.balance("uc", "USD"), Some(100));
    assert_eq!(store2.balance("op", "USD"), Some(-100));
    assert!(store2.verify_chain().is_ok());
    assert_eq!(store2.global_chain_head(), resp.hash_head);
    let posting = store2.get_posting("pg1").unwrap();
    assert_eq!(posting.entries.len(), 2);
    assert_eq!(posting.hash_head, resp.hash_head);
    let anchor = store2.hash_chain_anchor("pg1").unwrap();
    assert_eq!(anchor.head_hash, resp.hash_head);

    store2
        .create_account(
            serde_json::from_value(create_account_body("opc", "operational_crypto", "crypto"))
                .unwrap(),
        )
        .unwrap();
    let multi: PostingRequest = serde_json::from_value(json!({
        "posting_id": "pg2",
        "entries": [
            { "account_id": "uc", "direction": "debit", "amount": 50, "asset": "BTC" },
            { "account_id": "opc", "direction": "credit", "amount": 50, "asset": "BTC" }
        ]
    }))
    .unwrap();
    let (_resp3, replay3) = store2.post(multi).unwrap();
    assert!(!replay3);
    assert_eq!(store2.balance("uc", "BTC"), Some(50));
    assert_eq!(store2.balance("opc", "BTC"), Some(-50));
    assert!(store2.verify_chain().is_ok());

    let snaps = store2.write_snapshots();
    assert!(!snaps.is_empty());
    for s in &snaps {
        assert!(store2.reconcile_snapshot(s), "snapshot mismatch: {:?}", s);
    }
    assert_eq!(
        store2.balance_via_snapshot("uc", "USD"),
        Some(100),
        "balance via snapshot should match"
    );
    assert_eq!(store2.balance_via_snapshot("uc", "BTC"), Some(50));

    assert_eq!(store2.user_custodial_sum("USD"), 100);

    let _ = std::process::Command::new("psql")
        .arg("-d")
        .arg(url.rsplit_once('/').map(|(_, db)| db).unwrap_or("ledger"))
        .arg("-c")
        .arg("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
        .output();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_unbalanced_rolls_back_and_idempotent_retry() {
    let url = match db_url() {
        Some(u) => u,
        None => return,
    };
    let _guard = pg_lock().lock().await;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    sqlx::query("DROP SCHEMA public CASCADE;")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE SCHEMA public;")
        .execute(&pool)
        .await
        .unwrap();
    drop(pool);

    let store = Store::connect(&url).await.unwrap();
    store.run_migrations().await.unwrap();
    store.hydrate().await.unwrap();
    store
        .create_account(
            serde_json::from_value(create_account_body("uc", "user_custodial", "both")).unwrap(),
        )
        .unwrap();
    store
        .create_account(
            serde_json::from_value(create_account_body("op", "operational_fiat", "fiat")).unwrap(),
        )
        .unwrap();

    let bad: PostingRequest = serde_json::from_value(json!({
        "posting_id": "rb1",
        "entries": [
            { "account_id": "uc", "direction": "debit", "amount": 100, "asset": "USD" },
            { "account_id": "op", "direction": "credit", "amount": 50, "asset": "USD" }
        ]
    }))
    .unwrap();
    assert!(store.post(bad).is_err());
    assert_eq!(store.entry_count(), 0);

    let good: PostingRequest = serde_json::from_value(balanced_posting("rb1")).unwrap();
    let (resp, replay) = store.post(good).unwrap();
    assert!(!replay);
    assert_eq!(store.entry_count(), 2);
    assert_eq!(store.balance("uc", "USD"), Some(100));

    let store2 = Store::connect(&url).await.unwrap();
    store2.hydrate().await.unwrap();
    assert_eq!(store2.entry_count(), 2);
    assert_eq!(store2.balance("uc", "USD"), Some(100));
    assert_eq!(store2.global_chain_head(), resp.hash_head);

    let _ = std::process::Command::new("psql")
        .arg("-d")
        .arg(url.rsplit_once('/').map(|(_, db)| db).unwrap_or("ledger"))
        .arg("-c")
        .arg("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
        .output();
}
