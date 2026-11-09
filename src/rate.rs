//! Rating the meanness of utterances judged before it was asked, so `/stats` cards cover them
//! too. Each one is rebuilt as it was judged, as far as Discord and the activity log still say
//! (the conversation, what it replied to or reacted on, and images), and only its meanness is
//! asked: its verdict stands. Entries that already have a meanness are left alone, so it's safe
//! to run on every start.

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use poise::serenity_prelude::{self as serenity, ChannelId, GuildId, MessageId, UserId};
use serde::Deserialize;

use crate::ai::{self, jev::Image};
use crate::app::Data;
use crate::db;
use crate::events::chat_before;
use crate::hostility::{Line, Source, Utterance};
use crate::stats::UnratedRow;
use crate::voice::conversation::{MAX_LINES, WINDOW};

/// Rate every judged utterance that has no meanness yet, oldest first. Stops at the first Jev
/// failure, leaving the rest for the next start.
pub async fn rate_unrated(ctx: &serenity::Context, data: &Data) -> Result<()> {
    let Some(jev) = &data.jev else {
        return Ok(());
    };
    let rows = data.db.unrated().await?;
    if rows.is_empty() {
        return Ok(());
    }
    tracing::info!(n = rows.len(), "rating the meanness of earlier judgements");
    let mut names = Names::default();
    for row in &rows {
        let Some((u, images)) = rebuild(ctx, data, &mut names, row).await? else {
            continue;
        };
        let state = ai::state_for(&u, &images);
        let meanness = ai::rate_meanness(jev, state, images, u.source).await?;
        data.db.set_meanness(row.id, meanness).await?;
    }
    tracing::info!("rated the meanness of earlier judgements");
    Ok(())
}

/// What a judged log entry records about the utterance.
#[derive(Debug, Deserialize, PartialEq)]
struct Judged {
    /// Missing on the earliest entries, which were all chat.
    #[serde(default = "chat")]
    source: Source,
    said: String,
    message_id: Option<String>,
}

fn chat() -> Source {
    Source::Message
}

/// The utterance a judged entry is about, with its images. `None` if the entry doesn't say who
/// said what where.
async fn rebuild(
    ctx: &serenity::Context,
    data: &Data,
    names: &mut Names,
    row: &UnratedRow,
) -> Result<Option<(Utterance, Vec<Image>)>> {
    let Ok(judged) = serde_json::from_str::<Judged>(&row.data) else {
        return Ok(None);
    };
    let (Some(user), Some(channel), Some(at)) =
        (row.user_id, row.channel_id, db::parse_time(&row.at))
    else {
        return Ok(None);
    };
    let guild = GuildId::new(row.guild_id as u64);
    let user = UserId::new(user as u64);
    let mut u = Utterance {
        user,
        channel: ChannelId::new(channel as u64),
        source: judged.source,
        author: names.of(ctx, guild, user).await,
        said: judged.said,
        at,
        message: judged
            .message_id
            .and_then(|m| m.parse().ok())
            .map(MessageId::new),
        in_reply_to: None,
        recent: vec![],
        in_call: vec![],
    };
    let images = match u.source {
        Source::Voice => {
            voice_call(ctx, data, names, row, &mut u).await?;
            vec![]
        }
        _ => chat_message(ctx, &mut u).await,
    };
    Ok(Some((u, images)))
}

/// Fill in a chat utterance's conversation, what it replied to or reacted on, and its images, as
/// Discord still has them. A deleted message leaves just what was said.
async fn chat_message(ctx: &serenity::Context, u: &mut Utterance) -> Vec<Image> {
    let Some(id) = u.message else {
        return vec![];
    };
    if u.source != Source::Reaction {
        u.recent = chat_before(ctx, u.channel, id).await;
    }
    let Ok(msg) = u.channel.message(ctx, id).await else {
        return vec![];
    };
    match u.source {
        Source::Message => {
            u.in_reply_to = msg.referenced_message.as_ref().map(|r| r.content.clone());
            ai::message_images(&msg).await
        }
        Source::Reaction => {
            u.in_reply_to = Some(msg.content.clone());
            ai::message_images(&msg).await
        }
        Source::Edit | Source::Voice => vec![],
    }
}

/// Fill in a voice utterance's conversation and who was in the call, from what the log heard.
async fn voice_call(
    ctx: &serenity::Context,
    data: &Data,
    names: &mut Names,
    row: &UnratedRow,
    u: &mut Utterance,
) -> Result<()> {
    let since = db::fmt_time(u.at - WINDOW * 2);
    let heard: Vec<Heard> = data
        .db
        .heard_before(row.guild_id as u64, u.channel.get(), row.id, &since)
        .await?
        .into_iter()
        .filter_map(|(user, said, at)| {
            Some(Heard {
                user: UserId::new(user? as u64),
                said,
                at: db::parse_time(&at)?,
            })
        })
        .collect();
    let (at, before) = voice_context(&heard, u.user, &u.said, u.at);
    let guild = GuildId::new(row.guild_id as u64);
    u.at = at;
    for h in before {
        u.recent.push(Line {
            speaker: names.of(ctx, guild, h.user).await,
            said: h.said.clone(),
            at: h.at,
        });
    }
    u.in_call = in_call(&u.author, &u.recent);
    Ok(())
}

/// A transcript from the activity log.
#[derive(Clone, Debug, PartialEq)]
struct Heard {
    user: UserId,
    said: String,
    at: DateTime<Utc>,
}

/// When a voice line was said and what came just before it, as the voice monitor would have
/// given it: from what was heard before its verdict (newest first, its own line among them),
/// the lines in the [`WINDOW`] before it, at most [`MAX_LINES`], oldest first. If its own line
/// isn't there, it's dated `judged_at`.
fn voice_context<'a>(
    heard: &'a [Heard],
    user: UserId,
    said: &str,
    judged_at: DateTime<Utc>,
) -> (DateTime<Utc>, Vec<&'a Heard>) {
    let own = heard.iter().position(|h| h.user == user && h.said == said);
    let (at, earlier) = match own {
        Some(i) => (heard[i].at, &heard[i + 1..]),
        None => (judged_at, heard),
    };
    let mut before: Vec<_> = earlier
        .iter()
        .take_while(|h| at - h.at <= WINDOW)
        .take(MAX_LINES)
        .collect();
    before.reverse();
    (at, before)
}

/// Who was in the call, as far as anyone knows now: the speaker and whoever spoke just before.
fn in_call(speaker: &str, recent: &[Line]) -> Vec<String> {
    let mut names = vec![speaker.to_owned()];
    for l in recent {
        if !names.contains(&l.speaker) {
            names.push(l.speaker.clone());
        }
    }
    names
}

/// Display names, each looked up once.
#[derive(Default)]
struct Names(HashMap<(GuildId, UserId), String>);

impl Names {
    /// `user`'s display name in `guild`, or their id if Discord won't say.
    async fn of(&mut self, ctx: &serenity::Context, guild: GuildId, user: UserId) -> String {
        if let Some(n) = self.0.get(&(guild, user)) {
            return n.clone();
        }
        let name = match guild.member(ctx, user).await {
            Ok(m) => m.display_name().to_owned(),
            Err(_) => user.to_string(),
        };
        self.0.insert((guild, user), name.clone());
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    fn heard(user: u64, said: &str, secs_ago: i64) -> Heard {
        Heard {
            user: UserId::new(user),
            said: said.into(),
            at: DateTime::UNIX_EPOCH + TimeDelta::seconds(1000 - secs_ago),
        }
    }

    fn saids<'a>(lines: &[&'a Heard]) -> Vec<&'a str> {
        lines.iter().map(|h| h.said.as_str()).collect()
    }

    #[test]
    fn judged_entries_say_what_was_said_where() {
        let j: Judged = serde_json::from_str(
            r#"{"source":"voice","said":"shut up","score":0.9,"message_id":null}"#,
        )
        .unwrap();
        assert_eq!(
            j,
            Judged {
                source: Source::Voice,
                said: "shut up".into(),
                message_id: None
            }
        );
        let old: Judged = serde_json::from_str(r#"{"said":"hi","message_id":"5"}"#).unwrap();
        assert_eq!(
            (old.source, old.message_id),
            (Source::Message, Some("5".into()))
        );
        assert!(serde_json::from_str::<Judged>(r#"{"score":0.1}"#).is_err());
    }

    #[test]
    fn voice_context_is_what_came_before_the_line() {
        let log = [
            heard(8, "after it", 1),
            heard(7, "you're useless", 5),
            heard(8, "I carried that round", 10),
            heard(9, "nice", 30),
            heard(9, "way earlier", 120),
        ];
        let judged_at = DateTime::UNIX_EPOCH + TimeDelta::seconds(1000);
        let (at, before) = voice_context(&log[1..], UserId::new(7), "you're useless", judged_at);
        assert_eq!(at, log[1].at, "dated when it was heard");
        assert_eq!(saids(&before), ["nice", "I carried that round"]);

        let (at, before) = voice_context(&log, UserId::new(7), "not logged", judged_at);
        assert_eq!(at, judged_at);
        assert_eq!(
            saids(&before),
            ["nice", "I carried that round", "you're useless", "after it"]
        );
    }

    #[test]
    fn voice_context_keeps_the_latest_lines() {
        let log: Vec<_> = (1..=MAX_LINES as i64 + 2)
            .map(|i| heard(8, &i.to_string(), i))
            .collect();
        let judged_at = DateTime::UNIX_EPOCH + TimeDelta::seconds(1000);
        let (_, before) = voice_context(&log, UserId::new(7), "x", judged_at);
        assert_eq!(before.len(), MAX_LINES);
        assert_eq!(before.last().unwrap().said, "1", "newest last");
    }

    #[test]
    fn in_call_is_the_speaker_and_who_spoke() {
        let line = |speaker: &str| Line {
            speaker: speaker.into(),
            said: "x".into(),
            at: DateTime::UNIX_EPOCH,
        };
        assert_eq!(
            in_call(
                "Sam",
                &[line("Alex"), line("Sam"), line("Alex"), line("Jo")]
            ),
            ["Sam", "Alex", "Jo"]
        );
        assert_eq!(in_call("Sam", &[]), ["Sam"]);
    }
}
