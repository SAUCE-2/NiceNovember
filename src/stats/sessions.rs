//! In-memory state the activity log needs: when people joined VC, and what their presence was
//! last time we logged it (presence updates are very chatty; only real changes are logged).

use std::time::{Duration, Instant};

use dashmap::DashMap;
use poise::serenity_prelude::{GuildId, UserId};

/// When each person currently in VC joined, so time spent there can be logged on leaving.
/// Sessions running when the bot restarts are picked up again from the guild cache.
#[derive(Default)]
pub struct VcSessions(DashMap<(GuildId, UserId), Instant>);

impl VcSessions {
    /// Start a session unless one is already running (moving channels keeps it).
    pub fn start(&self, guild_id: GuildId, user: UserId) {
        self.0.entry((guild_id, user)).or_insert_with(Instant::now);
    }

    /// End a session, returning how long it lasted.
    pub fn end(&self, guild_id: GuildId, user: UserId) -> Option<Duration> {
        self.0.remove(&(guild_id, user)).map(|(_, t)| t.elapsed())
    }
}

/// The last presence logged per person, so repeats aren't logged again.
#[derive(Default)]
pub struct LastPresence(DashMap<(GuildId, UserId), String>);

impl LastPresence {
    /// Remember `fingerprint`; returns whether it differs from the last one.
    pub fn changed(&self, guild_id: GuildId, user: UserId, fingerprint: String) -> bool {
        match self.0.insert((guild_id, user), fingerprint.clone()) {
            Some(old) => old != fingerprint,
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_and_presence() {
        let s = VcSessions::default();
        let (g, u) = (GuildId::new(1), UserId::new(2));
        assert!(s.end(g, u).is_none());
        s.start(g, u);
        s.start(g, u);
        assert!(s.end(g, u).is_some());
        assert!(s.end(g, u).is_none());

        let p = LastPresence::default();
        assert!(p.changed(g, u, "online".into()));
        assert!(!p.changed(g, u, "online".into()));
        assert!(p.changed(g, u, "idle".into()));
    }
}
