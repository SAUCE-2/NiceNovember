//! Nobody gets to deafen the bot: if someone server-deafens it, it undeafens itself and times
//! them out for [`DEAFEN_TIMEOUT_SECS`].

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use poise::serenity_prelude::{
    self as serenity, EditMember, GuildId, UserId, VoiceState,
    audit_log::{Action, AuditLogEntry, Change, MemberAction},
};

use crate::app::Data;
use crate::timeout;

/// How long deafening the bot times someone out for.
pub const DEAFEN_TIMEOUT_SECS: u64 = 60;

/// Discord writes the audit log a moment after the voice update: look this many times, each a
/// second after the last.
const LOOKS: u32 = 5;

/// Whether this voice update is the bot being server-deafened (not already deafened).
pub fn newly_deafened(was: Option<bool>, now: bool) -> bool {
    now && was != Some(true)
}

/// Who server-deafened `bot` at or after `since`, from audit log entries (newest first).
/// `None` if no such entry, or the bot did it itself.
pub fn deafened_by(entries: &[AuditLogEntry], bot: UserId, since: DateTime<Utc>) -> Option<UserId> {
    let deafens = |e: &&AuditLogEntry| {
        e.changes.iter().flatten().any(|c| {
            matches!(
                c,
                Change::Deaf {
                    new: Some(true),
                    ..
                }
            )
        })
    };
    entries
        .iter()
        .filter(|e| *e.id.created_at() >= since)
        .filter(|e| e.target_id.is_some_and(|t| t.get() == bot.get()))
        .find(deafens)
        .map(|e| e.user_id)
        .filter(|u| *u != bot)
}

/// The bot's voice state changed from `old` to `new`: if it was just server-deafened, deal with
/// it in the background.
pub fn check(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    old: Option<&VoiceState>,
    new: &VoiceState,
) {
    if !newly_deafened(old.map(|o| o.deaf), new.deaf) {
        return;
    }
    let (ctx, data) = (ctx.clone(), data.clone());
    tokio::spawn(async move {
        if let Err(e) = on_deafened(&ctx, &data, guild_id).await {
            tracing::warn!(%guild_id, "handling a deafen failed: {e:#}");
        }
    });
}

/// The bot was server-deafened in `guild_id`: undeafen it and time out whoever did it.
async fn on_deafened(ctx: &serenity::Context, data: &Data, guild_id: GuildId) -> Result<()> {
    let since = Utc::now() - Duration::from_secs(10);
    let bot = ctx.cache.current_user().id;
    let undeafen = EditMember::new()
        .deafen(false)
        .audit_log_reason("NiceNovember: don't deafen the bot");
    if let Err(e) = guild_id.edit_member(ctx, bot, undeafen).await {
        tracing::warn!(%guild_id, "couldn't undeafen myself: {e}");
    }
    let Some(culprit) = find_culprit(ctx, guild_id, bot, since).await? else {
        tracing::warn!(%guild_id, "deafened, but the audit log doesn't say who");
        return Ok(());
    };
    timeout::time_out(
        ctx,
        data,
        guild_id,
        culprit,
        DEAFEN_TIMEOUT_SECS,
        "deafened the bot",
    )
    .await?;
    Ok(())
}

async fn find_culprit(
    ctx: &serenity::Context,
    guild_id: GuildId,
    bot: UserId,
    since: DateTime<Utc>,
) -> Result<Option<UserId>> {
    let updates = Some(Action::Member(MemberAction::Update));
    for _ in 0..LOOKS {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let log = guild_id
            .audit_logs(ctx, updates, None, None, Some(10))
            .await?;
        if let Some(who) = deafened_by(&log.entries, bot, since) {
            return Ok(Some(who));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const BOT: u64 = 900;
    /// A snowflake made at 2026-11-17 12:00:00 UTC.
    const AT: u64 = (1_794_916_800_000u64 - 1_420_070_400_000) << 22;

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_794_916_800, 0).unwrap()
    }

    fn entry(id: u64, by: u64, target: u64, changes: Value) -> AuditLogEntry {
        serde_json::from_value(json!({
            "id": id.to_string(),
            "action_type": 24,
            "user_id": by.to_string(),
            "target_id": target.to_string(),
            "changes": changes,
        }))
        .unwrap()
    }

    fn deaf(new: bool) -> Value {
        json!([{ "key": "deaf", "old_value": !new, "new_value": new }])
    }

    #[test]
    fn finds_who_deafened_the_bot() {
        let log = [
            entry(AT + 3, 7, BOT, deaf(false)), // the bot undeafening itself
            entry(AT + 2, 5, 42, deaf(true)),   // someone else deafened
            entry(AT + 1, 6, BOT, deaf(true)),
        ];
        assert_eq!(
            deafened_by(&log, UserId::new(BOT), at()),
            Some(UserId::new(6))
        );
    }

    #[test]
    fn ignores_old_entries_mutes_and_the_bot_itself() {
        let bot = UserId::new(BOT);
        let old = [entry(AT - (1 << 22) * 1000, 6, BOT, deaf(true))];
        assert_eq!(deafened_by(&old, bot, at()), None, "before this deafen");
        let mute = json!([{ "key": "mute", "old_value": false, "new_value": true }]);
        assert_eq!(deafened_by(&[entry(AT, 6, BOT, mute)], bot, at()), None);
        assert_eq!(
            deafened_by(&[entry(AT, BOT, BOT, deaf(true))], bot, at()),
            None
        );
    }

    #[test]
    fn only_a_new_deafen_counts() {
        assert!(newly_deafened(Some(false), true));
        assert!(newly_deafened(None, true));
        assert!(!newly_deafened(Some(true), true), "already deafened");
        assert!(!newly_deafened(Some(true), false));
    }
}
