use crate::chart::Direction;
use crate::chart::GENESIS_HASH;
use crate::posting;
use crate::store::LedgerState;

#[derive(Debug, Clone)]
pub struct ChainBreak {
    pub entry_id: String,
    pub reason: String,
}

pub fn verify_chain(state: &LedgerState) -> Result<(), ChainBreak> {
    let mut prev_hash = GENESIS_HASH.to_string();
    for e in &state.entries {
        let dir = match e.direction.as_str() {
            "debit" => Direction::Debit,
            "credit" => Direction::Credit,
            other => {
                return Err(ChainBreak {
                    entry_id: e.entry_id.clone(),
                    reason: format!("invalid direction: {}", other),
                })
            }
        };
        let canonical = posting::canonical_bytes(
            &prev_hash,
            &e.entry_id,
            &e.account_id,
            dir,
            e.amount,
            &e.asset,
            &e.created_at,
        );
        let expected = posting::compute_hash(&prev_hash, &canonical);
        if e.prev_hash != prev_hash {
            return Err(ChainBreak {
                entry_id: e.entry_id.clone(),
                reason: format!(
                    "prev_hash mismatch: expected {}, got {}",
                    prev_hash, e.prev_hash
                ),
            });
        }
        if e.this_hash != expected {
            return Err(ChainBreak {
                entry_id: e.entry_id.clone(),
                reason: format!(
                    "this_hash mismatch: expected {}, got {}",
                    expected, e.this_hash
                ),
            });
        }
        prev_hash = e.this_hash.clone();
    }
    Ok(())
}

pub fn global_head(state: &LedgerState) -> String {
    state
        .entries
        .last()
        .map(|e| e.this_hash.clone())
        .unwrap_or_else(|| GENESIS_HASH.to_string())
}

pub fn posting_head(state: &LedgerState, posting_id: &str) -> Option<String> {
    state.postings.get(posting_id).map(|p| p.hash_head.clone())
}
