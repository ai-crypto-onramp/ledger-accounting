use crate::store::{now_iso, AuditEvent, Store};

pub fn emit_audit_event(store: &Store, event: &AuditEvent) {
    store.record_audit_event(event.clone());
}

pub fn build_event(posting_id: &str, entry_ids: &[String], hash_head: &str) -> AuditEvent {
    AuditEvent {
        event_id: uuid::Uuid::new_v4().to_string(),
        posting_id: posting_id.to_string(),
        entry_ids: entry_ids.to_vec(),
        hash_head: hash_head.to_string(),
        created_at: now_iso(),
    }
}

#[derive(Debug, Clone)]
pub struct AuditSink {
    pub url: Option<String>,
}

impl AuditSink {
    pub fn new(url: Option<String>) -> Self {
        Self { url }
    }

    pub fn emit(&self, store: &Store, event: &AuditEvent) {
        emit_audit_event(store, event);
        if let Some(url) = &self.url {
            eprintln!(
                "[audit] would post event {} for posting {} to {}",
                event.event_id, event.posting_id, url
            );
        }
    }
}
