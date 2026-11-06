//! Local speech-to-text with whisper.cpp. Input is 16 kHz mono i16 PCM (what songbird decodes to).

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Semaphore;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Less than this much audio (0.3s) is treated as silence.
const MIN_SAMPLES: usize = 16_000 * 3 / 10;

#[derive(Clone)]
pub struct Stt {
    ctx: Arc<WhisperContext>,
    /// Caps concurrent transcriptions so a busy VC can't starve the CPU.
    permits: Arc<Semaphore>,
}

impl Stt {
    pub fn load(model_path: &str) -> Result<Self> {
        let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
            .with_context(|| format!("failed to load whisper model {model_path}"))?;
        Ok(Self {
            ctx: Arc::new(ctx),
            permits: Arc::new(Semaphore::new(2)),
        })
    }

    pub async fn transcribe(&self, pcm: Vec<i16>) -> Result<String> {
        if pcm.len() < MIN_SAMPLES {
            return Ok(String::new());
        }
        let _permit = self.permits.acquire().await?;
        let ctx = self.ctx.clone();
        tokio::task::spawn_blocking(move || {
            let audio = to_f32(&pcm);
            let mut state = ctx.create_state()?;
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_language(Some("en"));
            params.set_n_threads(4);
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            state.full(params, &audio)?;
            let text: Vec<String> = state.as_iter().map(|seg| seg.to_string()).collect();
            Ok(text.join(" ").trim().to_owned())
        })
        .await?
    }
}

pub fn to_f32(pcm: &[i16]) -> Vec<f32> {
    pcm.iter().map(|&s| s as f32 / 32768.0).collect()
}

#[cfg(test)]
mod tests {
    /// Needs a real model + 16 kHz mono WAV:
    /// STT_TEST_MODEL=models/ggml-base.en.bin STT_TEST_WAV=models/jfk.wav cargo test -- --ignored
    #[tokio::test]
    #[ignore]
    async fn transcribes_wav() {
        let model = std::env::var("STT_TEST_MODEL").unwrap();
        let wav = std::fs::read(std::env::var("STT_TEST_WAV").unwrap()).unwrap();
        let pcm: Vec<i16> = wav[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        let text = super::Stt::load(&model)
            .unwrap()
            .transcribe(pcm)
            .await
            .unwrap();
        println!("transcript: {text}");
        assert!(!text.is_empty());
    }

    #[test]
    fn pcm_scaling() {
        assert_eq!(super::to_f32(&[0, 16384, -32768]), vec![0.0, 0.5, -1.0]);
    }
}
