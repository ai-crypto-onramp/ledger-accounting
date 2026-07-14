use tonic::{Request, Response, Status};

use crate::account::CreateAccountRequest as AcctReq;
use crate::posting::PostingRequest as PostReq;
use crate::store::Store;

pub mod ledger {
    tonic::include_proto!("ledger.v1");
}

use ledger::ledger_server::{Ledger, LedgerServer};
use ledger::{
    BalanceResponse, CreateAccountRequest as PbCreateAccountRequest, CreateAccountResponse,
    GetBalanceRequest, GetPostingRequest, PostingRecord as PbPostingRecord,
    PostingRequest as PbPostingRequest, PostingResponse as PbPostingResponse, VerifyChainRequest,
    VerifyChainResponse,
};

pub struct LedgerGrpc {
    pub store: Store,
    pub allowed_callers: Vec<String>,
}

impl LedgerGrpc {
    pub fn new(store: Store, allowed_callers: Vec<String>) -> Self {
        Self {
            store,
            allowed_callers,
        }
    }

    #[allow(clippy::result_large_err)]
    fn authorize(&self, caller: Option<&str>) -> Result<(), Status> {
        match caller {
            Some(c) if self.allowed_callers.iter().any(|a| a == c) => Ok(()),
            _ => Err(Status::unauthenticated(
                "caller is not an authorized orchestrator",
            )),
        }
    }
}

#[tonic::async_trait]
impl Ledger for LedgerGrpc {
    async fn create_account(
        &self,
        req: Request<PbCreateAccountRequest>,
    ) -> Result<Response<CreateAccountResponse>, Status> {
        self.authorize(req.metadata().get("x-caller").and_then(|v| v.to_str().ok()))?;
        let req = req.into_inner();
        let account_req = AcctReq {
            account_id: req.account_id,
            type_name: req.r#type,
            asset_class: req.asset_class,
            label: req.label,
            parent_id: req.parent_id,
        };
        match self.store.create_account(account_req) {
            Ok(acc) => Ok(Response::new(CreateAccountResponse {
                account_id: acc.account_id,
            })),
            Err(e) => Err(Status::invalid_argument(e)),
        }
    }

    async fn post_posting(
        &self,
        req: Request<PbPostingRequest>,
    ) -> Result<Response<PbPostingResponse>, Status> {
        self.authorize(req.metadata().get("x-caller").and_then(|v| v.to_str().ok()))?;
        let req = req.into_inner();
        let entries: Vec<crate::posting::EntryInput> = req
            .entries
            .into_iter()
            .map(|e| crate::posting::EntryInput {
                account_id: e.account_id,
                direction: e.direction,
                amount: e.amount,
                asset: e.asset,
            })
            .collect();
        let post_req = PostReq {
            posting_id: req.posting_id,
            entries,
            memo: req.memo,
            ref_tx_id: req.ref_tx_id,
        };
        match self.store.post(post_req) {
            Ok((resp, _replay)) => Ok(Response::new(PbPostingResponse {
                posting_id: resp.posting_id,
                status: resp.status,
                entry_ids: resp.entry_ids,
                hash_head: resp.hash_head,
            })),
            Err(e) => Err(Status::invalid_argument(e.message())),
        }
    }

    async fn get_posting(
        &self,
        req: Request<GetPostingRequest>,
    ) -> Result<Response<PbPostingRecord>, Status> {
        let req = req.into_inner();
        match self.store.get_posting(&req.posting_id) {
            Some(p) => Ok(Response::new(posting_to_pb(&p))),
            None => Err(Status::not_found(format!(
                "posting not found: {}",
                req.posting_id
            ))),
        }
    }

    async fn get_balance(
        &self,
        req: Request<GetBalanceRequest>,
    ) -> Result<Response<BalanceResponse>, Status> {
        let req = req.into_inner();
        let asset = req.asset.unwrap_or_default();
        match self.store.balance(&req.account_id, &asset) {
            Some(bal) => Ok(Response::new(BalanceResponse {
                account_id: req.account_id,
                asset: if asset.is_empty() {
                    "all".to_string()
                } else {
                    asset
                },
                balance: bal.to_string(),
                as_of_ts: crate::store::now_iso(),
            })),
            None => Err(Status::not_found(format!(
                "account not found: {}",
                req.account_id
            ))),
        }
    }

    async fn verify_chain(
        &self,
        _req: Request<VerifyChainRequest>,
    ) -> Result<Response<VerifyChainResponse>, Status> {
        let state = self.store.inner.lock();
        let result = crate::hashchain::verify_chain(&state);
        match result {
            Ok(()) => Ok(Response::new(VerifyChainResponse {
                ok: true,
                entry_id: None,
                reason: None,
            })),
            Err(b) => Ok(Response::new(VerifyChainResponse {
                ok: false,
                entry_id: Some(b.entry_id),
                reason: Some(b.reason),
            })),
        }
    }
}

fn posting_to_pb(p: &crate::posting::PostingRecord) -> PbPostingRecord {
    PbPostingRecord {
        posting_id: p.posting_id.clone(),
        ref_tx_id: p.ref_tx_id.clone(),
        memo: p.memo.clone(),
        status: p.status.clone(),
        hash_head: p.hash_head.clone(),
        entries: p
            .entries
            .iter()
            .map(|e| ledger::EntryRecord {
                entry_id: e.entry_id.clone(),
                posting_id: e.posting_id.clone(),
                account_id: e.account_id.clone(),
                direction: e.direction.clone(),
                amount: e.amount,
                asset: e.asset.clone(),
                sequence_number: e.sequence_number,
                prev_hash: e.prev_hash.clone(),
                this_hash: e.this_hash.clone(),
                created_at: e.created_at.clone(),
            })
            .collect(),
        created_at: p.created_at.clone(),
    }
}

pub fn server(store: Store, allowed_callers: Vec<String>) -> LedgerServer<LedgerGrpc> {
    LedgerServer::new(LedgerGrpc::new(store, allowed_callers))
}
