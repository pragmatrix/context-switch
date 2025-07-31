use crate::AudioFrame;
use fundsp::{hacker::*, numeric_array::NumericArray};

/// Returns a processing function that can be called for each AudioFrame (mono, 16kHz, i16)
pub fn make_speech_gate_processor_(
    threshold: f32,
    attack_ms: f32,
    release_ms: f32,
) -> Box<dyn FnMut(&AudioFrame) -> AudioFrame> {
    let mut node = simple_speech_gate(threshold, 6.0, attack_ms, release_ms);
    let mut sample_rate = None;

    Box::new(move |frame: &AudioFrame| {
        let frame_sample_rate = frame.format.sample_rate as f64;
        match sample_rate {
            None => {
                node.set_sample_rate(frame_sample_rate);
                sample_rate = Some(frame_sample_rate);
            }
            Some(rate) if rate != frame_sample_rate => {
                panic!("Changing frame sample rate is not supported in the speech gate processor");
            }
            Some(_) => {
                // same rate, all good
            }
        }

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
    softness: f32, // e.g., 6.0 dB for soft knee
    attack_ms: f32,
    release_ms: f32,
) -> An<impl AudioNode<Inputs = U1, Outputs = U1>> {
    let envelope_follower = envelope(|x| x * x)
        >> lowpass_hz(10.0, 1.0) // smoother RMS, ~100 ms
        >> map(|x| x[0].sqrt());

    let soft_gate = envelope_follower
        >> map(move |level| {
            let db = 20.0 * level[0].log10().max(-120.0);
            let gain_db = if db < threshold - softness {
                -60.0 // silence
            } else if db > threshold + softness {
                0.0 // full gain
            } else {
                // Linear ramp over 2 * softness dB
                -60.0 * (1.0 - (db - (threshold - softness)) / (2.0 * softness))
            };
            db_to_gain(gain_db)
        })
        >> afollow(attack_ms / 1000.0, release_ms / 1000.0);

    pass() * soft_gate
}

// Convert dB to linear gain
fn db_to_gain(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

fn simple_speech_gate_v1(
    threshold: f32,
    attack_ms: f32,
    release_ms: f32,
) -> An<impl AudioNode<Inputs = U1, Outputs = U1>> {
    // Proper RMS envelope follower with 10ms window
    let rms = envelope(|x| x * x) >> lowpass_hz(100.0, 1.0) >> map(|x| x[0].sqrt());

    // Gate control with smoothing using follow
    let gate_control = rms
        >> map(move |level| if level[0] > threshold { 1.0 } else { 0.0 })
        >> afollow(attack_ms / 1000.0, release_ms / 1000.0);

    // Apply gating
    pass() * gate_control
}

/// Returns a processing function that applies an attack/release envelope-based speech gate (no fundsp), with lazy sample rate initialization.
pub fn make_speech_gate_processor(
    threshold: f32, // normalized, 0.0 to 1.0
    attack_ms: f32,
    release_ms: f32,
) -> Box<dyn FnMut(&AudioFrame) -> AudioFrame> {
    let mut envelope = 0.0f32;
    let mut sample_rate: Option<f32> = None;
    let mut attack_coeff = 0.0f32;
    let mut release_coeff = 0.0f32;
    Box::new(move |frame: &AudioFrame| {
        if sample_rate.is_none() {
            let sr = frame.format.sample_rate as f32;
            sample_rate = Some(sr);
            attack_coeff = (-1.0 / (attack_ms * 0.001 * sr)).exp();
            release_coeff = (-1.0 / (release_ms * 0.001 * sr)).exp();
        }
        let mut samples_i16 = Vec::with_capacity(frame.samples.len());
        for &s in frame.samples.iter() {
            let sample_f32 = s as f32 / 32768.0;
            let abs = sample_f32.abs();
            if abs > envelope {
                envelope = attack_coeff * (envelope - abs) + abs;
            } else {
                envelope = release_coeff * (envelope - abs) + abs;
            }
            if envelope >= threshold {
                samples_i16.push(s);
            } else {
                samples_i16.push(0);
            }
        }
        AudioFrame {
            format: frame.format,
            samples: samples_i16,
        }
    })
}
