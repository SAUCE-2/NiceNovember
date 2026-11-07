//! AI judgment via Jev.

pub mod jev;

use poise::serenity_prelude::Message;
use serde_json::{Value, json};

use crate::app::Data;
use crate::hostility::{AIMED, PUT_DOWN, STAMP, Source, Utterance, Verdict, questions};
use jev::{Answers, Image, Jev, normalize_image};

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

fn placeholders(images: &[Image]) -> Vec<&str> {
    images.iter().map(|i| i.placeholder.as_str()).collect()
}

/// Jev `state` for a chat message: who sent it when, the conversation before it, what it replies
/// to, and images.
pub fn text_state(u: &Utterance, images: &[Image]) -> Value {
    let mut state = json!({
        "author": u.author,
        "message_text": u.said,
        "sent_at": u.at.format(STAMP).to_string(),
        "recent_messages": u.recent,
    });
    if let Some(r) = &u.in_reply_to {
        state["replying_to_message"] = json!(r);
    }
    if !images.is_empty() {
        state["attached_images"] = json!(placeholders(images));
    }
    state
}

/// Jev `state` for something said in the voice call, with who's there and what came before.
pub fn voice_state(u: &Utterance) -> Value {
    json!({
        "speaker": u.author,
        "said_now": u.said,
        "said_at": u.at.format(STAMP).to_string(),
        "people_in_call": u.in_call,
        "recent_conversation": u.recent,
    })
}

/// Jev `state` for an emoji reaction, with the message (and its images) it's on.
pub fn reaction_state(emoji: &str, on_message: &str, images: &[Image]) -> Value {
    let mut state = json!({ "reaction": emoji, "on_message": on_message });
    if !images.is_empty() {
        state["on_message_images"] = json!(placeholders(images));
    }
    state
}

/// Jev `state` for any utterance: the form for its source.
pub fn state_for(u: &Utterance, images: &[Image]) -> Value {
    match u.source {
        Source::Voice => voice_state(u),
        Source::Reaction => reaction_state(
            &u.said,
            u.in_reply_to.as_deref().unwrap_or_default(),
            images,
        ),
        Source::Message | Source::Edit => text_state(u, images),
    }
}

/// Ask Jev whether what was said is aimed and a put-down. `None` if Jev isn't configured or the
/// call failed (logged).
pub async fn verdict(
    data: &Data,
    state: Value,
    images: Vec<Image>,
    source: Source,
) -> Option<Verdict> {
    let jev = data.jev.as_ref()?;
    match ask_verdict(jev, state, images, source).await {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("jev call failed: {e:#}");
            None
        }
    }
}

async fn ask_verdict(
    jev: &Jev,
    state: Value,
    images: Vec<Image>,
    source: Source,
) -> anyhow::Result<Verdict> {
    let answers = jev.ask(state, questions(source), images).await?;
    verdict_from(&answers)
}

fn verdict_from(answers: &Answers) -> anyhow::Result<Verdict> {
    Ok(Verdict {
        aimed: answers.noul(AIMED)?,
        put_down: answers.noul(PUT_DOWN)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hostility::{Line, utterance};
    use poise::serenity_prelude::{Attachment, Embed};

    fn alex(said: &str) -> Line {
        Line {
            speaker: "Alex".into(),
            said: said.into(),
            at: chrono::DateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn text_state_shape() {
        let mut u = utterance(Source::Message, "no u");
        u.recent = vec![alex("you're dumb")];
        u.in_reply_to = Some("you're dumb".into());
        let img = Image {
            placeholder: "image-1-1".into(),
            jpeg: vec![],
        };
        assert_eq!(
            text_state(&u, &[img]),
            json!({
                "author": "Sam",
                "message_text": "no u",
                "sent_at": "1970-01-01 00:00:00 UTC",
                "recent_messages": [
                    { "speaker": "Alex", "said": "you're dumb", "at": "1970-01-01 00:00:00 UTC" }
                ],
                "replying_to_message": "you're dumb",
                "attached_images": ["image-1-1"],
            })
        );
        let bare = text_state(&utterance(Source::Message, "hi"), &[]);
        assert!(bare.get("replying_to_message").is_none());
        assert!(bare.get("attached_images").is_none());
    }

    #[test]
    fn voice_state_shape() {
        let mut u = utterance(Source::Voice, "shut up, you were useless");
        u.in_call = vec!["Sam".into(), "Alex".into()];
        u.recent = vec![alex("I carried that round")];
        assert_eq!(
            voice_state(&u),
            json!({
                "speaker": "Sam",
                "said_now": "shut up, you were useless",
                "said_at": "1970-01-01 00:00:00 UTC",
                "people_in_call": ["Sam", "Alex"],
                "recent_conversation": [
                    { "speaker": "Alex", "said": "I carried that round", "at": "1970-01-01 00:00:00 UTC" }
                ],
            })
        );
    }

    #[test]
    fn state_follows_the_source() {
        let mut u = utterance(Source::Voice, "hi");
        u.in_reply_to = Some("yo".into());
        assert_eq!(state_for(&u, &[])["said_now"], "hi");
        u.source = Source::Edit;
        assert_eq!(state_for(&u, &[])["message_text"], "hi");
        u.source = Source::Reaction;
        u.said = "🤡".into();
        assert_eq!(
            state_for(&u, &[]),
            json!({ "reaction": "🤡", "on_message": "yo" })
        );
        let img = Image {
            placeholder: "image-1-1".into(),
            jpeg: vec![],
        };
        assert_eq!(
            state_for(&u, &[img])["on_message_images"],
            json!(["image-1-1"])
        );
    }

    #[test]
    fn verdict_reads_both_answers() {
        let answers =
            |a: Value| serde_json::from_value::<Answers>(json!({ "answers": a })).unwrap();
        let both = answers(json!({
            "aimed": { "type": "noul", "noul": 0.2 },
            "put_down": { "type": "noul", "noul": 0.9 },
        }));
        assert_eq!(
            verdict_from(&both).unwrap(),
            Verdict {
                aimed: 0.2,
                put_down: 0.9
            }
        );
        let one = answers(json!({ "aimed": { "type": "noul", "noul": 0.2 } }));
        assert!(verdict_from(&one).is_err());
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

/// Live check of where the not-nice line falls, at the default threshold (sends synthetic text
/// only): `./dev.sh cargo test live_boundary -- --ignored --nocapture` with JEV_API_KEY in `.env`.
#[cfg(test)]
mod live {
    use super::*;
    use crate::db::DEFAULT_THRESHOLD;
    use crate::hostility::{Line, is_hostile, utterance};
    use chrono::{TimeDelta, Utc};

    /// Sam says `said` now, after `before`: (who, what, how many seconds ago).
    fn sam_says(source: Source, before: &[(&str, &str, i64)], said: &str) -> Utterance {
        let mut u = utterance(source, said);
        u.at = Utc::now();
        u.in_call = vec!["Sam".into(), "Alex".into(), "Jo".into()];
        u.recent = before
            .iter()
            .map(|&(speaker, said, ago)| Line {
                speaker: speaker.into(),
                said: said.into(),
                at: u.at - TimeDelta::seconds(ago),
            })
            .collect();
        u
    }

    #[tokio::test]
    #[ignore]
    async fn live_boundary() {
        use Source::{Message, Voice};
        let jev = Jev::new(std::env::var("JEV_API_KEY").expect("JEV_API_KEY"));
        let drawing = (
            "Alex",
            "finally finished my first drawing, what do you think?",
            15,
        );
        let bot_word = (
            "Alex",
            "what does the bot call you when it catches you?",
            10,
        );
        let map = [
            ("Alex", "here's my drawing btw", 1200),
            ("Jo", "anyone tried the new map yet?", 40),
            ("Jo", "I think it's really badly designed", 20),
        ];
        let cliff = (
            "Jo",
            "what if we all just jump off the cliff, it's quicker",
            6,
        );
        let carried = [("Alex", "I carried that round", 4)];
        let money = [("Alex", "I've made like 2 million today", 8)];
        let spotify = [
            ("Sam", "That's not even mean come.", 12),
            ("Sam", "I'll see you.", 9),
            ("Sam", "We will.", 6),
        ];
        let song = ("Alex", "what song is this?", 10);
        let update = ("Jo", "the new update removed ranked mode", 20);
        let cases: [(Source, &[_], &str, bool); 23] = [
            (Message, &[], "hostile", false),
            (Message, &[], "toxic", false),
            (Message, &[], "not nice", false),
            (Message, &[], "this bot is broken lol", false),
            (Message, &[], "gg, well played", false),
            (Message, &[], "lmao your aim is trash", true),
            (Message, &[], "nobody asked you", true),
            (Message, &[cliff], "oh wow, what a genius idea", true),
            (Message, &[bot_word], "hostile", false),
            (Message, &[drawing], "cringe", true),
            (Message, &[drawing], "wow, I love the colours", false),
            (Message, &map, "yeah it's trash", false),
            (Voice, &carried, "shut up, you were useless", true),
            (Voice, &carried, "hostile", false),
            (
                Voice,
                &money,
                "Are you fobbing it? Is that why you're making so much money?",
                false,
            ),
            (Voice, &[], "Why are you stalking me, twin?", false),
            (Voice, &[], "I've told him off.", false),
            (Voice, &[], "Stop gaslighting me.", false),
            (
                Voice,
                &[],
                "You're a nasty man, you're a really nasty man.",
                true,
            ),
            (Voice, &[], "I'm not as nasty as you.", true),
            (
                Voice,
                &spotify,
                "It's my favourite artist on Spotify. You know how else is one of my favourite \
                 artists on Spotify? Big Nigger 42. He's my favourite artist on Spotify. And if I \
                 get muted, that was in context. So it doesn't count.",
                false,
            ),
            (
                Message,
                &[song],
                "it's CeeLo: \"I see you driving 'round town with the girl I love and I'm like, \
                 fuck you\"",
                false,
            ),
            (Message, &[update], "that's so gay", true),
        ];
        let mut wrong = Vec::new();
        for (source, before, said, not_nice) in cases {
            let u = sam_says(source, before, said);
            let v = ask_verdict(&jev, state_for(&u, &[]), vec![], source).await;
            let v = v.unwrap();
            let score = v.score();
            println!(
                "{score:.2} (aimed {:.2}, put-down {:.2}) {said} (after {} lines)",
                v.aimed,
                v.put_down,
                before.len()
            );
            if is_hostile(score, DEFAULT_THRESHOLD) != not_nice {
                wrong.push(said);
            }
        }
        assert!(wrong.is_empty(), "misjudged: {wrong:?}");
    }
}
