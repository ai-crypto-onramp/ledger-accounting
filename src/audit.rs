use crate::store::{now_iso, AuditEvent, Store};

pub fn emit_audit_event(store: &Store, event: &AuditEvent) {
    store.record_audit_event(event.clone());
}

pub fn build_event(posting_id: &str, entry_ids: &[String], hash_head: &str) -> AuditEvent {
    AuditEvent {
        event_id: uuid::Uuid::now_v7().to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_event_populates_fields() {
        let ids = vec!["e1".to_string(), "e2".to_string()];
        let ev = build_event("p1", &ids, "head123");
        assert_eq!(ev.posting_id, "p1");
        assert_eq!(ev.entry_ids, ids);
        assert_eq!(ev.hash_head, "head123");
        assert!(!ev.event_id.is_empty());
        assert!(!ev.created_at.is_empty());
    }

    #[test]
    fn emit_audit_event_records_into_store() {
        let store = Store::new();
        let ev = build_event("p2", &["e1".to_string()], "h");
        emit_audit_event(&store, &ev);
        assert_eq!(store.audit_events().len(), 1);
        assert_eq!(store.audit_events()[0].posting_id, "p2");
    }

    #[test]
    fn audit_sink_emit_no_url_still_records() {
        let store = Store::new();
        let sink = AuditSink::new(None);
        let ev = build_event("p3", &["e1".to_string()], "h");
        sink.emit(&store, &ev);
        assert_eq!(store.audit_events().len(), 1);
    }

    #[test]
    fn audit_sink_emit_with_url_records_and_logs() {
        let store = Store::new();
        let sink = AuditSink::new(Some("http://example/log".to_string()));
        let ev = build_event("p4", &["e1".to_string()], "h");
        sink.emit(&store, &ev);
        assert_eq!(store.audit_events().len(), 1);
        assert_eq!(store.audit_events()[0].posting_id, "p4");
    }
}
