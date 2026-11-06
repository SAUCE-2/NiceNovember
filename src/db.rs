//! SQLite persistence. Snowflakes are stored as i64; times as fixed-width RFC3339 UTC text.

use std::fmt;
use std::str::FromStr;

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

#[derive(Clone)]
pub struct Db(pub SqlitePool);

/// Jev score at or above which an utterance counts as hostile, unless the guild set its own.
pub const DEFAULT_THRESHOLD: f32 = 0.7;

#[derive(Clone, Debug, Default, sqlx::FromRow)]
pub struct GuildConfig {
    pub participant_role_id: Option<i64>,
    pub announce_channel_id: Option<i64>,
    pub log_channel_id: Option<i64>,
    pub voice_channel_id: Option<i64>,
    pub threshold: Option<f64>,
    pub last_setup_hash: Option<String>,
    /// Judge as usual, but only report would-be timeouts to the log channel.
    pub shadow: bool,
}

impl GuildConfig {
    pub fn threshold(&self) -> f32 {
        self.threshold.map_or(DEFAULT_THRESHOLD, |t| t as f32)
    }

    /// Where details and setup notices go: the log channel, else the announce channel.
    pub fn log_or_announce(&self) -> Option<i64> {
        self.log_channel_id.or(self.announce_channel_id)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum GuildField {
    ParticipantRole,
    AnnounceChannel,
    LogChannel,
    VoiceChannel,
}

/// What `/nice setup` changes; `None` leaves a setting as it is.
#[derive(Clone, Copy, Debug, Default)]
pub struct SetupChanges {
    pub participant_role: Option<u64>,
    pub announce_channel: Option<u64>,
    pub log_channel: Option<u64>,
    pub voice_channel: Option<u64>,
    pub threshold: Option<f32>,
    pub shadow: Option<bool>,
}

/// The optional settings `/nice unset` can clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clear {
    LogChannel,
    VoiceChannel,
    Threshold,
}

/// How a timeout is applied. Discord refuses native timeouts on the owner, admins and anyone at
/// or above the bot's top role; those get a forced one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Native,
    Forced,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Method::Native => "native",
            Method::Forced => "forced",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct TimeoutRow {
    pub guild_id: i64,
    pub user_id: i64,
    pub until: String,
    pub method: String,
    pub voice_muted: bool,
}

impl TimeoutRow {
    pub fn until(&self) -> Option<DateTime<Utc>> {
        parse_time(&self.until)
    }
}

/// Fixed-width UTC timestamp, so stored times compare correctly as strings.
pub fn fmt_time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn now() -> String {
    fmt_time(Utc::now())
}

pub fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

impl Db {
    pub async fn connect(url: &str) -> Result<Self> {
        if let Some(path) = url.strip_prefix("sqlite://")
            && let Some(dir) = std::path::Path::new(path).parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let opts = SqliteConnectOptions::from_str(url)?.create_if_missing(true);
        // Each in-memory connection is its own database, so keep just one.
        let max = if url.contains(":memory:") { 1 } else { 5 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max)
            .connect_with(opts)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self(pool))
    }

    // ---- guild config ----

    pub async fn guild(&self, guild_id: u64) -> Result<GuildConfig> {
        let row = sqlx::query_as::<_, GuildConfig>(
            "SELECT participant_role_id, announce_channel_id, log_channel_id, voice_channel_id,
                    threshold, last_setup_hash, shadow
             FROM guild_config WHERE guild_id = ?",
        )
        .bind(guild_id as i64)
        .fetch_optional(&self.0)
        .await?;
        Ok(row.unwrap_or_default())
    }

    /// Guilds with a monitored voice channel set: `(guild_id, channel_id)`.
    pub async fn voice_channels(&self) -> Result<Vec<(i64, i64)>> {
        Ok(sqlx::query_as(
            "SELECT guild_id, voice_channel_id FROM guild_config WHERE voice_channel_id IS NOT NULL",
        )
        .fetch_all(&self.0)
        .await?)
    }

    pub async fn set_guild_field(
        &self,
        guild_id: u64,
        field: GuildField,
        value: Option<i64>,
    ) -> Result<()> {
        let sql = match field {
            GuildField::ParticipantRole => {
                "INSERT INTO guild_config (guild_id, participant_role_id) VALUES (?1, ?2)
                 ON CONFLICT (guild_id) DO UPDATE SET participant_role_id = excluded.participant_role_id"
            }
            GuildField::AnnounceChannel => {
                "INSERT INTO guild_config (guild_id, announce_channel_id) VALUES (?1, ?2)
                 ON CONFLICT (guild_id) DO UPDATE SET announce_channel_id = excluded.announce_channel_id"
            }
            GuildField::LogChannel => {
                "INSERT INTO guild_config (guild_id, log_channel_id) VALUES (?1, ?2)
                 ON CONFLICT (guild_id) DO UPDATE SET log_channel_id = excluded.log_channel_id"
            }
            GuildField::VoiceChannel => {
                "INSERT INTO guild_config (guild_id, voice_channel_id) VALUES (?1, ?2)
                 ON CONFLICT (guild_id) DO UPDATE SET voice_channel_id = excluded.voice_channel_id"
            }
        };
        sqlx::query(sql)
            .bind(guild_id as i64)
            .bind(value)
            .execute(&self.0)
            .await?;
        Ok(())
    }

    pub async fn set_threshold(&self, guild_id: u64, threshold: Option<f32>) -> Result<()> {
        sqlx::query(
            "INSERT INTO guild_config (guild_id, threshold) VALUES (?1, ?2)
             ON CONFLICT (guild_id) DO UPDATE SET threshold = excluded.threshold",
        )
        .bind(guild_id as i64)
        .bind(threshold.map(f64::from))
        .execute(&self.0)
        .await?;
        Ok(())
    }

    pub async fn apply_setup(&self, guild_id: u64, c: &SetupChanges) -> Result<()> {
        let fields = [
            (GuildField::ParticipantRole, c.participant_role),
            (GuildField::AnnounceChannel, c.announce_channel),
            (GuildField::LogChannel, c.log_channel),
            (GuildField::VoiceChannel, c.voice_channel),
        ];
        for (field, id) in fields {
            if let Some(id) = id {
                self.set_guild_field(guild_id, field, Some(id as i64))
                    .await?;
            }
        }
        if let Some(t) = c.threshold {
            self.set_threshold(guild_id, Some(t)).await?;
        }
        if let Some(on) = c.shadow {
            sqlx::query(
                "INSERT INTO guild_config (guild_id, shadow) VALUES (?1, ?2)
                 ON CONFLICT (guild_id) DO UPDATE SET shadow = excluded.shadow",
            )
            .bind(guild_id as i64)
            .bind(on)
            .execute(&self.0)
            .await?;
        }
        Ok(())
    }

    pub async fn clear(&self, guild_id: u64, what: Clear) -> Result<()> {
        match what {
            Clear::LogChannel => {
                self.set_guild_field(guild_id, GuildField::LogChannel, None)
                    .await
            }
            Clear::VoiceChannel => {
                self.set_guild_field(guild_id, GuildField::VoiceChannel, None)
                    .await
            }
            Clear::Threshold => self.set_threshold(guild_id, None).await,
        }
    }

    pub async fn set_setup_hash(&self, guild_id: u64, hash: Option<&str>) -> Result<()> {
        sqlx::query(
            "INSERT INTO guild_config (guild_id, last_setup_hash) VALUES (?1, ?2)
             ON CONFLICT (guild_id) DO UPDATE SET last_setup_hash = excluded.last_setup_hash",
        )
        .bind(guild_id as i64)
        .bind(hash)
        .execute(&self.0)
        .await?;
        Ok(())
    }

    // ---- timeouts ----

    /// Time `user_id` out until `until`, unless they're already out for longer. Keeps an owed
    /// unmute owed.
    pub async fn start_timeout(
        &self,
        guild_id: u64,
        user_id: u64,
        until: DateTime<Utc>,
        method: Method,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO timeouts (guild_id, user_id, until, method) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (guild_id, user_id) DO UPDATE SET
               until = max(until, excluded.until), method = excluded.method",
        )
        .bind(guild_id as i64)
        .bind(user_id as i64)
        .bind(fmt_time(until))
        .bind(method.to_string())
        .execute(&self.0)
        .await?;
        Ok(())
    }

    /// Any timeout row for the user, active or still owed an unmute.
    pub async fn timeout(&self, guild_id: u64, user_id: u64) -> Result<Option<TimeoutRow>> {
        Ok(
            sqlx::query_as("SELECT * FROM timeouts WHERE guild_id = ? AND user_id = ?")
                .bind(guild_id as i64)
                .bind(user_id as i64)
                .fetch_optional(&self.0)
                .await?,
        )
    }

    /// When `user_id`'s active timeout ends, or `None` if they aren't timed out.
    pub async fn timed_out(&self, guild_id: u64, user_id: u64) -> Result<Option<DateTime<Utc>>> {
        let row = self.timeout(guild_id, user_id).await?;
        Ok(row
            .and_then(|r| r.until())
            .filter(|until| *until > Utc::now()))
    }

    pub async fn set_voice_muted(&self, guild_id: u64, user_id: u64, muted: bool) -> Result<()> {
        sqlx::query("UPDATE timeouts SET voice_muted = ? WHERE guild_id = ? AND user_id = ?")
            .bind(muted)
            .bind(guild_id as i64)
            .bind(user_id as i64)
            .execute(&self.0)
            .await?;
        Ok(())
    }

    /// Timeouts that have ended (at `now`) and still need releasing.
    pub async fn ended_timeouts(&self, now: DateTime<Utc>) -> Result<Vec<TimeoutRow>> {
        Ok(sqlx::query_as("SELECT * FROM timeouts WHERE until <= ?")
            .bind(fmt_time(now))
            .fetch_all(&self.0)
            .await?)
    }

    pub async fn delete_timeout(&self, guild_id: u64, user_id: u64) -> Result<()> {
        sqlx::query("DELETE FROM timeouts WHERE guild_id = ? AND user_id = ?")
            .bind(guild_id as i64)
            .bind(user_id as i64)
            .execute(&self.0)
            .await?;
        Ok(())
    }

    /// End a timeout now, leaving the release (unmute) to the sweeper. Returns whether one was
    /// active.
    pub async fn end_timeout(&self, guild_id: u64, user_id: u64) -> Result<bool> {
        let n = sqlx::query(
            "UPDATE timeouts SET until = ?1 WHERE guild_id = ?2 AND user_id = ?3 AND until > ?1",
        )
        .bind(now())
        .bind(guild_id as i64)
        .bind(user_id as i64)
        .execute(&self.0)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Timeouts in force right now, in one guild or all of them.
    pub async fn active_timeouts(&self, guild_id: Option<u64>) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM timeouts WHERE (?1 IS NULL OR guild_id = ?1) AND until > ?2",
        )
        .bind(guild_id.map(|g| g as i64))
        .bind(now())
        .fetch_one(&self.0)
        .await?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    pub async fn db() -> Db {
        Db::connect("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn guild_config_defaults_and_updates() {
        let db = db().await;
        let g = db.guild(1).await.unwrap();
        assert!(g.participant_role_id.is_none());
        assert_eq!(g.threshold(), DEFAULT_THRESHOLD);
        assert_eq!(g.log_or_announce(), None);

        for (field, v) in [
            (GuildField::ParticipantRole, 42),
            (GuildField::AnnounceChannel, 43),
            (GuildField::VoiceChannel, 44),
        ] {
            db.set_guild_field(1, field, Some(v)).await.unwrap();
        }
        db.set_threshold(1, Some(0.5)).await.unwrap();
        db.set_setup_hash(1, Some("x")).await.unwrap();
        let g = db.guild(1).await.unwrap();
        assert_eq!(g.participant_role_id, Some(42));
        assert_eq!(g.announce_channel_id, Some(43));
        assert_eq!(g.voice_channel_id, Some(44));
        assert_eq!(g.threshold(), 0.5);
        assert_eq!(g.last_setup_hash.as_deref(), Some("x"));
        assert_eq!(g.log_or_announce(), Some(43), "falls back to announce");

        db.set_guild_field(1, GuildField::LogChannel, Some(45))
            .await
            .unwrap();
        assert_eq!(db.guild(1).await.unwrap().log_or_announce(), Some(45));
        assert_eq!(db.voice_channels().await.unwrap(), vec![(1, 44)]);

        db.set_guild_field(1, GuildField::VoiceChannel, None)
            .await
            .unwrap();
        assert!(db.voice_channels().await.unwrap().is_empty());
        db.set_threshold(1, None).await.unwrap();
        assert_eq!(db.guild(1).await.unwrap().threshold(), DEFAULT_THRESHOLD);
    }

    #[tokio::test]
    async fn setup_changes_only_what_was_given_and_clears() {
        let db = db().await;
        let all = SetupChanges {
            participant_role: Some(1),
            announce_channel: Some(2),
            log_channel: Some(3),
            voice_channel: Some(4),
            threshold: Some(0.4),
            shadow: Some(true),
        };
        assert!(!db.guild(1).await.unwrap().shadow, "off by default");
        db.apply_setup(1, &all).await.unwrap();
        let only_voice = SetupChanges {
            voice_channel: Some(5),
            ..Default::default()
        };
        db.apply_setup(1, &only_voice).await.unwrap();
        let g = db.guild(1).await.unwrap();
        assert!(g.shadow, "left as it was");
        assert_eq!(
            (
                g.participant_role_id,
                g.announce_channel_id,
                g.log_channel_id,
                g.voice_channel_id,
                g.threshold()
            ),
            (Some(1), Some(2), Some(3), Some(5), 0.4)
        );

        db.clear(1, Clear::LogChannel).await.unwrap();
        assert_eq!(db.guild(1).await.unwrap().log_channel_id, None);
        db.clear(1, Clear::VoiceChannel).await.unwrap();
        assert_eq!(db.guild(1).await.unwrap().voice_channel_id, None);
        db.clear(1, Clear::Threshold).await.unwrap();
        let g = db.guild(1).await.unwrap();
        assert_eq!(g.threshold(), DEFAULT_THRESHOLD);
        assert_eq!(g.participant_role_id, Some(1), "the rest stays");

        let off = SetupChanges {
            shadow: Some(false),
            ..Default::default()
        };
        db.apply_setup(1, &off).await.unwrap();
        assert!(!db.guild(1).await.unwrap().shadow);
    }

    #[tokio::test]
    async fn timeouts_extend_end_and_release() {
        let db = db().await;
        let soon = Utc::now() + Duration::seconds(10);
        assert!(db.timed_out(1, 7).await.unwrap().is_none());

        db.start_timeout(1, 7, soon, Method::Forced).await.unwrap();
        assert!(db.timed_out(1, 7).await.unwrap().is_some());
        assert_eq!(db.active_timeouts(None).await.unwrap(), 1);
        assert_eq!(db.active_timeouts(Some(2)).await.unwrap(), 0);

        // A shorter one doesn't cut it short, but the method follows the latest.
        db.start_timeout(1, 7, Utc::now(), Method::Native)
            .await
            .unwrap();
        let row = db.timeout(1, 7).await.unwrap().unwrap();
        assert_eq!(
            row.until(),
            Some(soon).map(|t| parse_time(&fmt_time(t)).unwrap())
        );
        assert_eq!(row.method, "native");
        assert!(!row.voice_muted);

        db.set_voice_muted(1, 7, true).await.unwrap();
        assert!(db.ended_timeouts(Utc::now()).await.unwrap().is_empty());
        assert_eq!(
            db.ended_timeouts(soon + Duration::seconds(1))
                .await
                .unwrap()
                .len(),
            1
        );

        assert!(db.end_timeout(1, 7).await.unwrap());
        assert!(!db.end_timeout(1, 7).await.unwrap(), "already ended");
        assert!(db.timed_out(1, 7).await.unwrap().is_none());
        let owed = db.ended_timeouts(Utc::now()).await.unwrap();
        assert_eq!(owed.len(), 1);
        assert!(owed[0].voice_muted, "still owed an unmute");

        db.delete_timeout(1, 7).await.unwrap();
        assert!(db.timeout(1, 7).await.unwrap().is_none());
    }
}
