use std::fmt::Write as _;

use anyhow::{Context as _, Result, bail};
use poise::CreateReply;
use poise::serenity_prelude::{self as serenity, CreateAttachment, CreateEmbed};

use crate::app::{Ctx, Data};
use crate::stats::{self, Period};

/// Discord's upload limit for servers without boosts is 10 MiB.
const MAX_UPLOAD: usize = 9_500_000;

fn guild(ctx: Ctx<'_>) -> Result<serenity::GuildId> {
    ctx.guild_id().context("use this in a server")
}

fn period(data: &Data, s: Option<&str>) -> Result<Period> {
    let s = s.unwrap_or_default();
    Period::parse(s, data.today()).with_context(|| {
        format!("couldn't understand the month `{s}` (try `2026-12`, `december`, `last` or `all`)")
    })
}

/// The activity log, for building stats at the end of the month.
#[poise::command(
    slash_command,
    guild_only,
    subcommands("summary", "export", "wipe"),
    subcommand_required,
    required_permissions = "ADMINISTRATOR"
)]
pub async fn stats(_ctx: Ctx<'_>) -> Result<()> {
    Ok(())
}

/// How much has been logged, by kind.
#[poise::command(slash_command, required_permissions = "ADMINISTRATOR")]
async fn summary(
    ctx: Ctx<'_>,
    #[description = "Month: 2026-12, december, last, all (default: this month)"] month: Option<
        String,
    >,
) -> Result<()> {
    let guild_id = guild(ctx)?;
    let data = ctx.data();
    let period = period(data, month.as_deref())?;
    let counts = data.db.activity_counts(guild_id.get(), period).await?;
    let mut text = String::new();
    if counts.is_empty() {
        text.push_str("Nothing logged yet.");
    }
    let total: i64 = counts.iter().map(|c| c.n).sum();
    for c in &counts {
        let _ = writeln!(text, "`{}` · {} ({} people)", c.kind, c.n, c.people);
    }
    if total > 0 {
        let _ = write!(
            text,
            "\n**{total}** entries. `/stats export` to download them."
        );
    }
    let embed = CreateEmbed::new()
        .title(format!("📒 Activity log · {}", period.label()))
        .description(text)
        .colour(0xf1c40f);
    ctx.send(CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

/// Download the activity log as gzipped JSON Lines, to build stats from.
#[poise::command(slash_command, required_permissions = "ADMINISTRATOR")]
async fn export(
    ctx: Ctx<'_>,
    #[description = "Month: 2026-12, december, last, all (default: this month)"] month: Option<
        String,
    >,
) -> Result<()> {
    let guild_id = guild(ctx)?;
    let data = ctx.data();
    let period = period(data, month.as_deref())?;
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
    let (gz, n) = stats::export_gz(&data.db, guild_id.get(), period, |id| {
        names.get(&id).cloned()
    })
    .await?;
    if gz.len() > MAX_UPLOAD {
        bail!(
            "the export is {:.1} MB, too big for Discord. Copy the database off the server \
             instead (see \"Activity log\" in the README).",
            gz.len() as f64 / 1e6
        );
    }
    let file = CreateAttachment::bytes(gz, format!("nicenovember-{}.jsonl.gz", period.slug()));
    ctx.send(
        CreateReply::default()
            .content(format!("{n} entries ({}).", period.label()))
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
    #[description = "Only wipe this user (default: everyone, all months)"] user: Option<
        serenity::User,
    >,
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
