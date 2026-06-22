// TODO(merge): extract to openai-realtime-core, shared with `openai-dialog`'s transcription state.
// Voice Live is transcription-only, so only the input-side buffers are duplicated here.

use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct TranscriptionState {
    input_transcription_buffers: HashMap<InputTranscriptionKey, String>,
}

impl TranscriptionState {
    pub fn apply_input_delta(
        &mut self,
        item_id: String,
        content_index: u32,
        delta: String,
    ) -> String {
        let key = InputTranscriptionKey::new(item_id, content_index);
        let entry = self.input_transcription_buffers.entry(key).or_default();
        entry.push_str(&delta);
        entry.clone()
    }

    pub fn complete_input_transcription(
        &mut self,
        item_id: String,
        content_index: u32,
        transcript: String,
    ) -> Option<String> {
        let key = InputTranscriptionKey::new(item_id, content_index);
        let text = if transcript.is_empty() {
            self.input_transcription_buffers
                .remove(&key)
                .unwrap_or_default()
        } else {
            self.input_transcription_buffers.remove(&key);
            transcript
        };

        (!text.is_empty()).then_some(text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct InputTranscriptionKey {
    item_id: String,
    content_index: u32,
}

impl InputTranscriptionKey {
    fn new(item_id: String, content_index: u32) -> Self {
        Self {
            item_id,
            content_index,
        }
    }
}
