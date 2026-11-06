//! The monitored voice channel: the bot sits in one VC per server all the time and listens.
//! Discord sends each speaker's audio separately (by SSRC); the [`Segmenter`] cuts it into one
//! segment per thing said, and each segment is transcribed by Whisper and judged like a message.
//!
//! [`ensure`] is the only thing that joins or leaves. It's called on startup, when the setting
//! changes, when the bot is moved or kicked, and every 30 s as a watchdog.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::Result;
use async_trait::async_trait;
use poise::serenity_prelude::{self as serenity, ChannelId, GuildId, UserId};
use serde_json::json;
use songbird::events::{CoreEvent, Event, EventContext, EventHandler};
use tokio::sync::mpsc;

use super::segmenter::{Limits, Segmenter};
use crate::app::Data;
use crate::hostility::{self, Source, Utterance};
use crate::stats::{self, Log, kind};

/// One finished speech segment, waiting to be transcribed.
pub struct Heard {
    pub guild_id: GuildId,
    pub channel: ChannelId,
    pub user: UserId,
    /// 16 kHz mono PCM.
    pub pcm: Vec<i16>,
}

struct Listening {
    channel: Option<ChannelId>,
    ssrc_to_user: HashMap<u32, UserId>,
    segmenter: Segmenter<UserId>,
    /// The last join error logged, so the watchdog doesn't log the same one every 30 s.
    last_error: Option<String>,
}

impl Default for Listening {
    fn default() -> Self {
        Self {
            channel: None,
            ssrc_to_user: HashMap::new(),
            segmenter: Segmenter::new(Limits::default()),
            last_error: None,
        }
    }
}

impl Listening {
    /// The bot is now in `channel`: start afresh (SSRCs are per voice session).
    fn joined(&mut self, channel: ChannelId) {
        *self = Self {
            channel: Some(channel),
            ..Self::default()
        };
    }

    /// Joining failed. Returns whether this error is new (worth logging).
    fn join_failed(&mut self, error: &str) -> bool {
        self.channel = None;
        self.last_error.replace(error.to_owned()).as_deref() != Some(error)
    }

    fn speaking(&mut self, ssrc: u32, user: UserId) {
        self.ssrc_to_user.insert(ssrc, user);
    }

    /// One 20 ms tick of `(ssrc, decoded audio)`: the speech segments it finished, with who
    /// said them. Audio from SSRCs not yet tied to a user is ignored.
    fn tick<'a>(
        &mut self,
        speaking: impl Iterator<Item = (u32, Option<&'a [i16]>)>,
    ) -> Vec<(UserId, Vec<i16>)> {
        if self.channel.is_none() {
            return vec![];
        }
        let frames: Vec<(UserId, &[i16])> = speaking
            .filter_map(|(ssrc, pcm)| Some((*self.ssrc_to_user.get(&ssrc)?, pcm?)))
            .collect();
        self.segmenter.tick(&frames)
    }
}

/// What [`ensure`] has to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    Leave,
    Join(ChannelId),
}

/// From the channel the bot should be in, the one Discord says it's in, and the one it's
/// listening to.
pub fn plan(
    want: Option<ChannelId>,
    current: Option<ChannelId>,
    listening: Option<ChannelId>,
) -> Step {
    match want {
        None if current.is_none() && listening.is_none() => Step::Stay,
        None => Step::Leave,
        Some(w) if current == Some(w) && listening == Some(w) => Step::Stay,
        Some(w) => Step::Join(w),
    }
}

type Shared = Arc<Mutex<Listening>>;

#[derive(Default)]
pub struct VoiceMonitor {
    guilds: Mutex<HashMap<GuildId, Shared>>,
    /// Serializes [`ensure`] per guild so two callers can't both join.
    joins: dashmap::DashMap<GuildId, Arc<tokio::sync::Mutex<()>>>,
    /// Finished segments go here; one worker transcribes and judges them.
    heard: OnceLock<mpsc::UnboundedSender<Heard>>,
}

impl VoiceMonitor {
    fn guild(&self, guild_id: GuildId) -> Shared {
        self.guilds
            .lock()
            .unwrap()
            .entry(guild_id)
            .or_default()
            .clone()
    }

    /// The channel the bot is listening in, if any.
    pub fn listening_in(&self, guild_id: GuildId) -> Option<ChannelId> {
        self.guild(guild_id).lock().unwrap().channel
    }

    /// Someone left (or moved out of) `channel`: what they were saying is finished.
    pub fn user_left(&self, guild_id: GuildId, user: UserId, channel: ChannelId) {
        let st = self.guild(guild_id);
        let seg = {
            let mut s = st.lock().unwrap();
            if s.channel != Some(channel) {
                return;
            }
            s.segmenter.flush(user)
        };
        if let (Some((user, pcm)), Some(tx)) = (seg, self.heard.get()) {
            tx.send(Heard {
                guild_id,
                channel,
                user,
                pcm,
            })
            .ok();
        }
    }

    fn sender(&self, ctx: &serenity::Context, data: &Data) -> mpsc::UnboundedSender<Heard> {
        self.heard
            .get_or_init(|| {
                let (tx, rx) = mpsc::unbounded_channel();
                tokio::spawn(work(ctx.clone(), data.clone(), rx));
                tx
            })
            .clone()
    }
}

/// Make the bot sit in the guild's monitored channel (or leave VC if none is set).
pub async fn ensure(ctx: &serenity::Context, data: &Data, guild_id: GuildId) -> Result<()> {
    let lock = data.monitor.joins.entry(guild_id).or_default().clone();
    let _held = lock.lock().await;
    let manager = songbird::get(ctx)
        .await
        .ok_or_else(|| anyhow::anyhow!("songbird not registered"))?;
    let want = data
        .db
        .guild(guild_id.get())
        .await?
        .voice_channel_id
        .map(|c| ChannelId::new(c as u64));
    let current = match manager.get(guild_id) {
        Some(call) => call
            .lock()
            .await
            .current_channel()
            .map(|c| ChannelId::new(c.0.get())),
        None => None,
    };
    let st = data.monitor.guild(guild_id);
    let listening = st.lock().unwrap().channel;
    match plan(want, current, listening) {
        Step::Stay => {}
        Step::Leave => {
            manager.remove(guild_id).await.ok();
            st.lock().unwrap().channel = None;
        }
        Step::Join(channel) => join(ctx, data, guild_id, &manager, &st, channel).await,
    }
    Ok(())
}

async fn join(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    manager: &songbird::Songbird,
    st: &Shared,
    channel: ChannelId,
) {
    let log = match manager.join(guild_id, channel).await {
        Ok(call) => {
            // Reset before the handlers go on, so no speaker mapping that arrives first is wiped.
            st.lock().unwrap().joined(channel);
            let mut call = call.lock().await;
            call.remove_all_global_events();
            let rx = Receiver {
                guild_id,
                st: st.clone(),
                tx: data.monitor.sender(ctx, data),
            };
            call.add_global_event(CoreEvent::SpeakingStateUpdate.into(), rx.clone());
            call.add_global_event(CoreEvent::VoiceTick.into(), rx);
            call.mute(true).await.ok(); // it only listens
            tracing::info!(%guild_id, %channel, "monitoring voice");
            Log::new(kind::MONITOR_JOINED, json!({}))
        }
        Err(e) => {
            let error = e.to_string();
            if !st.lock().unwrap().join_failed(&error) {
                return; // the watchdog retries every 30 s: log each error once
            }
            tracing::warn!(%guild_id, %channel, "voice join failed: {error}");
            Log::new(kind::MONITOR_FAILED, json!({ "error": error }))
        }
    };
    stats::record(data, guild_id, log.channel(channel)).await;
}

/// Every server with a monitored channel set.
pub async fn ensure_all(ctx: &serenity::Context, data: &Data) -> Result<()> {
    for (guild_id, _) in data.db.voice_channels().await? {
        let guild_id = GuildId::new(guild_id as u64);
        if ctx.cache.guild(guild_id).is_none() {
            continue; // not (or no longer) in that server
        }
        if let Err(e) = ensure(ctx, data, guild_id).await {
            tracing::warn!(%guild_id, "voice monitor: {e:#}");
        }
    }
    Ok(())
}

/// Transcribes and judges finished segments, several at once (Whisper caps its own CPU use).
async fn work(ctx: serenity::Context, data: Data, mut rx: mpsc::UnboundedReceiver<Heard>) {
    while let Some(h) = rx.recv().await {
        let (ctx, data) = (ctx.clone(), data.clone());
        tokio::spawn(async move {
            let guild_id = h.guild_id;
            if let Err(e) = on_heard(&ctx, &data, h).await {
                tracing::warn!(%guild_id, "voice judging failed: {e:#}");
            }
        });
    }
}

async fn on_heard(ctx: &serenity::Context, data: &Data, h: Heard) -> Result<()> {
    // Non-participants' audio is dropped untranscribed.
    if !crate::participant::is_participant(ctx, data, h.guild_id, h.user, None).await? {
        return Ok(());
    }
    let Some(stt) = &data.stt else {
        return Ok(());
    };
    let audio_ms = h.pcm.len() as u64 * 1000 / 16_000;
    let started = Instant::now();
    let raw = stt.transcribe(h.pcm).await?;
    let transcribe_ms = started.elapsed().as_millis() as u64;
    let text = hostility::clean_transcript(&raw);
    let log = Log::new(
        kind::HEARD,
        json!({ "text": text, "raw": raw, "audio_ms": audio_ms, "transcribe_ms": transcribe_ms }),
    )
    .user(h.user)
    .channel(h.channel)
    .participant(Some(true));
    stats::record(data, h.guild_id, log).await;
    if text.is_empty() {
        return Ok(());
    }
    let u = Utterance {
        user: h.user,
        channel: h.channel,
        source: Source::Voice,
        said: text,
        message: None,
        in_reply_to: None,
    };
    crate::judge::judge(ctx, data, h.guild_id, u, vec![]).await
}

#[derive(Clone)]
struct Receiver {
    guild_id: GuildId,
    st: Shared,
    tx: mpsc::UnboundedSender<Heard>,
}

#[async_trait]
impl EventHandler for Receiver {
    async fn act(&self, ev: &EventContext<'_>) -> Option<Event> {
        let mut s = self.st.lock().unwrap();
        match ev {
            EventContext::SpeakingStateUpdate(sp) => {
                if let Some(u) = sp.user_id {
                    s.speaking(sp.ssrc, UserId::new(u.0));
                }
            }
            EventContext::VoiceTick(tick) => {
                let channel = s.channel?;
                let speaking = tick
                    .speaking
                    .iter()
                    .map(|(ssrc, d)| (*ssrc, d.decoded_voice.as_deref()));
                for (user, pcm) in s.tick(speaking) {
                    let heard = Heard {
                        guild_id: self.guild_id,
                        channel,
                        user,
                        pcm,
                    };
                    self.tx.send(heard).ok();
                }
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(n: u64) -> Option<ChannelId> {
        Some(ChannelId::new(n))
    }

    #[test]
    fn plans() {
        assert_eq!(plan(None, None, None), Step::Stay);
        assert_eq!(plan(None, ch(1), None), Step::Leave);
        assert_eq!(plan(None, None, ch(1)), Step::Leave);
        assert_eq!(plan(ch(1), ch(1), ch(1)), Step::Stay);
        assert_eq!(plan(ch(1), None, None), Step::Join(ChannelId::new(1)));
        assert_eq!(
            plan(ch(1), ch(2), ch(2)),
            Step::Join(ChannelId::new(1)),
            "moved away"
        );
        assert_eq!(
            plan(ch(1), ch(1), None),
            Step::Join(ChannelId::new(1)),
            "not listening"
        );
        assert_eq!(
            plan(ch(1), None, ch(1)),
            Step::Join(ChannelId::new(1)),
            "kicked"
        );
    }

    #[test]
    fn join_errors_are_new_once() {
        let mut l = Listening::default();
        l.joined(ChannelId::new(1));
        assert!(l.join_failed("nope"));
        assert_eq!(l.channel, None);
        assert!(!l.join_failed("nope"), "same error again");
        assert!(l.join_failed("other"));
        l.joined(ChannelId::new(1));
        assert!(l.join_failed("other"), "joining forgets the last error");
    }

    #[test]
    fn ticks_map_ssrcs_to_speakers() {
        let loud = [1000i16; 320];
        let mut l = Listening::default();
        let speak = |l: &mut Listening, ssrc: u32| {
            let mut out = vec![];
            for _ in 0..20 {
                out.extend(l.tick([(ssrc, Some(&loud[..])), (99, None)].into_iter()));
            }
            for _ in 0..60 {
                out.extend(l.tick(std::iter::empty()));
            }
            out
        };
        l.speaking(5, UserId::new(7));
        assert!(speak(&mut l, 5).is_empty(), "not in a channel yet");
        l.joined(ChannelId::new(1));
        assert!(speak(&mut l, 5).is_empty(), "joining forgets SSRCs");
        l.speaking(5, UserId::new(7));
        assert!(speak(&mut l, 6).is_empty(), "unknown SSRC");
        let heard = speak(&mut l, 5);
        assert_eq!(heard.len(), 1);
        assert_eq!(heard[0].0, UserId::new(7));
        assert_eq!(heard[0].1.len(), 20 * 320);
    }
}
