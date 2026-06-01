use std::collections::BTreeSet;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{ConversationOutput, OutputPath};

const SEGMENT_OUTPUT_PATH: OutputPath = OutputPath::Media;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum Segment {
    UserSpeaking,
    AssistantSpeaking,
    Processing,
    #[serde(rename_all = "camelCase")]
    WaitingForFunctionResult {
        call_ids: Vec<String>,
    },
    Idle,
}

#[derive(Debug)]
pub struct SegmentController {
    current: Option<Segment>,
    pending_function_call_ids: BTreeSet<String>,
    event_output: ConversationOutput,
    event_mapper: fn(Segment) -> serde_json::Value,
}

impl SegmentController {
    pub fn new(output: ConversationOutput, event_mapper: fn(Segment) -> serde_json::Value) -> Self {
        Self {
            current: Some(Segment::Idle),
            pending_function_call_ids: BTreeSet::new(),
            event_output: output,
            event_mapper,
        }
    }

    pub fn begin_user_speech(&mut self) -> Result<Option<Segment>> {
        self.pending_function_call_ids.clear();
        self.transition(Segment::UserSpeaking)
    }

    pub fn begin_processing(&mut self) -> Result<Option<Segment>> {
        self.pending_function_call_ids.clear();
        self.transition(Segment::Processing)
    }

    pub fn begin_assistant_speech(&mut self) -> Result<Option<Segment>> {
        self.pending_function_call_ids.clear();
        self.transition(Segment::AssistantSpeaking)
    }

    pub fn begin_function_wait(&mut self, call_ids: Vec<String>) -> Result<Option<Segment>> {
        self.pending_function_call_ids.extend(call_ids);

        if self.pending_function_call_ids.is_empty() {
            return Ok(None);
        }

        self.transition(Segment::WaitingForFunctionResult {
            call_ids: self.pending_function_call_ids.iter().cloned().collect(),
        })
    }

    pub fn end_function_wait(&mut self, call_id: &str) -> Result<Option<Segment>> {
        self.pending_function_call_ids.remove(call_id);

        if self.pending_function_call_ids.is_empty() {
            self.transition(Segment::Idle)
        } else {
            Ok(None)
        }
    }

    pub fn become_idle(&mut self) -> Result<Option<Segment>> {
        self.pending_function_call_ids.clear();
        self.transition(Segment::Idle)
    }

    pub fn is_idle(&self) -> bool {
        self.current == Some(Segment::Idle)
    }

    fn transition(&mut self, segment: Segment) -> Result<Option<Segment>> {
        if self.current.as_ref() == Some(&segment) {
            return Ok(None);
        }

        self.current = Some(segment.clone());
        self.emit_segment_started(segment.clone())?;
        Ok(Some(segment))
    }

    fn emit_segment_started(&self, segment: Segment) -> Result<()> {
        self.event_output
            .service_event(SEGMENT_OUTPUT_PATH, (self.event_mapper)(segment))?;
        Ok(())
    }
}
