use std::fmt::Write as _;

use anyhow::{Context as _, Result};
use poise::CreateReply;
use poise::serenity_prelude::{
    self as serenity, Channel, CreateAllowedMentions, CreateEmbed, Role,
};
use serde_json::Value;

use crate::app::Ctx;
use crate::db::{Clear, DEFAULT_THRESHOLD, SetupChanges};
use crate::stats::kind;
use crate::voice::monitor;
use crate::{setup, timeout};

fn guild(ctx: Ctx<'_>) -> Result<serenity::GuildId> {
    ctx.guild_id().context("use this in a server")
}

async fn reply(ctx: Ctx<'_>, text: impl Into<String>) -> Result<()> {
    ctx.send(
        CreateReply::default()
            .content(text)
            .ephemeral(true)
            .allowed_mentions(CreateAllowedMentions::new()),
    )
    .await?;
    Ok(())
}

/// Be nice, or be timed out.
#[poise::command(
    slash_command,
    guild_only,
    subcommands("setup", "unset", "pardon", "recent"),
    subcommand_required
)]
pub async fn nice(_ctx: Ctx<'_>) -> Result<()> {
    Ok(())
}

/// Parse a threshold: a number from 0.05 to 1, or a percentage.
pub fn parse_threshold(t: f64) -> Option<f32> {
    let t = if t > 1.0 { t / 100.0 } else { t };
    (0.05..=1.0).contains(&t).then_some(t as f32)
}

/// Show the setup checklist, optionally fixing things in one go.
#[poise::command(slash_command, required_permissions = "ADMINISTRATOR")]
async fn setup(
    ctx: Ctx<'_>,
    #[description = "Members with this role are judged and timed out"] role: Option<Role>,
    #[description = "Where hostile quotes are posted"]
    #[channel_types("Text", "News")]
    announce_channel: Option<Channel>,
    #[description = "Where details and setup notices go (default: the announce channel)"]
    #[channel_types("Text", "News")]
    log_channel: Option<Channel>,
    #[description = "The voice channel I always sit in and listen to"]
    #[channel_types("Voice", "Stage")]
    voice_channel: Option<Channel>,
    #[description = "Jev score that counts as hostile, 0.05 to 1 (default 0.7). Lower = stricter"]
    threshold: Option<f64>,
    #[description = "Judge as usual, but only report would-be timeouts to the log channel"]
    shadow: Option<bool>,
) -> Result<()> {
    let guild_id = guild(ctx)?;
    let data = ctx.data();
    let db = &data.db;
    ctx.defer_ephemeral().await.ok();
    let threshold = threshold
        .map(|t| parse_threshold(t).context("the threshold has to be between 0.05 and 1"))
        .transpose()?;
    let changes = SetupChanges {
        participant_role: role.map(|r| r.id.get()),
        announce_channel: announce_channel.map(|c| c.id().get()),
        log_channel: log_channel.map(|c| c.id().get()),
        voice_channel: voice_channel.map(|c| c.id().get()),
        threshold,
        shadow,
    };
    db.apply_setup(guild_id.get(), &changes).await?;
    monitor::ensure(ctx.serenity_context(), data, guild_id).await?;
    let problems = setup::problems(ctx.serenity_context(), data, guild_id).await?;
    // Keep the "changed since last report" fingerprint in sync with what the admin just saw.
    db.set_setup_hash(guild_id.get(), Some(&problems.join("\n")))
        .await?;
    let gcfg = db.guild(guild_id.get()).await?;
    let shadow = if gcfg.shadow {
        "👻 on: nobody is timed out"
    } else {
        "off"
    };
    let embed = setup::embed(&problems)
        .field(
            "Threshold",
            format!("{:.2} (default {DEFAULT_THRESHOLD:.2})", gcfg.threshold()),
            true,
        )
        .field("Shadow mode", shadow, true);
    ctx.send(CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

#[derive(Debug, poise::ChoiceParameter)]
enum Setting {
    #[name = "log channel"]
    LogChannel,
    #[name = "voice channel (stop listening)"]
    VoiceChannel,
    #[name = "threshold (back to default)"]
    Threshold,
}

/// Clear an optional setting.
#[poise::command(slash_command, required_permissions = "ADMINISTRATOR")]
async fn unset(ctx: Ctx<'_>, setting: Setting) -> Result<()> {
    let guild_id = guild(ctx)?;
    let data = ctx.data();
    let (what, clear) = match setting {
        Setting::LogChannel => (
            "log channel: details go to the announce channel now",
            Clear::LogChannel,
        ),
        Setting::VoiceChannel => ("voice channel: I've stopped listening", Clear::VoiceChannel),
        Setting::Threshold => ("threshold: back to the default", Clear::Threshold),
    };
    data.db.clear(guild_id.get(), clear).await?;
    monitor::ensure(ctx.serenity_context(), data, guild_id).await?;
    reply(ctx, format!("Cleared the {what}.")).await
}

/// End someone's timeout early.
#[poise::command(slash_command, required_permissions = "ADMINISTRATOR")]
async fn pardon(ctx: Ctx<'_>, user: serenity::User) -> Result<()> {
    let guild_id = guild(ctx)?;
    let done = timeout::pardon(
        ctx.serenity_context(),
        ctx.data(),
        guild_id,
        user.id,
        ctx.author().id,
    )
    .await?;
    let text = if done {
        format!("<@{}> is free to go. 🤝", user.id)
    } else {
        format!("<@{}> isn't timed out.", user.id)
    };
    reply(ctx, text).await
}

/// The latest not-nice things people said.
#[poise::command(slash_command)]
async fn recent(
    ctx: Ctx<'_>,
    #[description = "How many (default 10)"]
    #[min = 1]
    #[max = 25]
    count: Option<i64>,
) -> Result<()> {
    let guild_id = guild(ctx)?;
    let rows = ctx
        .data()
        .db
        .recent(guild_id.get(), kind::HOSTILE, count.unwrap_or(10))
        .await?;
    let mut text = String::new();
    if rows.is_empty() {
        text.push_str("Nobody's been hostile yet. 🤝");
    }
    for r in rows {
        let d: Value = serde_json::from_str(&r.data).unwrap_or_default();
        let said: String = d["said"].as_str().unwrap_or("").chars().take(120).collect();
        let at = crate::db::parse_time(&r.at).map_or(0, |t| t.timestamp());
        let _ = writeln!(
            text,
            "• <t:{at}:R> <@{}> ({}, {:.2}): {said}",
            r.user_id.unwrap_or(0),
            d["source"].as_str().unwrap_or("?"),
            d["score"].as_f64().unwrap_or(0.0),
        );
    }
    let embed = CreateEmbed::new()
        .title("🚫 Recently not nice")
        .description(text)
        .colour(0xc0392b);
    ctx.send(
        CreateReply::default()
            .embed(embed)
            .ephemeral(true)
            .allowed_mentions(CreateAllowedMentions::new()),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds() {
        assert_eq!(parse_threshold(0.6), Some(0.6));
        assert_eq!(parse_threshold(1.0), Some(1.0));
        assert_eq!(parse_threshold(80.0), Some(0.8), "a percentage");
        assert_eq!(parse_threshold(0.01), None);
        assert_eq!(parse_threshold(150.0), None);
    }
}
