use std::fmt::Write as _;

use crate::app::Ctx;
use crate::stats::{self, KindCount, SaidHighlight, SourceStats, UserStats};
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

/// Top Trumps card for a user: meanest/nicest lines and not niceness per source, plus timeouts.
#[poise::command(slash_command, guild_only)]
pub async fn stats(
    ctx: Ctx<'_>,
    #[description = "Whose stats to show"] user: serenity::User,
) -> Result<()> {
    let guild_id = guild(ctx)?;
    let data = ctx.data();
    let uid = user.id.get();
    let s = data.db.user_stats(guild_id.get(), uid).await?;
    let sources = data.db.user_sources(guild_id.get(), uid).await?;

    let embed = CreateEmbed::new()
        .title(format!("🃏 {}", user.display_name()))
        .description(format_user_card(&s, &sources))
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

fn format_user_card(s: &UserStats, sources: &[SourceStats]) -> String {
    let mut text = String::new();
    if sources.is_empty() {
        text.push_str("Nothing judged yet.\n\n");
    }
    for src in sources {
        let avg = src.avg_meanness.map_or_else(|| "—".into(), percent);
        let _ = write!(
            text,
            "**{}**\nMeanest thing: {}\nFriendliest thing: {}\n\
             Not niceness: avg {avg} · {} not nice of {} judged\n\n",
            src.source,
            format_highlight(src.meanest.as_ref()),
            format_highlight(src.friendliest.as_ref()),
            src.hostile,
            src.judged
        );
    }
    let _ = write!(
        text,
        "**Timeouts:** {} ({})",
        s.timed_out,
        fmt_secs(s.timeout_secs)
    );
    text
}

fn format_highlight(h: Option<&SaidHighlight>) -> String {
    let Some(h) = h else {
        return "—".into();
    };
    let said: String = h.said.chars().take(100).collect();
    match h.meanness {
        Some(m) => format!("\"{said}\" ({} not nice)", percent(m)),
        None => format!("\"{said}\""),
    }
}

/// Meanness (0 to 1) as people read it: a percentage of not niceness.
fn percent(meanness: f64) -> String {
    format!("{:.0}%", meanness * 100.0)
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn user_card_text() {
        assert_eq!(
            format_user_card(&UserStats::default(), &[]),
            "Nothing judged yet.\n\n**Timeouts:** 0 (0s)"
        );

        let s = UserStats {
            timed_out: 2,
            timeout_secs: 180,
        };
        let said = |said: &str, meanness| {
            Some(SaidHighlight {
                said: said.into(),
                meanness,
            })
        };
        let sources = [
            SourceStats {
                source: "voice".into(),
                judged: 3,
                avg_meanness: Some(0.42),
                hostile: 1,
                meanest: said("you're the worst", Some(0.75)),
                friendliest: said("have a nice day", None),
            },
            SourceStats {
                source: "edit".into(),
                judged: 0,
                avg_meanness: None,
                hostile: 0,
                meanest: None,
                friendliest: None,
            },
        ];
        assert_eq!(
            format_user_card(&s, &sources),
            "**voice**\n\
             Meanest thing: \"you're the worst\" (75% not nice)\n\
             Friendliest thing: \"have a nice day\"\n\
             Not niceness: avg 42% · 1 not nice of 3 judged\n\n\
             **edit**\n\
             Meanest thing: —\n\
             Friendliest thing: —\n\
             Not niceness: avg — · 0 not nice of 0 judged\n\n\
             **Timeouts:** 2 (3m)"
        );
    }
}
