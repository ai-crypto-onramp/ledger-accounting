use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;

use crate::account::{Account, CreateAccountRequest};
use crate::chart::{self, Direction, GENESIS_HASH};
use crate::posting::{self, EntryRecord, PostingRecord, PostingRequest, PostingResponse};

pub const MAX_ENTRIES_PER_POSTING: usize = 64;
pub const MAX_AMOUNT: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    pub event_id: String,
    pub posting_id: String,
    pub entry_ids: Vec<String>,
    pub hash_head: String,
    pub created_at: String,
}

pub struct LedgerState {
    pub accounts: HashMap<String, Account>,
    pub postings: HashMap<String, PostingRecord>,
    pub entries: Vec<EntryRecord>,
    pub sequence: u64,
    pub audit_events: Vec<AuditEvent>,
}

impl LedgerState {
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
            postings: HashMap::new(),
            entries: Vec::new(),
            sequence: 0,
            audit_events: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<Mutex<LedgerState>>,
}

impl Store {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LedgerState::new())),
        }
    }

    pub fn create_account(&self, req: CreateAccountRequest) -> Result<Account, String> {
        let (type_name, _asset_class) = crate::account::validate(&req)?;

        let mut state = self.inner.lock();
        let account_id = match req.account_id.clone() {
            Some(id) => {
                if state.accounts.contains_key(&id) {
                    return Err(format!("account already exists: {}", id));
                }
                id
            }
            None => uuid::Uuid::new_v4().to_string(),
        };

        let now = now_iso();
        let account = Account {
            account_id: account_id.clone(),
            type_name: type_name.clone(),
            asset_class: req.asset_class.clone(),
            label: req.label.clone(),
            parent_id: req.parent_id.clone(),
            status: "active".to_string(),
            created_at: now,
        };
        state.accounts.insert(account_id, account.clone());
        Ok(account)
    }

    pub fn get_account(&self, account_id: &str) -> Option<Account> {
        self.inner.lock().accounts.get(account_id).cloned()
    }

    pub fn balance(&self, account_id: &str, asset: &str) -> Option<i128> {
        let state = self.inner.lock();
        if !state.accounts.contains_key(account_id) {
            return None;
        }
        Some(compute_balance(&state, account_id, asset))
    }

    pub fn ledger(
        &self,
        account_id: &str,
        from: Option<&str>,
        to: Option<&str>,
        limit: usize,
        cursor: Option<u64>,
    ) -> Result<LedgerPage, String> {
        let state = self.inner.lock();
        if !state.accounts.contains_key(account_id) {
            return Err(format!("account not found: {}", account_id));
        }
        let limit = limit.clamp(1, 200);

        let mut entries: Vec<&EntryRecord> = state
            .entries
            .iter()
            .filter(|e| e.account_id == account_id)
            .filter(|e| {
                from.as_ref()
                    .is_none_or(|f| e.created_at.as_str() >= &f[..])
            })
            .filter(|e| to.as_ref().is_none_or(|t| e.created_at.as_str() <= &t[..]))
            .filter(|e| cursor.as_ref().is_none_or(|c| e.sequence_number > *c))
            .collect();
        entries.sort_by_key(|e| e.sequence_number);

        let running = compute_balance(&state, account_id, "");
        let next_cursor = if entries.len() > limit {
            Some(entries[limit - 1].sequence_number)
        } else {
            None
        };
        let page: Vec<(&EntryRecord, i128)> = entries
            .into_iter()
            .take(limit)
            .scan(0i128, |acc, e| {
                match e.direction.as_str() {
                    "debit" => *acc += e.amount as i128,
                    "credit" => *acc -= e.amount as i128,
                    _ => {}
                }
                Some((e, *acc))
            })
            .collect();

        Ok(LedgerPage {
            account_id: account_id.to_string(),
            entries: page
                .into_iter()
                .map(|(e, bal)| LedgerEntryItem {
                    entry_id: e.entry_id.clone(),
                    posting_id: e.posting_id.clone(),
                    account_id: e.account_id.clone(),
                    direction: e.direction.clone(),
                    amount: e.amount,
                    asset: e.asset.clone(),
                    sequence_number: e.sequence_number,
                    this_hash: e.this_hash.clone(),
                    prev_hash: e.prev_hash.clone(),
                    created_at: e.created_at.clone(),
                    running_balance: bal,
                })
                .collect(),
            next_cursor,
            final_balance: running,
        })
    }

    pub fn post(&self, req: PostingRequest) -> Result<(PostingResponse, bool), PostError> {
        if req.entries.is_empty() {
            return Err(PostError::Validation("entries must be non-empty".into()));
        }
        if req.entries.len() > MAX_ENTRIES_PER_POSTING {
            return Err(PostError::Validation(format!(
                "too many entries: {} > {}",
                req.entries.len(),
                MAX_ENTRIES_PER_POSTING
            )));
        }

        let mut state = self.inner.lock();

        if state.postings.contains_key(&req.posting_id) {
            let existing = state.postings.get(&req.posting_id).unwrap().clone();
            return Ok((
                PostingResponse {
                    posting_id: existing.posting_id,
                    status: existing.status,
                    entry_ids: existing
                        .entries
                        .iter()
                        .map(|e| e.entry_id.clone())
                        .collect(),
                    hash_head: existing.hash_head,
                },
                true,
            ));
        }

        for e in &req.entries {
            if e.amount == 0 {
                return Err(PostError::Validation("amount must be > 0".into()));
            }
            if e.amount > MAX_AMOUNT {
                return Err(PostError::Validation(format!(
                    "amount {} exceeds MAX_AMOUNT {}",
                    e.amount, MAX_AMOUNT
                )));
            }
        }

        let mut entries_parsed: Vec<(String, Direction, u64, String)> =
            Vec::with_capacity(req.entries.len());
        for e in &req.entries {
            let dir = match crate::account::parse_direction(&e.direction) {
                Some(d) => d,
                None => {
                    return Err(PostError::Validation(format!(
                        "invalid direction: {}",
                        e.direction
                    )))
                }
            };
            entries_parsed.push((e.account_id.clone(), dir, e.amount, e.asset.clone()));
        }

        for (account_id, _dir, _amount, _asset) in &entries_parsed {
            match state.accounts.get(account_id) {
                Some(acc) => {
                    if acc.status != "active" {
                        return Err(PostError::Validation(format!(
                            "account not active: {}",
                            account_id
                        )));
                    }
                }
                None => {
                    return Err(PostError::Validation(format!(
                        "account not found: {}",
                        account_id
                    )))
                }
            }
        }

        for (account_id, dir, _amount, _asset) in &entries_parsed {
            let acc = state.accounts.get(account_id).unwrap();
            let account_type = chart::find_type(&acc.type_name).unwrap();
            if !chart::direction_allowed(account_type, *dir) {
                return Err(PostError::Validation(format!(
                    "direction {:?} not allowed for type {}",
                    dir, acc.type_name
                )));
            }
        }

        let mut per_asset: HashMap<String, (i128, i128)> = HashMap::new();
        for (_account_id, dir, amount, asset) in &entries_parsed {
            let entry = per_asset.entry(asset.clone()).or_insert((0, 0));
            match dir {
                Direction::Debit => entry.0 += *amount as i128,
                Direction::Credit => entry.1 += *amount as i128,
            }
        }
        for (asset, (debits, credits)) in &per_asset {
            if debits != credits {
                return Err(PostError::Unbalanced(format!(
                    "asset {} unbalanced: debits={} credits={}",
                    asset, debits, credits
                )));
            }
        }

        let now = now_iso();
        let mut prev_hash = GENESIS_HASH.to_string();
        let mut entry_ids: Vec<String> = Vec::new();
        let mut created_entries: Vec<EntryRecord> = Vec::new();

        for (account_id, dir, amount, asset) in entries_parsed {
            let entry_id = uuid::Uuid::new_v4().to_string();
            state.sequence += 1;
            let seq = state.sequence;
            let canonical = posting::canonical_bytes(
                &prev_hash,
                &entry_id,
                &account_id,
                dir,
                amount,
                &asset,
                &now,
            );
            let this_hash = posting::compute_hash(&prev_hash, &canonical);
            let record = EntryRecord {
                entry_id: entry_id.clone(),
                posting_id: req.posting_id.clone(),
                account_id,
                direction: match dir {
                    Direction::Debit => "debit".to_string(),
                    Direction::Credit => "credit".to_string(),
                },
                amount,
                asset,
                sequence_number: seq,
                prev_hash: prev_hash.clone(),
                this_hash: this_hash.clone(),
                created_at: now.clone(),
            };
            entry_ids.push(entry_id.clone());
            created_entries.push(record);
            prev_hash = this_hash;
        }

        let hash_head = prev_hash;
        let posting_record = PostingRecord {
            posting_id: req.posting_id.clone(),
            ref_tx_id: req.ref_tx_id.clone(),
            memo: req.memo.clone(),
            status: "posted".to_string(),
            hash_head: hash_head.clone(),
            entries: created_entries.clone(),
            created_at: now.clone(),
        };
        state
            .postings
            .insert(req.posting_id.clone(), posting_record);
        state.entries.extend(created_entries);

        let event = AuditEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            posting_id: req.posting_id.clone(),
            entry_ids: entry_ids.clone(),
            hash_head: hash_head.clone(),
            created_at: now,
        };
        state.audit_events.push(event);

        Ok((
            PostingResponse {
                posting_id: req.posting_id,
                status: "posted".to_string(),
                entry_ids,
                hash_head,
            },
            false,
        ))
    }

    pub fn get_posting(&self, posting_id: &str) -> Option<PostingRecord> {
        self.inner.lock().postings.get(posting_id).cloned()
    }

    #[allow(dead_code)]
    pub fn audit_events(&self) -> Vec<AuditEvent> {
        self.inner.lock().audit_events.clone()
    }

    #[allow(dead_code)]
    pub fn entry_count(&self) -> usize {
        self.inner.lock().entries.len()
    }
}

#[derive(Debug)]
pub enum PostError {
    Validation(String),
    Unbalanced(String),
}

impl PostError {
    pub fn status(&self) -> u16 {
        match self {
            PostError::Validation(_) => 400,
            PostError::Unbalanced(_) => 400,
        }
    }

    pub fn message(&self) -> String {
        match self {
            PostError::Validation(s) => s.clone(),
            PostError::Unbalanced(s) => s.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LedgerPage {
    pub account_id: String,
    pub entries: Vec<LedgerEntryItem>,
    pub next_cursor: Option<u64>,
    pub final_balance: i128,
}

#[derive(Debug, Clone, Serialize)]
pub struct LedgerEntryItem {
    pub entry_id: String,
    pub posting_id: String,
    pub account_id: String,
    pub direction: String,
    pub amount: u64,
    pub asset: String,
    pub sequence_number: u64,
    pub this_hash: String,
    pub prev_hash: String,
    pub created_at: String,
    pub running_balance: i128,
}

fn compute_balance(state: &LedgerState, account_id: &str, asset: &str) -> i128 {
    state
        .entries
        .iter()
        .filter(|e| e.account_id == account_id)
        .filter(|e| asset.is_empty() || e.asset == asset)
        .fold(0i128, |acc, e| match e.direction.as_str() {
            "debit" => acc + e.amount as i128,
            "credit" => acc - e.amount as i128,
            _ => acc,
        })
}

pub fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}", secs)
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}
