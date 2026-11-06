//! Activity log: a raw record of everything that happens, so stats (leaderboards, who's the
//! meanest, how loud the VC was...) can be worked out from it afterwards, without deciding up
//! front what to measure.
//!
//! - [`capture`] logs Discord events as they arrive (messages, edits, deletes, reactions, voice,
//!   presence, members, typing, commands, threads), with the full Discord payload as JSON.
//! - The bot logs its own events (transcripts, judgements, hostility, timeouts, pardons, the
//!   voice monitor) with [`record`].
//!
//! `/stats export` downloads it all as gzipped JSON Lines; or copy the SQLite database itself.

pub mod capture;
pub mod sessions;

use std::io::Write as _;

use anyhow::Result;
use chrono::{Datelike, Months, NaiveDate};
use poise::serenity_prelude::{ChannelId, GuildId, UserId};
use serde::Serialize;
use serde_json::Value;

use crate::app::Data;
use crate::db::{self, Db};

/// Kinds the bot logs itself. Discord events use the names in [`capture`].
pub mod kind {
    /// Whisper transcribed a participant in the monitored VC:
    /// `{text, raw, audio_ms, transcribe_ms}`.
    pub const HEARD: &str = "heard";
    /// Jev judged an utterance: `{source, said, score, hostile, threshold, jev_ms, images,
    /// message_id}`.
    pub const JUDGED: &str = "judged";
    /// Jev couldn't judge an utterance (not configured, or the call failed): `{source, said}`.
    pub const JUDGE_FAILED: &str = "judge_failed";
    /// An utterance was hostile: `{source, said, score, threshold, message_id, muted,
    /// timeout_error, announced}`.
    pub const HOSTILE: &str = "hostile";
    /// Someone was timed out: `{secs, until, reason, muted}` (`muted`: server-muted and deafened).
    pub const TIMED_OUT: &str = "timed_out";
    /// A timeout ended and was lifted: `{unmuted, until}`.
    pub const TIMEOUT_RELEASED: &str = "timeout_released";
    /// A timed-out user's message was deleted: `{message_id, content, until}`.
    pub const TIMEOUT_MESSAGE: &str = "timeout_message";
    /// A timed-out user's reaction was removed: `{message_id, emoji}`.
    pub const TIMEOUT_REACTION: &str = "timeout_reaction";
    /// A timed-out user was server-muted and deafened on joining VC: `{until}`.
    pub const TIMEOUT_VC: &str = "timeout_vc";
    /// An admin ended someone's timeout early: `{by}`.
    pub const PARDONED: &str = "pardoned";
    /// Time spent in VC, logged on leaving: `{secs}`.
    pub const VC_SESSION: &str = "vc_session";
    /// The bot joined the monitored voice channel (server-wide): `{}`.
    pub const MONITOR_JOINED: &str = "monitor_joined";
    /// The bot couldn't join the monitored voice channel (server-wide): `{error}`.
    pub const MONITOR_FAILED: &str = "monitor_failed";
}

/// One log entry, built up and then passed to [`record`].
#[derive(Clone, Debug)]
pub struct Log {
    pub kind: &'static str,
    pub user: Option<u64>,
    pub channel: Option<u64>,
    pub participant: Option<bool>,
    pub data: Value,
}

impl Log {
    pub fn new(kind: &'static str, data: impl Serialize) -> Self {
        Self {
            kind,
            user: None,
            channel: None,
            participant: None,
            data: serde_json::to_value(data).unwrap_or(Value::Null),
        }
    }
    pub fn user(mut self, u: UserId) -> Self {
        self.user = Some(u.get());
        self
    }
    pub fn channel(mut self, c: ChannelId) -> Self {
        self.channel = Some(c.get());
        self
    }
    pub fn participant(mut self, p: Option<bool>) -> Self {
        self.participant = p;
        self
    }
}

/// Write a log entry. Never fails: logging must not get in the way of moderation (errors are
/// traced).
pub async fn record(data: &Data, guild_id: GuildId, log: Log) {
    if let Err(e) = data
        .db
        .log_activity(guild_id.get(), &log, data.today())
        .await
    {
        tracing::warn!(%guild_id, kind = log.kind, "activity log failed: {e:#}");
    }
}

// ---- periods ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    All,
    Month(NaiveDate), // first of the month
}

impl Period {
    /// Parse "2026-12", "december", "dec", "this", "last" or "all". Empty = this month.
    /// A bare month name means the most recent one (so "december" in January is last year's).
    pub fn parse(s: &str, today: NaiveDate) -> Option<Self> {
        let s = s.trim().to_lowercase();
        let this = today.with_day(1).unwrap();
        match s.as_str() {
            "" | "this" | "this month" | "now" => return Some(Period::Month(this)),
            "last" | "last month" => return Some(Period::Month(this - Months::new(1))),
            "all" | "all time" | "ever" => return Some(Period::All),
            _ => {}
        }
        if let Ok(d) = NaiveDate::parse_from_str(&format!("{s}-01"), "%Y-%m-%d") {
            return Some(Period::Month(d));
        }
        const MONTHS: [&str; 12] = [
            "january",
            "february",
            "march",
            "april",
            "may",
            "june",
            "july",
            "august",
            "september",
            "october",
            "november",
            "december",
        ];
        if s.len() < 3 {
            return None;
        }
        let m = MONTHS.iter().position(|name| name.starts_with(&s))? as u32 + 1;
        let year = if m > today.month() {
            today.year() - 1
        } else {
            today.year()
        };
        NaiveDate::from_ymd_opt(year, m, 1).map(Period::Month)
    }

    /// `all` for all time, otherwise the month written with the chrono format `month`.
    fn render(&self, all: &str, month: &str) -> String {
        match self {
            Period::All => all.into(),
            Period::Month(d) => d.format(month).to_string(),
        }
    }

    /// `LIKE` pattern matching this period's `day` values.
    fn pattern(&self) -> String {
        self.render("%", "%Y-%m-%%")
    }

    pub fn label(&self) -> String {
        self.render("all time", "%B %Y")
    }

    /// For file names.
    pub fn slug(&self) -> String {
        self.render("all", "%Y-%m")
    }
}

// ---- storage ----

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct LogRow {
    pub id: i64,
    pub user_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub kind: String,
    pub participant: Option<bool>,
    pub data: String,
    pub day: String,
    pub at: String,
}

/// Moderation totals, from the log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct ModerationStats {
    pub heard: i64,
    pub judged: i64,
    pub hostile: i64,
    pub hostile_last_day: i64,
    pub timed_out: i64,
}

/// How much of each kind was logged.
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct KindCount {
    pub kind: String,
    pub n: i64,
    pub people: i64,
}

impl Db {
    pub async fn log_activity(&self, guild_id: u64, log: &Log, day: NaiveDate) -> Result<()> {
        sqlx::query(
            "INSERT INTO activity_log (guild_id, user_id, channel_id, kind, participant, data, day, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(guild_id as i64)
        .bind(log.user.map(|u| u as i64))
        .bind(log.channel.map(|c| c as i64))
        .bind(log.kind)
        .bind(log.participant)
        .bind(log.data.to_string())
        .bind(day.to_string())
        .bind(db::now())
        .execute(&self.0)
        .await?;
        Ok(())
    }

    /// Entries in the period, oldest first, `after` a given id (for paging through big exports).
    pub async fn activity(
        &self,
        guild_id: u64,
        period: Period,
        after: i64,
        limit: i64,
    ) -> Result<Vec<LogRow>> {
        Ok(sqlx::query_as(
            "SELECT id, user_id, channel_id, kind, participant, data, day, at FROM activity_log
             WHERE guild_id = ? AND day LIKE ? AND id > ? ORDER BY id LIMIT ?",
        )
        .bind(guild_id as i64)
        .bind(period.pattern())
        .bind(after)
        .bind(limit)
        .fetch_all(&self.0)
        .await?)
    }

    pub async fn activity_counts(&self, guild_id: u64, period: Period) -> Result<Vec<KindCount>> {
        Ok(sqlx::query_as(
            "SELECT kind, COUNT(*) AS n, COUNT(DISTINCT user_id) AS people FROM activity_log
             WHERE guild_id = ? AND day LIKE ? GROUP BY kind ORDER BY n DESC, kind",
        )
        .bind(guild_id as i64)
        .bind(period.pattern())
        .fetch_all(&self.0)
        .await?)
    }

    /// Moderation counts for `/status`, in one guild or across all of them.
    pub async fn moderation_stats(&self, guild_id: Option<u64>) -> Result<ModerationStats> {
        let day_ago = db::fmt_time(chrono::Utc::now() - chrono::Duration::days(1));
        Ok(sqlx::query_as(
            "SELECT
                COALESCE(SUM(kind = 'heard'), 0) AS heard,
                COALESCE(SUM(kind = 'judged'), 0) AS judged,
                COALESCE(SUM(kind = 'hostile'), 0) AS hostile,
                COALESCE(SUM(kind = 'hostile' AND at > ?2), 0) AS hostile_last_day,
                COALESCE(SUM(kind = 'timed_out'), 0) AS timed_out
             FROM activity_log WHERE ?1 IS NULL OR guild_id = ?1",
        )
        .bind(guild_id.map(|g| g as i64))
        .bind(day_ago)
        .fetch_one(&self.0)
        .await?)
    }

    /// The latest entries of one kind, newest first.
    pub async fn recent(&self, guild_id: u64, kind: &str, limit: i64) -> Result<Vec<LogRow>> {
        Ok(sqlx::query_as(
            "SELECT id, user_id, channel_id, kind, participant, data, day, at FROM activity_log
             WHERE guild_id = ? AND kind = ? ORDER BY id DESC LIMIT ?",
        )
        .bind(guild_id as i64)
        .bind(kind)
        .bind(limit)
        .fetch_all(&self.0)
        .await?)
    }

    /// Who wrote a message we logged (to attribute deletes).
    pub async fn logged_message_author(
        &self,
        guild_id: u64,
        message_id: u64,
    ) -> Result<Option<i64>> {
        let row: Option<(Option<i64>,)> = sqlx::query_as(
            "SELECT user_id FROM activity_log
             WHERE guild_id = ? AND kind = 'message' AND json_extract(data, '$.id') = ?
             ORDER BY id DESC LIMIT 1",
        )
        .bind(guild_id as i64)
        .bind(message_id.to_string())
        .fetch_optional(&self.0)
        .await?;
        Ok(row.and_then(|(u,)| u))
    }

    /// Delete the log for the guild (or one user). Returns how many entries were deleted.
    pub async fn wipe_activity(&self, guild_id: u64, user: Option<u64>) -> Result<u64> {
        Ok(sqlx::query(
            "DELETE FROM activity_log WHERE guild_id = ?1 AND (?2 IS NULL OR user_id = ?2)",
        )
        .bind(guild_id as i64)
        .bind(user.map(|u| u as i64))
        .execute(&self.0)
        .await?
        .rows_affected())
    }
}

/// One JSON Lines record, with `data` parsed back into JSON and the user's name if known.
pub fn jsonl_line(r: &LogRow, name: Option<&str>) -> String {
    let data: Value = serde_json::from_str(&r.data).unwrap_or(Value::String(r.data.clone()));
    let v = serde_json::json!({
        "id": r.id,
        "at": r.at,
        "day": r.day,
        "kind": r.kind,
        // As strings: snowflakes don't fit in a JS number.
        "user_id": r.user_id.map(|u| u.to_string()),
        "user": name,
        "channel_id": r.channel_id.map(|c| c.to_string()),
        "participant": r.participant,
        "data": data,
    });
    v.to_string()
}

/// Gzipped JSON Lines of the whole period, read page by page from the database.
pub async fn export_gz(
    db: &Db,
    guild_id: u64,
    period: Period,
    name: impl Fn(u64) -> Option<String>,
) -> Result<(Vec<u8>, usize)> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let (mut after, mut total) = (0, 0);
    loop {
        let rows = db.activity(guild_id, period, after, 5000).await?;
        let Some(last) = rows.last() else { break };
        after = last.id;
        total += rows.len();
        for r in &rows {
            let n = r.user_id.and_then(|u| name(u as u64));
            writeln!(gz, "{}", jsonl_line(r, n.as_deref()))?;
        }
    }
    Ok((gz.finish()?, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn periods() {
        let today = d("2027-01-10");
        let m = |s| Some(Period::Month(d(s)));
        assert_eq!(Period::parse("", today), m("2027-01-01"));
        assert_eq!(Period::parse("last", today), m("2026-12-01"));
        assert_eq!(Period::parse("dec", today), m("2026-12-01"));
        assert_eq!(Period::parse("January", today), m("2027-01-01"));
        assert_eq!(Period::parse("2025-11", today), m("2025-11-01"));
        assert_eq!(Period::parse("all", today), Some(Period::All));
        assert_eq!(Period::parse("ju", today), None);
        assert_eq!(Period::parse("nope", today), None);
        let dec = Period::Month(d("2026-12-01"));
        assert_eq!(dec.pattern(), "2026-12-%");
        assert_eq!(dec.label(), "December 2026");
        assert_eq!(dec.slug(), "2026-12");
        assert_eq!(
            (
                Period::All.pattern(),
                Period::All.label(),
                Period::All.slug()
            ),
            ("%".into(), "all time".into(), "all".into())
        );
    }

    #[tokio::test]
    async fn log_export_and_wipe() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let log = |kind, u: u64| {
            Log::new(kind, json!({"content": "be nice", "n": 1}))
                .user(UserId::new(u))
                .channel(ChannelId::new(9))
                .participant(Some(true))
        };
        for (g, kind, u, day) in [
            (1, "message", 7, "2026-12-01"),
            (1, "message", 8, "2026-12-02"),
            (1, "message", 7, "2026-11-30"),
            (2, "message", 7, "2026-12-01"),
        ] {
            db.log_activity(g, &log(kind, u), d(day)).await.unwrap();
        }
        let hostile = log(kind::HOSTILE, 7);
        db.log_activity(1, &hostile, d("2026-12-02")).await.unwrap();
        let server_wide = Log::new(kind::MONITOR_JOINED, json!({}));
        db.log_activity(1, &server_wide, d("2026-12-03"))
            .await
            .unwrap();

        let dec = Period::Month(d("2026-12-01"));
        let counts = db.activity_counts(1, dec).await.unwrap();
        let get = |k: &str| counts.iter().find(|c| c.kind == k).unwrap().clone();
        assert_eq!((get("message").n, get("message").people), (2, 2));
        assert_eq!(get(kind::HOSTILE).n, 1);
        assert_eq!(get(kind::MONITOR_JOINED).people, 0);

        let rows = db.activity(1, dec, 0, 2).await.unwrap();
        assert_eq!(rows.len(), 2, "paged");
        let line: Value = serde_json::from_str(&jsonl_line(&rows[0], Some("bob"))).unwrap();
        assert_eq!(line["user_id"], "7");
        assert_eq!(line["user"], "bob");
        assert_eq!(line["data"]["content"], "be nice");
        assert_eq!(line["participant"], true);

        let (gz, n) = export_gz(&db, 1, dec, |_| None).await.unwrap();
        assert_eq!(n, 4);
        let mut text = String::new();
        std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&gz[..]), &mut text)
            .unwrap();
        assert_eq!(text.lines().count(), 4);
        assert!(text.contains("\"kind\":\"hostile\""));

        let msg = Log::new("message", json!({"id": "555", "content": "hi"})).user(UserId::new(8));
        db.log_activity(1, &msg, d("2026-12-04")).await.unwrap();
        assert_eq!(db.logged_message_author(1, 555).await.unwrap(), Some(8));
        assert_eq!(db.logged_message_author(1, 556).await.unwrap(), None);

        assert_eq!(db.wipe_activity(1, Some(7)).await.unwrap(), 3);
        assert_eq!(db.activity(1, Period::All, 0, 100).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn moderation_stats_and_recent() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        assert_eq!(
            db.moderation_stats(None).await.unwrap(),
            ModerationStats::default()
        );
        let today = d("2026-11-06");
        let entries = [
            (1, Log::new(kind::HEARD, json!({"text": "hi"}))),
            (1, Log::new(kind::JUDGED, json!({"score": 0.1}))),
            (1, Log::new(kind::JUDGED, json!({"score": 0.9}))),
            (1, Log::new(kind::HOSTILE, json!({"said": "old"}))),
            (1, Log::new(kind::HOSTILE, json!({"said": "new"}))),
            (1, Log::new(kind::TIMED_OUT, json!({"secs": 10}))),
            (1, Log::new(kind::TIMED_OUT, json!({"secs": 10}))),
            (2, Log::new(kind::TIMED_OUT, json!({"secs": 10}))),
        ];
        for (g, log) in &entries {
            db.log_activity(*g, log, today).await.unwrap();
        }
        sqlx::query(
            "UPDATE activity_log SET at = '2000-01-01T00:00:00.000Z' WHERE data LIKE '%old%'",
        )
        .execute(&db.0)
        .await
        .unwrap();
        let g1 = db.moderation_stats(Some(1)).await.unwrap();
        assert_eq!(
            g1,
            ModerationStats {
                heard: 1,
                judged: 2,
                hostile: 2,
                hostile_last_day: 1,
                timed_out: 2,
            }
        );
        assert_eq!(db.moderation_stats(None).await.unwrap().timed_out, 3);

        let recent = db.recent(1, kind::HOSTILE, 1).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert!(recent[0].data.contains("new"), "newest first");
        assert!(db.recent(2, kind::HOSTILE, 5).await.unwrap().is_empty());
    }
}
