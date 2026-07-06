//! Versioned chart of accounts: the static catalog of account types, their
//! normal balance side, and the directions allowed on entries against them.
//!
//! This module mirrors the rows seeded by the
//! `20260706000002_seed_chart_of_accounts` migration so the application can
//! validate `POST /v1/accounts` requests without a round-trip to the DB. The
//! DB rows are authoritative at runtime; this struct is the source of truth
//! for request validation in the write path.

use serde::{Deserialize, Serialize};

/// Normal balance side of an account type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NormalBalance {
    Debit,
    Credit,
    /// Either side is valid (e.g. `fx_gain_loss`).
    Either,
}

/// A single entry in the chart of accounts catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartEntry {
    pub account_type: String,
    pub version: i32,
    pub normal_balance: NormalBalance,
    pub allowed_directions: Vec<String>,
    pub description: String,
}

/// The full versioned chart of accounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartOfAccounts {
    pub version: i32,
    pub entries: Vec<ChartEntry>,
}

/// Supported asset classes for an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetClass {
    Fiat,
    Crypto,
}

impl AssetClass {
    /// Parse a wire string into an `AssetClass`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fiat" => Some(Self::Fiat),
            "crypto" => Some(Self::Crypto),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fiat => "fiat",
            Self::Crypto => "crypto",
        }
    }
}

/// The static chart of accounts seeded by the Stage 1 migration.
///
/// Order matches the README table so `GET /v1/chart-of-accounts` returns a
/// stable, reviewable catalog.
pub const SEED_VERSION: i32 = 1;

pub fn seed_chart() -> Vec<ChartEntry> {
    vec![
        ChartEntry {
            account_type: "user_custodial".into(),
            version: SEED_VERSION,
            normal_balance: NormalBalance::Credit,
            allowed_directions: vec!["credit".into()],
            description:
                "Funds held on behalf of users (liability to the platform). Per-user, per-asset."
                    .into(),
        },
        ChartEntry {
            account_type: "user_payable".into(),
            version: SEED_VERSION,
            normal_balance: NormalBalance::Credit,
            allowed_directions: vec!["credit".into()],
            description: "Funds owed but not yet credited to the user's custodial account.".into(),
        },
        ChartEntry {
            account_type: "operational_fiat".into(),
            version: SEED_VERSION,
            normal_balance: NormalBalance::Debit,
            allowed_directions: vec!["debit".into()],
            description: "Operational fiat float (settlement accounts, rail holdings).".into(),
        },
        ChartEntry {
            account_type: "operational_crypto".into(),
            version: SEED_VERSION,
            normal_balance: NormalBalance::Debit,
            allowed_directions: vec!["debit".into()],
            description: "Operational crypto float (hot-wallet funding buffers).".into(),
        },
        ChartEntry {
            account_type: "treasury_fiat".into(),
            version: SEED_VERSION,
            normal_balance: NormalBalance::Debit,
            allowed_directions: vec!["debit".into()],
            description: "Treasury fiat reserves and bank accounts.".into(),
        },
        ChartEntry {
            account_type: "treasury_crypto".into(),
            version: SEED_VERSION,
            normal_balance: NormalBalance::Debit,
            allowed_directions: vec!["debit".into()],
            description: "Treasury crypto reserves (cold/warm custody).".into(),
        },
        ChartEntry {
            account_type: "fx_gain_loss".into(),
            version: SEED_VERSION,
            normal_balance: NormalBalance::Either,
            allowed_directions: vec!["debit".into(), "credit".into()],
            description: "Realized FX gains and losses from currency conversion.".into(),
        },
        ChartEntry {
            account_type: "fee_revenue".into(),
            version: SEED_VERSION,
            normal_balance: NormalBalance::Credit,
            allowed_directions: vec!["credit".into()],
            description: "Fee revenue recognized per transaction.".into(),
        },
        ChartEntry {
            account_type: "rail_settlement".into(),
            version: SEED_VERSION,
            normal_balance: NormalBalance::Debit,
            allowed_directions: vec!["debit".into()],
            description: "In-transit funds on a payment rail awaiting settlement.".into(),
        },
        ChartEntry {
            account_type: "venue_settlement".into(),
            version: SEED_VERSION,
            normal_balance: NormalBalance::Debit,
            allowed_directions: vec!["debit".into()],
            description: "In-transit funds at an exchange/OTC venue awaiting settlement.".into(),
        },
        ChartEntry {
            account_type: "chargeback_reserve".into(),
            version: SEED_VERSION,
            normal_balance: NormalBalance::Credit,
            allowed_directions: vec!["credit".into()],
            description: "Reserve for anticipated chargebacks and disputes.".into(),
        },
    ]
}

/// The authoritative in-memory chart of accounts, used to validate
/// `POST /v1/accounts` requests without a DB round-trip.
#[derive(Debug, Clone)]
pub struct Chart {
    pub version: i32,
    entries: std::collections::HashMap<String, ChartEntry>,
}

impl Chart {
    /// Build the chart from the static seed definition.
    pub fn from_seed() -> Self {
        let entries = seed_chart();
        let map = entries
            .iter()
            .map(|e| (e.account_type.clone(), e.clone()))
            .collect();
        Self {
            version: SEED_VERSION,
            entries: map,
        }
    }

    /// Look up an account type definition.
    pub fn get(&self, account_type: &str) -> Option<&ChartEntry> {
        self.entries.get(account_type)
    }

    /// True if `account_type` is a known chart-of-accounts type.
    pub fn contains(&self, account_type: &str) -> bool {
        self.entries.contains_key(account_type)
    }

    /// Serialize the chart for `GET /v1/chart-of-accounts`.
    pub fn to_catalog(&self) -> ChartOfAccounts {
        let mut entries = self.entries.values().cloned().collect::<Vec<_>>();
        // Stable order matching the README/seed for reviewability.
        entries.sort_by(|a, b| a.account_type.cmp(&b.account_type));
        ChartOfAccounts {
            version: self.version,
            entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_has_all_readme_account_types() {
        let chart = Chart::from_seed();
        for ty in [
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
            assert!(chart.contains(ty), "missing account type {ty}");
        }
        assert_eq!(chart.entries.len(), 11);
    }

    #[test]
    fn normal_balances_match_readme() {
        let chart = Chart::from_seed();
        assert_eq!(
            chart.get("user_custodial").unwrap().normal_balance,
            NormalBalance::Credit
        );
        assert_eq!(
            chart.get("operational_fiat").unwrap().normal_balance,
            NormalBalance::Debit
        );
        assert_eq!(
            chart.get("fx_gain_loss").unwrap().normal_balance,
            NormalBalance::Either
        );
        assert_eq!(
            chart.get("fee_revenue").unwrap().normal_balance,
            NormalBalance::Credit
        );
        assert_eq!(
            chart.get("chargeback_reserve").unwrap().normal_balance,
            NormalBalance::Credit
        );
    }

    #[test]
    fn allowed_directions_match_normal_balance() {
        let chart = Chart::from_seed();
        for entry in chart.entries.values() {
            match entry.normal_balance {
                NormalBalance::Debit => {
                    assert_eq!(entry.allowed_directions, vec!["debit".to_string()]);
                }
                NormalBalance::Credit => {
                    assert_eq!(entry.allowed_directions, vec!["credit".to_string()]);
                }
                NormalBalance::Either => {
                    assert_eq!(
                        entry.allowed_directions,
                        vec!["debit".to_string(), "credit".to_string()]
                    );
                }
            }
        }
    }

    #[test]
    fn catalog_serializes_with_version() {
        let chart = Chart::from_seed();
        let cat = chart.to_catalog();
        assert_eq!(cat.version, SEED_VERSION);
        assert_eq!(cat.entries.len(), 11);
        let json = serde_json::to_value(&cat).unwrap();
        assert!(json.get("version").is_some());
        assert!(json.get("entries").unwrap().is_array());
    }

    #[test]
    fn asset_class_parse_roundtrips() {
        for s in ["fiat", "crypto"] {
            let ac = AssetClass::parse(s).unwrap();
            assert_eq!(ac.as_str(), s);
        }
        assert!(AssetClass::parse("stock").is_none());
    }
}
