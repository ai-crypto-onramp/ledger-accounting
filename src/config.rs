use std::env;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub db_url: String,
    pub db_isolation: String,
    pub db_max_connections: u32,
    pub snapshot_interval_minutes: u64,
    pub hash_chain_alg: String,
    pub hash_chain_salt: Option<String>,
    pub audit_event_log_url: Option<String>,
    pub max_entries_per_posting: usize,
    pub max_amount: u64,
    pub log_level: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 8080,
            db_url: "postgres://ledger:ledger@localhost:5432/ledger".to_string(),
            db_isolation: "serializable".to_string(),
            db_max_connections: 32,
            snapshot_interval_minutes: 15,
            hash_chain_alg: "sha256".to_string(),
            hash_chain_salt: None,
            audit_event_log_url: None,
            max_entries_per_posting: 64,
            max_amount: 1_000_000_000_000,
            log_level: "info".to_string(),
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let mut c = Self::default();
        if let Ok(v) = env::var("PORT") {
            if let Ok(p) = v.parse() {
                c.port = p;
            }
        }
        if let Ok(v) = env::var("DB_URL") {
            c.db_url = v;
        }
        if let Ok(v) = env::var("DB_ISOLATION") {
            c.db_isolation = v;
        }
        if let Ok(v) = env::var("DB_MAX_CONNECTIONS") {
            if let Ok(p) = v.parse() {
                c.db_max_connections = p;
            }
        }
        if let Ok(v) = env::var("SNAPSHOT_INTERVAL_MINUTES") {
            if let Ok(p) = v.parse() {
                c.snapshot_interval_minutes = p;
            }
        }
        if let Ok(v) = env::var("HASH_CHAIN_ALG") {
            c.hash_chain_alg = v;
        }
        if let Ok(v) = env::var("HASH_CHAIN_SALT") {
            c.hash_chain_salt = Some(v);
        }
        if let Ok(v) = env::var("AUDIT_EVENT_LOG_URL") {
            c.audit_event_log_url = Some(v);
        }
        if let Ok(v) = env::var("MAX_ENTRIES_PER_POSTING") {
            if let Ok(p) = v.parse() {
                c.max_entries_per_posting = p;
            }
        }
        if let Ok(v) = env::var("MAX_AMOUNT") {
            if let Ok(p) = v.parse() {
                c.max_amount = p;
            }
        }
        if let Ok(v) = env::var("LOG_LEVEL") {
            c.log_level = v;
        }
        c
    }

    pub fn snapshot_interval(&self) -> Duration {
        Duration::from_secs(self.snapshot_interval_minutes * 60)
    }

    pub fn assert_isolation(&self) -> Result<(), String> {
        if self.db_isolation.eq_ignore_ascii_case("serializable") {
            Ok(())
        } else {
            Err(format!(
                "refusing to boot: DB_ISOLATION must be 'serializable', got '{}'",
                self.db_isolation
            ))
        }
    }
}
