//! Guild config health check: tells admins what's missing and how to fix it.

use anyhow::Result;
use poise::serenity_prelude::{
    self as serenity, ChannelId, ChannelType, CreateEmbed, CreateMessage, GuildId, RoleId,
};

use crate::app::Data;

/// Everything wrong with this guild's setup, each with the fix.
pub async fn problems(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
) -> Result<Vec<String>> {
    let gcfg = data.db.guild(guild_id.get()).await?;
    let bot_id = ctx.cache.current_user().id;
    let bot_member = guild_id.member(ctx, bot_id).await.ok();
    let mut out = Vec::new();
    {
        let Some(g) = ctx.cache.guild(guild_id) else {
            return Ok(vec![
                "Server isn't cached yet; try again in a moment.".into(),
            ]);
        };
        match gcfg.participant_role_id.map(|r| RoleId::new(r as u64)) {
            None => out
                .push("No participant role, so nobody is judged → `/nice setup role:@Role`".into()),
            Some(r) if !g.roles.contains_key(&r) => {
                out.push("The participant role was deleted → `/nice setup role:@Role`".into())
            }
            _ => {}
        }
        let channel = |id: Option<i64>| id.and_then(|c| g.channels.get(&ChannelId::new(c as u64)));
        if gcfg.announce_channel_id.is_none() {
            out.push(
                "No announce channel (hostile quotes go there) → `/nice setup announce_channel:#channel`"
                    .into(),
            );
        } else if channel(gcfg.announce_channel_id).is_none() {
            out.push(
                "The announce channel was deleted → `/nice setup announce_channel:#channel`".into(),
            );
        }
        if gcfg.log_channel_id.is_some() && channel(gcfg.log_channel_id).is_none() {
            out.push("The log channel was deleted → `/nice setup log_channel:#channel`".into());
        }
        match (gcfg.voice_channel_id, channel(gcfg.voice_channel_id)) {
            (None, _) => out.push(
                "No monitored voice channel, so voice isn't judged → `/nice setup voice_channel:🔊channel`"
                    .into(),
            ),
            (Some(_), None) => out.push(
                "The monitored voice channel was deleted → `/nice setup voice_channel:🔊channel`"
                    .into(),
            ),
            (Some(_), Some(c)) if !matches!(c.kind, ChannelType::Voice | ChannelType::Stage) => out
                .push(
                    "The monitored channel isn't a voice channel → `/nice setup voice_channel:🔊channel`"
                        .into(),
                ),
            _ => {}
        }
        if let Some(me) = &bot_member
            && !g.member_permissions(me).administrator()
        {
            out.push(
                "I need **Administrator** → give my role it in Server Settings → Roles".into(),
            );
        }
    }
    if data.jev.is_none() {
        out.push("`JEV_API_KEY` isn't set on the bot, so nothing can be judged".into());
    }
    if data.stt.is_none() && gcfg.voice_channel_id.is_some() {
        out.push(
            "The Whisper model isn't loaded (check `WHISPER_MODEL`), so voice can't be transcribed"
                .into(),
        );
    }
    Ok(out)
}

pub fn embed(problems: &[String]) -> CreateEmbed {
    if problems.is_empty() {
        return CreateEmbed::new()
            .title("✅ NiceNovember is fully set up")
            .description(format!(
                "Participants who get hostile are timed out for {}s and quoted in the announce channel.",
                crate::timeout::TIMEOUT_SECS
            ))
            .colour(0x2ecc71);
    }
    let body: String = problems.iter().map(|p| format!("• {p}\n")).collect();
    CreateEmbed::new()
        .title("🤝 NiceNovember needs some setup")
        .description(format!(
            "{body}\nAdmins: run `/nice setup` any time to see this checklist."
        ))
        .colour(0xe74c3c)
}

/// Run the check and post it if the set of problems changed since the last report.
pub async fn report(ctx: &serenity::Context, data: &Data, guild_id: GuildId) -> Result<()> {
    let problems = problems(ctx, data, guild_id).await?;
    let fingerprint = problems.join("\n");
    let last = data.db.guild(guild_id.get()).await?.last_setup_hash;
    if !should_report(last.as_deref(), &fingerprint) {
        return Ok(());
    }
    data.db
        .set_setup_hash(guild_id.get(), Some(&fingerprint))
        .await?;
    send(
        ctx,
        data,
        guild_id,
        CreateMessage::new().embed(embed(&problems)),
    )
    .await;
    Ok(())
}

/// Post when the problems changed, except "all fine" when nothing was ever reported.
pub fn should_report(last: Option<&str>, fingerprint: &str) -> bool {
    let last_was_ok = last.is_none_or(str::is_empty);
    last != Some(fingerprint) && !(fingerprint.is_empty() && last_was_ok)
}

/// Log channel → announce channel → system channel → first writable channel → DM the owner.
async fn send(ctx: &serenity::Context, data: &Data, guild_id: GuildId, msg: CreateMessage) {
    let preferred = data
        .db
        .guild(guild_id.get())
        .await
        .ok()
        .and_then(|g| g.log_or_announce());
    let (candidates, owner) = {
        let Some(g) = ctx.cache.guild(guild_id) else {
            return;
        };
        let me = g.members.get(&ctx.cache.current_user().id);
        let mut text: Vec<_> = g
            .channels
            .values()
            .filter(|c| c.kind == ChannelType::Text)
            .collect();
        text.sort_by_key(|c| c.position);
        let writable = text
            .into_iter()
            .filter(|c| me.is_some_and(|m| g.user_permissions_in(c, m).send_messages()))
            .map(|c| c.id);
        let candidates: Vec<ChannelId> = preferred
            .map(|c| ChannelId::new(c as u64))
            .into_iter()
            .chain(g.system_channel_id)
            .chain(writable)
            .collect();
        (candidates, g.owner_id)
    };
    for ch in candidates {
        if ch.send_message(ctx, msg.clone()).await.is_ok() {
            return;
        }
    }
    if let Err(e) = owner.direct_message(ctx, msg).await {
        tracing::warn!(%guild_id, "couldn't deliver setup notice anywhere: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_only_changes() {
        assert!(should_report(None, "a"));
        assert!(!should_report(Some("a"), "a"));
        assert!(should_report(Some("a"), "b"));
        assert!(should_report(Some("a"), ""), "fixed: say so");
        assert!(!should_report(None, ""), "fine from the start: stay quiet");
        assert!(!should_report(Some(""), ""));
    }
}
