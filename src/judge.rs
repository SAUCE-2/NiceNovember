//! Judging what participants say, and dealing with it when it's hostile: a timeout, a public
//! quote with Jev's score in the announce channel, and all of it in the activity log.

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
use crate::timeout;

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
        "shadow": gcfg.shadow,
    });
    stats::record(data, guild_id, log(kind::JUDGED, judged)).await;
    if !hostile {
        return Ok(());
    }
    if gcfg.shadow {
        announce(ctx, &gcfg, &u, score).await;
        return Ok(());
    }

    let reason = format!("hostile, jev {score:.2}: {}", u.said);
    let applied = timeout::time_out(ctx, data, guild_id, u.user, &reason).await;
    let announced = announce(ctx, &gcfg, &u, score).await;
    let (muted, error) = match &applied {
        Ok(a) => (a.muted, None),
        Err(e) => {
            tracing::warn!(%guild_id, user = %u.user, "timeout failed: {e:#}");
            (false, Some(format!("{e:#}")))
        }
    };
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

/// Post the quote to the announce channel. Returns whether it was posted.
async fn announce(ctx: &serenity::Context, gcfg: &GuildConfig, u: &Utterance, score: f32) -> bool {
    let Some(ch) = gcfg.announce_channel_id else {
        return false;
    };
    let msg = CreateMessage::new()
        .content(hostility::announcement(u, score, gcfg.shadow))
        .allowed_mentions(CreateAllowedMentions::new());
    match ChannelId::new(ch as u64).send_message(ctx, msg).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(channel = ch, "announcement failed: {e}");
            false
        }
    }
}
