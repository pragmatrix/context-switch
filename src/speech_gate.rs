use crate::AudioFrame;
use fundsp::{hacker::*, numeric_array::NumericArray};

/// Returns a processing function that can be called for each AudioFrame (mono, 16kHz, i16)
pub fn make_speech_gate_processor(
    threshold: f32,
    attack_ms: f32,
    release_ms: f32,
) -> Box<dyn FnMut(&AudioFrame) -> AudioFrame> {
    let mut node = simple_speech_gate(threshold, attack_ms, release_ms);
    Box::new(move |frame: &AudioFrame| {
        let samples_f32: Vec<f32> = frame.samples.iter().map(|&s| s as f32 / 32768.0).collect();
        let processed: Vec<f32> = samples_f32
            .iter()
            .map(|&sample| node.tick(&NumericArray::from([sample]))[0])
            .collect();
        let processed_i16: Vec<i16> = processed
            .iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .collect();
        AudioFrame {
            format: frame.format,
            samples: processed_i16,
        }
    })
}

fn simple_speech_gate(
    threshold: f32,
    attack_ms: f32,
    release_ms: f32,
) -> An<impl AudioNode<Inputs = U1, Outputs = U1>> {
    // Proper RMS envelope follower with 10ms window
    let rms = envelope(|x| x * x) >> lowpass_hz(100.0, 1.0) >> map(|x| x[0].sqrt());

    // Gate control with smoothing using follow
    let smoothing = (attack_ms.max(release_ms)) / 1000.0;
    let gate_control =
        rms >> map(move |level| if level[0] > threshold { 1.0 } else { 0.0 }) >> follow(smoothing);

    // Apply gating
    pass() * gate_control
}
