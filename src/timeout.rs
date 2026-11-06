//! Timeouts. Discord's native timeout is used where Discord allows it; the owner, admins and
//! anyone at or above the bot's top role get a forced one instead. Either way the bot enforces it
//! too, so both behave the same: until it ends, the user's messages are deleted, their reactions
//! are removed, and they're server-muted in VC.
//!
//! A timeout row lives in the database from [`time_out`] until [`release`] has unmuted the user
//! (if the bot muted them). [`release`] is the only place a row is removed.

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use poise::serenity_prelude::{
    self as serenity, EditMember, GuildId, Member, Message, Reaction, Timestamp, UserId,
};
use serde_json::json;

use crate::app::Data;
use crate::db::{Method, TimeoutRow};
use crate::stats::{self, Log, kind};

/// How long a hostile participant is timed out for.
pub const TIMEOUT_SECS: u64 = 10;

/// What decides whether Discord lets the bot time a member out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Standing {
    pub is_owner: bool,
    pub is_admin: bool,
    /// Position of the member's highest role (0 = only @everyone).
    pub top_role: u16,
    /// Position of the bot's highest role.
    pub bot_top_role: u16,
}

/// Discord refuses native timeouts on the owner, admins, and anyone whose top role isn't below
/// the bot's.
pub fn method(s: Standing) -> Method {
    if s.is_owner || s.is_admin || s.top_role >= s.bot_top_role {
        Method::Forced
    } else {
        Method::Native
    }
}

fn standing(ctx: &serenity::Context, guild_id: GuildId, member: &Member) -> Option<Standing> {
    let g = ctx.cache.guild(guild_id)?;
    let bot = g.members.get(&ctx.cache.current_user().id)?;
    let top = |m: &Member| {
        m.roles
            .iter()
            .filter_map(|r| g.roles.get(r))
            .map(|r| r.position)
            .max()
            .unwrap_or(0)
    };
    Some(Standing {
        is_owner: g.owner_id == member.user.id,
        is_admin: g.member_permissions(member).administrator(),
        top_role: top(member),
        bot_top_role: top(bot),
    })
}

/// Audit log reasons are capped at 512 characters by Discord.
fn audit_reason(reason: &str) -> String {
    reason.chars().take(500).collect()
}

/// What [`time_out`] did.
#[derive(Clone, Debug)]
pub struct Applied {
    pub method: Method,
    pub until: DateTime<Utc>,
    /// Why a native timeout was refused, when it fell back to forced.
    pub native_error: Option<String>,
    /// Whether they were in VC and the bot server-muted them.
    pub muted: bool,
}

/// Time `user` out for [`TIMEOUT_SECS`], natively if Discord allows it, otherwise forced.
pub async fn time_out(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user: UserId,
    reason: &str,
) -> Result<Applied> {
    let until = Utc::now() + Duration::from_secs(TIMEOUT_SECS);
    let member = guild_id.member(ctx, user).await?;
    let mut method = standing(ctx, guild_id, &member).map_or(Method::Forced, method);
    let mut native_error = None;
    if method == Method::Native {
        let reason = audit_reason(reason);
        let edit = EditMember::new()
            .disable_communication_until_datetime(Timestamp::from(until))
            .audit_log_reason(&reason);
        if let Err(e) = guild_id.edit_member(ctx, user, edit).await {
            tracing::info!(%guild_id, %user, "native timeout refused, forcing it: {e}");
            native_error = Some(e.to_string());
            method = Method::Forced;
        }
    }
    data.db
        .start_timeout(guild_id.get(), user.get(), until, method)
        .await?;
    let muted = in_voice(ctx, guild_id, user) && mute(ctx, data, guild_id, user).await;
    let applied = Applied {
        method,
        until,
        native_error,
        muted,
    };
    let log = Log::new(
        kind::TIMED_OUT,
        json!({
            "method": applied.method.to_string(),
            "secs": TIMEOUT_SECS,
            "until": applied.until,
            "reason": reason,
            "native_error": applied.native_error,
            "muted": applied.muted,
        }),
    )
    .user(user)
    .participant(Some(true));
    stats::record(data, guild_id, log).await;
    Ok(applied)
}

fn in_voice(ctx: &serenity::Context, guild_id: GuildId, user: UserId) -> bool {
    voice_state(ctx, guild_id, user).is_some()
}

/// `(channel, server-muted)` if the user is in VC, from the cache.
fn voice_state(ctx: &serenity::Context, guild_id: GuildId, user: UserId) -> Option<(u64, bool)> {
    let g = ctx.cache.guild(guild_id)?;
    let vs = g.voice_states.get(&user)?;
    Some((vs.channel_id?.get(), vs.mute))
}

/// Server-mute `user` (in VC) for their timeout, unless someone else already muted them.
/// Returns whether the bot muted them.
async fn mute(ctx: &serenity::Context, data: &Data, guild_id: GuildId, user: UserId) -> bool {
    if voice_state(ctx, guild_id, user).is_some_and(|(_, muted)| muted) {
        return false; // a moderator's mute: not ours to lift later
    }
    // Record the debt first: if the bot dies between these two lines, it unmutes on restart.
    if let Err(e) = data
        .db
        .set_voice_muted(guild_id.get(), user.get(), true)
        .await
    {
        tracing::warn!(%guild_id, %user, "couldn't record mute: {e:#}");
        return false;
    }
    let edit = EditMember::new()
        .mute(true)
        .audit_log_reason("NiceNovember: timed out");
    match guild_id.edit_member(ctx, user, edit).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(%guild_id, %user, "server mute failed: {e}");
            data.db
                .set_voice_muted(guild_id.get(), user.get(), false)
                .await
                .ok();
            false
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseStep {
    /// Nothing to undo: just drop the row.
    Drop,
    /// Lift the bot's server mute, then drop the row.
    Unmute,
    /// Owed an unmute but not in VC, where Discord can't lift it: keep the row for later.
    Wait,
}

pub fn release_step(voice_muted: bool, in_voice: bool) -> ReleaseStep {
    match (voice_muted, in_voice) {
        (false, _) => ReleaseStep::Drop,
        (true, true) => ReleaseStep::Unmute,
        (true, false) => ReleaseStep::Wait,
    }
}

/// Lift a timeout that has ended: unmute them if the bot muted them, then drop the row. If
/// they're owed an unmute but aren't in VC (a server mute sticks until lifted), the row stays and
/// [`on_voice_join`] unmutes them when they're back.
pub async fn release(ctx: &serenity::Context, data: &Data, row: &TimeoutRow) -> Result<()> {
    let (guild_id, user) = (
        GuildId::new(row.guild_id as u64),
        UserId::new(row.user_id as u64),
    );
    match release_step(row.voice_muted, voice_state(ctx, guild_id, user).is_some()) {
        ReleaseStep::Wait => return Ok(()),
        ReleaseStep::Unmute => {
            let edit = EditMember::new()
                .mute(false)
                .audit_log_reason("NiceNovember: timeout over");
            guild_id.edit_member(ctx, user, edit).await?;
        }
        ReleaseStep::Drop => {}
    }
    data.db.delete_timeout(guild_id.get(), user.get()).await?;
    let log = Log::new(
        kind::TIMEOUT_RELEASED,
        json!({ "method": row.method, "unmuted": row.voice_muted, "until": row.until }),
    )
    .user(user);
    stats::record(data, guild_id, log).await;
    Ok(())
}

/// Release every timeout that has ended. Runs every second.
pub async fn sweep(ctx: &serenity::Context, data: &Data) -> Result<()> {
    for row in data.db.ended_timeouts(Utc::now()).await? {
        if let Err(e) = release(ctx, data, &row).await {
            tracing::warn!(
                guild_id = row.guild_id,
                user_id = row.user_id,
                "release failed: {e:#}"
            );
        }
    }
    Ok(())
}

/// End a timeout early. Returns whether one was active.
pub async fn pardon(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user: UserId,
    by: UserId,
) -> Result<bool> {
    let Some(row) = data.db.timeout(guild_id.get(), user.get()).await? else {
        return Ok(false);
    };
    if !data.db.end_timeout(guild_id.get(), user.get()).await? {
        return Ok(false);
    }
    if row.method == Method::Native.to_string() {
        let edit = EditMember::new()
            .enable_communication()
            .audit_log_reason("NiceNovember: pardoned");
        guild_id.edit_member(ctx, user, edit).await.ok();
    }
    let log = Log::new(kind::PARDONED, json!({ "by": by.to_string() })).user(user);
    stats::record(data, guild_id, log).await;
    release(ctx, data, &row).await?;
    Ok(true)
}

/// Someone joined (or moved within) VC: mute them if they're timed out, unmute them if they're
/// owed it.
pub async fn on_voice_join(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user: UserId,
) -> Result<()> {
    let Some(row) = data.db.timeout(guild_id.get(), user.get()).await? else {
        return Ok(());
    };
    let active = row.until().is_some_and(|u| u > Utc::now());
    if !active {
        return release(ctx, data, &row).await;
    }
    if !row.voice_muted && mute(ctx, data, guild_id, user).await {
        let log = Log::new(kind::TIMEOUT_VC, json!({ "until": row.until })).user(user);
        stats::record(data, guild_id, log).await;
    }
    Ok(())
}

/// A timed-out user posted: delete it, with a short-lived notice. Returns whether they were
/// timed out.
pub async fn on_message(ctx: &serenity::Context, data: &Data, msg: &Message) -> Result<bool> {
    let Some(guild_id) = msg.guild_id else {
        return Ok(false);
    };
    let Some(until) = data
        .db
        .timed_out(guild_id.get(), msg.author.id.get())
        .await?
    else {
        return Ok(false);
    };
    msg.delete(ctx).await.ok();
    let log = Log::new(
        kind::TIMEOUT_MESSAGE,
        json!({ "message_id": msg.id, "content": msg.content, "until": until }),
    )
    .user(msg.author.id)
    .channel(msg.channel_id);
    stats::record(data, guild_id, log).await;
    let notice = format!(
        "⛔ <@{}> you're timed out for being hostile. Back <t:{}:R>.",
        msg.author.id,
        until.timestamp()
    );
    if let Ok(notice) = msg.channel_id.say(ctx, notice).await {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(TIMEOUT_SECS)).await;
            notice.delete(&ctx).await.ok();
        });
    }
    Ok(true)
}

/// A timed-out user reacted: take it back off.
pub async fn on_reaction(ctx: &serenity::Context, data: &Data, r: &Reaction) -> Result<()> {
    let (Some(guild_id), Some(user)) = (r.guild_id, r.user_id) else {
        return Ok(());
    };
    if data
        .db
        .timed_out(guild_id.get(), user.get())
        .await?
        .is_none()
    {
        return Ok(());
    }
    r.delete(ctx).await.ok();
    let log = Log::new(
        kind::TIMEOUT_REACTION,
        json!({ "message_id": r.message_id, "emoji": r.emoji }),
    )
    .user(user)
    .channel(r.channel_id);
    stats::record(data, guild_id, log).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(is_owner: bool, is_admin: bool, top_role: u16) -> Standing {
        Standing {
            is_owner,
            is_admin,
            top_role,
            bot_top_role: 5,
        }
    }

    #[test]
    fn untouchable_members_get_forced_timeouts() {
        assert_eq!(method(s(false, false, 0)), Method::Native);
        assert_eq!(method(s(false, false, 4)), Method::Native);
        assert_eq!(method(s(false, false, 5)), Method::Forced, "same level");
        assert_eq!(method(s(false, false, 9)), Method::Forced, "above the bot");
        assert_eq!(method(s(true, false, 0)), Method::Forced, "owner");
        assert_eq!(method(s(false, true, 0)), Method::Forced, "admin");
    }

    #[test]
    fn mutes_are_lifted_only_in_voice() {
        assert_eq!(release_step(false, false), ReleaseStep::Drop);
        assert_eq!(release_step(false, true), ReleaseStep::Drop);
        assert_eq!(release_step(true, true), ReleaseStep::Unmute);
        assert_eq!(release_step(true, false), ReleaseStep::Wait);
    }

    #[test]
    fn audit_reasons_fit() {
        assert_eq!(audit_reason("short"), "short");
        assert_eq!(audit_reason(&"x".repeat(900)).chars().count(), 500);
    }
}
