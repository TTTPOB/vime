//! Associate the final text with the explicitly acknowledged audio commit.
use crate::protocol::ServerEvent;

/// Text actually received from this recording, retained for best-effort error
/// recovery. Delta events append; completed events replace the draft text.
#[derive(Default)]
pub struct ReceivedText {
    items: HashMap<String, String>,
    committed: Option<String>,
    latest: Option<String>,
}
impl ReceivedText {
    pub fn observe(&mut self, event: &ServerEvent) -> Result<()> {
        match event {
            ServerEvent::Committed { item_id } => self.committed = Some(item_id.clone()),
            ServerEvent::Delta { item_id, text } | ServerEvent::Final { item_id, text } => {
                if !self.items.contains_key(item_id) && self.items.len() >= 8 {
                    bail!("too many ASR transcript items");
                }
                let entry = self.items.entry(item_id.clone()).or_default();
                if matches!(event, ServerEvent::Final { .. }) {
                    *entry = text.clone();
                } else if entry.len() + text.len() <= 1024 * 1024 {
                    entry.push_str(text);
                } else {
                    bail!("ASR transcript exceeds recovery size limit");
                }
                self.latest = Some(item_id.clone());
            }
            _ => {}
        }
        Ok(())
    }
    pub fn recover(&self) -> Option<String> {
        let id = self.committed.as_ref().or(self.latest.as_ref())?;
        self.items
            .get(id)
            .filter(|text| !text.trim().is_empty())
            .cloned()
    }
}
use anyhow::{bail, Result};
use std::collections::HashMap;

#[derive(Default)]
pub struct FinalCollector {
    item: Option<String>,
    pending: HashMap<String, String>,
    delivered: bool,
}
impl FinalCollector {
    /// Called only after the client has sent commit. Completion is authoritative,
    /// never constructed by concatenating deltas. A final can precede its ack.
    pub fn observe(&mut self, event: ServerEvent) -> Result<Option<String>> {
        if self.delivered {
            return Ok(None);
        }
        match event {
            ServerEvent::Committed { item_id } => {
                if self.item.as_ref().is_some_and(|id| id != &item_id) {
                    bail!("ASR acknowledged multiple items for one recording");
                }
                self.item = Some(item_id.clone());
                if let Some(text) = self.pending.remove(&item_id) {
                    self.delivered = true;
                    return Ok(Some(text));
                }
            }
            ServerEvent::Final { item_id, text } => {
                if self.item.as_ref() == Some(&item_id) {
                    self.delivered = true;
                    return Ok(Some(text));
                }
                if let Some(previous) = self.pending.get(&item_id) {
                    if previous != &text {
                        bail!("ASR sent conflicting final results");
                    }
                } else {
                    if self.pending.len() >= 8 {
                        bail!("too many unrelated ASR final results");
                    }
                    self.pending.insert(item_id, text);
                }
            }
            ServerEvent::Error { message } => bail!("{message}"),
            _ => {}
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn final_event(id: &str, text: &str) -> ServerEvent {
        ServerEvent::Final {
            item_id: id.into(),
            text: text.into(),
        }
    }
    #[test]
    fn waits_for_ack_and_delivers_matching_authoritative_final_once() {
        let mut collector = FinalCollector::default();
        assert_eq!(
            collector.observe(final_event("wrong", "wrong")).unwrap(),
            None
        );
        assert_eq!(
            collector
                .observe(final_event("right", "中文 English"))
                .unwrap(),
            None
        );
        assert_eq!(
            collector
                .observe(ServerEvent::Committed {
                    item_id: "right".into()
                })
                .unwrap(),
            Some("中文 English".into())
        );
        assert_eq!(
            collector
                .observe(final_event("right", "中文 English"))
                .unwrap(),
            None
        );
    }
    #[test]
    fn conflicting_results_and_errors_fail_closed() {
        let mut collector = FinalCollector::default();
        collector.observe(final_event("id", "one")).unwrap();
        assert!(collector.observe(final_event("id", "two")).is_err());
        assert!(FinalCollector::default()
            .observe(ServerEvent::Error {
                message: "failure".into()
            })
            .is_err());
    }
    #[test]
    fn recovery_preserves_received_text_and_prefers_committed_item() {
        let mut text = ReceivedText::default();
        text.observe(&ServerEvent::Delta {
            item_id: "one".into(),
            text: "你好 ".into(),
        })
        .unwrap();
        text.observe(&ServerEvent::Delta {
            item_id: "one".into(),
            text: "English".into(),
        })
        .unwrap();
        assert_eq!(text.recover().as_deref(), Some("你好 English"));
        text.observe(&ServerEvent::Committed {
            item_id: "one".into(),
        })
        .unwrap();
        text.observe(&final_event("other", "unrelated")).unwrap();
        assert_eq!(text.recover().as_deref(), Some("你好 English"));
        text.observe(&final_event("one", "你好，English。"))
            .unwrap();
        assert_eq!(text.recover().as_deref(), Some("你好，English。"));
    }
}
