use serde::{Deserialize, Serialize};

use crate::chart::{self, AssetClass, Direction};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub account_id: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub asset_class: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateAccountRequest {
    pub account_id: Option<String>,
    #[serde(rename = "type")]
    pub type_name: String,
    pub asset_class: String,
    pub label: String,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountResponse {
    pub account_id: String,
}

pub fn parse_asset_class(s: &str) -> Option<AssetClass> {
    match s {
        "fiat" => Some(AssetClass::Fiat),
        "crypto" => Some(AssetClass::Crypto),
        "both" => Some(AssetClass::Both),
        _ => None,
    }
}

pub fn validate(req: &CreateAccountRequest) -> Result<(String, AssetClass), String> {
    let account_type = match chart::find_type(&req.type_name) {
        Some(t) => t,
        None => return Err(format!("unknown account type: {}", req.type_name)),
    };

    let asset_class = match parse_asset_class(&req.asset_class) {
        Some(c) => c,
        None => return Err(format!("invalid asset_class: {}", req.asset_class)),
    };

    if !chart::asset_class_allowed(account_type, asset_class) {
        return Err(format!(
            "asset_class {} not allowed for type {}",
            req.asset_class, req.type_name
        ));
    }

    Ok((req.type_name.clone(), asset_class))
}

pub fn parse_direction(s: &str) -> Option<Direction> {
    match s {
        "debit" => Some(Direction::Debit),
        "credit" => Some(Direction::Credit),
        _ => None,
    }
}
