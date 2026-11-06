use std::ops::Deref;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};

use crate::ai::jev::Jev;
use crate::config::Config;
use crate::db::Db;
use crate::stats::sessions::{LastPresence, VcSessions};
use crate::voice::monitor::VoiceMonitor;
use crate::voice::stt::Stt;

pub type Error = anyhow::Error;
pub type Ctx<'a> = poise::Context<'a, Data, Error>;

/// Shared bot state. Cheap to clone into background tasks.
#[derive(Clone)]
pub struct Data(Arc<Inner>);

pub struct Inner {
    pub cfg: Config,
    pub db: Db,
    /// None if the Whisper model couldn't be loaded; voice can't be judged then.
    pub stt: Option<Stt>,
    /// None if JEV_API_KEY isn't set; nothing can be judged then.
    pub jev: Option<Jev>,
    pub monitor: VoiceMonitor,
    /// Who's in VC since when, for time-in-VC stats.
    pub vc_sessions: VcSessions,
    /// Last presence logged per person, so only changes are logged.
    pub last_presence: LastPresence,
    /// When the process started, for `/status` uptime.
    pub started_at: DateTime<Utc>,
}

impl Deref for Data {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl Data {
    pub fn new(inner: Inner) -> Self {
        Self(Arc::new(inner))
    }

    /// "Today" in the bot's configured timezone.
    pub fn today(&self) -> NaiveDate {
        Utc::now().with_timezone(&self.cfg.tz).date_naive()
    }
}
