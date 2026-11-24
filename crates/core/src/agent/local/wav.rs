//! A turn's audio, as the chat API wants it: WAV, 16-bit PCM, base64.
//!
//! Written by hand rather than with a crate: the header is forty-four bytes of
//! fixed layout, and `aria-voice` — which has a WAV module — is exactly the
//! dependency the core must never take.

use base64::Engine as _;

/// The capture rate. The voice chain resamples to it before anything else.
pub const SAMPLE_RATE: u32 = 16_000;

/// Mono `f32` samples in `[-1, 1]` to a WAV file in memory.
pub fn encode(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM header size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// The same, base64-encoded for an `input_audio` part.
pub fn base64(samples: &[f32]) -> String {
    base64::engine::general_purpose::STANDARD.encode(encode(samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_says_what_follows() {
        let wav = encode(&[0.0, 1.0, -1.0]);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 6);
        assert_eq!(wav.len(), 44 + 6);
        // Full scale, both ways, and nothing wraps.
        assert_eq!(i16::from_le_bytes([wav[46], wav[47]]), i16::MAX);
        assert_eq!(i16::from_le_bytes([wav[48], wav[49]]), -i16::MAX);
    }

    #[test]
    fn a_sample_out_of_range_is_clipped_not_wrapped() {
        let wav = encode(&[3.0]);
        assert_eq!(i16::from_le_bytes([wav[44], wav[45]]), i16::MAX);
    }
}
