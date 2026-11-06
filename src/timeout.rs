//! Timeouts. Discord's own timeout isn't used: the bot enforces every timeout itself, the same
//! for everyone. Until it ends, the user's messages are deleted, their reactions are removed, and
//! they're server-muted and deafened in VC.
//!
//! A timeout row lives in the database from [`time_out`] until [`release`] has undone the mute
//! and deafen (if the bot applied them). [`release`] is the only place a row is removed.

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use poise::serenity_prelude::{self as serenity, EditMember, GuildId, Message, Reaction, UserId};
use serde_json::json;

use crate::app::Data;
use crate::db::TimeoutRow;
use crate::stats::{self, Log, kind};

/// How long a hostile participant is timed out for.
pub const TIMEOUT_SECS: u64 = 10;

/// What [`time_out`] did.
#[derive(Clone, Debug)]
pub struct Applied {
    pub until: DateTime<Utc>,
    /// Whether they were in VC and the bot server-muted and deafened them.
    pub muted: bool,
}

/// Time `user` out for [`TIMEOUT_SECS`].
pub async fn time_out(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user: UserId,
    reason: &str,
) -> Result<Applied> {
    let until = Utc::now() + Duration::from_secs(TIMEOUT_SECS);
    data.db
        .start_timeout(guild_id.get(), user.get(), until)
        .await?;
    let muted = in_voice(ctx, guild_id, user) && mute(ctx, data, guild_id, user).await;
    let applied = Applied { until, muted };
    let log = Log::new(
        kind::TIMED_OUT,
        json!({
            "secs": TIMEOUT_SECS,
            "until": applied.until,
            "reason": reason,
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

/// Server-mute and deafen `user` (in VC) for their timeout, unless someone else already muted
/// them. Returns whether the bot did.
async fn mute(ctx: &serenity::Context, data: &Data, guild_id: GuildId, user: UserId) -> bool {
    // ponytail: one flag covers both, so a moderator's deafen (without a mute) is lifted with
    // ours. Track mute and deafen separately if that ever matters.
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
        .deafen(true)
        .audit_log_reason("NiceNovember: timed out");
    match guild_id.edit_member(ctx, user, edit).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(%guild_id, %user, "server mute/deafen failed: {e}");
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
    /// Lift the bot's server mute and deafen, then drop the row.
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

/// Lift a timeout that has ended: unmute and undeafen them if the bot muted them, then drop the
/// row. If they're owed an unmute but aren't in VC (a server mute sticks until lifted), the row
/// stays and [`on_voice_join`] unmutes them when they're back.
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
                .deafen(false)
                .audit_log_reason("NiceNovember: timeout over");
            guild_id.edit_member(ctx, user, edit).await?;
        }
        ReleaseStep::Drop => {}
    }
    data.db.delete_timeout(guild_id.get(), user.get()).await?;
    let log = Log::new(
        kind::TIMEOUT_RELEASED,
        json!({ "unmuted": row.voice_muted, "until": row.until }),
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
    let log = Log::new(kind::PARDONED, json!({ "by": by.to_string() })).user(user);
    stats::record(data, guild_id, log).await;
    release(ctx, data, &row).await?;
    Ok(true)
}

/// Someone joined (or moved within) VC: mute and deafen them if they're timed out, undo it if
/// they're owed that.
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

/// A timed-out user reacted: take it back off. Returns whether it did.
pub async fn on_reaction(ctx: &serenity::Context, data: &Data, r: &Reaction) -> Result<bool> {
    let (Some(guild_id), Some(user)) = (r.guild_id, r.user_id) else {
        return Ok(false);
    };
    if data
        .db
        .timed_out(guild_id.get(), user.get())
        .await?
        .is_none()
    {
        return Ok(false);
    }
    r.delete(ctx).await.ok();
    let log = Log::new(
        kind::TIMEOUT_REACTION,
        json!({ "message_id": r.message_id, "emoji": r.emoji }),
    )
    .user(user)
    .channel(r.channel_id);
    stats::record(data, guild_id, log).await;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutes_are_lifted_only_in_voice() {
        assert_eq!(release_step(false, false), ReleaseStep::Drop);
        assert_eq!(release_step(false, true), ReleaseStep::Drop);
        assert_eq!(release_step(true, true), ReleaseStep::Unmute);
        assert_eq!(release_step(true, false), ReleaseStep::Wait);
    }
}
