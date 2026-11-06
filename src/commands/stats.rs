use std::fmt::Write as _;

use anyhow::{Context as _, Result, bail};
use poise::CreateReply;
use poise::serenity_prelude::{
    self as serenity, CreateAllowedMentions, CreateAttachment, CreateEmbed,
};
use serde_json::Value;

use crate::app::Ctx;
use crate::stats::{self, KindCount, LogRow, UserStats};

/// Discord's upload limit for servers without boosts is 10 MiB.
const MAX_UPLOAD: usize = 9_500_000;

fn guild(ctx: Ctx<'_>) -> Result<serenity::GuildId> {
    ctx.guild_id().context("use this in a server")
}

/// Activity log and per-user stats.
#[poise::command(
    slash_command,
    guild_only,
    subcommands("user", "summary", "export", "wipe"),
    subcommand_required
)]
pub async fn stats(_ctx: Ctx<'_>) -> Result<()> {
    Ok(())
}

/// Aggregated stats for a user: messages, VC, judgements, and recent things they said.
#[poise::command(slash_command)]
async fn user(
    ctx: Ctx<'_>,
    #[description = "Whose stats to show"] user: serenity::User,
    #[description = "How many recent judged things to show (default 10)"]
    #[min = 1]
    #[max = 25]
    count: Option<i64>,
) -> Result<()> {
    let guild_id = guild(ctx)?;
    let data = ctx.data();
    let uid = user.id.get();
    let s = data.db.user_stats(guild_id.get(), uid).await?;
    let recent = data
        .db
        .user_recent_judged(guild_id.get(), uid, count.unwrap_or(10))
        .await?;

    let mut text = format_user_stats(&s);
    text.push('\n');
    text.push_str(&format_recent_said(&recent));

    let embed = CreateEmbed::new()
        .title(format!("📊 Stats · {}", user.display_name()))
        .description(text)
        .colour(0x3498db);
    ctx.send(
        CreateReply::default()
            .embed(embed)
            .ephemeral(true)
            .allowed_mentions(CreateAllowedMentions::new()),
    )
    .await?;
    Ok(())
}

/// How much has been logged, by kind.
#[poise::command(slash_command, required_permissions = "ADMINISTRATOR")]
async fn summary(ctx: Ctx<'_>) -> Result<()> {
    let guild_id = guild(ctx)?;
    let counts = ctx.data().db.activity_counts(guild_id.get()).await?;
    let embed = CreateEmbed::new()
        .title("📒 Activity log")
        .description(format_kind_counts(&counts))
        .colour(0xf1c40f);
    ctx.send(CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

/// Download the activity log as gzipped JSON Lines.
#[poise::command(slash_command, required_permissions = "ADMINISTRATOR")]
async fn export(ctx: Ctx<'_>) -> Result<()> {
    let guild_id = guild(ctx)?;
    let data = ctx.data();
    ctx.defer_ephemeral().await.ok();
    let names: std::collections::HashMap<u64, String> = ctx
        .cache()
        .guild(guild_id)
        .map(|g| {
            g.members
                .iter()
                .map(|(id, m)| (id.get(), m.display_name().to_owned()))
                .collect()
        })
        .unwrap_or_default();
    let (gz, n) = stats::export_gz(&data.db, guild_id.get(), |id| names.get(&id).cloned()).await?;
    if gz.len() > MAX_UPLOAD {
        bail!(
            "the export is {:.1} MB, too big for Discord. Copy the database off the server \
             instead (see \"Activity log\" in the README).",
            gz.len() as f64 / 1e6
        );
    }
    let file = CreateAttachment::bytes(gz, "nicenovember.jsonl.gz");
    ctx.send(
        CreateReply::default()
            .content(format!("{n} entries."))
            .attachment(file)
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Delete logged activity (e.g. after testing). Can't be undone.
#[poise::command(slash_command, required_permissions = "ADMINISTRATOR")]
async fn wipe(
    ctx: Ctx<'_>,
    #[description = "Must be true: this deletes the log for good"] confirm: bool,
    #[description = "Only wipe this user (default: everyone)"] user: Option<serenity::User>,
) -> Result<()> {
    let guild_id = guild(ctx)?;
    if !confirm {
        bail!("nothing deleted: set `confirm` to true to wipe the log");
    }
    let n = ctx
        .data()
        .db
        .wipe_activity(guild_id.get(), user.as_ref().map(|u| u.id.get()))
        .await?;
    let who = user.map_or("everyone".to_owned(), |u| u.name);
    ctx.send(
        CreateReply::default()
            .content(format!("🧹 Deleted {n} logged entries for {who}."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

fn format_kind_counts(counts: &[KindCount]) -> String {
    if counts.is_empty() {
        return "Nothing logged yet.".into();
    }
    let mut text = String::new();
    let total: i64 = counts.iter().map(|c| c.n).sum();
    for c in counts {
        let _ = writeln!(text, "`{}` · {} ({} people)", c.kind, c.n, c.people);
    }
    let _ = write!(
        text,
        "\n**{total}** entries. `/stats export` to download them."
    );
    text
}

fn format_user_stats(s: &UserStats) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "**Messages:** {}", s.messages);
    let _ = writeln!(text, "**Reactions:** {}", s.reactions);
    let _ = writeln!(
        text,
        "**Heard in VC:** {} ({})",
        s.heard,
        fmt_ms(s.heard_audio_ms)
    );
    let score = s
        .avg_score
        .map(|v| format!("avg {v:.2}"))
        .unwrap_or_else(|| "—".into());
    let _ = writeln!(text, "**Judged:** {} ({score})", s.judged);
    let _ = writeln!(text, "**Hostile:** {}", s.hostile);
    let _ = writeln!(
        text,
        "**Timeouts:** {} ({})",
        s.timed_out,
        fmt_secs(s.timeout_secs)
    );
    let _ = write!(text, "**Time in VC:** {}", fmt_secs(s.vc_secs));
    text
}

fn format_recent_said(rows: &[LogRow]) -> String {
    let mut text = String::from("**Recently said:**\n");
    if rows.is_empty() {
        text.push_str("Nothing judged yet.");
        return text;
    }
    for r in rows {
        let d: Value = serde_json::from_str(&r.data).unwrap_or_default();
        let said: String = d["said"].as_str().unwrap_or("").chars().take(120).collect();
        let at = crate::db::parse_time(&r.at).map_or(0, |t| t.timestamp());
        let mark = if d["hostile"].as_bool() == Some(true) {
            "🚫"
        } else {
            "·"
        };
        let _ = writeln!(
            text,
            "{mark} <t:{at}:R> ({}, {:.2}): {said}",
            d["source"].as_str().unwrap_or("?"),
            d["score"].as_f64().unwrap_or(0.0),
        );
    }
    text
}

fn fmt_secs(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        return format!("{secs}s");
    }
    let (m, s) = (secs / 60, secs % 60);
    if m < 60 {
        return if s == 0 {
            format!("{m}m")
        } else {
            format!("{m}m {s}s")
        };
    }
    let (h, m) = (m / 60, m % 60);
    if m == 0 {
        format!("{h}h")
    } else {
        format!("{h}h {m}m")
    }
}

fn fmt_ms(ms: i64) -> String {
    fmt_secs(ms.max(0) / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::UserStats;

    #[test]
    fn kind_counts_text() {
        assert_eq!(format_kind_counts(&[]), "Nothing logged yet.");
        let text = format_kind_counts(&[
            KindCount {
                kind: "message".into(),
                n: 3,
                people: 2,
            },
            KindCount {
                kind: "hostile".into(),
                n: 1,
                people: 1,
            },
        ]);
        assert!(text.contains("`message` · 3 (2 people)"));
        assert!(text.contains("**4** entries"));
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_secs(-3), "0s");
        assert_eq!(fmt_secs(0), "0s");
        assert_eq!(fmt_secs(45), "45s");
        assert_eq!(fmt_secs(59), "59s");
        assert_eq!(fmt_secs(60), "1m");
        assert_eq!(fmt_secs(90), "1m 30s");
        assert_eq!(fmt_secs(3599), "59m 59s");
        assert_eq!(fmt_secs(3600), "1h");
        assert_eq!(fmt_secs(3660), "1h 1m");
        assert_eq!(fmt_ms(-1), "0s");
        assert_eq!(fmt_ms(0), "0s");
        assert_eq!(fmt_ms(999), "0s");
        assert_eq!(fmt_ms(2500), "2s");
        assert_eq!(fmt_ms(120_000), "2m");
    }

    #[test]
    fn user_stats_text() {
        let empty = format_user_stats(&UserStats::default());
        assert!(empty.contains("**Messages:** 0"));
        assert!(empty.contains("**Judged:** 0 (—)"));
        assert!(empty.contains("**Time in VC:** 0s"));

        let s = UserStats {
            messages: 3,
            reactions: 1,
            heard: 2,
            heard_audio_ms: 90_000,
            judged: 4,
            avg_score: Some(0.42),
            hostile: 1,
            timed_out: 2,
            timeout_secs: 180,
            vc_secs: 3600,
        };
        let text = format_user_stats(&s);
        assert!(text.contains("**Heard in VC:** 2 (1m 30s)"));
        assert!(text.contains("**Judged:** 4 (avg 0.42)"));
        assert!(text.contains("**Timeouts:** 2 (3m)"));
        assert!(text.contains("**Time in VC:** 1h"));
    }

    #[test]
    fn recent_said_text() {
        assert_eq!(
            format_recent_said(&[]),
            "**Recently said:**\nNothing judged yet."
        );
        let hostile = LogRow {
            id: 1,
            user_id: Some(7),
            channel_id: None,
            kind: "judged".into(),
            participant: Some(true),
            data: r#"{"said":"be mean","score":0.9,"source":"voice","hostile":true}"#.into(),
            day: "2026-11-06".into(),
            at: "2026-11-06T12:00:00.000Z".into(),
        };
        let nice = LogRow {
            id: 2,
            user_id: Some(7),
            channel_id: None,
            kind: "judged".into(),
            participant: Some(true),
            data: r#"{"said":"hello","score":0.1,"source":"message","hostile":false}"#.into(),
            day: "2026-11-06".into(),
            at: "not-a-time".into(),
        };
        let text = format_recent_said(&[hostile, nice]);
        assert!(text.contains("🚫"));
        assert!(text.contains("(voice, 0.90): be mean"));
        assert!(text.contains("· <t:0:R> (message, 0.10): hello"));
    }
}
