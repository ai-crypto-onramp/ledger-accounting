use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::account::{AccountResponse, CreateAccountRequest};
use crate::chart::{CHART, GENESIS_HASH};
use crate::posting::PostingRequest;
use crate::store::{LedgerPage, Store};

pub fn router(store: Store) -> axum::Router {
    axum::Router::new()
        .merge(read_router(store.clone()))
        .merge(write_router(store))
}

pub fn read_router(store: Store) -> axum::Router {
    axum::Router::new()
        .route("/healthz", axum::routing::get(healthz))
        .route("/readyz", axum::routing::get(readyz))
        .route(
            "/v1/chart-of-accounts",
            axum::routing::get(chart_of_accounts),
        )
        .route(
            "/v1/accounts/:id/balance",
            axum::routing::get(account_balance),
        )
        .route(
            "/v1/accounts/:id/ledger",
            axum::routing::get(account_ledger),
        )
        .route("/v1/postings/:id", axum::routing::get(get_posting))
        .route(
            "/v1/reconciliation/user-custodial-sum",
            axum::routing::get(user_custodial_sum),
        )
        .route("/v1/chain/verify", axum::routing::get(verify_chain))
        .with_state(store)
}

pub fn write_router(store: Store) -> axum::Router {
    axum::Router::new()
        .route("/v1/accounts", axum::routing::post(create_account))
        .route("/v1/postings", axum::routing::post(create_posting))
        .with_state(store)
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn readyz() -> Json<Value> {
    Json(json!({ "status": "ready" }))
}

async fn chart_of_accounts(State(_store): State<Store>) -> Json<Value> {
    Json(json!({
        "version": "1.0.0",
        "genesis_hash": GENESIS_HASH,
        "account_types": CHART,
    }))
}

async fn create_account(
    State(store): State<Store>,
    Json(req): Json<CreateAccountRequest>,
) -> Result<(StatusCode, Json<AccountResponse>), (StatusCode, Json<Value>)> {
    match store.create_account(req) {
        Ok(account) => Ok((
            StatusCode::CREATED,
            Json(AccountResponse {
                account_id: account.account_id,
            }),
        )),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(json!({ "error": e })))),
    }
}

#[derive(Debug, Deserialize)]
struct BalanceQuery {
    asset: Option<String>,
}

async fn account_balance(
    State(store): State<Store>,
    Path(id): Path<String>,
    Query(q): Query<BalanceQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let asset = q.asset.unwrap_or_default();
    match store.balance(&id, &asset) {
        Some(bal) => {
            let account = store.get_account(&id);
            let as_of_ts = account
                .map(|a| a.created_at)
                .unwrap_or_else(crate::store::now_iso);
            Ok(Json(json!({
                "account_id": id,
                "asset": if asset.is_empty() { "all" } else { &asset },
                "balance": bal.to_string(),
                "as_of_ts": as_of_ts,
            })))
        }
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("account not found: {}", id) })),
        )),
    }
}

#[derive(Debug, Deserialize)]
struct LedgerQuery {
    from: Option<String>,
    to: Option<String>,
    limit: Option<usize>,
    cursor: Option<u64>,
}

async fn account_ledger(
    State(store): State<Store>,
    Path(id): Path<String>,
    Query(q): Query<LedgerQuery>,
) -> Result<Json<LedgerPage>, (StatusCode, Json<Value>)> {
    let limit = q.limit.unwrap_or(50);
    match store.ledger(&id, q.from.as_deref(), q.to.as_deref(), limit, q.cursor) {
        Ok(page) => Ok(Json(page)),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(json!({ "error": e })))),
    }
}

async fn create_posting(
    State(store): State<Store>,
    Json(req): Json<PostingRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    match store.post(req) {
        Ok((resp, replay)) => {
            let status = if replay {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            Ok((
                status,
                Json(json!({
                    "posting_id": resp.posting_id,
                    "status": resp.status,
                    "entry_ids": resp.entry_ids,
                    "hash_head": resp.hash_head,
                })),
            ))
        }
        Err(e) => {
            let code = match e.status() {
                400 => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            Err((code, Json(json!({ "error": e.message() }))))
        }
    }
}

async fn get_posting(
    State(store): State<Store>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match store.get_posting(&id) {
        Some(p) => Ok(Json(serde_json::to_value(&p).unwrap())),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("posting not found: {}", id) })),
        )),
    }
}

#[derive(Debug, Deserialize)]
struct UserCustodialQuery {
    asset: Option<String>,
}

async fn user_custodial_sum(
    State(store): State<Store>,
    Query(q): Query<UserCustodialQuery>,
) -> Json<Value> {
    let asset = q.asset.unwrap_or_default();
    let sum = store.user_custodial_sum(&asset);
    Json(json!({
        "asset": if asset.is_empty() { "all" } else { &asset },
        "user_custodial_sum": sum.to_string(),
    }))
}

async fn verify_chain(State(store): State<Store>) -> Json<Value> {
    match store.verify_chain() {
        Ok(()) => Json(json!({ "ok": true })),
        Err(b) => Json(json!({
            "ok": false,
            "entry_id": b.entry_id,
            "reason": b.reason,
        })),
    }
}
