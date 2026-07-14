use serde::Serialize;

use crate::posting::EntryRecord;
use crate::store::{now_iso, LedgerState};

#[derive(Debug, Clone, Serialize)]
pub struct BalanceSnapshot {
    pub account_id: String,
    pub asset: String,
    pub balance: i128,
    pub as_of_ts: String,
    pub last_entry_id: String,
    pub last_sequence: u64,
}

pub fn build_snapshot(state: &LedgerState, account_id: &str, asset: &str) -> BalanceSnapshot {
    let (balance, last_entry_id, last_sequence) = state
        .entries
        .iter()
        .filter(|e| e.account_id == account_id)
        .filter(|e| asset.is_empty() || e.asset == asset)
        .fold((0i128, String::new(), 0u64), |(bal, _last, _seq), e| {
            let delta = match e.direction.as_str() {
                "debit" => e.amount as i128,
                "credit" => -(e.amount as i128),
                _ => 0,
            };
            (bal + delta, e.entry_id.clone(), e.sequence_number)
        });
    BalanceSnapshot {
        account_id: account_id.to_string(),
        asset: if asset.is_empty() {
            "all".to_string()
        } else {
            asset.to_string()
        },
        balance,
        as_of_ts: now_iso(),
        last_entry_id,
        last_sequence,
    }
}

pub fn snapshot_all(state: &LedgerState) -> Vec<BalanceSnapshot> {
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut snaps: Vec<BalanceSnapshot> = Vec::new();
    for e in state.entries.iter() {
        let key = (e.account_id.clone(), e.asset.clone());
        if seen.insert(key) {
            let snap = build_snapshot(state, &e.account_id, &e.asset);
            snaps.push(snap);
        }
    }
    snaps
}

pub fn reconcile_snapshot(state: &LedgerState, snap: &BalanceSnapshot) -> bool {
    let computed = build_snapshot(state, &snap.account_id, &snap.asset);
    computed.balance == snap.balance && computed.last_entry_id == snap.last_entry_id
}

pub fn last_entry_id_before(
    state: &LedgerState,
    account_id: &str,
    asset: &str,
    as_of: &str,
) -> String {
    state
        .entries
        .iter()
        .filter(|e| e.account_id == account_id)
        .filter(|e| asset.is_empty() || e.asset == asset)
        .filter(|e| e.created_at.as_str() <= as_of)
        .map(|e| e.entry_id.clone())
        .next_back()
        .unwrap_or_default()
}

pub fn entries_since<'a>(state: &'a LedgerState, after_entry_id: &str) -> Vec<&'a EntryRecord> {
    if after_entry_id.is_empty() {
        return state.entries.iter().collect();
    }
    let mut found = false;
    state
        .entries
        .iter()
        .filter(|e| {
            if found {
                true
            } else if e.entry_id == after_entry_id {
                found = true;
                false
            } else {
                false
            }
        })
        .collect()
}
