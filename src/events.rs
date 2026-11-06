//! Routes Discord events: everything is logged, timeouts are enforced, participants' messages
//! and edits are judged, and the voice monitor is kept in its channel.

use anyhow::Result;
use poise::serenity_prelude::{
    self as serenity, FullEvent, GuildId, Message, MessageUpdateEvent, VoiceState,
};

use crate::app::Data;
use crate::hostility::{Source, Utterance};
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
        FullEvent::ReactionAdd { add_reaction } => {
            timeout::on_reaction(ctx, data, add_reaction).await?
        }
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
        said: msg.content.clone(),
        message: Some(msg.id),
        in_reply_to: msg.referenced_message.as_ref().map(|r| r.content.clone()),
        call: None,
    };
    judge::judge(ctx, data, guild_id, u, images).await
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
        said: text.to_owned(),
        message: Some(e.id),
        in_reply_to: None,
        call: None,
    };
    judge::judge(ctx, data, guild_id, u, vec![]).await
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
