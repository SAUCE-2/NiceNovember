use std::fmt::Write as _;

use crate::app::Ctx;
use crate::stats::{self, KindCount, SaidHighlight, UserHighlights, UserStats};
use anyhow::{Context as _, Result, bail};
use poise::CreateReply;
use poise::serenity_prelude::{
    self as serenity, CreateAllowedMentions, CreateAttachment, CreateEmbed,
};

/// Discord's upload limit for servers without boosts is 10 MiB.
const MAX_UPLOAD: usize = 9_500_000;

fn guild(ctx: Ctx<'_>) -> Result<serenity::GuildId> {
    ctx.guild_id().context("use this in a server")
}

/// Top Trumps card for a user: meanest/nicest lines plus a few aggregates.
#[poise::command(slash_command, guild_only)]
pub async fn stats(
    ctx: Ctx<'_>,
    #[description = "Whose stats to show"] user: serenity::User,
) -> Result<()> {
    let guild_id = guild(ctx)?;
    let data = ctx.data();
    let uid = user.id.get();
    let s = data.db.user_stats(guild_id.get(), uid).await?;
    let highlights = data.db.user_highlights(guild_id.get(), uid).await?;

    let embed = CreateEmbed::new()
        .title(format!("🃏 {}", user.display_name()))
        .description(format_user_card(&s, &highlights))
        .colour(0x3498db);
    // Channel-visible on purpose: this is the public call-out of how nice they've been.
    ctx.send(
        CreateReply::default()
            .embed(embed)
            .allowed_mentions(CreateAllowedMentions::new()),
    )
    .await?;
    Ok(())
}

/// The activity log (admin).
#[poise::command(
    slash_command,
    guild_only,
    subcommands("summary", "export", "wipe"),
    subcommand_required,
    default_member_permissions = "ADMINISTRATOR",
    required_permissions = "ADMINISTRATOR"
)]
pub async fn log(_ctx: Ctx<'_>) -> Result<()> {
    Ok(())
}

/// How much has been logged, by kind.
#[poise::command(slash_command)]
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
#[poise::command(slash_command)]
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
#[poise::command(slash_command)]
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
        "\n**{total}** entries. `/log export` to download them."
    );
    text
}

fn format_user_card(s: &UserStats, h: &UserHighlights) -> String {
    let mut text = String::new();
    let _ = writeln!(
        text,
        "**Meanest thing:** {}",
        format_highlight(h.meanest.as_ref())
    );
    let _ = writeln!(
        text,
        "**Friendliest thing:** {}",
        format_highlight(h.friendliest.as_ref())
    );
    let avg = s
        .avg_score
        .map(|v| format!("{v:.2}"))
        .unwrap_or_else(|| "—".into());
    let _ = writeln!(
        text,
        "**Hostility:** avg {avg} · {} hostile · {} timeouts ({})",
        s.hostile,
        s.timed_out,
        fmt_secs(s.timeout_secs)
    );
    let _ = write!(
        text,
        "**Presence:** {} messages · {} in VC · {} of speech",
        s.messages,
        fmt_secs(s.vc_secs),
        fmt_ms(s.heard_audio_ms)
    );
    text
}

fn format_highlight(h: Option<&SaidHighlight>) -> String {
    let Some(h) = h else {
        return "—".into();
    };
    let said: String = h.said.chars().take(100).collect();
    format!("\"{said}\" ({:.2} · {})", h.score, h.source)
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
        assert!(text.contains("`/log export`"));
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
    fn user_card_text() {
        let empty = format_user_card(&UserStats::default(), &UserHighlights::default());
        assert!(empty.contains("**Meanest thing:** —"));
        assert!(empty.contains("**Friendliest thing:** —"));
        assert!(empty.contains("**Hostility:** avg — · 0 hostile · 0 timeouts (0s)"));
        assert!(empty.contains("**Presence:** 0 messages · 0s in VC · 0s of speech"));

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
        let h = UserHighlights {
            meanest: Some(SaidHighlight {
                said: "you're the worst".into(),
                score: 0.9,
                source: "voice".into(),
            }),
            friendliest: Some(SaidHighlight {
                said: "have a nice day".into(),
                score: 0.1,
                source: "message".into(),
            }),
        };
        let text = format_user_card(&s, &h);
        assert!(text.contains("**Meanest thing:** \"you're the worst\" (0.90 · voice)"));
        assert!(text.contains("**Friendliest thing:** \"have a nice day\" (0.10 · message)"));
        assert!(text.contains("**Hostility:** avg 0.42 · 1 hostile · 2 timeouts (3m)"));
        assert!(text.contains("**Presence:** 3 messages · 1h in VC · 1m 30s of speech"));
        assert!(!text.contains("Recently said"));
    }
}
