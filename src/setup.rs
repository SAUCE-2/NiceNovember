//! Guild config health check: tells admins what's missing and how to fix it.

use anyhow::Result;
use poise::serenity_prelude::{
    self as serenity, ChannelId, ChannelType, CreateEmbed, CreateMessage, GuildId, RoleId,
};

use crate::app::Data;

/// Whether a configured thing still exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting<T> {
    Unset,
    Deleted,
    Present(T),
}

impl<T> Setting<T> {
    fn of(id: Option<i64>, find: impl FnOnce(i64) -> Option<T>) -> Self {
        match id {
            None => Setting::Unset,
            Some(id) => find(id).map_or(Setting::Deleted, Setting::Present),
        }
    }
}

/// What the checklist looks at, gathered from the cache and the bot's config.
#[derive(Clone, Copy, Debug)]
pub struct Facts {
    pub role: Setting<()>,
    pub announce: Setting<()>,
    pub log: Setting<()>,
    pub voice: Setting<ChannelType>,
    /// `None` if the bot's own member couldn't be looked up.
    pub bot_is_admin: Option<bool>,
    pub jev: bool,
    pub whisper: bool,
    pub shadow: bool,
}

const FIX_ROLE: &str = "`/nice setup role:@Role`";
const FIX_ANNOUNCE: &str = "`/nice setup announce_channel:#channel`";
const FIX_VOICE: &str = "`/nice setup voice_channel:🔊channel`";

/// Everything wrong, each with the fix.
pub fn checklist(f: &Facts) -> Vec<String> {
    let mut out = Vec::new();
    let mut add = |s: String| out.push(s);
    match f.role {
        Setting::Unset => add(format!(
            "No participant role, so nobody is judged → {FIX_ROLE}"
        )),
        Setting::Deleted => add(format!("The participant role was deleted → {FIX_ROLE}")),
        Setting::Present(()) => {}
    }
    match f.announce {
        Setting::Unset => add(format!(
            "No announce channel (hostile quotes go there) → {FIX_ANNOUNCE}"
        )),
        Setting::Deleted => add(format!("The announce channel was deleted → {FIX_ANNOUNCE}")),
        Setting::Present(()) => {}
    }
    if f.log == Setting::Deleted {
        add("The log channel was deleted → `/nice setup log_channel:#channel`".into());
    }
    match f.voice {
        Setting::Unset => add(format!(
            "No monitored voice channel, so voice isn't judged → {FIX_VOICE}"
        )),
        Setting::Deleted => add(format!(
            "The monitored voice channel was deleted → {FIX_VOICE}"
        )),
        Setting::Present(ChannelType::Voice | ChannelType::Stage) => {}
        Setting::Present(_) => add(format!(
            "The monitored channel isn't a voice channel → {FIX_VOICE}"
        )),
    }
    if f.bot_is_admin == Some(false) {
        add("I need **Administrator** → give my role it in Server Settings → Roles".into());
    }
    if !f.jev {
        add("`JEV_API_KEY` isn't set on the bot, so nothing can be judged".into());
    }
    if !f.whisper && f.voice != Setting::Unset {
        add(
            "The Whisper model isn't loaded (check `WHISPER_MODEL`), so voice can't be transcribed"
                .into(),
        );
    }
    if f.shadow && f.log != Setting::Present(()) {
        add(
            "Shadow mode is on but there's no log channel, so would-be timeouts are only in \
             the activity log → `/nice setup log_channel:#channel`"
                .into(),
        );
    }
    out
}

/// Everything wrong with this guild's setup, each with the fix.
pub async fn problems(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
) -> Result<Vec<String>> {
    let gcfg = data.db.guild(guild_id.get()).await?;
    let bot_id = ctx.cache.current_user().id;
    let bot_member = guild_id.member(ctx, bot_id).await.ok();
    let facts = {
        let Some(g) = ctx.cache.guild(guild_id) else {
            return Ok(vec![
                "Server isn't cached yet; try again in a moment.".into(),
            ]);
        };
        let channel = |id: i64| g.channels.get(&ChannelId::new(id as u64)).map(|c| c.kind);
        Facts {
            role: Setting::of(gcfg.participant_role_id, |r| {
                g.roles.contains_key(&RoleId::new(r as u64)).then_some(())
            }),
            announce: Setting::of(gcfg.announce_channel_id, |c| channel(c).map(|_| ())),
            log: Setting::of(gcfg.log_channel_id, |c| channel(c).map(|_| ())),
            voice: Setting::of(gcfg.voice_channel_id, channel),
            bot_is_admin: bot_member.map(|m| g.member_permissions(&m).administrator()),
            jev: data.jev.is_some(),
            whisper: data.stt.is_some(),
            shadow: gcfg.shadow,
        }
    };
    Ok(checklist(&facts))
}

pub fn embed(problems: &[String]) -> CreateEmbed {
    if problems.is_empty() {
        return CreateEmbed::new()
            .title("✅ NiceNovember is fully set up")
            .description(format!(
                "Participants who get hostile are timed out for {}s and quoted in the announce channel.",
                crate::timeout::TIMEOUT_SECS
            ))
            .colour(0x2ecc71);
    }
    let body: String = problems.iter().map(|p| format!("• {p}\n")).collect();
    CreateEmbed::new()
        .title("🤝 NiceNovember needs some setup")
        .description(format!(
            "{body}\nAdmins: run `/nice setup` any time to see this checklist."
        ))
        .colour(0xe74c3c)
}

/// Run the check and post it if the set of problems changed since the last report.
pub async fn report(ctx: &serenity::Context, data: &Data, guild_id: GuildId) -> Result<()> {
    let problems = problems(ctx, data, guild_id).await?;
    let fingerprint = problems.join("\n");
    let last = data.db.guild(guild_id.get()).await?.last_setup_hash;
    if !should_report(last.as_deref(), &fingerprint) {
        return Ok(());
    }
    data.db
        .set_setup_hash(guild_id.get(), Some(&fingerprint))
        .await?;
    send(
        ctx,
        data,
        guild_id,
        CreateMessage::new().embed(embed(&problems)),
    )
    .await;
    Ok(())
}

/// Post when the problems changed, except "all fine" when nothing was ever reported.
pub fn should_report(last: Option<&str>, fingerprint: &str) -> bool {
    let last_was_ok = last.is_none_or(str::is_empty);
    last != Some(fingerprint) && !(fingerprint.is_empty() && last_was_ok)
}

/// Log channel → announce channel → system channel → first writable channel → DM the owner.
async fn send(ctx: &serenity::Context, data: &Data, guild_id: GuildId, msg: CreateMessage) {
    let preferred = data
        .db
        .guild(guild_id.get())
        .await
        .ok()
        .and_then(|g| g.log_or_announce());
    let (candidates, owner) = {
        let Some(g) = ctx.cache.guild(guild_id) else {
            return;
        };
        let me = g.members.get(&ctx.cache.current_user().id);
        let mut text: Vec<_> = g
            .channels
            .values()
            .filter(|c| c.kind == ChannelType::Text)
            .collect();
        text.sort_by_key(|c| c.position);
        let writable = text
            .into_iter()
            .filter(|c| me.is_some_and(|m| g.user_permissions_in(c, m).send_messages()))
            .map(|c| c.id);
        let candidates: Vec<ChannelId> = preferred
            .map(|c| ChannelId::new(c as u64))
            .into_iter()
            .chain(g.system_channel_id)
            .chain(writable)
            .collect();
        (candidates, g.owner_id)
    };
    for ch in candidates {
        if ch.send_message(ctx, msg.clone()).await.is_ok() {
            return;
        }
    }
    if let Err(e) = owner.direct_message(ctx, msg).await {
        tracing::warn!(%guild_id, "couldn't deliver setup notice anywhere: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fine() -> Facts {
        Facts {
            role: Setting::Present(()),
            announce: Setting::Present(()),
            log: Setting::Unset,
            voice: Setting::Present(ChannelType::Voice),
            bot_is_admin: Some(true),
            jev: true,
            whisper: true,
            shadow: false,
        }
    }

    fn check(f: impl FnOnce(&mut Facts)) -> Vec<String> {
        let mut facts = fine();
        f(&mut facts);
        checklist(&facts)
    }

    #[test]
    fn all_set_up_is_empty() {
        assert!(checklist(&fine()).is_empty());
        assert!(check(|f| f.log = Setting::Present(())).is_empty());
        assert!(check(|f| f.voice = Setting::Present(ChannelType::Stage)).is_empty());
        assert!(
            check(|f| f.bot_is_admin = None).is_empty(),
            "unknown isn't a problem"
        );
        assert!(
            check(|f| (f.voice, f.whisper) = (Setting::Unset, false)).len() == 1,
            "no Whisper only matters with a voice channel"
        );
        assert!(check(|f| (f.shadow, f.log) = (true, Setting::Present(()))).is_empty());
    }

    #[test]
    fn each_problem_has_its_fix() {
        let one = |f: fn(&mut Facts)| {
            let p = check(f);
            assert_eq!(p.len(), 1, "{p:?}");
            p[0].clone()
        };
        assert!(one(|f| f.role = Setting::Unset).starts_with("No participant role"));
        assert!(
            one(|f| f.role = Setting::Deleted).contains("role was deleted → `/nice setup role")
        );
        assert!(one(|f| f.announce = Setting::Unset).starts_with("No announce channel"));
        assert!(
            one(|f| f.announce = Setting::Deleted).starts_with("The announce channel was deleted")
        );
        assert!(one(|f| f.log = Setting::Deleted).starts_with("The log channel was deleted"));
        assert!(one(|f| f.voice = Setting::Unset).starts_with("No monitored voice channel"));
        assert!(
            one(|f| f.voice = Setting::Deleted)
                .starts_with("The monitored voice channel was deleted")
        );
        assert!(
            one(|f| f.voice = Setting::Present(ChannelType::Text))
                .contains("isn't a voice channel")
        );
        assert!(one(|f| f.bot_is_admin = Some(false)).contains("Administrator"));
        assert!(one(|f| f.jev = false).contains("JEV_API_KEY"));
        assert!(one(|f| f.whisper = false).contains("WHISPER_MODEL"));
        assert!(one(|f| f.shadow = true).starts_with("Shadow mode is on but there's no log"));
    }

    #[test]
    fn settings_resolve() {
        assert_eq!(Setting::of(None, |_| Some(1)), Setting::Unset);
        assert_eq!(Setting::of(Some(5), |_| None::<u8>), Setting::Deleted);
        assert_eq!(
            Setting::of(Some(5), |id| Some(id * 2)),
            Setting::Present(10)
        );
    }

    #[test]
    fn reports_only_changes() {
        assert!(should_report(None, "a"));
        assert!(!should_report(Some("a"), "a"));
        assert!(should_report(Some("a"), "b"));
        assert!(should_report(Some("a"), ""), "fixed: say so");
        assert!(!should_report(None, ""), "fine from the start: stay quiet");
        assert!(!should_report(Some(""), ""));
    }
}
