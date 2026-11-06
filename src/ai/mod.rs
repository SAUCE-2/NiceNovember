//! AI judgment via Jev.

pub mod jev;

use poise::serenity_prelude::Message;
use serde_json::{Value, json};

use crate::app::Data;
use jev::{Image, normalize_image, noul};

/// Max images per message sent to Jev (keeps requests small and fast).
const MAX_IMAGES: usize = 4;
/// Skip anything bigger than Jev's per-image limit before even downloading.
const MAX_IMAGE_BYTES: u32 = 16 * 1024 * 1024;

/// Image URLs worth judging in a message: image attachments, then embed previews (e.g. GIFs).
pub fn image_urls(msg: &Message) -> Vec<String> {
    let attachments = msg
        .attachments
        .iter()
        .filter(|a| {
            a.content_type
                .as_deref()
                .is_some_and(|t| t.starts_with("image/"))
        })
        .filter(|a| a.size <= MAX_IMAGE_BYTES)
        .map(|a| a.url.clone());
    let embeds = msg.embeds.iter().filter_map(|e| {
        e.image
            .as_ref()
            .map(|i| i.url.clone())
            .or_else(|| e.thumbnail.as_ref().map(|t| t.url.clone()))
    });
    attachments.chain(embeds).take(MAX_IMAGES).collect()
}

/// Download a message's images, normalized for Jev. Failures are skipped: a broken image just
/// means Jev judges the text alone.
pub async fn message_images(msg: &Message) -> Vec<Image> {
    let mut out = Vec::new();
    for (i, url) in image_urls(msg).into_iter().enumerate() {
        match fetch_image(&url).await {
            Ok(jpeg) => out.push(Image {
                placeholder: format!("image-{}-{}", msg.id, i + 1),
                jpeg,
            }),
            Err(e) => tracing::debug!("skipping image {url}: {e:#}"),
        }
    }
    out
}

async fn fetch_image(url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = reqwest::get(url).await?.error_for_status()?;
    let bytes = resp.bytes().await?;
    anyhow::ensure!(bytes.len() <= MAX_IMAGE_BYTES as usize, "too large");
    tokio::task::spawn_blocking(move || normalize_image(&bytes)).await?
}

/// Jev `state` for something said: plain text, or text plus what it replies to and images.
pub fn utterance_state(text: &str, in_reply_to: Option<&str>, images: &[Image]) -> Value {
    if images.is_empty() && in_reply_to.is_none() {
        return json!(text);
    }
    let mut state = json!({ "message_text": text });
    if let Some(r) = in_reply_to {
        state["replying_to_message"] = json!(r);
    }
    if !images.is_empty() {
        let refs: Vec<&str> = images.iter().map(|i| i.placeholder.as_str()).collect();
        state["attached_images"] = json!(refs);
    }
    state
}

/// Ask a single yes/no question. `None` if Jev isn't configured or the call failed (logged).
pub async fn ask_noul(
    data: &Data,
    state: Value,
    images: Vec<Image>,
    question: &str,
    criteria: Option<(&str, &str)>,
) -> Option<f32> {
    let jev = data.jev.as_ref()?;
    match jev
        .ask(state, [("q", noul(question, criteria))], images)
        .await
    {
        Ok(a) => a.noul("q").ok(),
        Err(e) => {
            tracing::warn!("jev call failed: {e:#}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use poise::serenity_prelude::{Attachment, Embed};

    #[test]
    fn state_shapes() {
        assert_eq!(utterance_state("hi", None, &[]), json!("hi"));
        let img = Image {
            placeholder: "image-1-1".into(),
            jpeg: vec![],
        };
        assert_eq!(
            utterance_state("hi", None, std::slice::from_ref(&img)),
            json!({ "message_text": "hi", "attached_images": ["image-1-1"] })
        );
        assert_eq!(
            utterance_state("no u", Some("you're dumb"), &[]),
            json!({ "message_text": "no u", "replying_to_message": "you're dumb" })
        );
    }

    fn attachment(content_type: Option<&str>, size: u32, url: &str) -> Attachment {
        serde_json::from_value(json!({
            "id": "1", "filename": "f", "size": size, "url": url, "proxy_url": url,
            "content_type": content_type,
        }))
        .unwrap()
    }

    #[test]
    fn picks_images_only_and_caps_them() {
        let mut m = Message::default();
        m.attachments = vec![
            attachment(Some("image/png"), 10, "a"),
            attachment(Some("text/plain"), 10, "b"),
            attachment(None, 10, "c"),
            attachment(Some("image/gif"), MAX_IMAGE_BYTES + 1, "d"),
        ];
        let embed: Embed = serde_json::from_value(json!({ "thumbnail": { "url": "e" } })).unwrap();
        let empty: Embed = serde_json::from_value(json!({})).unwrap();
        m.embeds = vec![embed, empty];
        assert_eq!(image_urls(&m), vec!["a", "e"]);

        m.attachments = (0..6)
            .map(|i| attachment(Some("image/jpeg"), 1, &i.to_string()))
            .collect();
        assert_eq!(image_urls(&m).len(), MAX_IMAGES);
    }
}
