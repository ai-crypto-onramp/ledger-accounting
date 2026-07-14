use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct AssetSpec {
    pub code: &'static str,
    pub class: &'static str,
    pub scale: u32,
    pub max_amount: u64,
}

impl AssetSpec {
    pub const fn fiat(code: &'static str, scale: u32, max_amount: u64) -> Self {
        Self {
            code,
            class: "fiat",
            scale,
            max_amount,
        }
    }

    pub const fn crypto(code: &'static str, scale: u32, max_amount: u64) -> Self {
        Self {
            code,
            class: "crypto",
            scale,
            max_amount,
        }
    }
}

pub const ASSET_REGISTRY: &[AssetSpec] = &[
    AssetSpec::fiat("USD", 2, 1_000_000_000_000),
    AssetSpec::fiat("EUR", 2, 1_000_000_000_000),
    AssetSpec::fiat("GBP", 2, 1_000_000_000_000),
    AssetSpec::crypto("BTC", 8, 21_000_000_000_000),
    AssetSpec::crypto("ETH", 18, 1_000_000_000_000_000_000),
    AssetSpec::crypto("USDC", 6, 1_000_000_000_000),
];

pub fn registry_map() -> &'static HashMap<&'static str, AssetSpec> {
    static REGISTRY: OnceLock<HashMap<&'static str, AssetSpec>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut m = HashMap::new();
        for spec in ASSET_REGISTRY.iter() {
            m.insert(spec.code, spec.clone());
        }
        m
    })
}

pub fn find_asset(code: &str) -> Option<&'static AssetSpec> {
    registry_map().get(code)
}

pub fn validate_amount(asset: &str, amount: u64) -> Result<(), String> {
    let spec = match find_asset(asset) {
        Some(s) => s,
        None => return Err(format!("unknown asset: {}", asset)),
    };
    if amount == 0 {
        return Err("amount must be > 0".to_string());
    }
    if amount > spec.max_amount {
        return Err(format!(
            "amount {} exceeds MAX_AMOUNT {} for asset {}",
            amount, spec.max_amount, asset
        ));
    }
    Ok(())
}

pub fn max_amount_for(asset: &str) -> u64 {
    find_asset(asset)
        .map(|s| s.max_amount)
        .unwrap_or(crate::store::MAX_AMOUNT)
}

pub fn validate_scale(asset: &str, _amount: u64) -> Result<(), String> {
    let spec = match find_asset(asset) {
        Some(s) => s,
        None => return Ok(()),
    };
    let scale_mod = 10u64.checked_pow(spec.scale).unwrap_or(u64::MAX);
    if scale_mod == 0 {
        return Ok(());
    }
    Ok(())
}
