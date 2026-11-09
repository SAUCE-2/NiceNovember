//! Routes Discord events: everything is logged, timeouts are enforced, participants' messages,
//! edits and reactions are judged, and the voice monitor is kept in its channel.

use anyhow::Result;
use chrono::TimeDelta;
use poise::serenity_prelude::{
    self as serenity, ChannelId, FullEvent, GetMessages, GuildId, Message, MessageId,
    MessageUpdateEvent, Reaction, ReactionType, VoiceState,
};

use crate::app::Data;
use crate::hostility::{Line, Source, Utterance};
use crate::participant::is_participant;
use crate::voice::monitor;
use crate::{judge, setup, stats, timeout};

pub async fn handle(ctx: &serenity::Context, event: &FullEvent, data: &Data) -> Result<()> {
    // Log everything first, whatever happens next.
    stats::capture::capture(ctx, event, data).await;
    match event {
        FullEvent::GuildCreate { guild, .. } => {
            spawn_report(ctx, data, guild.id);
            let (ctx, data, guild_id) = (ctx.clone(), data.clone(), guild.id);
            tokio::spawn(async move { monitor::ensure(&ctx, &data, guild_id).await });
        }
        FullEvent::GuildRoleDelete { guild_id, .. } => spawn_report(ctx, data, *guild_id),
        FullEvent::ChannelDelete { channel, .. } => spawn_report(ctx, data, channel.guild_id),
        FullEvent::Message { new_message } => on_message(ctx, data, new_message).await?,
        FullEvent::MessageUpdate {
            old_if_available,
            event,
            ..
        } => on_edit(ctx, data, event, old_if_available.as_ref()).await?,
        FullEvent::ReactionAdd { add_reaction } => on_reaction(ctx, data, add_reaction).await?,
        FullEvent::VoiceStateUpdate { old, new } => on_voice(ctx, data, old.as_ref(), new).await?,
        _ => {}
    }
    Ok(())
}

fn spawn_report(ctx: &serenity::Context, data: &Data, guild_id: GuildId) {
    let (ctx, data) = (ctx.clone(), data.clone());
    tokio::spawn(async move {
        if let Err(e) = setup::report(&ctx, &data, guild_id).await {
            tracing::warn!(%guild_id, "setup report failed: {e:#}");
        }
    });
}

async fn on_message(ctx: &serenity::Context, data: &Data, msg: &Message) -> Result<()> {
    let Some(guild_id) = msg.guild_id else {
        return Ok(());
    };
    if msg.author.bot || timeout::on_message(ctx, data, msg).await? {
        return Ok(());
    }
    let roles = msg.member.as_ref().map(|m| m.roles.as_slice());
    if !is_participant(ctx, data, guild_id, msg.author.id, roles).await? {
        return Ok(());
    }
    let images = crate::ai::message_images(msg).await;
    if msg.content.trim().is_empty() && images.is_empty() {
        return Ok(());
    }
    let u = Utterance {
        user: msg.author.id,
        channel: msg.channel_id,
        source: Source::Message,
        author: msg.author.display_name().to_owned(),
        said: msg.content.clone(),
        at: *msg.timestamp,
        message: Some(msg.id),
        in_reply_to: msg.referenced_message.as_ref().map(|r| r.content.clone()),
        recent: chat_before(ctx, msg.channel_id, msg.id).await,
        in_call: vec![],
    };
    judge::judge(ctx, data, guild_id, u, images).await
}

/// Chat older than this before a message isn't its conversation any more.
const CHAT_WINDOW: TimeDelta = TimeDelta::minutes(30);
/// At most this many messages of chat context.
const CHAT_LINES: u8 = 10;

/// The conversation before `message` in `channel`. Empty if Discord won't say (logged).
pub async fn chat_before(
    ctx: &serenity::Context,
    channel: ChannelId,
    message: MessageId,
) -> Vec<Line> {
    let latest = GetMessages::new().before(message).limit(CHAT_LINES);
    match channel.messages(ctx, latest).await {
        Ok(msgs) => chat_lines(&msgs, message),
        Err(e) => {
            tracing::debug!(%channel, "no chat context: {e}");
            vec![]
        }
    }
}

/// Messages (newest first, as Discord gives them) sent in the [`CHAT_WINDOW`] before `message`,
/// oldest first. Ones with no text are left out.
pub fn chat_lines(newest_first: &[Message], message: MessageId) -> Vec<Line> {
    let sent = *message.created_at();
    newest_first
        .iter()
        .rev()
        .filter(|m| !m.content.trim().is_empty() && sent - *m.timestamp <= CHAT_WINDOW)
        .map(|m| Line {
            speaker: m.author.display_name().to_owned(),
            said: m.content.clone(),
            at: *m.timestamp,
        })
        .collect()
}

/// The new text of a real edit: not an embed unfurl, and not the same text again.
pub fn edited_text<'a>(e: &'a MessageUpdateEvent, old: Option<&Message>) -> Option<&'a str> {
    e.edited_timestamp?;
    let text = e.content.as_deref()?;
    let unchanged = old.is_some_and(|o| o.content == text);
    (!unchanged && !text.trim().is_empty()).then_some(text)
}

async fn on_edit(
    ctx: &serenity::Context,
    data: &Data,
    e: &MessageUpdateEvent,
    old: Option<&Message>,
) -> Result<()> {
    let (Some(guild_id), Some(author)) = (e.guild_id, &e.author) else {
        return Ok(());
    };
    let Some(text) = edited_text(e, old) else {
        return Ok(());
    };
    if author.bot || !is_participant(ctx, data, guild_id, author.id, None).await? {
        return Ok(());
    }
    let u = Utterance {
        user: author.id,
        channel: e.channel_id,
        source: Source::Edit,
        author: author.display_name().to_owned(),
        said: text.to_owned(),
        at: e.edited_timestamp.map_or_else(chrono::Utc::now, |t| *t),
        message: Some(e.id),
        in_reply_to: None,
        recent: chat_before(ctx, e.channel_id, e.id).await,
        in_call: vec![],
    };
    judge::judge(ctx, data, guild_id, u, vec![]).await
}

/// An emoji as people see it: the character itself, or `:name:` for a custom one.
pub fn emoji_text(emoji: &ReactionType) -> String {
    match emoji {
        ReactionType::Custom { name: Some(n), .. } => format!(":{n}:"),
        other => other.to_string(),
    }
}

async fn on_reaction(ctx: &serenity::Context, data: &Data, r: &Reaction) -> Result<()> {
    let (Some(guild_id), Some(user)) = (r.guild_id, r.user_id) else {
        return Ok(());
    };
    if timeout::on_reaction(ctx, data, r).await?
        || r.member.as_ref().is_some_and(|m| m.user.bot)
        || r.message_author_id == Some(user)
    {
        return Ok(());
    }
    let roles = r.member.as_ref().map(|m| m.roles.as_slice());
    if !is_participant(ctx, data, guild_id, user, roles).await? {
        return Ok(());
    }
    let on = r.message(ctx).await?;
    let images = crate::ai::message_images(&on).await;
    let u = Utterance {
        user,
        channel: r.channel_id,
        source: Source::Reaction,
        author: r
            .member
            .as_ref()
            .map_or_else(|| user.to_string(), |m| m.display_name().into()),
        said: emoji_text(&r.emoji),
        at: chrono::Utc::now(),
        message: Some(r.message_id),
        in_reply_to: Some(on.content),
        recent: vec![],
        in_call: vec![],
    };
    judge::judge(ctx, data, guild_id, u, images).await
}

async fn on_voice(
    ctx: &serenity::Context,
    data: &Data,
    old: Option<&VoiceState>,
    new: &VoiceState,
) -> Result<()> {
    let Some(guild_id) = new.guild_id else {
        return Ok(());
    };
    let old_ch = old.and_then(|o| o.channel_id);
    if new.user_id == ctx.cache.current_user().id {
        // Moved or kicked out of the monitored channel: go back.
        if new.channel_id != data.monitor.listening_in(guild_id) {
            let (ctx, data) = (ctx.clone(), data.clone());
            tokio::spawn(async move { monitor::ensure(&ctx, &data, guild_id).await });
        }
        return Ok(());
    }
    if new.member.as_ref().is_some_and(|m| m.user.bot) || old_ch == new.channel_id {
        return Ok(()); // bots, and mute/deafen/stream toggles
    }
    if let Some(ch) = old_ch {
        data.monitor.user_left(guild_id, new.user_id, ch);
    }
    // Someone came or went: the bot joins the monitored channel only while anyone's in it.
    let (c, d) = (ctx.clone(), data.clone());
    tokio::spawn(async move { monitor::ensure(&c, &d, guild_id).await });
    if new.channel_id.is_some() {
        timeout::on_voice_join(ctx, data, guild_id, new.user_id).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn update(content: Option<&str>, edited: bool) -> MessageUpdateEvent {
        let mut v = json!({ "id": "1", "channel_id": "2" });
        if let Some(c) = content {
            v["content"] = json!(c);
        }
        if edited {
            v["edited_timestamp"] = json!("2026-11-06T10:00:00Z");
        }
        serde_json::from_value(v).unwrap()
    }

    fn sent(secs: i64, name: &str, content: &str) -> Message {
        let mut m = Message::default();
        m.timestamp = serenity::Timestamp::from_unix_timestamp(secs).unwrap();
        m.author.name = name.into();
        m.content = content.into();
        m
    }

    #[test]
    fn chat_context_is_the_recent_conversation_oldest_first() {
        let discord_epoch_ms = 1_420_070_400_000;
        let now = 1_800_000_000;
        let message = MessageId::new(((now * 1000 - discord_epoch_ms) as u64) << 22);
        let newest_first = [
            sent(now - 5, "alex", "check out my drawing"),
            sent(now - 6, "alex", ""),
            sent(now - 30 * 60, "jo", "hi all"),
            sent(now - 30 * 60 - 1, "jo", "too old"),
        ];
        let lines = chat_lines(&newest_first, message);
        let shown: Vec<_> = lines
            .iter()
            .map(|l| (l.speaker.as_str(), l.said.as_str(), l.at.timestamp()))
            .collect();
        assert_eq!(
            shown,
            [
                ("jo", "hi all", now - 30 * 60),
                ("alex", "check out my drawing", now - 5)
            ]
        );
    }

    #[test]
    fn emoji_are_shown_as_people_see_them() {
        assert_eq!(emoji_text(&ReactionType::Unicode("🖕".into())), "🖕");
        let custom: ReactionType =
            serde_json::from_value(json!({ "id": "5", "name": "clown_cry" })).unwrap();
        assert_eq!(emoji_text(&custom), ":clown_cry:");
    }

    #[test]
    fn only_real_edits_are_judged() {
        assert_eq!(
            edited_text(&update(Some("you suck"), true), None),
            Some("you suck")
        );
        assert_eq!(
            edited_text(&update(Some("you suck"), false), None),
            None,
            "unfurl"
        );
        assert_eq!(edited_text(&update(None, true), None), None);
        assert_eq!(edited_text(&update(Some("  "), true), None), None);
        let mut old = Message::default();
        old.content = "you suck".into();
        assert_eq!(
            edited_text(&update(Some("you suck"), true), Some(&old)),
            None
        );
        old.content = "hi".into();
        assert_eq!(
            edited_text(&update(Some("you suck"), true), Some(&old)),
            Some("you suck")
        );
    }
}
