use ledger_accounting::config;
use ledger_accounting::grpc;
use ledger_accounting::handlers;
use ledger_accounting::store::Store;
use tonic::transport::Server as TonicServer;

#[allow(dead_code)]
fn app() -> axum::Router {
    handlers::router(Store::new())
}

#[allow(dead_code)]
async fn serve(listener: tokio::net::TcpListener) {
    axum::serve(listener, app()).await.unwrap();
}

async fn run_grpc(store: Store, addr: std::net::SocketAddr) {
    let caller = std::env::var("ALLOWED_CALLERS")
        .unwrap_or_else(|_| "transaction-orchestrator,treasury-orchestration".to_string());
    let allowed_callers: Vec<String> = caller.split(',').map(|s| s.trim().to_string()).collect();
    let svc = grpc::server(store, allowed_callers);
    if let Err(e) = TonicServer::builder().add_service(svc).serve(addr).await {
        eprintln!("[grpc] server error: {}", e);
    }
}

async fn run_snapshot_task(store: Store) {
    let cfg = config::Config::from_env();
    let interval = cfg.snapshot_interval();
    loop {
        tokio::time::sleep(interval).await;
        let snaps = store.write_snapshots();
        eprintln!("[snapshot] wrote {} balance snapshots", snaps.len());
    }
}

fn verify_chain_at_startup(store: &Store) {
    match store.verify_chain() {
        Ok(()) => {
            eprintln!("[chain] verification passed at startup");
        }
        Err(b) => {
            eprintln!(
                "[chain] FATAL: hash chain broken at entry {}: {}",
                b.entry_id, b.reason
            );
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn main() {
    let cfg = config::Config::from_env();
    if let Err(e) = cfg.assert_isolation() {
        eprintln!("[boot] {}", e);
        std::process::exit(1);
    }

    let store = if cfg.db_url.is_empty() {
        Store::new()
    } else {
        match Store::connect(&cfg.db_url).await {
            Ok(s) => {
                if let Err(e) = s.run_migrations().await {
                    eprintln!("[boot] {}", e);
                    std::process::exit(1);
                }
                if let Err(e) = s.hydrate().await {
                    eprintln!("[boot] {}", e);
                    std::process::exit(1);
                }
                eprintln!("[boot] connected to postgres at {}", cfg.db_url);
                s
            }
            Err(e) => {
                eprintln!("[boot] failed to connect to postgres: {}", e);
                std::process::exit(1);
            }
        }
    };
    verify_chain_at_startup(&store);

    let port = cfg.port;
    let rest_addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
    let grpc_addr: std::net::SocketAddr = ([0, 0, 0, 0], port + 1).into();

    let grpc_store = store.clone();
    let snap_store = store.clone();
    let rest_store = store.clone();
    tokio::spawn(async move {
        run_grpc(grpc_store, grpc_addr).await;
    });
    tokio::spawn(async move {
        run_snapshot_task(snap_store).await;
    });

    let listener = tokio::net::TcpListener::bind(rest_addr).await.unwrap();
    axum::serve(listener, handlers::router(rest_store))
        .await
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use ledger_accounting::chart;
    use ledger_accounting::posting::PostingRequest;
    use ledger_accounting::store::Store;
    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::ServiceExt;

    fn create_account_body(id: &str, type_name: &str, asset_class: &str) -> Value {
        json!({
            "account_id": id,
            "type": type_name,
            "asset_class": asset_class,
            "label": format!("{}-{}", type_name, id),
        })
    }

    async fn post_json(router: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let res = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let val: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, val)
    }

    async fn get_json(router: &axum::Router, uri: &str) -> (StatusCode, Value) {
        let res = router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let val: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, val)
    }

    fn balanced_posting_body(posting_id: &str) -> Value {
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

    fn unbalanced_posting_body(posting_id: &str) -> Value {
        json!({
            "posting_id": posting_id,
            "entries": [
                { "account_id": "uc", "direction": "debit", "amount": 100, "asset": "USD" },
                { "account_id": "op", "direction": "credit", "amount": 50, "asset": "USD" }
            ]
        })
    }

    async fn setup_two_accounts(router: &axum::Router) {
        let _ = post_json(
            router,
            "/v1/accounts",
            create_account_body("uc", "user_custodial", "both"),
        )
        .await;
        let _ = post_json(
            router,
            "/v1/accounts",
            create_account_body("op", "operational_fiat", "fiat"),
        )
        .await;
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let res = app()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let val: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(val, json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn readyz_returns_ok() {
        let res = app()
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let val: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(val, json!({ "status": "ready" }));
    }

    #[tokio::test]
    async fn router_returns_404_for_unknown_route() {
        let res = app()
            .oneshot(
                Request::builder()
                    .uri("/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn serve_handles_real_http_connections() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener));

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("\"status\":\"ok\""));
    }

    #[tokio::test]
    async fn chart_of_accounts_returns_catalog() {
        let (status, val) = get_json(&app(), "/v1/chart-of-accounts").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(val["version"], "1.0.0");
        let types = val["account_types"].as_array().unwrap();
        assert!(types.len() >= 11);
        let names: Vec<&str> = types.iter().map(|t| t["type"].as_str().unwrap()).collect();
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
            assert!(names.contains(&expected), "missing {}", expected);
        }
    }

    #[tokio::test]
    async fn create_account_rejects_unknown_type() {
        let router = app();
        let (status, val) = post_json(
            &router,
            "/v1/accounts",
            create_account_body("a1", "bogus", "fiat"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(val["error"]
            .as_str()
            .unwrap()
            .contains("unknown account type"));
    }

    #[tokio::test]
    async fn create_account_rejects_bad_asset_class_for_type() {
        let router = app();
        let (status, val) = post_json(
            &router,
            "/v1/accounts",
            create_account_body("a2", "operational_fiat", "crypto"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(val["error"]
            .as_str()
            .unwrap()
            .contains("asset_class crypto not allowed for type operational_fiat"));
    }

    #[tokio::test]
    async fn create_account_returns_201_and_id() {
        let router = app();
        let (status, val) = post_json(
            &router,
            "/v1/accounts",
            create_account_body("acct-uc", "user_custodial", "fiat"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(val["account_id"], "acct-uc");
    }

    #[tokio::test]
    async fn balance_returns_404_for_unknown_account() {
        let router = app();
        let (status, _) = get_json(&router, "/v1/accounts/nope/balance?asset=USD").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn posting_balanced_returns_201_with_hash_head() {
        let router = app();
        setup_two_accounts(&router).await;
        let (status, val) = post_json(&router, "/v1/postings", balanced_posting_body("p1")).await;
        assert_eq!(status, StatusCode::CREATED, "body: {:?}", val);
        assert_eq!(val["status"], "posted");
        let entry_ids = val["entry_ids"].as_array().unwrap();
        assert_eq!(entry_ids.len(), 2);
        let hash_head = val["hash_head"].as_str().unwrap();
        assert!(!hash_head.is_empty());
    }

    #[tokio::test]
    async fn posting_unbalanced_returns_400() {
        let router = app();
        setup_two_accounts(&router).await;
        let (status, val) = post_json(&router, "/v1/postings", unbalanced_posting_body("p2")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(val["error"].as_str().unwrap().contains("unbalanced"));
    }

    #[tokio::test]
    async fn posting_unknown_account_returns_400() {
        let router = app();
        let _ = post_json(
            &router,
            "/v1/accounts",
            create_account_body("uc", "user_custodial", "both"),
        )
        .await;
        let (status, val) = post_json(
            &router,
            "/v1/postings",
            json!({
                "posting_id": "p3",
                "entries": [
                    { "account_id": "nope", "direction": "debit", "amount": 10, "asset": "USD" },
                    { "account_id": "uc", "direction": "credit", "amount": 10, "asset": "USD" }
                ]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(val["error"].as_str().unwrap().contains("account not found"));
    }

    #[tokio::test]
    async fn posting_max_entries_exceeded_returns_400() {
        let router = app();
        let _ = post_json(
            &router,
            "/v1/accounts",
            create_account_body("uc", "user_custodial", "both"),
        )
        .await;
        let mut entries = Vec::new();
        for i in 0..65 {
            entries.push(json!({
                "account_id": "uc",
                "direction": if i % 2 == 0 { "debit" } else { "credit" },
                "amount": 1,
                "asset": "USD"
            }));
        }
        let (status, val) = post_json(
            &router,
            "/v1/postings",
            json!({ "posting_id": "pmax", "entries": entries }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(val["error"].as_str().unwrap().contains("too many entries"));
    }

    #[tokio::test]
    async fn posting_zero_amount_rejected() {
        let router = app();
        let _ = post_json(
            &router,
            "/v1/accounts",
            create_account_body("uc", "user_custodial", "both"),
        )
        .await;
        let (status, val) = post_json(
            &router,
            "/v1/postings",
            json!({
                "posting_id": "pz",
                "entries": [
                    { "account_id": "uc", "direction": "debit", "amount": 0, "asset": "USD" },
                    { "account_id": "uc", "direction": "credit", "amount": 0, "asset": "USD" }
                ]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(val["error"]
            .as_str()
            .unwrap()
            .contains("amount must be > 0"));
    }

    #[tokio::test]
    async fn idempotency_replay_returns_200_same_result() {
        let router = app();
        setup_two_accounts(&router).await;
        let (status1, val1) =
            post_json(&router, "/v1/postings", balanced_posting_body("pidem")).await;
        assert_eq!(status1, StatusCode::CREATED);
        let (status2, val2) =
            post_json(&router, "/v1/postings", balanced_posting_body("pidem")).await;
        assert_eq!(status2, StatusCode::OK);
        assert_eq!(val1["entry_ids"], val2["entry_ids"]);
        assert_eq!(val1["hash_head"], val2["hash_head"]);
    }

    #[tokio::test]
    async fn get_posting_returns_full_record() {
        let router = app();
        setup_two_accounts(&router).await;
        let (_, val1) = post_json(&router, "/v1/postings", balanced_posting_body("pget")).await;
        let (status, val2) = get_json(&router, "/v1/postings/pget").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(val2["posting_id"], "pget");
        assert_eq!(val2["status"], "posted");
        let entries = val2["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["entry_id"], val1["entry_ids"][0]);
    }

    #[tokio::test]
    async fn get_posting_unknown_returns_404() {
        let router = app();
        let (status, _) = get_json(&router, "/v1/postings/does-not-exist").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn balance_reflects_entries() {
        let router = app();
        setup_two_accounts(&router).await;
        let _ = post_json(&router, "/v1/postings", balanced_posting_body("pb1")).await;
        let (status, val) = get_json(&router, "/v1/accounts/uc/balance?asset=USD").await;
        assert_eq!(status, StatusCode::OK);
        let bal: i128 = val["balance"].as_str().unwrap().parse().unwrap();
        assert_eq!(bal, 100);
    }

    #[tokio::test]
    async fn ledger_returns_paginated_with_running_balance() {
        let router = app();
        setup_two_accounts(&router).await;
        let _ = post_json(&router, "/v1/postings", balanced_posting_body("l1")).await;
        let _ = post_json(&router, "/v1/postings", balanced_posting_body("l2")).await;
        let (status, val) = get_json(&router, "/v1/accounts/uc/ledger?limit=10").await;
        assert_eq!(status, StatusCode::OK);
        let entries = val["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["running_balance"], 100);
        assert_eq!(entries[1]["running_balance"], 200);
    }

    #[tokio::test]
    async fn ledger_404_for_unknown_account() {
        let router = app();
        let (status, _) = get_json(&router, "/v1/accounts/nope/ledger?asset=USD").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn hash_chain_continuity_holds() {
        let router = app();
        setup_two_accounts(&router).await;
        let (_, val) = post_json(&router, "/v1/postings", balanced_posting_body("hc1")).await;
        let posting = get_json(&router, "/v1/postings/hc1").await.1;
        let entries = posting["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        let prev0 = entries[0]["prev_hash"].as_str().unwrap();
        assert_eq!(prev0, chart::GENESIS_HASH);
        let this0 = entries[0]["this_hash"].as_str().unwrap();
        let prev1 = entries[1]["prev_hash"].as_str().unwrap();
        assert_eq!(prev1, this0);
        let this1 = entries[1]["this_hash"].as_str().unwrap();
        assert_eq!(this1, val["hash_head"].as_str().unwrap());
    }

    #[tokio::test]
    async fn audit_event_emitted_per_posting() {
        let store = Store::new();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("uc", "user_custodial", "both"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("op", "operational_fiat", "fiat"))
                    .unwrap(),
            )
            .unwrap();
        let req: PostingRequest = serde_json::from_value(balanced_posting_body("ae1")).unwrap();
        let _ = store.post(req).unwrap();
        let events = store.audit_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].posting_id, "ae1");
    }

    #[tokio::test]
    async fn unit_balance_computation() {
        let store = Store::new();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("uc", "user_custodial", "both"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("op", "operational_fiat", "fiat"))
                    .unwrap(),
            )
            .unwrap();
        let req: PostingRequest = serde_json::from_value(balanced_posting_body("u1")).unwrap();
        store.post(req).unwrap();
        let bal = store.balance("uc", "USD").unwrap();
        assert_eq!(bal, 100);
        let bal_op = store.balance("op", "USD").unwrap();
        assert_eq!(bal_op, -100);
    }

    #[tokio::test]
    async fn unit_reject_unbalanced_direct() {
        let store = Store::new();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("uc", "user_custodial", "both"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("op", "operational_fiat", "fiat"))
                    .unwrap(),
            )
            .unwrap();
        let req: PostingRequest = serde_json::from_value(unbalanced_posting_body("uu1")).unwrap();
        let res = store.post(req);
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn unit_idempotency_direct() {
        let store = Store::new();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("uc", "user_custodial", "both"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("op", "operational_fiat", "fiat"))
                    .unwrap(),
            )
            .unwrap();
        let req: PostingRequest = serde_json::from_value(balanced_posting_body("idem1")).unwrap();
        let (r1, replay1) = store.post(req.clone()).unwrap();
        assert!(!replay1);
        let (r2, replay2) = store.post(req).unwrap();
        assert!(replay2);
        assert_eq!(r1.entry_ids, r2.entry_ids);
        assert_eq!(r1.hash_head, r2.hash_head);
        assert_eq!(store.entry_count(), 2);
    }

    #[tokio::test]
    async fn unit_hash_chain_continuity_direct() {
        let store = Store::new();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("uc", "user_custodial", "both"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("op", "operational_fiat", "fiat"))
                    .unwrap(),
            )
            .unwrap();
        let req: PostingRequest = serde_json::from_value(balanced_posting_body("hcc1")).unwrap();
        let (resp, _) = store.post(req).unwrap();
        let posting = store.get_posting("hcc1").unwrap();
        let entries = &posting.entries;
        assert_eq!(entries[0].prev_hash, chart::GENESIS_HASH);
        assert_eq!(entries[1].prev_hash, entries[0].this_hash);
        assert_eq!(entries[1].this_hash, resp.hash_head);
    }

    #[tokio::test]
    async fn multi_asset_posting_per_asset_balance() {
        let router = app();
        setup_two_accounts(&router).await;
        let _ = post_json(
            &router,
            "/v1/accounts",
            create_account_body("opc", "operational_crypto", "crypto"),
        )
        .await;
        let body = json!({
            "posting_id": "multi1",
            "entries": [
                { "account_id": "uc", "direction": "debit", "amount": 100, "asset": "USD" },
                { "account_id": "op", "direction": "credit", "amount": 100, "asset": "USD" },
                { "account_id": "uc", "direction": "debit", "amount": 50, "asset": "BTC" },
                { "account_id": "opc", "direction": "credit", "amount": 50, "asset": "BTC" }
            ]
        });
        let (status, val) = post_json(&router, "/v1/postings", body).await;
        assert_eq!(status, StatusCode::CREATED, "body: {:?}", val);
        let (s1, b1) = get_json(&router, "/v1/accounts/uc/balance?asset=USD").await;
        assert_eq!(s1, StatusCode::OK);
        assert_eq!(b1["balance"], "100");
        let (s2, b2) = get_json(&router, "/v1/accounts/uc/balance?asset=BTC").await;
        assert_eq!(s2, StatusCode::OK);
        assert_eq!(b2["balance"], "50");
    }

    #[tokio::test]
    async fn unbalanced_per_asset_rejected() {
        let router = app();
        setup_two_accounts(&router).await;
        let body = json!({
            "posting_id": "ub1",
            "entries": [
                { "account_id": "uc", "direction": "debit", "amount": 100, "asset": "USD" },
                { "account_id": "op", "direction": "credit", "amount": 100, "asset": "USD" },
                { "account_id": "uc", "direction": "debit", "amount": 50, "asset": "BTC" },
                { "account_id": "op", "direction": "credit", "amount": 30, "asset": "BTC" }
            ]
        });
        let (status, val) = post_json(&router, "/v1/postings", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(val["error"].as_str().unwrap().contains("BTC unbalanced"));
    }

    #[tokio::test]
    async fn disallowed_direction_for_account_type_rejected() {
        let router = app();
        let _ = post_json(
            &router,
            "/v1/accounts",
            create_account_body("fr", "fee_revenue", "fiat"),
        )
        .await;
        let _ = post_json(
            &router,
            "/v1/accounts",
            create_account_body("op", "operational_fiat", "fiat"),
        )
        .await;
        let body = json!({
            "posting_id": "dirbad",
            "entries": [
                { "account_id": "op", "direction": "credit", "amount": 10, "asset": "USD" },
                { "account_id": "fr", "direction": "sideways", "amount": 10, "asset": "USD" }
            ]
        });
        let (status, val) = post_json(&router, "/v1/postings", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(val["error"].as_str().unwrap().contains("invalid direction"));
    }

    #[tokio::test]
    async fn duplicate_account_id_rejected() {
        let router = app();
        let _ = post_json(
            &router,
            "/v1/accounts",
            create_account_body("dup", "user_custodial", "fiat"),
        )
        .await;
        let (status, _) = post_json(
            &router,
            "/v1/accounts",
            create_account_body("dup", "user_custodial", "fiat"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn empty_entries_rejected() {
        let router = app();
        let (status, _) = post_json(
            &router,
            "/v1/postings",
            json!({ "posting_id": "empty", "entries": [] }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn ledger_pagination_cursor() {
        let router = app();
        setup_two_accounts(&router).await;
        for i in 0..5 {
            let _ = post_json(
                &router,
                "/v1/postings",
                json!({
                    "posting_id": format!("page{}", i),
                    "entries": [
                        { "account_id": "uc", "direction": "debit", "amount": 1, "asset": "USD" },
                        { "account_id": "op", "direction": "credit", "amount": 1, "asset": "USD" }
                    ]
                }),
            )
            .await;
        }
        let (s1, v1) = get_json(&router, "/v1/accounts/uc/ledger?limit=2").await;
        assert_eq!(s1, StatusCode::OK);
        let e1 = v1["entries"].as_array().unwrap();
        assert_eq!(e1.len(), 2);
        let cursor = v1["next_cursor"].as_u64().unwrap();
        let (s2, v2) = get_json(
            &router,
            &format!("/v1/accounts/uc/ledger?limit=2&cursor={}", cursor),
        )
        .await;
        assert_eq!(s2, StatusCode::OK);
        let e2 = v2["entries"].as_array().unwrap();
        assert_eq!(e2.len(), 2);
    }

    #[tokio::test]
    async fn unknown_asset_rejected() {
        let router = app();
        setup_two_accounts(&router).await;
        let (status, val) = post_json(
            &router,
            "/v1/postings",
            json!({
                "posting_id": "unkasset",
                "entries": [
                    { "account_id": "uc", "direction": "debit", "amount": 10, "asset": "WOBBLE" },
                    { "account_id": "op", "direction": "credit", "amount": 10, "asset": "WOBBLE" }
                ]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(val["error"].as_str().unwrap().contains("unknown asset"));
    }

    #[tokio::test]
    async fn hash_chain_anchor_and_global_head() {
        let store = Store::new();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("uc", "user_custodial", "both"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("op", "operational_fiat", "fiat"))
                    .unwrap(),
            )
            .unwrap();
        let req: PostingRequest = serde_json::from_value(balanced_posting_body("anchor1")).unwrap();
        let (resp, _) = store.post(req).unwrap();
        let anchor = store.hash_chain_anchor("anchor1").unwrap();
        assert_eq!(anchor.head_hash, resp.hash_head);
        assert_eq!(store.global_chain_head(), resp.hash_head);
    }

    #[tokio::test]
    async fn verify_chain_passes_clean_db() {
        let store = Store::new();
        assert!(store.verify_chain().is_ok());
    }

    #[tokio::test]
    async fn verify_chain_detects_tamper() {
        let store = Store::new();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("uc", "user_custodial", "both"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("op", "operational_fiat", "fiat"))
                    .unwrap(),
            )
            .unwrap();
        let req: PostingRequest = serde_json::from_value(balanced_posting_body("tamper1")).unwrap();
        store.post(req).unwrap();
        {
            let mut state = store.inner.lock();
            state.entries[0].this_hash = "deadbeef".to_string();
        }
        assert!(store.verify_chain().is_err());
    }

    #[tokio::test]
    async fn user_custodial_sum_matches_entries() {
        let store = Store::new();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("uc1", "user_custodial", "both"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("uc2", "user_custodial", "both"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("op", "operational_fiat", "fiat"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store.post(
            serde_json::from_value(json!({
                "posting_id": "ucs1",
                "entries": [
                    { "account_id": "uc1", "direction": "debit", "amount": 70, "asset": "USD" },
                    { "account_id": "op", "direction": "credit", "amount": 70, "asset": "USD" }
                ]
            }))
            .unwrap(),
        );
        let _ = store.post(
            serde_json::from_value(json!({
                "posting_id": "ucs2",
                "entries": [
                    { "account_id": "uc2", "direction": "debit", "amount": 30, "asset": "USD" },
                    { "account_id": "op", "direction": "credit", "amount": 30, "asset": "USD" }
                ]
            }))
            .unwrap(),
        );
        let sum = store.user_custodial_sum("USD");
        assert_eq!(sum, 100);
    }

    #[tokio::test]
    async fn snapshot_write_and_reconcile() {
        let store = Store::new();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("uc", "user_custodial", "both"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("op", "operational_fiat", "fiat"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .post(serde_json::from_value(balanced_posting_body("snap1")).unwrap())
            .unwrap();
        let snaps = store.write_snapshots();
        assert!(!snaps.is_empty());
        for s in &snaps {
            assert!(store.reconcile_snapshot(s), "snapshot mismatch: {:?}", s);
        }
    }

    #[tokio::test]
    async fn balance_via_snapshot_matches_direct() {
        let store = Store::new();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("uc", "user_custodial", "both"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("op", "operational_fiat", "fiat"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .post(serde_json::from_value(balanced_posting_body("bsnap1")).unwrap())
            .unwrap();
        store.write_snapshots();
        let _ = store
            .post(serde_json::from_value(balanced_posting_body("bsnap2")).unwrap())
            .unwrap();
        let direct = store.balance("uc", "USD").unwrap();
        let via = store.balance_via_snapshot("uc", "USD").unwrap();
        assert_eq!(direct, via);
    }

    #[tokio::test]
    async fn fx_posting_routes_gain_to_fx_gain_loss() {
        let store = Store::new();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("opf", "operational_fiat", "fiat"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("vs", "venue_settlement", "both"))
                    .unwrap(),
            )
            .unwrap();
        let _ = store
            .create_account(
                serde_json::from_value(create_account_body("fx", "fx_gain_loss", "both")).unwrap(),
            )
            .unwrap();
        let _ = store.post(
            serde_json::from_value(json!({
                "posting_id": "fx1",
                "entries": [
                    { "account_id": "vs", "direction": "debit", "amount": 50, "asset": "BTC" },
                    { "account_id": "opf", "direction": "credit", "amount": 50, "asset": "BTC" },
                    { "account_id": "opf", "direction": "debit", "amount": 105, "asset": "USD" },
                    { "account_id": "vs", "direction": "credit", "amount": 100, "asset": "USD" },
                    { "account_id": "fx", "direction": "credit", "amount": 5, "asset": "USD" }
                ]
            }))
            .unwrap(),
        );
        let fx_bal = store.balance("fx", "USD").unwrap();
        assert_eq!(fx_bal, -5);
    }

    #[tokio::test]
    async fn list_accounts_returns_all() {
        let router = app();
        setup_two_accounts(&router).await;
        let (status, val) = get_json(&router, "/v1/accounts").await;
        assert_eq!(status, StatusCode::OK);
        let accounts = val["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 2);
    }

    #[tokio::test]
    async fn list_accounts_filters_by_type() {
        let router = app();
        setup_two_accounts(&router).await;
        let (status, val) = get_json(&router, "/v1/accounts?type=user_custodial").await;
        assert_eq!(status, StatusCode::OK);
        let accounts = val["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["type"], "user_custodial");
    }

    #[tokio::test]
    async fn list_postings_returns_all() {
        let router = app();
        setup_two_accounts(&router).await;
        let _ = post_json(&router, "/v1/postings", balanced_posting_body("lp1")).await;
        let _ = post_json(&router, "/v1/postings", balanced_posting_body("lp2")).await;
        let (status, val) = get_json(&router, "/v1/postings").await;
        assert_eq!(status, StatusCode::OK);
        let postings = val["postings"].as_array().unwrap();
        assert_eq!(postings.len(), 2);
    }

    #[tokio::test]
    async fn list_postings_respects_limit() {
        let router = app();
        setup_two_accounts(&router).await;
        for i in 0..3 {
            let _ = post_json(
                &router,
                "/v1/postings",
                balanced_posting_body(&format!("lplim{}", i)),
            )
            .await;
        }
        let (status, val) = get_json(&router, "/v1/postings?limit=2").await;
        assert_eq!(status, StatusCode::OK);
        let postings = val["postings"].as_array().unwrap();
        assert_eq!(postings.len(), 2);
    }
}
