//! Activity log: a raw record of everything that happens, so stats (leaderboards, who's the
//! meanest, how loud the VC was...) can be worked out from it afterwards, without deciding up
//! front what to measure.
//!
//! - [`capture`] logs Discord events as they arrive (messages, edits, deletes, reactions, voice,
//!   presence, members, typing, commands, threads), with the full Discord payload as JSON.
//! - The bot logs its own events (transcripts, judgements, hostility, timeouts, pardons, the
//!   voice monitor) with [`record`].
//!
//! `/log export` downloads it all as gzipped JSON Lines; or copy the SQLite database itself.

pub mod capture;
pub mod sessions;

use std::io::Write as _;

use anyhow::Result;
use chrono::NaiveDate;
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
    /// Jev judged an utterance: `{source, said, score, aimed, put_down, meanness, hostile,
    /// threshold, jev_ms, images, message_id}` (`score` is the lower of `aimed` and `put_down`;
    /// `meanness` is 0 to 1, added afterwards to entries from before it was asked).
    pub const JUDGED: &str = "judged";
    /// Jev couldn't judge an utterance (not configured, or the call failed): `{source, said}`.
    pub const JUDGE_FAILED: &str = "judge_failed";
    /// An utterance was hostile: `{source, said, score, aimed, put_down, meanness, threshold,
    /// message_id, muted, timeout_error, announced}`.
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
    /// The bot left voice: nobody else was in the monitored channel, or it was unset
    /// (server-wide): `{}`.
    pub const MONITOR_LEFT: &str = "monitor_left";
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

/// A judged log entry with no meanness yet.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct UnratedRow {
    pub id: i64,
    pub guild_id: i64,
    pub user_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub data: String,
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

/// Timeout totals for one user in a guild, for `/stats`.
#[derive(Clone, Debug, Default, PartialEq, sqlx::FromRow)]
pub struct UserStats {
    pub timed_out: i64,
    pub timeout_secs: i64,
}

/// How one user's utterances from one source were judged, for `/stats`.
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct SourceStats {
    pub source: String,
    pub judged: i64,
    /// Average meanness, 0 to 1, of the judged utterances rated so far.
    pub avg_meanness: Option<f64>,
    pub hostile: i64,
    /// Meanest judged utterance from this source.
    #[sqlx(skip)]
    pub meanest: Option<SaidHighlight>,
    /// Least mean judged utterance from this source.
    #[sqlx(skip)]
    pub friendliest: Option<SaidHighlight>,
}

/// One judged utterance, for Top Trumps highlights on the `/stats` card.
#[derive(Clone, Debug, PartialEq)]
pub struct SaidHighlight {
    pub said: String,
    /// How mean it was, 0 to 1; `None` until it's been rated.
    pub meanness: Option<f64>,
}

impl SaidHighlight {
    /// Pull `said` / `meanness` out of a judged log row's JSON.
    pub fn from_judged_data(data: &str) -> Option<Self> {
        let d: Value = serde_json::from_str(data).ok()?;
        let said = d.get("said")?.as_str()?.to_owned();
        let meanness = d.get("meanness").and_then(Value::as_f64);
        Some(Self { said, meanness })
    }
}

/// A judged or hostile row's utterance source, `?` if it wasn't logged. A macro so queries can
/// `concat!` it: sqlx only takes static SQL.
macro_rules! source_sql {
    () => {
        "COALESCE(json_extract(data, '$.source'), '?')"
    };
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

    /// Entries, oldest first, `after` a given id (for paging through big exports).
    pub async fn activity(&self, guild_id: u64, after: i64, limit: i64) -> Result<Vec<LogRow>> {
        Ok(sqlx::query_as(
            "SELECT id, user_id, channel_id, kind, participant, data, day, at FROM activity_log
             WHERE guild_id = ? AND id > ? ORDER BY id LIMIT ?",
        )
        .bind(guild_id as i64)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.0)
        .await?)
    }

    pub async fn activity_counts(&self, guild_id: u64) -> Result<Vec<KindCount>> {
        Ok(sqlx::query_as(
            "SELECT kind, COUNT(*) AS n, COUNT(DISTINCT user_id) AS people FROM activity_log
             WHERE guild_id = ? GROUP BY kind ORDER BY n DESC, kind",
        )
        .bind(guild_id as i64)
        .fetch_all(&self.0)
        .await?)
    }

    /// Timeout totals for one user in this guild.
    pub async fn user_stats(&self, guild_id: u64, user_id: u64) -> Result<UserStats> {
        Ok(sqlx::query_as(
            "SELECT
                COALESCE(SUM(kind = 'timed_out'), 0) AS timed_out,
                COALESCE(SUM(CASE WHEN kind = 'timed_out'
                    THEN CAST(json_extract(data, '$.secs') AS INTEGER) ELSE 0 END), 0)
                    AS timeout_secs
             FROM activity_log WHERE guild_id = ? AND user_id = ?",
        )
        .bind(guild_id as i64)
        .bind(user_id as i64)
        .fetch_one(&self.0)
        .await?)
    }

    /// How one user's utterances were judged, per source (most judged first), with the meanest
    /// and friendliest thing they said from each.
    pub async fn user_sources(&self, guild_id: u64, user_id: u64) -> Result<Vec<SourceStats>> {
        let mut sources: Vec<SourceStats> = sqlx::query_as(concat!(
            "SELECT ",
            source_sql!(),
            " AS source,
                SUM(kind = 'judged') AS judged,
                AVG(CASE WHEN kind = 'judged' THEN json_extract(data, '$.meanness') END)
                    AS avg_meanness,
                SUM(kind = 'hostile') AS hostile
             FROM activity_log
             WHERE guild_id = ? AND user_id = ? AND kind IN ('judged', 'hostile')
             GROUP BY 1 ORDER BY judged DESC, source"
        ))
        .bind(guild_id as i64)
        .bind(user_id as i64)
        .fetch_all(&self.0)
        .await?;
        for s in &mut sources {
            s.meanest = self
                .judged_by_meanness(guild_id, user_id, &s.source, true)
                .await?;
            s.friendliest = self
                .judged_by_meanness(guild_id, user_id, &s.source, false)
                .await?;
        }
        Ok(sources)
    }

    /// The meanest (or least mean) judged utterance, rated ones first, ties going to Jev's score
    /// and then the latest.
    async fn judged_by_meanness(
        &self,
        guild_id: u64,
        user_id: u64,
        source: &str,
        meanest: bool,
    ) -> Result<Option<SaidHighlight>> {
        let row: Option<(String,)> = sqlx::query_as(concat!(
            "SELECT data FROM activity_log
             WHERE guild_id = ? AND user_id = ? AND kind = 'judged' AND ",
            source_sql!(),
            " = ? ORDER BY json_extract(data, '$.meanness') IS NULL,
                json_extract(data, '$.meanness') * ?4, json_extract(data, '$.score') * ?4, id DESC
             LIMIT 1"
        ))
        .bind(guild_id as i64)
        .bind(user_id as i64)
        .bind(source)
        .bind(if meanest { -1 } else { 1 })
        .fetch_optional(&self.0)
        .await?;
        Ok(row.and_then(|(data,)| SaidHighlight::from_judged_data(&data)))
    }

    /// Judged utterances from before meanness was asked, oldest first, to rate now.
    pub async fn unrated(&self) -> Result<Vec<UnratedRow>> {
        Ok(sqlx::query_as(
            "SELECT id, guild_id, user_id, channel_id, data, at FROM activity_log
             WHERE kind = 'judged' AND json_extract(data, '$.meanness') IS NULL ORDER BY id",
        )
        .fetch_all(&self.0)
        .await?)
    }

    /// Add a judged utterance's meanness to its log entry.
    pub async fn set_meanness(&self, id: i64, meanness: f32) -> Result<()> {
        sqlx::query("UPDATE activity_log SET data = json_set(data, '$.meanness', ?) WHERE id = ?")
            .bind(meanness)
            .bind(id)
            .execute(&self.0)
            .await?;
        Ok(())
    }

    /// What was heard in a voice channel before log entry `before`, from `since` on, newest first,
    /// as (who, what, when). Empty transcripts are left out.
    pub async fn heard_before(
        &self,
        guild_id: u64,
        channel_id: u64,
        before: i64,
        since: &str,
    ) -> Result<Vec<(Option<i64>, String, String)>> {
        Ok(sqlx::query_as(
            "SELECT user_id, json_extract(data, '$.text'), at FROM activity_log
             WHERE guild_id = ? AND channel_id = ? AND kind = 'heard' AND id < ? AND at >= ?
                AND json_extract(data, '$.text') != ''
             ORDER BY id DESC LIMIT 50",
        )
        .bind(guild_id as i64)
        .bind(channel_id as i64)
        .bind(before)
        .bind(since)
        .fetch_all(&self.0)
        .await?)
    }

    /// Moderation counts for `/status`, in one guild. Stats never cross guilds.
    pub async fn moderation_stats(&self, guild_id: u64) -> Result<ModerationStats> {
        let day_ago = db::fmt_time(chrono::Utc::now() - chrono::Duration::days(1));
        Ok(sqlx::query_as(
            "SELECT
                COALESCE(SUM(kind = 'heard'), 0) AS heard,
                COALESCE(SUM(kind = 'judged'), 0) AS judged,
                COALESCE(SUM(kind = 'hostile'), 0) AS hostile,
                COALESCE(SUM(kind = 'hostile' AND at > ?2), 0) AS hostile_last_day,
                COALESCE(SUM(kind = 'timed_out'), 0) AS timed_out
             FROM activity_log WHERE guild_id = ?1",
        )
        .bind(guild_id as i64)
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

/// Gzipped JSON Lines of the whole log, read page by page from the database.
pub async fn export_gz(
    db: &Db,
    guild_id: u64,
    name: impl Fn(u64) -> Option<String>,
) -> Result<(Vec<u8>, usize)> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let (mut after, mut total) = (0, 0);
    loop {
        let rows = db.activity(guild_id, after, 5000).await?;
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

        let counts = db.activity_counts(1).await.unwrap();
        let get = |k: &str| counts.iter().find(|c| c.kind == k).unwrap().clone();
        assert_eq!((get("message").n, get("message").people), (3, 2));
        assert_eq!(get(kind::HOSTILE).n, 1);
        assert_eq!(get(kind::MONITOR_JOINED).people, 0);

        let rows = db.activity(1, 0, 2).await.unwrap();
        assert_eq!(rows.len(), 2, "paged");
        let line: Value = serde_json::from_str(&jsonl_line(&rows[0], Some("bob"))).unwrap();
        assert_eq!(line["user_id"], "7");
        assert_eq!(line["user"], "bob");
        assert_eq!(line["data"]["content"], "be nice");
        assert_eq!(line["participant"], true);

        let (gz, n) = export_gz(&db, 1, |_| None).await.unwrap();
        assert_eq!(n, 5);
        let mut text = String::new();
        std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&gz[..]), &mut text)
            .unwrap();
        assert_eq!(text.lines().count(), 5);
        assert!(text.contains("\"kind\":\"hostile\""));

        let msg = Log::new("message", json!({"id": "555", "content": "hi"})).user(UserId::new(8));
        db.log_activity(1, &msg, d("2026-12-04")).await.unwrap();
        assert_eq!(db.logged_message_author(1, 555).await.unwrap(), Some(8));
        assert_eq!(db.logged_message_author(1, 556).await.unwrap(), None);

        assert_eq!(db.wipe_activity(1, Some(7)).await.unwrap(), 3);
        assert_eq!(db.activity(1, 0, 100).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn moderation_stats_and_recent() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        assert_eq!(
            db.moderation_stats(1).await.unwrap(),
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
        let g1 = db.moderation_stats(1).await.unwrap();
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
        let g2 = db.moderation_stats(2).await.unwrap();
        assert_eq!((g2.timed_out, g2.hostile), (1, 0));
        assert_eq!(
            db.moderation_stats(3).await.unwrap(),
            ModerationStats::default()
        );

        let recent = db.recent(1, kind::HOSTILE, 1).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert!(recent[0].data.contains("new"), "newest first");
        assert!(db.recent(2, kind::HOSTILE, 5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn user_stats_and_highlights() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let day = d("2026-11-06");
        assert_eq!(db.user_stats(1, 7).await.unwrap(), UserStats::default());
        let judged = |said: &str, score: f64, meanness: Option<f64>, source: &str| {
            let mut d = json!({"said": said, "score": score, "source": source});
            if let Some(m) = meanness {
                d["meanness"] = json!(m);
            }
            Log::new(kind::JUDGED, d).user(UserId::new(7))
        };
        let entries = [
            Log::new("message", json!({})).user(UserId::new(7)),
            Log::new("message", json!({})).user(UserId::new(7)),
            Log::new("reaction_add", json!({})).user(UserId::new(7)),
            Log::new(kind::HEARD, json!({"text": "yo", "audio_ms": 1500})).user(UserId::new(7)),
            Log::new(kind::HEARD, json!({"text": "hey", "audio_ms": 500})).user(UserId::new(7)),
            judged("have a nice day", 0.1, Some(0.0), "message"),
            judged("you're the worst", 0.9, Some(0.75), "voice"),
            judged("meh", 0.5, Some(0.25), "message"),
            judged("ok genius", 0.3, Some(0.5), "message"),
            judged("not rated yet", 0.95, None, "message"),
            Log::new(
                kind::HOSTILE,
                json!({"said": "you're the worst", "score": 0.9, "source": "voice"}),
            )
            .user(UserId::new(7)),
            Log::new(kind::TIMED_OUT, json!({"secs": 60})).user(UserId::new(7)),
            Log::new(kind::TIMED_OUT, json!({"secs": 120})).user(UserId::new(7)),
            Log::new(kind::VC_SESSION, json!({"secs": 300})).user(UserId::new(7)),
            Log::new(kind::JUDGED, json!({"said": "other", "score": 1.0})).user(UserId::new(8)),
        ];
        for log in &entries {
            db.log_activity(1, log, day).await.unwrap();
        }

        let s = db.user_stats(1, 7).await.unwrap();
        assert_eq!(s.timed_out, 2);
        assert_eq!(s.timeout_secs, 180);

        let said = |h: &Option<SaidHighlight>| h.as_ref().unwrap().said.clone();
        let by_source = db.user_sources(1, 7).await.unwrap();
        let sources: Vec<_> = by_source.iter().map(|s| s.source.as_str()).collect();
        assert_eq!(sources, ["message", "voice"], "most judged first");
        let (message, voice) = (&by_source[0], &by_source[1]);
        assert_eq!((message.judged, message.hostile), (4, 0));
        assert!((message.avg_meanness.unwrap() - 0.25).abs() < 1e-9);
        assert_eq!(
            said(&message.meanest),
            "ok genius",
            "by meanness, not score"
        );
        assert_eq!(said(&message.friendliest), "have a nice day");
        assert_eq!(message.friendliest.as_ref().unwrap().meanness, Some(0.0));
        assert_eq!((voice.judged, voice.hostile), (1, 1));
        assert!((voice.avg_meanness.unwrap() - 0.75).abs() < 1e-9);
        assert_eq!(said(&voice.meanest), "you're the worst");
        assert_eq!(said(&voice.friendliest), "you're the worst");
        let unknown = &db.user_sources(1, 8).await.unwrap()[0];
        assert_eq!(
            (unknown.source.as_str(), said(&unknown.meanest)),
            ("?", "other".into())
        );
        assert_eq!(unknown.avg_meanness, None, "nothing rated yet");
        assert!(db.user_sources(1, 9).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unrated_judgements_get_rated() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let day = d("2026-11-06");
        let entries = [
            (
                1,
                Log::new(kind::JUDGED, json!({"said": "old", "score": 0.9})),
            ),
            (2, Log::new(kind::JUDGED, json!({"said": "elsewhere"}))),
            (
                1,
                Log::new(kind::JUDGED, json!({"said": "new", "meanness": 0.5})),
            ),
            (1, Log::new(kind::HOSTILE, json!({"said": "old"}))),
        ];
        for (g, log) in &entries {
            db.log_activity(*g, log, day).await.unwrap();
        }
        let unrated = db.unrated().await.unwrap();
        let ids: Vec<_> = unrated.iter().map(|r| (r.id, r.guild_id)).collect();
        assert_eq!(ids, [(1, 1), (2, 2)], "judged and unrated, every guild");

        db.set_meanness(1, 0.75).await.unwrap();
        let rows = db.activity(1, 0, 10).await.unwrap();
        let data: Value = serde_json::from_str(&rows[0].data).unwrap();
        assert_eq!(data, json!({"said": "old", "score": 0.9, "meanness": 0.75}));
        assert_eq!(db.unrated().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn heard_before_is_the_channel_newest_first() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let day = d("2026-11-06");
        let heard = |text: &str, user, channel| {
            Log::new(kind::HEARD, json!({"text": text}))
                .user(UserId::new(user))
                .channel(ChannelId::new(channel))
        };
        for log in [
            heard("too old", 7, 9),
            heard("hi", 7, 9),
            heard("", 8, 9),
            heard("other channel", 8, 5),
            heard("yo", 8, 9),
            heard("after", 8, 9),
        ] {
            db.log_activity(1, &log, day).await.unwrap();
        }
        sqlx::query("UPDATE activity_log SET at = '2000-01-01T00:00:00.000Z' WHERE id = 1")
            .execute(&db.0)
            .await
            .unwrap();
        let lines = db
            .heard_before(1, 9, 6, "2001-01-01T00:00:00.000Z")
            .await
            .unwrap();
        let said: Vec<_> = lines.iter().map(|(u, t, _)| (*u, t.as_str())).collect();
        assert_eq!(said, [(Some(8), "yo"), (Some(7), "hi")]);
        assert!(db.heard_before(2, 9, 6, "").await.unwrap().is_empty());
    }

    #[test]
    fn highlight_from_judged_json() {
        assert!(SaidHighlight::from_judged_data("nope").is_none());
        let h = SaidHighlight::from_judged_data(r#"{"said":"hi","meanness":0.25}"#).unwrap();
        assert_eq!(h.said, "hi");
        assert_eq!(h.meanness, Some(0.25));
        let unrated = SaidHighlight::from_judged_data(r#"{"said":"hi","score":0.9}"#).unwrap();
        assert_eq!(unrated.meanness, None);
    }
}
