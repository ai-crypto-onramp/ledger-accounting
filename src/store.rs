use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use sqlx::FromRow;

use crate::account::{Account, CreateAccountRequest};
use crate::chart::{self, Direction, GENESIS_HASH};
use crate::posting::{self, EntryRecord, PostingRecord, PostingRequest, PostingResponse};
use crate::snapshot::BalanceSnapshot;

pub const MAX_ENTRIES_PER_POSTING: usize = 64;
pub const MAX_AMOUNT: u64 = 1_000_000_000_000;

pub const MIGRATION_INIT_SCHEMA: &str =
    include_str!("../migrations/20240101000001_init_schema.sql");
pub const MIGRATION_SET_SERIALIZABLE: &str =
    include_str!("../migrations/20240101000002_set_serializable.sql");

#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    pub event_id: String,
    pub posting_id: String,
    pub entry_ids: Vec<String>,
    pub hash_head: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HashChainAnchor {
    pub posting_id: String,
    pub head_hash: String,
    pub global_sequence_head: String,
    pub created_at: String,
}

pub struct LedgerState {
    pub accounts: HashMap<String, Account>,
    pub postings: HashMap<String, PostingRecord>,
    pub entries: Vec<EntryRecord>,
    pub sequence: u64,
    pub audit_events: Vec<AuditEvent>,
    pub hash_chain_anchors: HashMap<String, HashChainAnchor>,
    pub global_chain_head: String,
    pub snapshots: Vec<BalanceSnapshot>,
}

impl Default for LedgerState {
    fn default() -> Self {
        Self::new()
    }
}

impl LedgerState {
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
            postings: HashMap::new(),
            entries: Vec::new(),
            sequence: 0,
            audit_events: Vec::new(),
            hash_chain_anchors: HashMap::new(),
            global_chain_head: GENESIS_HASH.to_string(),
            snapshots: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct Store {
    pub inner: Arc<Mutex<LedgerState>>,
    pub pool: Option<sqlx::PgPool>,
}

impl Store {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LedgerState::new())),
            pool: None,
        }
    }

    pub fn with_pool(pool: sqlx::PgPool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LedgerState::new())),
            pool: Some(pool),
        }
    }

    pub async fn connect(db_url: &str) -> Result<Self, sqlx::Error> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(16)
            .connect(db_url)
            .await?;
        Ok(Self::with_pool(pool))
    }

    pub async fn run_migrations(&self) -> Result<(), String> {
        let pool = match &self.pool {
            Some(p) => p,
            None => return Ok(()),
        };
        if let Err(e) = sqlx::raw_sql(MIGRATION_INIT_SCHEMA).execute(pool).await {
            if is_already_applied(&e) {
                return Ok(());
            }
            return Err(format!("migration init_schema failed: {}", e));
        }
        if let Err(e) = sqlx::raw_sql(MIGRATION_SET_SERIALIZABLE)
            .execute(pool)
            .await
        {
            if is_already_applied(&e) {
                return Ok(());
            }
            return Err(format!("migration set_serializable failed: {}", e));
        }
        Ok(())
    }

    pub async fn hydrate(&self) -> Result<(), String> {
        let pool = match &self.pool {
            Some(p) => p,
            None => return Ok(()),
        };

        let accounts: Vec<AccountRow> = sqlx::query_as::<_, AccountRow>(
            "SELECT account_id, type_name AS type_field, asset_class, label, parent_id, status, floor(extract(epoch from created_at))::bigint::text AS created_at FROM accounts ORDER BY created_at",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| format!("hydrate accounts failed: {}", e))?;
        let mut new_accounts: HashMap<String, Account> = HashMap::new();
        for a in accounts {
            new_accounts.insert(
                a.account_id.clone(),
                Account {
                    account_id: a.account_id,
                    type_name: a.type_field,
                    asset_class: a.asset_class,
                    label: a.label,
                    parent_id: a.parent_id,
                    status: a.status,
                    created_at: a.created_at,
                },
            );
        }

        let entries: Vec<EntryRow> = sqlx::query_as::<_, EntryRow>(
            "SELECT entry_id, posting_id, account_id, direction, amount::text AS amount, asset, sequence_number, prev_hash, this_hash, floor(extract(epoch from created_at))::bigint::text AS created_at FROM entries ORDER BY sequence_number",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| format!("hydrate entries failed: {}", e))?;
        let mut by_posting: std::collections::HashMap<String, Vec<EntryRecord>> =
            std::collections::HashMap::new();
        let mut new_entries: Vec<EntryRecord> = Vec::with_capacity(entries.len());
        let mut new_sequence: u64 = 0;
        for e in entries {
            let amount: u64 = e
                .amount
                .parse()
                .map_err(|_| "hydrate: invalid amount".to_string())?;
            let record = EntryRecord {
                entry_id: e.entry_id,
                posting_id: e.posting_id.clone(),
                account_id: e.account_id,
                direction: e.direction,
                amount,
                asset: e.asset,
                sequence_number: e.sequence_number as u64,
                prev_hash: e.prev_hash,
                this_hash: e.this_hash,
                created_at: e.created_at,
            };
            if record.sequence_number > new_sequence {
                new_sequence = record.sequence_number;
            }
            by_posting
                .entry(record.posting_id.clone())
                .or_default()
                .push(record.clone());
            new_entries.push(record);
        }
        let new_global_chain_head = new_entries
            .last()
            .map(|e| e.this_hash.clone())
            .unwrap_or_else(|| GENESIS_HASH.to_string());

        let postings: Vec<PostingRow> = sqlx::query_as::<_, PostingRow>(
            "SELECT posting_id, ref_tx_id, memo, status, hash_chain_head, floor(extract(epoch from created_at))::bigint::text AS created_at FROM postings ORDER BY created_at",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| format!("hydrate postings failed: {}", e))?;
        let mut new_postings: HashMap<String, PostingRecord> = HashMap::new();
        for p in postings {
            let entries = by_posting.remove(&p.posting_id).unwrap_or_default();
            new_postings.insert(
                p.posting_id.clone(),
                PostingRecord {
                    posting_id: p.posting_id,
                    ref_tx_id: p.ref_tx_id,
                    memo: p.memo,
                    status: p.status,
                    hash_head: p.hash_chain_head,
                    entries,
                    created_at: p.created_at,
                },
            );
        }

        let anchors: Vec<AnchorRow> = sqlx::query_as::<_, AnchorRow>(
            "SELECT posting_id, head_hash, global_sequence_head, floor(extract(epoch from created_at))::bigint::text AS created_at FROM hash_chain",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| format!("hydrate anchors failed: {}", e))?;
        let mut new_anchors: HashMap<String, HashChainAnchor> = HashMap::new();
        for a in anchors {
            new_anchors.insert(
                a.posting_id.clone(),
                HashChainAnchor {
                    posting_id: a.posting_id,
                    head_hash: a.head_hash,
                    global_sequence_head: a.global_sequence_head,
                    created_at: a.created_at,
                },
            );
        }

        let snapshots: Vec<SnapshotRow> = sqlx::query_as::<_, SnapshotRow>(
            "SELECT account_id, asset, balance::text AS balance, floor(extract(epoch from as_of_ts))::bigint::text AS as_of_ts, last_entry_id FROM balance_snapshots",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| format!("hydrate snapshots failed: {}", e))?;
        let mut new_snapshots: Vec<BalanceSnapshot> = Vec::with_capacity(snapshots.len());
        for s in snapshots {
            let balance: i128 = s
                .balance
                .parse()
                .map_err(|_| "hydrate: invalid snapshot balance".to_string())?;
            let row: (String,) = sqlx::query_as("SELECT entry_id FROM entries WHERE entry_id = $1")
                .bind(&s.last_entry_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| format!("hydrate snapshot last_entry lookup failed: {}", e))?
                .unwrap_or_default();
            let last_entry_id = row.0;
            let last_sequence: (i64,) =
                sqlx::query_as("SELECT sequence_number FROM entries WHERE entry_id = $1")
                    .bind(&s.last_entry_id)
                    .fetch_optional(pool)
                    .await
                    .map_err(|e| format!("hydrate snapshot last_seq lookup failed: {}", e))?
                    .unwrap_or((0,));
            new_snapshots.push(BalanceSnapshot {
                account_id: s.account_id,
                asset: s.asset,
                balance,
                as_of_ts: s.as_of_ts,
                last_entry_id,
                last_sequence: last_sequence.0 as u64,
            });
        }

        let mut state = self.inner.lock();
        state.accounts = new_accounts;
        state.postings = new_postings;
        state.entries = new_entries;
        state.hash_chain_anchors = new_anchors;
        state.snapshots = new_snapshots;
        state.sequence = new_sequence;
        state.global_chain_head = new_global_chain_head;

        Ok(())
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
        drop(state);

        if let Some(pool) = &self.pool {
            let mut tx =
                block_on(pool.begin()).map_err(|e| format!("create_account: begin tx: {}", e))?;
            block_on(
                sqlx::query(
                    "INSERT INTO accounts (account_id, type_name, asset_class, label, parent_id, status, created_at)
                 VALUES ($1, $2, $3, $4, $5, 'active', to_timestamp($6))
                 ON CONFLICT (account_id) DO NOTHING",
                )
                .bind(&account.account_id)
                .bind(&account.type_name)
                .bind(&account.asset_class)
                .bind(&account.label)
                .bind(account.parent_id.as_ref())
                .bind(secs(&account.created_at))
                .execute(&mut *tx),
            )
            .map_err(|e| format!("create_account: insert: {}", e))?;
            block_on(tx.commit()).map_err(|e| format!("create_account: commit: {}", e))?;
        }

        Ok(account)
    }

    pub fn get_account(&self, account_id: &str) -> Option<Account> {
        self.inner.lock().accounts.get(account_id).cloned()
    }

    pub fn list_accounts(&self, type_filter: Option<&str>) -> Vec<Account> {
        let state = self.inner.lock();
        let mut out: Vec<Account> = state
            .accounts
            .values()
            .filter(|a| type_filter.is_none_or(|t| a.type_name == t))
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.account_id.cmp(&b.account_id))
        });
        out
    }

    pub fn list_postings(&self, limit: usize) -> Vec<PostingRecord> {
        let state = self.inner.lock();
        let limit = limit.clamp(1, 200);
        let mut out: Vec<PostingRecord> = state.postings.values().cloned().collect();
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.posting_id.cmp(&b.posting_id))
        });
        out.truncate(limit);
        out
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
            if let Err(msg) = crate::asset::validate_amount(&e.asset, e.amount) {
                return Err(PostError::Validation(msg));
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
        let mut prev_hash = state.global_chain_head.clone();
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
        let global_sequence_head = hash_head.clone();
        let posting_record = PostingRecord {
            posting_id: req.posting_id.clone(),
            ref_tx_id: req.ref_tx_id.clone(),
            memo: req.memo.clone(),
            status: "posted".to_string(),
            hash_head: hash_head.clone(),
            entries: created_entries.clone(),
            created_at: now.clone(),
        };

        if let Some(pool) = &self.pool {
            let mut tx = block_on(pool.begin())
                .map_err(|e| PostError::Validation(format!("post: begin tx: {}", e)))?;
            let inserted_posting = block_on(
                sqlx::query(
                    "INSERT INTO postings (posting_id, ref_tx_id, memo, status, hash_chain_head, created_at)
                 VALUES ($1, $2, $3, 'posted', $4, to_timestamp($5))
                 ON CONFLICT (posting_id) DO NOTHING",
                )
                .bind(&posting_record.posting_id)
                .bind(posting_record.ref_tx_id.as_ref())
                .bind(posting_record.memo.as_ref())
                .bind(&posting_record.hash_head)
                .bind(secs(&posting_record.created_at))
                .execute(&mut *tx),
            )
            .map_err(|e| PostError::Validation(format!("post: insert posting: {}", e)))?;
            if inserted_posting.rows_affected() == 0 {
                drop(tx);
                let existing = block_on(
                    sqlx::query_as::<_, PostingRow>(
                        "SELECT posting_id, ref_tx_id, memo, status, hash_chain_head, created_at
                         FROM postings WHERE posting_id = $1",
                    )
                    .bind(&req.posting_id)
                    .fetch_one(pool),
                )
                .map_err(|e| PostError::Validation(format!("post: fetch existing: {}", e)))?;
                let entries: Vec<EntryRow> = block_on(
                    sqlx::query_as::<_, EntryRow>(
                        "SELECT entry_id, posting_id, account_id, direction, amount::text AS amount, asset, sequence_number, prev_hash, this_hash, created_at
                         FROM entries WHERE posting_id = $1 ORDER BY sequence_number",
                    )
                    .bind(&req.posting_id)
                    .fetch_all(pool),
                )
                .map_err(|e| PostError::Validation(format!("post: fetch existing entries: {}", e)))?;
                let entry_ids: Vec<String> = entries.iter().map(|e| e.entry_id.clone()).collect();
                let replay_resp = PostingResponse {
                    posting_id: existing.posting_id,
                    status: existing.status,
                    entry_ids,
                    hash_head: existing.hash_chain_head,
                };
                let mut st = self.inner.lock();
                if !st.postings.contains_key(&replay_resp.posting_id) {
                    st.postings
                        .insert(replay_resp.posting_id.clone(), posting_record.clone());
                    for e in entries {
                        let amount: u64 = e.amount.parse().unwrap_or(0);
                        st.entries.push(EntryRecord {
                            entry_id: e.entry_id,
                            posting_id: e.posting_id,
                            account_id: e.account_id,
                            direction: e.direction,
                            amount,
                            asset: e.asset,
                            sequence_number: e.sequence_number as u64,
                            prev_hash: e.prev_hash,
                            this_hash: e.this_hash,
                            created_at: e.created_at,
                        });
                    }
                }
                return Ok((replay_resp, true));
            }

            for e in &created_entries {
                block_on(
                    sqlx::query(
                        "INSERT INTO entries (entry_id, posting_id, account_id, direction, amount, asset, sequence_number, prev_hash, this_hash, created_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, to_timestamp($10))",
                    )
                    .bind(&e.entry_id)
                    .bind(&e.posting_id)
                    .bind(&e.account_id)
                    .bind(&e.direction)
                    .bind(e.amount as i64)
                    .bind(&e.asset)
                    .bind(e.sequence_number as i64)
                    .bind(&e.prev_hash)
                    .bind(&e.this_hash)
                    .bind(secs(&e.created_at))
                    .execute(&mut *tx),
                )
                .map_err(|err| PostError::Validation(format!("post: insert entry: {}", err)))?;
            }

            block_on(
                sqlx::query(
                    "INSERT INTO hash_chain (posting_id, head_hash, global_sequence_head, created_at)
                 VALUES ($1, $2, $3, to_timestamp($4))
                 ON CONFLICT (posting_id) DO NOTHING",
                )
                .bind(&req.posting_id)
                .bind(&hash_head)
                .bind(&global_sequence_head)
                .bind(secs(&now))
                .execute(&mut *tx),
            )
            .map_err(|e| PostError::Validation(format!("post: insert anchor: {}", e)))?;

            if let Err(e) = block_on(tx.commit()) {
                let mut st = self.inner.lock();
                let entry_ids_set: std::collections::HashSet<String> =
                    entry_ids.iter().cloned().collect();
                st.entries.retain(|e| !entry_ids_set.contains(&e.entry_id));
                st.postings.remove(&req.posting_id);
                st.hash_chain_anchors.remove(&req.posting_id);
                st.global_chain_head = st
                    .entries
                    .last()
                    .map(|e| e.this_hash.clone())
                    .unwrap_or_else(|| GENESIS_HASH.to_string());
                return Err(PostError::Validation(format!("post: commit: {}", e)));
            }
        }

        state
            .postings
            .insert(req.posting_id.clone(), posting_record);
        state.entries.extend(created_entries);

        let anchor = HashChainAnchor {
            posting_id: req.posting_id.clone(),
            head_hash: hash_head.clone(),
            global_sequence_head: global_sequence_head.clone(),
            created_at: now.clone(),
        };
        state
            .hash_chain_anchors
            .insert(req.posting_id.clone(), anchor);
        state.global_chain_head = global_sequence_head.clone();

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
    pub fn record_audit_event(&self, event: AuditEvent) {
        self.inner.lock().audit_events.push(event);
    }

    #[allow(dead_code)]
    pub fn entry_count(&self) -> usize {
        self.inner.lock().entries.len()
    }

    pub fn hash_chain_anchor(&self, posting_id: &str) -> Option<HashChainAnchor> {
        self.inner
            .lock()
            .hash_chain_anchors
            .get(posting_id)
            .cloned()
    }

    pub fn global_chain_head(&self) -> String {
        self.inner.lock().global_chain_head.clone()
    }

    pub fn verify_chain(&self) -> Result<(), crate::hashchain::ChainBreak> {
        let state = self.inner.lock();
        crate::hashchain::verify_chain(&state)
    }

    pub fn user_custodial_sum(&self, asset: &str) -> i128 {
        let state = self.inner.lock();
        let mut total: i128 = 0;
        for (account_id, acc) in state.accounts.iter() {
            if acc.type_name == "user_custodial" {
                total += compute_balance(&state, account_id, asset);
            }
        }
        total
    }

    pub fn write_snapshots(&self) -> Vec<BalanceSnapshot> {
        let mut state = self.inner.lock();
        let snaps = crate::snapshot::snapshot_all(&state);
        state.snapshots = snaps.clone();
        drop(state);

        if let Some(pool) = &self.pool {
            for s in &snaps {
                let _ = block_on(
                    sqlx::query(
                        "INSERT INTO balance_snapshots (account_id, asset, balance, as_of_ts, last_entry_id)
                     VALUES ($1, $2, $3::numeric, to_timestamp($4), $5)
                     ON CONFLICT (account_id, asset, as_of_ts) DO NOTHING",
                    )
                    .bind(&s.account_id)
                    .bind(&s.asset)
                    .bind(s.balance.to_string())
                    .bind(secs(&s.as_of_ts))
                    .bind(&s.last_entry_id)
                    .execute(pool),
                );
            }
        }

        snaps
    }

    pub fn latest_snapshot(&self, account_id: &str, asset: &str) -> Option<BalanceSnapshot> {
        let state = self.inner.lock();
        state
            .snapshots
            .iter()
            .filter(|s| s.account_id == account_id)
            .rfind(|s| asset.is_empty() || s.asset == asset)
            .cloned()
    }

    pub fn balance_via_snapshot(&self, account_id: &str, asset: &str) -> Option<i128> {
        let state = self.inner.lock();
        if !state.accounts.contains_key(account_id) {
            return None;
        }
        let snap = state
            .snapshots
            .iter()
            .filter(|s| s.account_id == account_id)
            .rfind(|s| asset.is_empty() || s.asset == asset);
        match snap {
            Some(s) => {
                let mut bal = s.balance;
                let last_seq = s.last_sequence;
                let delta: i128 = state
                    .entries
                    .iter()
                    .filter(|e| e.account_id == account_id)
                    .filter(|e| asset.is_empty() || e.asset == asset)
                    .filter(|e| e.sequence_number > last_seq)
                    .fold(0i128, |acc, e| {
                        let d = match e.direction.as_str() {
                            "debit" => e.amount as i128,
                            "credit" => -(e.amount as i128),
                            _ => 0,
                        };
                        acc + d
                    });
                bal += delta;
                Some(bal)
            }
            None => Some(compute_balance(&state, account_id, asset)),
        }
    }

    pub fn reconcile_snapshot(&self, snap: &BalanceSnapshot) -> bool {
        let state = self.inner.lock();
        crate::snapshot::reconcile_snapshot(&state, snap)
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

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

fn secs(s: &str) -> i64 {
    s.parse::<i64>().unwrap_or(0)
}

fn is_already_applied(err: &sqlx::Error) -> bool {
    if let Some(db) = err.as_database_error() {
        if let Some(code) = db.code() {
            return code == "42710" || code == "42P07";
        }
    }
    false
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, FromRow)]
struct AccountRow {
    account_id: String,
    type_field: String,
    asset_class: String,
    label: String,
    parent_id: Option<String>,
    status: String,
    created_at: String,
}

#[derive(Debug, Clone, FromRow)]
struct PostingRow {
    posting_id: String,
    ref_tx_id: Option<String>,
    memo: Option<String>,
    status: String,
    hash_chain_head: String,
    created_at: String,
}

#[derive(Debug, Clone, FromRow)]
struct EntryRow {
    entry_id: String,
    posting_id: String,
    account_id: String,
    direction: String,
    amount: String,
    asset: String,
    sequence_number: i64,
    prev_hash: String,
    this_hash: String,
    created_at: String,
}

#[derive(Debug, Clone, FromRow)]
struct AnchorRow {
    posting_id: String,
    head_hash: String,
    global_sequence_head: String,
    created_at: String,
}

#[derive(Debug, Clone, FromRow)]
struct SnapshotRow {
    account_id: String,
    asset: String,
    balance: String,
    as_of_ts: String,
    last_entry_id: String,
}
