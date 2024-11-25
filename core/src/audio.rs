

pub fn into_i16(audio: Vec<f32>) -> Vec<i16> {
    audio
        .into_iter()
        .map(|sample| (sample * i16::MAX as f32) as i16)
        .collect()
}

pub fn into_le_bytes(audio: Vec<i16>) -> Vec<u8> {
    let mut result = Vec::with_capacity(audio.len() * 2);
    for sample in audio {
        result.extend_from_slice(&sample.to_le_bytes());
    }
    result
}

// Max is 15KB, so we do 8192 bytes max, which should also be aligned on a sample.
pub fn chunk_8192(audio: Vec<u8>) -> Vec<Vec<u8>> {
    const MAX_CHUNK_SIZE: usize = 8192;
    if audio.len() <= MAX_CHUNK_SIZE {
        return vec![audio];
    }
    audio
        .chunks(MAX_CHUNK_SIZE)
        .map(|chunk| chunk.to_vec())
        .collect()
}
