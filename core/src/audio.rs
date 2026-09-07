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

pub fn to_le_bytes(audio: impl AsRef<[i16]>) -> Vec<u8> {
    let audio = audio.as_ref();
    let mut result = Vec::with_capacity(audio.len() * 2);
    for sample in audio {
        result.extend_from_slice(&sample.to_le_bytes());
    }
    result
}

// TODO: This may fail! use Result<> here.
pub fn from_le_bytes(audio: impl AsRef<[u8]>) -> Vec<i16> {
    audio
        .as_ref()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&chunk| i16::from_le_bytes(chunk))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_le_bytes_decodes_samples() {
        let samples = from_le_bytes([0x00, 0x00, 0x00, 0x80, 0xff, 0x7f]);
        assert_eq!(samples, vec![0, i16::MIN, i16::MAX]);
    }

    #[test]
    fn from_le_bytes_round_trips_to_le_bytes() {
        let samples = vec![0i16, 42, -1, 32000, -32000, i16::MAX, i16::MIN];
        assert_eq!(from_le_bytes(to_le_bytes(&samples)), samples);
    }

    #[test]
    fn from_le_bytes_drops_trailing_partial_sample() {
        // Odd number of bytes: the final byte cannot form a full sample and is dropped.
        assert_eq!(from_le_bytes([0x01, 0x00, 0xff]), vec![1]);
    }

    #[test]
    fn from_le_bytes_empty_input() {
        assert_eq!(from_le_bytes([]), Vec::<i16>::new());
    }
}
