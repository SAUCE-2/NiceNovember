use std::fmt::Write as _;

use anyhow::Result;
use chrono::Utc;
use poise::CreateReply;
use poise::serenity_prelude::CreateEmbed;

use crate::app::Ctx;
use crate::stats::ModerationStats;

/// Bot uptime, latency and stats.
#[poise::command(slash_command)]
pub async fn status(ctx: Ctx<'_>) -> Result<()> {
    let data = ctx.data();
    let started = data.started_at;
    let uptime = fmt_duration((Utc::now() - started).num_seconds());

    let ping = ctx.ping().await;
    let ping = if ping.is_zero() {
        "measuring…".to_owned()
    } else {
        format!("{} ms", ping.as_millis())
    };

    let mut bot = String::new();
    let _ = writeln!(
        bot,
        "**Uptime:** {uptime} (since <t:{}:f>)",
        started.timestamp()
    );
    let _ = writeln!(bot, "**Gateway latency:** {ping}");
    let _ = writeln!(bot, "**Servers:** {}", ctx.cache().guild_count());
    if let Some(mb) = memory_mb() {
        let _ = writeln!(bot, "**Memory:** {mb:.1} MB");
    }
    let _ = writeln!(
        bot,
        "**Version:** {}",
        fmt_version(env!("CARGO_PKG_VERSION"), option_env!("GIT_SHA"))
    );
    let _ = write!(
        bot,
        "**Voice (Whisper):** {} · **AI (Jev):** {}",
        on_off(data.stt.is_some()),
        on_off(data.jev.is_some()),
    );

    let all = data.db.moderation_stats(None).await?;
    let mut embed = CreateEmbed::new()
        .title("🤖 Bot status")
        .description(bot)
        .colour(0x27ae60);

    if let Some(guild_id) = ctx.guild_id() {
        let here = data.db.moderation_stats(Some(guild_id.get())).await?;
        let listening = data
            .monitor
            .listening_in(guild_id)
            .map_or("not in VC".to_owned(), |c| format!("<#{c}>"));
        let active = data.db.active_timeouts(Some(guild_id.get())).await?;
        embed = embed.field(
            "This server",
            format!(
                "Listening in: {listening}\n{}\nTimed out now: {active}",
                summary(&here)
            ),
            true,
        );
    }
    let active = data.db.active_timeouts(None).await?;
    embed = embed.field(
        "All servers",
        format!("{}\nTimed out now: {active}", summary(&all)),
        true,
    );

    ctx.send(CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

fn summary(s: &ModerationStats) -> String {
    format!(
        "Heard in VC: {}\nJudged: {}\nHostile: {} ({} in 24h)\nTimeouts: {} native, {} forced",
        s.heard, s.judged, s.hostile, s.hostile_last_day, s.native, s.forced
    )
}

fn on_off(b: bool) -> &'static str {
    if b { "✅" } else { "❌" }
}

/// The crate version, plus a link to the commit when the image build passed one in.
fn fmt_version(pkg: &str, sha: Option<&str>) -> String {
    match sha.filter(|s| !s.is_empty()) {
        Some(sha) => format!(
            "{pkg} ([`{}`](https://github.com/SAUCE-2/NiceNovember/commit/{sha}))",
            &sha[..sha.len().min(7)]
        ),
        None => pkg.to_owned(),
    }
}

/// `93784` → `1d 2h 3m 4s`, dropping leading zero units.
fn fmt_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m, s) = (
        secs / 86_400,
        secs % 86_400 / 3_600,
        secs % 3_600 / 60,
        secs % 60,
    );
    let mut out = String::new();
    for (n, unit) in [(d, "d"), (h, "h"), (m, "m")] {
        if n > 0 || !out.is_empty() {
            let _ = write!(out, "{n}{unit} ");
        }
    }
    let _ = write!(out, "{s}s");
    out
}

/// Resident memory from /proc (Linux only; `None` elsewhere).
fn memory_mb() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: f64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()?;
    Some(kb / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(0), "0s");
        assert_eq!(fmt_duration(59), "59s");
        assert_eq!(fmt_duration(3_600), "1h 0m 0s");
        assert_eq!(fmt_duration(93_784), "1d 2h 3m 4s");
        assert_eq!(fmt_duration(-5), "0s");
    }

    #[test]
    fn versions() {
        assert_eq!(fmt_version("0.1.0", None), "0.1.0");
        assert_eq!(fmt_version("0.1.0", Some("")), "0.1.0");
        assert_eq!(
            fmt_version("0.1.0", Some("faa6c9e1234")),
            "0.1.0 ([`faa6c9e`](https://github.com/SAUCE-2/NiceNovember/commit/faa6c9e1234))"
        );
    }
}
