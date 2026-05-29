use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy)]
pub struct TranscriptionSettings {
    pub input: bool,
    pub output: bool,
}

#[derive(Debug, Default)]
pub struct TranscriptionState {
    input_transcription_buffers: HashMap<InputTranscriptionKey, String>,
    output_transcription_buffers: HashMap<OutputTranscriptionKey, String>,
    output_transcription_responses: HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct InputTranscriptionKey {
    item_id: String,
    content_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct OutputTranscriptionKey {
    item_id: String,
    output_index: u32,
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

impl OutputTranscriptionKey {
    fn new(item_id: String, output_index: u32, content_index: u32) -> Self {
        Self {
            item_id,
            output_index,
            content_index,
        }
    }
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

    pub fn apply_output_delta(
        &mut self,
        response_id: String,
        item_id: String,
        output_index: u32,
        content_index: u32,
        delta: String,
    ) -> String {
        self.output_transcription_responses.insert(response_id);
        let key = OutputTranscriptionKey::new(item_id, output_index, content_index);
        let entry = self.output_transcription_buffers.entry(key).or_default();
        entry.push_str(&delta);
        entry.clone()
    }

    pub fn complete_output_transcription(
        &mut self,
        response_id: String,
        item_id: String,
        output_index: u32,
        content_index: u32,
        transcript: String,
    ) -> Option<String> {
        self.output_transcription_responses.insert(response_id);
        let key = OutputTranscriptionKey::new(item_id, output_index, content_index);
        let text = if transcript.is_empty() {
            self.output_transcription_buffers
                .remove(&key)
                .unwrap_or_default()
        } else {
            self.output_transcription_buffers.remove(&key);
            transcript
        };

        (!text.is_empty()).then_some(text)
    }

    pub fn clear_output_response_tracking(&mut self, response_id: &str) {
        self.output_transcription_responses.remove(response_id);
    }

    pub fn clear_item(&mut self, item_id: &str) {
        self.input_transcription_buffers
            .retain(|buffer_key, _| buffer_key.item_id != item_id);
        self.output_transcription_buffers
            .retain(|buffer_key, _| buffer_key.item_id != item_id);
    }

    pub fn clear_content_index(&mut self, item_id: String, content_index: u32) {
        let key = InputTranscriptionKey::new(item_id.clone(), content_index);
        self.input_transcription_buffers.remove(&key);
        self.output_transcription_buffers.retain(|buffer_key, _| {
            buffer_key.item_id != item_id || buffer_key.content_index != content_index
        });
    }
}