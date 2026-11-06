//! Judging what participants say, and dealing with it when it's hostile: a timeout, a public
//! quote in the announce channel, details in the log channel, and all of it in the activity log.

use std::time::Instant;

use anyhow::Result;
use poise::serenity_prelude::{
    self as serenity, ChannelId, CreateAllowedMentions, CreateMessage, GuildId,
};
use serde_json::json;

use crate::ai::{self, jev::Image};
use crate::app::Data;
use crate::db::GuildConfig;
use crate::hostility::{self, NO, Utterance, YES};
use crate::stats::{self, Log, kind};
use crate::timeout::{self, TIMEOUT_SECS};

/// Judge `u` (with any images it carries) and act on it if it's hostile.
pub async fn judge(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    u: Utterance,
    images: Vec<Image>,
) -> Result<()> {
    let gcfg = data.db.guild(guild_id.get()).await?;
    let threshold = gcfg.threshold();
    let n_images = images.len();
    let state = ai::state_for(&u, &images);
    let started = Instant::now();
    let question = hostility::question(u.source);
    let score = ai::ask_noul(data, state, images, question, Some((YES, NO))).await;
    let jev_ms = started.elapsed().as_millis() as u64;

    let log = |kind, extra: serde_json::Value| {
        let mut d = json!({
            "source": u.source,
            "said": u.said,
            "message_id": u.message.map(|m| m.to_string()),
        });
        if let (Some(d), Some(extra)) = (d.as_object_mut(), extra.as_object()) {
            d.extend(extra.clone());
        }
        Log::new(kind, d)
            .user(u.user)
            .channel(u.channel)
            .participant(Some(true))
    };
    let Some(score) = score else {
        stats::record(data, guild_id, log(kind::JUDGE_FAILED, json!({}))).await;
        return Ok(());
    };
    let hostile = hostility::is_hostile(score, threshold);
    let judged = json!({
        "score": score,
        "hostile": hostile,
        "threshold": threshold,
        "jev_ms": jev_ms,
        "images": n_images,
        "context_lines": u.call.as_ref().map(|c| c.recent.len()),
        "in_call": u.call.as_ref().map(|c| c.in_call.len()),
    });
    stats::record(data, guild_id, log(kind::JUDGED, judged)).await;
    if !hostile {
        return Ok(());
    }

    let reason = format!("hostile, jev {score:.2}: {}", u.said);
    let applied = timeout::time_out(ctx, data, guild_id, u.user, &reason).await;
    let announced = announce(ctx, &gcfg, &u).await;
    let (muted, error) = match &applied {
        Ok(a) => (a.muted, None),
        Err(e) => {
            tracing::warn!(%guild_id, user = %u.user, "timeout failed: {e:#}");
            (false, Some(format!("{e:#}")))
        }
    };
    let details = details(guild_id, &u, score, threshold, &applied);
    post_log(ctx, &gcfg, details).await;
    let entry = json!({
        "score": score,
        "threshold": threshold,
        "muted": muted,
        "timeout_error": error,
        "announced": announced,
    });
    stats::record(data, guild_id, log(kind::HOSTILE, entry)).await;
    Ok(())
}

/// Post the quote to the announce channel (log channel if none). Returns whether it was posted.
async fn announce(ctx: &serenity::Context, gcfg: &GuildConfig, u: &Utterance) -> bool {
    let Some(ch) = gcfg.announce_channel_id.or(gcfg.log_channel_id) else {
        return false;
    };
    let msg = CreateMessage::new()
        .content(hostility::announcement(u, TIMEOUT_SECS))
        .allowed_mentions(CreateAllowedMentions::new());
    match ChannelId::new(ch as u64).send_message(ctx, msg).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(channel = ch, "announcement failed: {e}");
            false
        }
    }
}

fn details(
    guild_id: GuildId,
    u: &Utterance,
    score: f32,
    threshold: f32,
    applied: &Result<timeout::Applied>,
) -> String {
    let link = u.message.map_or(String::new(), |m| {
        format!(
            " · [message](https://discord.com/channels/{guild_id}/{}/{m})",
            u.channel
        )
    });
    let outcome = match applied {
        Ok(a) if a.muted => "timed out, server-muted and deafened".into(),
        Ok(_) => "timed out".into(),
        Err(e) => format!("timeout failed: {e}"),
    };
    format!(
        "🔎 <@{}> · {:?} in <#{}> · jev {score:.2} (threshold {threshold:.2}) · {outcome}{link}",
        u.user, u.source, u.channel
    )
}

/// Post to the log channel, if it's set and isn't the announce channel. Failures are traced.
pub async fn post_log(ctx: &serenity::Context, gcfg: &GuildConfig, content: String) {
    let Some(ch) = gcfg.log_channel_id else {
        return;
    };
    if gcfg.announce_channel_id == Some(ch) {
        return;
    }
    let msg = CreateMessage::new()
        .content(content)
        .allowed_mentions(CreateAllowedMentions::new());
    if let Err(e) = ChannelId::new(ch as u64).send_message(ctx, msg).await {
        tracing::warn!(channel = ch, "log post failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hostility::Source;
    use poise::serenity_prelude::{MessageId, UserId};

    fn u(message: Option<u64>) -> Utterance {
        Utterance {
            user: UserId::new(7),
            channel: ChannelId::new(9),
            source: Source::Message,
            said: "x".into(),
            message: message.map(MessageId::new),
            in_reply_to: None,
            call: None,
        }
    }

    #[test]
    fn details_explain_what_happened() {
        let g = GuildId::new(1);
        let applied = |muted| {
            Ok(timeout::Applied {
                until: chrono::Utc::now(),
                muted,
            })
        };
        assert_eq!(
            details(g, &u(Some(5)), 0.912, 0.7, &applied(true)),
            "🔎 <@7> · Message in <#9> · jev 0.91 (threshold 0.70) · timed out, server-muted \
             and deafened · [message](https://discord.com/channels/1/9/5)"
        );
        assert!(details(g, &u(None), 0.8, 0.7, &applied(false)).ends_with("· timed out"));
        let failed = Err(anyhow::anyhow!("database is locked"));
        assert!(
            details(g, &u(None), 0.8, 0.7, &failed)
                .ends_with("· timeout failed: database is locked")
        );
    }
}
