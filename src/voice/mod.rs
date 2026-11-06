pub mod monitor;
pub mod segmenter;
pub mod stt;

/// The live voice path on real speech: 20 ms ticks through the segmenter with the production
/// limits, then Whisper on each segment. Needs a model and a 16 kHz mono WAV:
/// `STT_TEST_MODEL=models/ggml-base.en.bin STT_TEST_WAV=models/jfk.wav cargo test real_speech -- --ignored --nocapture`
#[cfg(test)]
mod tests {
    use super::segmenter::{Limits, Segmenter};

    #[tokio::test]
    #[ignore]
    async fn real_speech_is_segmented_and_transcribed() {
        let wav = std::fs::read(std::env::var("STT_TEST_WAV").unwrap()).unwrap();
        let pcm: Vec<i16> = wav[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        let mut seg = Segmenter::new(Limits::default());
        let mut segments = Vec::new();
        for tick in pcm.chunks(320) {
            segments.extend(seg.tick(&[(1u8, tick)]));
        }
        for _ in 0..60 {
            segments.extend(seg.tick(&[])); // they stop sending
        }
        let stt = super::stt::Stt::load(&std::env::var("STT_TEST_MODEL").unwrap()).unwrap();
        let mut heard = Vec::new();
        for (_, audio) in segments {
            let secs = audio.len() as f32 / 16_000.0;
            let text = crate::hostility::clean_transcript(&stt.transcribe(audio).await.unwrap());
            println!("{secs:.1}s: {text}");
            heard.push(text);
        }
        let all = heard.join(" ").to_lowercase();
        assert!(!heard.is_empty(), "the speech made a segment");
        assert!(
            all.contains("ask not what your country can do for you"),
            "{all}"
        );
    }
}
