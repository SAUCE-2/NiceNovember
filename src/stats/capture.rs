//! Logs Discord events as they arrive, with the full payload. Only participants are logged: events
//! about anyone else (and bots) are dropped. Entries about nobody in particular are kept.
//!
//! | kind | user | data |
//! |---|---|---|
//! | `message` | author | the whole message (content, attachments, embeds, mentions, reply, ...) |
//! | `message_edit` | author | the update event (new content, edited_timestamp, ...) |
//! | `message_delete` | author (only messages we logged) | `{message_id}` |
//! | `message_delete_bulk` | | `{message_ids}` |
//! | `reaction_add` / `reaction_remove` | reactor | the reaction (emoji, message_id, message_author_id) |
//! | `voice` | member | `{change: join/leave/move/update, old, new}` voice states (mute, deaf, stream, video) |
//! | `vc_session` | member | `{secs, channel_id}` on leaving VC |
//! | `presence` | member | status, client_status and activities (games, Spotify, custom status); changes only |
//! | `typing` | typer | `{timestamp}` |
//! | `command` | user | `{name, data}` slash command used |
//! | `member_seen` | member | the member (names, nick, roles, joined_at), once per bot start |
//! | `member_join` / `member_leave` / `member_update` | member | member details; updates include the old nick and roles, and are logged when they had the role before or after |
//! | `thread_create` | owner | `{name, parent_id}` |

use poise::serenity_prelude::{
    self as serenity, ChannelId, FullEvent, GuildId, Interaction, UserId, VoiceState,
};
use serde_json::json;

use super::{Log, kind, record};
use crate::app::Data;
use crate::participant::holds;

/// The guild's participant role (`None` if it isn't set or can't be read).
async fn participant_role(data: &Data, guild_id: GuildId) -> Option<i64> {
    data.db
        .guild(guild_id.get())
        .await
        .ok()?
        .participant_role_id
}

/// Does `user` hold the participant role, from the cache (`None` = not cached).
pub async fn participant(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user: UserId,
) -> Option<bool> {
    let Some(role) = participant_role(data, guild_id).await else {
        return Some(false);
    };
    let g = ctx.cache.guild(guild_id)?;
    let member = g.members.get(&user)?;
    Some(holds(Some(role), &member.roles))
}

/// Is someone logged? Only when they're known to hold the participant role.
fn tracked(participant: Option<bool>) -> bool {
    participant == Some(true)
}

fn is_bot(ctx: &serenity::Context, user: UserId) -> bool {
    user == ctx.cache.current_user().id || ctx.cache.user(user).is_some_and(|u| u.bot)
}

/// Log `log` about `user` if they're a participant. Skips bots and everyone else.
async fn log_user(ctx: &serenity::Context, data: &Data, guild_id: GuildId, user: UserId, log: Log) {
    if is_bot(ctx, user) || !tracked(participant(ctx, data, guild_id, user).await) {
        return;
    }
    record(data, guild_id, log.user(user).participant(Some(true))).await;
}

pub async fn capture(ctx: &serenity::Context, event: &FullEvent, data: &Data) {
    match event {
        FullEvent::Message { new_message: m } => {
            let Some(g) = m.guild_id else { return };
            if m.author.bot {
                return;
            }
            let log = Log::new("message", m).channel(m.channel_id);
            log_user(ctx, data, g, m.author.id, log).await;
        }
        FullEvent::MessageUpdate { event: e, .. } => {
            let (Some(g), Some(author)) = (e.guild_id, &e.author) else {
                return;
            };
            if author.bot {
                return;
            }
            let log = Log::new("message_edit", e).channel(e.channel_id);
            log_user(ctx, data, g, author.id, log).await;
        }
        FullEvent::MessageDelete {
            channel_id,
            deleted_message_id,
            guild_id: Some(g),
        } => {
            let id = deleted_message_id.get();
            let author = data
                .db
                .logged_message_author(g.get(), id)
                .await
                .ok()
                .flatten();
            // A message we didn't log was someone else's, or from before the bot was here.
            let Some(u) = author else { return };
            let log = Log::new("message_delete", json!({ "message_id": id.to_string() }))
                .channel(*channel_id);
            log_user(ctx, data, *g, UserId::new(u as u64), log).await;
        }
        FullEvent::MessageDeleteBulk {
            channel_id,
            multiple_deleted_messages_ids: ids,
            guild_id: Some(g),
        } => {
            let ids: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
            let log =
                Log::new("message_delete_bulk", json!({ "message_ids": ids })).channel(*channel_id);
            record(data, *g, log).await;
        }
        FullEvent::ReactionAdd { add_reaction: r } => reaction(ctx, data, "reaction_add", r).await,
        FullEvent::ReactionRemove {
            removed_reaction: r,
        } => reaction(ctx, data, "reaction_remove", r).await,
        FullEvent::VoiceStateUpdate { old, new } => voice(ctx, data, old.as_ref(), new).await,
        FullEvent::PresenceUpdate { new_data: p } => {
            let Some(g) = p.guild_id else { return };
            let acts: Vec<_> = p
                .activities
                .iter()
                .map(|a| (&a.name, a.kind, &a.details, &a.state))
                .collect();
            let fingerprint = json!([p.status, acts]).to_string();
            if data.last_presence.changed(g, p.user.id, fingerprint) {
                let log = Log::new(
                    "presence",
                    json!({
                        "status": p.status,
                        "client_status": p.client_status,
                        "activities": p.activities,
                    }),
                );
                log_user(ctx, data, g, p.user.id, log).await;
            }
        }
        FullEvent::TypingStart { event: e } => {
            let Some(g) = e.guild_id else { return };
            let log = Log::new("typing", json!({ "timestamp": e.timestamp })).channel(e.channel_id);
            log_user(ctx, data, g, e.user_id, log).await;
        }
        FullEvent::InteractionCreate {
            interaction: Interaction::Command(c),
        } => {
            let Some(g) = c.guild_id else { return };
            let log = Log::new("command", json!({ "name": c.data.name, "data": c.data }))
                .channel(c.channel_id);
            log_user(ctx, data, g, c.user.id, log).await;
        }
        FullEvent::GuildCreate { guild, .. } => {
            for (id, m) in &guild.members {
                if !m.user.bot {
                    log_user(ctx, data, guild.id, *id, Log::new("member_seen", m)).await;
                }
            }
            // People already in VC (e.g. after a restart): count their time from now.
            for (id, vs) in &guild.voice_states {
                if vs.channel_id.is_some() {
                    data.vc_sessions.start(guild.id, *id);
                }
            }
        }
        FullEvent::GuildMemberAddition { new_member: m } => {
            if !m.user.bot {
                log_user(ctx, data, m.guild_id, m.user.id, Log::new("member_join", m)).await;
            }
        }
        FullEvent::GuildMemberRemoval {
            guild_id,
            user,
            member_data_if_available,
        } => {
            // They're gone from the cache, so only the member data says if they had the role.
            let role = participant_role(data, *guild_id).await;
            let was = member_data_if_available
                .as_ref()
                .map(|m| holds(role, &m.roles));
            if !user.bot && tracked(was) {
                let log = Log::new(
                    "member_leave",
                    json!({ "user": user, "member": member_data_if_available }),
                )
                .user(user.id)
                .participant(Some(true));
                record(data, *guild_id, log).await;
            }
        }
        FullEvent::GuildMemberUpdate {
            old_if_available,
            event: e,
            ..
        } => {
            // Log gaining or losing the role too, not just changes while they hold it.
            let role = participant_role(data, e.guild_id).await;
            let was = old_if_available.as_ref().map(|m| holds(role, &m.roles));
            let is = holds(role, &e.roles);
            if !e.user.bot && (is || tracked(was)) {
                let log = Log::new(
                    "member_update",
                    json!({
                        "old_nick": old_if_available.as_ref().map(|m| &m.nick),
                        "old_roles": old_if_available.as_ref().map(|m| &m.roles),
                        "new": e,
                    }),
                );
                let log = log.user(e.user.id).participant(Some(is));
                record(data, e.guild_id, log).await;
            }
        }
        FullEvent::ThreadCreate { thread } => {
            if let Some(owner) = thread.owner_id {
                let log = Log::new(
                    "thread_create",
                    json!({ "name": thread.name, "parent_id": thread.parent_id }),
                )
                .channel(thread.id);
                log_user(ctx, data, thread.guild_id, owner, log).await;
            }
        }
        _ => {}
    }
}

async fn reaction(
    ctx: &serenity::Context,
    data: &Data,
    kind: &'static str,
    r: &serenity::Reaction,
) {
    let (Some(g), Some(user)) = (r.guild_id, r.user_id) else {
        return;
    };
    if r.member.as_ref().is_some_and(|m| m.user.bot) {
        return;
    }
    let log = Log::new(
        kind,
        json!({
            "emoji": r.emoji,
            "message_id": r.message_id,
            "message_author_id": r.message_author_id,
            "burst": r.burst,
        }),
    )
    .channel(r.channel_id);
    log_user(ctx, data, g, user, log).await;
}

/// What a voice state update was: `join`, `leave`, `move`, or `update` (mute, deafen, stream...).
pub fn voice_change(old: Option<ChannelId>, new: Option<ChannelId>) -> &'static str {
    match (old, new) {
        (None, Some(_)) => "join",
        (Some(_), None) => "leave",
        (Some(a), Some(b)) if a != b => "move",
        _ => "update",
    }
}

/// A voice state without the bulky member object (it's the same person every time).
fn strip(v: &VoiceState) -> serde_json::Value {
    json!({
        "channel_id": v.channel_id,
        "self_mute": v.self_mute,
        "self_deaf": v.self_deaf,
        "self_stream": v.self_stream,
        "self_video": v.self_video,
        "mute": v.mute,
        "deaf": v.deaf,
        "suppress": v.suppress,
    })
}

async fn voice(ctx: &serenity::Context, data: &Data, old: Option<&VoiceState>, new: &VoiceState) {
    let Some(g) = new.guild_id else { return };
    if new.member.as_ref().is_some_and(|m| m.user.bot) {
        return;
    }
    let user = new.user_id;
    let old_ch = old.and_then(|o| o.channel_id);
    let change = voice_change(old_ch, new.channel_id);
    let mut log = Log::new(
        "voice",
        json!({ "change": change, "old": old.map(strip), "new": strip(new) }),
    );
    if let Some(ch) = new.channel_id.or(old_ch) {
        log = log.channel(ch);
    }
    log_user(ctx, data, g, user, log).await;

    match change {
        "join" | "move" => data.vc_sessions.start(g, user),
        "leave" => {
            if let Some(spent) = data.vc_sessions.end(g, user) {
                let secs = spent.as_secs();
                let mut log = Log::new(kind::VC_SESSION, json!({ "secs": secs }));
                if let Some(ch) = old_ch {
                    log = log.channel(ChannelId::new(ch.get()));
                }
                log_user(ctx, data, g, user, log).await;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use poise::serenity_prelude::{Message, MessageId};

    #[test]
    fn voice_changes() {
        use super::{ChannelId, voice_change};
        let (a, b) = (Some(ChannelId::new(1)), Some(ChannelId::new(2)));
        assert_eq!(voice_change(None, a), "join");
        assert_eq!(voice_change(a, None), "leave");
        assert_eq!(voice_change(a, b), "move");
        assert_eq!(voice_change(a, a), "update");
        assert_eq!(voice_change(None, None), "update");
    }

    #[test]
    fn only_participants_are_tracked() {
        use super::tracked;
        assert!(tracked(Some(true)));
        assert!(!tracked(Some(false)), "not in the role");
        assert!(!tracked(None), "not cached, so not known to be in the role");
    }

    #[test]
    fn message_ids_serialize_as_strings() {
        // `logged_message_author` matches on `json_extract(data, '$.id')` as a string.
        let mut m = Message::default();
        m.id = MessageId::new(555);
        assert_eq!(serde_json::to_value(&m).unwrap()["id"], "555");
    }
}
