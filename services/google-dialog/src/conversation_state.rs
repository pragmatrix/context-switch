use anyhow::{Result, bail};
use std::collections::hash_map;
use tracing::warn;

#[derive(Debug)]
pub struct ConversationState {
    pub output_transcription_buffer: String,
    pub tool_calls: ToolCallTracker,
}

#[derive(Debug, Default)]
pub struct ToolCallTracker {
    calls: hash_map::HashMap<String, ToolCallEntry>,
}

#[derive(Debug)]
enum ToolCallEntry {
    Pending { name: String },
    Canceled,
}

impl ConversationState {
    pub fn new() -> Self {
        Self {
            output_transcription_buffer: String::new(),
            tool_calls: ToolCallTracker::default(),
        }
    }
}

impl ToolCallTracker {
    pub fn register(&mut self, call_id: String, name: String) -> Result<()> {
        match self.calls.entry(call_id) {
            hash_map::Entry::Vacant(entry) => {
                entry.insert(ToolCallEntry::Pending { name });
                Ok(())
            }
            hash_map::Entry::Occupied(entry) => match entry.get() {
                ToolCallEntry::Pending { .. } => {
                    bail!(
                        "tool call `{}` is already pending before register",
                        entry.key()
                    )
                }
                ToolCallEntry::Canceled => {
                    bail!(
                        "tool call `{}` was already canceled before register",
                        entry.key()
                    )
                }
            },
        }
    }

    pub fn cancel(&mut self, call_id: String) -> Result<()> {
        match self.calls.entry(call_id) {
            hash_map::Entry::Occupied(mut entry) => match entry.get() {
                ToolCallEntry::Pending { .. } => {
                    entry.insert(ToolCallEntry::Canceled);
                    Ok(())
                }
                ToolCallEntry::Canceled => {
                    bail!(
                        "tool call `{}` was already canceled before cancellation",
                        entry.key()
                    )
                }
            },
            hash_map::Entry::Vacant(entry) => {
                bail!(
                    "tool call `{}` is not pending when cancellation arrives",
                    entry.key()
                )
            }
        }
    }

    pub fn resolve(&mut self, call_id: &str) -> Result<Option<String>> {
        match self.calls.remove(call_id) {
            Some(ToolCallEntry::Pending { name }) => Ok(Some(name)),
            Some(ToolCallEntry::Canceled) => {
                warn!(
                    %call_id,
                    "Ignoring functionCallResult for canceled tool call"
                );
                Ok(None)
            }
            None => bail!("functionCallResult references unknown `callId`: `{call_id}`"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ToolCallTracker;

    #[test]
    fn register_then_resolve_returns_name() {
        let mut tracker = ToolCallTracker::default();

        tracker
            .register("id-1".to_string(), "lookup_weather".to_string())
            .expect("register should succeed");

        let resolved = tracker.resolve("id-1").expect("resolve should succeed");

        assert_eq!(resolved, Some("lookup_weather".to_string()));
    }

    #[test]
    fn cancellation_then_resolve_returns_none() {
        let mut tracker = ToolCallTracker::default();

        tracker
            .register("id-2".to_string(), "lookup_weather".to_string())
            .expect("register should succeed");
        tracker
            .cancel("id-2".to_string())
            .expect("cancel should succeed");

        let resolved = tracker.resolve("id-2").expect("resolve should succeed");

        assert_eq!(resolved, None);
    }

    #[test]
    fn duplicate_register_errors() {
        let mut tracker = ToolCallTracker::default();

        tracker
            .register("id-3".to_string(), "lookup_weather".to_string())
            .expect("first register should succeed");

        let error = tracker
            .register("id-3".to_string(), "lookup_weather".to_string())
            .expect_err("second register should fail");

        assert!(
            error
                .to_string()
                .contains("is already pending before register"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn duplicate_cancellation_errors() {
        let mut tracker = ToolCallTracker::default();

        tracker
            .register("id-4".to_string(), "lookup_weather".to_string())
            .expect("register should succeed");
        tracker
            .cancel("id-4".to_string())
            .expect("first cancel should succeed");

        let error = tracker
            .cancel("id-4".to_string())
            .expect_err("second cancel should fail");

        assert!(
            error
                .to_string()
                .contains("was already canceled before cancellation"),
            "unexpected error: {error}"
        );
    }
}
