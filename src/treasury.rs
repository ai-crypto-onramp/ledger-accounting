use serde::Serialize;

use crate::posting::PostingRequest;
use crate::store::Store;

#[derive(Debug, Clone, Serialize)]
pub struct TreasuryPattern {
    pub name: &'static str,
    pub posting_id: String,
}

pub fn hedge_entry(
    store: &Store,
    posting_id: &str,
    treasury_account: &str,
    venue_account: &str,
    asset: &str,
    amount: u64,
) -> Result<(), String> {
    let req = PostingRequest {
        posting_id: posting_id.to_string(),
        entries: vec![
            crate::posting::EntryInput {
                account_id: treasury_account.to_string(),
                direction: "debit".to_string(),
                amount,
                asset: asset.to_string(),
            },
            crate::posting::EntryInput {
                account_id: venue_account.to_string(),
                direction: "credit".to_string(),
                amount,
                asset: asset.to_string(),
            },
        ],
        memo: Some(format!("hedge:{}", posting_id)),
        ref_tx_id: Some(posting_id.to_string()),
    };
    store.post(req).map(|_| ()).map_err(|e| e.message())
}

pub fn rebalance(
    store: &Store,
    posting_id: &str,
    from_account: &str,
    to_account: &str,
    asset: &str,
    amount: u64,
) -> Result<(), String> {
    let req = PostingRequest {
        posting_id: posting_id.to_string(),
        entries: vec![
            crate::posting::EntryInput {
                account_id: to_account.to_string(),
                direction: "debit".to_string(),
                amount,
                asset: asset.to_string(),
            },
            crate::posting::EntryInput {
                account_id: from_account.to_string(),
                direction: "credit".to_string(),
                amount,
                asset: asset.to_string(),
            },
        ],
        memo: Some(format!("rebalance:{}", posting_id)),
        ref_tx_id: Some(posting_id.to_string()),
    };
    store.post(req).map(|_| ()).map_err(|e| e.message())
}

pub fn derive_posting_id(saga_id: &str, step: &str) -> String {
    format!("{}_{}", saga_id, step)
}
