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
        "shadow": gcfg.shadow,
    });
    stats::record(data, guild_id, log(kind::JUDGED, judged)).await;
    if !hostile {
        return Ok(());
    }
    if gcfg.shadow {
        if let Some(ch) = gcfg.log_channel_id {
            send(ctx, ch, shadow_report(guild_id, &u, score, threshold)).await;
        }
        return Ok(());
    }

    let reason = format!("hostile, jev {score:.2}: {}", u.said);
    let applied = timeout::time_out(ctx, data, guild_id, u.user, &reason).await;
    let announced = announce(ctx, &gcfg, &u).await;
    let (method, muted, error) = match &applied {
        Ok(a) => (Some(a.method.to_string()), a.muted, None),
        Err(e) => {
            tracing::warn!(%guild_id, user = %u.user, "timeout failed: {e:#}");
            (None, false, Some(format!("{e:#}")))
        }
    };
    let outcome = outcome(method.as_deref(), &applied);
    post_log(
        ctx,
        &gcfg,
        details(guild_id, &u, score, threshold, &outcome),
    )
    .await;
    let entry = json!({
        "score": score,
        "threshold": threshold,
        "method": method,
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

fn outcome(method: Option<&str>, applied: &Result<timeout::Applied>) -> String {
    match (method, applied) {
        (Some(m), Ok(a)) => {
            let refused = a
                .native_error
                .as_deref()
                .map_or(String::new(), |e| format!(" (native refused: {e})"));
            let muted = if a.muted { ", server-muted" } else { "" };
            format!("{m} timeout{refused}{muted}")
        }
        (_, Err(e)) => format!("timeout failed: {e}"),
        _ => "no timeout".into(),
    }
}

fn details(guild_id: GuildId, u: &Utterance, score: f32, threshold: f32, outcome: &str) -> String {
    let link = u.message.map_or(String::new(), |m| {
        format!(
            " · [message](https://discord.com/channels/{guild_id}/{}/{m})",
            u.channel
        )
    });
    format!(
        "🔎 <@{}> · {:?} in <#{}> · jev {score:.2} (threshold {threshold:.2}) · {outcome}{link}",
        u.user, u.source, u.channel
    )
}

/// The log line for a hostile verdict in shadow mode, quoting what was said since nothing else
/// does.
fn shadow_report(guild_id: GuildId, u: &Utterance, score: f32, threshold: f32) -> String {
    let outcome = "👻 shadow mode, would have been timed out";
    format!(
        "{}\n{}",
        details(guild_id, u, score, threshold, outcome),
        hostility::quote(&u.said)
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
    send(ctx, ch, content).await;
}

async fn send(ctx: &serenity::Context, ch: i64, content: String) {
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
    use crate::db::Method;
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
        let forced = Ok(timeout::Applied {
            method: Method::Forced,
            until: chrono::Utc::now(),
            native_error: Some("Missing Permissions".into()),
            muted: true,
        });
        assert_eq!(
            details(
                g,
                &u(Some(5)),
                0.912,
                0.7,
                &outcome(Some("forced"), &forced)
            ),
            "🔎 <@7> · Message in <#9> · jev 0.91 (threshold 0.70) · forced timeout \
             (native refused: Missing Permissions), server-muted · \
             [message](https://discord.com/channels/1/9/5)"
        );
        let failed = Err(anyhow::anyhow!("unknown member"));
        assert_eq!(outcome(None, &failed), "timeout failed: unknown member");
    }

    #[test]
    fn shadow_report_says_what_would_have_happened() {
        assert_eq!(
            shadow_report(GuildId::new(1), &u(Some(5)), 0.912, 0.7),
            "🔎 <@7> · Message in <#9> · jev 0.91 (threshold 0.70) · 👻 shadow mode, \
             would have been timed out · [message](https://discord.com/channels/1/9/5)\n> x"
        );
    }
}
