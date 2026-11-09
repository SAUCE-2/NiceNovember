//! The keys in dashboard links: each one lets whoever has the link read one guild's log for a while.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use crate::db::{self, Db};

/// How long a dashboard link works.
pub const LINK_LIFETIME: Duration = Duration::days(7);

/// 32 random bytes as hex.
fn random_key() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl Db {
    /// A new key for `guild_id`'s dashboard, working until `LINK_LIFETIME` after `now`. Expired keys
    /// are deleted on the way.
    pub async fn new_dashboard_key(&self, guild_id: u64, now: DateTime<Utc>) -> Result<String> {
        let key = random_key()?;
        sqlx::query("DELETE FROM dashboard_keys WHERE expires <= ?")
            .bind(db::fmt_time(now))
            .execute(&self.0)
            .await?;
        sqlx::query("INSERT INTO dashboard_keys (key, guild_id, expires) VALUES (?, ?, ?)")
            .bind(&key)
            .bind(guild_id as i64)
            .bind(db::fmt_time(now + LINK_LIFETIME))
            .execute(&self.0)
            .await?;
        Ok(key)
    }

    /// The guild a key opens, if it's still valid at `now`.
    pub async fn dashboard_guild(&self, key: &str, now: DateTime<Utc>) -> Result<Option<u64>> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT guild_id FROM dashboard_keys WHERE key = ? AND expires > ?")
                .bind(key)
                .bind(db::fmt_time(now))
                .fetch_optional(&self.0)
                .await?;
        Ok(row.map(|(g,)| g as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keys_open_their_guild_until_they_expire() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let now = Utc::now();
        let a = db.new_dashboard_key(1, now).await.unwrap();
        let b = db.new_dashboard_key(2, now).await.unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);

        assert_eq!(db.dashboard_guild(&a, now).await.unwrap(), Some(1));
        assert_eq!(db.dashboard_guild(&b, now).await.unwrap(), Some(2));
        assert_eq!(db.dashboard_guild("nope", now).await.unwrap(), None);

        let later = now + LINK_LIFETIME;
        assert_eq!(
            db.dashboard_guild(&a, later).await.unwrap(),
            None,
            "expired"
        );

        // Making a key clears out the expired ones.
        db.new_dashboard_key(1, later).await.unwrap();
        let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM dashboard_keys")
            .fetch_one(&db.0)
            .await
            .unwrap();
        assert_eq!(left, 1);
    }
}
