pub fn into_i16(audio: impl AsRef<[f32]>) -> Vec<i16> {
    audio
        .as_ref()
        .iter()
        .map(|sample| (sample * i16::MAX as f32) as i16)
        .collect()
}

pub fn from_i16(audio: impl AsRef<[i16]>) -> Vec<f32> {
    const I16_MAX: f32 = i16::MAX as f32;
    audio
        .as_ref()
        .iter()
        .map(|&sample| sample as f32 / I16_MAX)
        .collect()
}

pub fn into_le_bytes(audio: impl AsRef<[i16]>) -> Vec<u8> {
    let audio = audio.as_ref();
    let mut result = Vec::with_capacity(audio.len() * 2);
    for sample in audio {
        result.extend_from_slice(&sample.to_le_bytes());
    }
    result
}

pub fn from_le_bytes(audio: impl AsRef<[u8]>) -> Vec<i16> {
    audio
        .as_ref()
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

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
