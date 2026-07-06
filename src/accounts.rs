//! Accounts service: creates accounts and rejects unknown `type` or
//! `asset_class` per the chart of accounts.

use anyhow::Result;
use axum::http::StatusCode;
use axum::{
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::chart_of_accounts::{AssetClass, Chart};

/// Request body for `POST /v1/accounts`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAccountRequest {
    pub account_id: String,
    #[serde(rename = "type")]
    pub account_type: String,
    pub asset_class: String,
    pub label: Option<String>,
    pub parent_id: Option<String>,
}

/// Response body for `POST /v1/accounts`.
#[derive(Debug, Clone, Serialize)]
pub struct CreateAccountResponse {
    pub account_id: String,
    pub status: String,
}

/// Validation / service error. Maps to HTTP status codes.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("unknown account type '{0}'")]
    UnknownAccountType(String),
    #[error("unknown asset class '{0}' (expected 'fiat' or 'crypto')")]
    UnknownAssetClass(String),
    #[error("account '{0}' already exists")]
    AccountExists(String),
    #[error("parent account '{0}' does not exist")]
    #[allow(dead_code)]
    UnknownParent(String),
    #[error("database error: {0}")]
    Db(String),
}

impl From<sqlx::Error> for AccountError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e.to_string())
    }
}

impl IntoResponse for AccountError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::UnknownAccountType(_) | Self::UnknownAssetClass(_) | Self::UnknownParent(_) => {
                StatusCode::BAD_REQUEST
            }
            Self::AccountExists(_) => StatusCode::CONFLICT,
            Self::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = Json(serde_json::json!({ "error": self.to_string() }));
        (status, body).into_response()
    }
}

/// Validate a `CreateAccountRequest` against the chart of accounts. Returns
/// the parsed `AssetClass` on success.
pub fn validate(req: &CreateAccountRequest, chart: &Chart) -> Result<AssetClass, AccountError> {
    if !chart.contains(&req.account_type) {
        return Err(AccountError::UnknownAccountType(req.account_type.clone()));
    }
    let asset_class = AssetClass::parse(&req.asset_class)
        .ok_or_else(|| AccountError::UnknownAssetClass(req.asset_class.clone()))?;
    Ok(asset_class)
}

/// Create a new account. Validates `type` and `asset_class` against the chart
/// of accounts, then inserts the row. The DB's `accounts.asset_class` CHECK
/// constraint is the final authority on valid asset classes; the chart is the
/// authority on valid account types.
pub async fn create_account(
    pool: &PgPool,
    chart: &Chart,
    req: CreateAccountRequest,
) -> Result<CreateAccountResponse, AccountError> {
    let asset_class = validate(&req, chart)?;

    // Insert. Rely on the PK for the duplicate-account case so concurrent
    // creations collapse to a single winner.
    let rows = sqlx::query(
        r#"
        INSERT INTO accounts (account_id, type, asset_class, label, parent_id, status)
        VALUES ($1, $2, $3, $4, $5, 'active')
        ON CONFLICT (account_id) DO NOTHING
        "#,
    )
    .bind(&req.account_id)
    .bind(&req.account_type)
    .bind(asset_class.as_str())
    .bind(req.label.unwrap_or_default())
    .bind(req.parent_id)
    .execute(pool)
    .await?;

    if rows.rows_affected() == 0 {
        return Err(AccountError::AccountExists(req.account_id.clone()));
    }

    Ok(CreateAccountResponse {
        account_id: req.account_id,
        status: "active".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chart() -> Chart {
        Chart::from_seed()
    }

    fn req(ty: &str, ac: &str) -> CreateAccountRequest {
        CreateAccountRequest {
            account_id: "acct_1".into(),
            account_type: ty.into(),
            asset_class: ac.into(),
            label: None,
            parent_id: None,
        }
    }

    #[test]
    fn rejects_unknown_account_type() {
        let err = validate(&req("does_not_exist", "fiat"), &chart()).unwrap_err();
        assert!(matches!(err, AccountError::UnknownAccountType(_)));
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn rejects_unknown_asset_class() {
        let err = validate(&req("user_custodial", "stock"), &chart()).unwrap_err();
        assert!(matches!(err, AccountError::UnknownAssetClass(_)));
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn accepts_known_type_and_asset_class() {
        let ac = validate(&req("user_custodial", "fiat"), &chart()).unwrap();
        assert_eq!(ac.as_str(), "fiat");
        let ac = validate(&req("operational_crypto", "crypto"), &chart()).unwrap();
        assert_eq!(ac.as_str(), "crypto");
    }

    #[test]
    fn accepts_either_direction_account_type() {
        let ac = validate(&req("fx_gain_loss", "fiat"), &chart()).unwrap();
        assert_eq!(ac.as_str(), "fiat");
    }
}
