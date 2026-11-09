use anyhow::{Context, Result};
use chrono_tz::Tz;

/// Process-wide settings from the environment.
#[derive(Clone, Debug)]
pub struct Config {
    pub discord_token: String,
    /// Register slash commands in this guild only (instant updates while developing).
    pub dev_guild_id: Option<u64>,
    pub database_url: String,
    pub whisper_model: String,
    pub jev_api_key: Option<String>,
    /// Timezone that defines "today" for the activity log's `day`.
    pub tz: Tz,
    /// Port the dashboard listens on.
    pub dashboard_port: u16,
    /// Where people open the dashboard, for the links `/log dashboard` hands out.
    pub dashboard_url: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    /// Parse from any variable source (the environment, or a map in tests). Blank = unset.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let var = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let dashboard_port = var("DASHBOARD_PORT")
            .map(|v| v.trim().parse())
            .transpose()
            .context("DASHBOARD_PORT must be a port number")?
            .unwrap_or(8080);
        Ok(Self {
            discord_token: var("DISCORD_TOKEN").context("DISCORD_TOKEN is not set")?,
            dev_guild_id: var("DEV_GUILD_ID")
                .map(|v| v.trim().parse())
                .transpose()
                .context("DEV_GUILD_ID must be a number")?,
            database_url: var("DATABASE_URL")
                .unwrap_or_else(|| "sqlite://data/nicenovember.db".into()),
            whisper_model: var("WHISPER_MODEL").unwrap_or_else(|| "models/ggml-base.en.bin".into()),
            jev_api_key: var("JEV_API_KEY"),
            tz: var("BOT_TZ")
                .unwrap_or_else(|| "Europe/London".into())
                .parse()
                .map_err(|e| anyhow::anyhow!("BOT_TZ is not a valid timezone: {e}"))?,
            dashboard_port,
            dashboard_url: var("DASHBOARD_URL")
                .unwrap_or_else(|| format!("http://localhost:{dashboard_port}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn parse(vars: &[(&str, &str)]) -> Result<Config> {
        let m: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_vars(|k| m.get(k).cloned())
    }

    #[test]
    fn defaults_and_errors() {
        let c = parse(&[("DISCORD_TOKEN", "t"), ("JEV_API_KEY", " ")]).unwrap();
        assert_eq!(c.discord_token, "t");
        assert_eq!(c.dev_guild_id, None);
        assert_eq!(c.jev_api_key, None, "blank is unset");
        assert_eq!(c.database_url, "sqlite://data/nicenovember.db");
        assert_eq!(c.tz, chrono_tz::Europe::London);
        assert_eq!(c.dashboard_port, 8080);
        assert_eq!(c.dashboard_url, "http://localhost:8080");

        let c = parse(&[
            ("DISCORD_TOKEN", "t"),
            ("DEV_GUILD_ID", "12"),
            ("BOT_TZ", "UTC"),
            ("DASHBOARD_PORT", "9000"),
        ])
        .unwrap();
        assert_eq!(c.dev_guild_id, Some(12));
        assert_eq!(c.tz, chrono_tz::UTC);
        assert_eq!(c.dashboard_url, "http://localhost:9000", "follows the port");

        let c = parse(&[
            ("DISCORD_TOKEN", "t"),
            ("DASHBOARD_URL", "http://steamdeck.local:8080"),
        ])
        .unwrap();
        assert_eq!(c.dashboard_url, "http://steamdeck.local:8080");

        assert!(parse(&[]).is_err());
        assert!(parse(&[("DISCORD_TOKEN", "t"), ("DEV_GUILD_ID", "x")]).is_err());
        assert!(parse(&[("DISCORD_TOKEN", "t"), ("BOT_TZ", "Mars/Base")]).is_err());
        assert!(parse(&[("DISCORD_TOKEN", "t"), ("DASHBOARD_PORT", "99999")]).is_err());
    }
}
