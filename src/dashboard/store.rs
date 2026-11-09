//! What the dashboard reads from the activity log: everyone in it, and each person's entries.

use std::collections::HashMap;
use std::fmt;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::db::Db;
use crate::stats::LogRow;

/// Whose entries: one user's, or the server-wide ones that have no user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    User(u64),
    Server,
}

impl Who {
    /// A user id, or `server`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "server" => Some(Self::Server),
            _ => s.parse().ok().filter(|u| *u != 0).map(Self::User),
        }
    }

    /// Whose a row is, from its `user_id` column: NULL for server-wide entries.
    pub fn of_row(user_id: Option<i64>) -> Self {
        user_id.map_or(Self::Server, |u| Self::User(u as u64))
    }
}

/// Several people as SQL parameters: (their user ids as a JSON array, whether server-wide is one).
/// The queries match them with `user_id IN (SELECT value FROM json_each(?)) OR (? AND user_id IS NULL)`.
fn people_params(people: &[Who]) -> (String, bool) {
    let users: Vec<i64> = people
        .iter()
        .filter_map(|w| match w {
            Who::User(u) => Some(*u as i64),
            Who::Server => None,
        })
        .collect();
    (
        Value::from(users).to_string(),
        people.contains(&Who::Server),
    )
}

impl fmt::Display for Who {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User(u) => write!(f, "{u}"),
            Self::Server => f.write_str("server"),
        }
    }
}

/// What sort of thing an entry is, for the dashboard's filters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    /// Things they said or did to messages: messages, edits, deletes, speech, reactions, threads.
    Said,
    /// Judging and timeouts.
    Moderation,
    /// Voice joins, leaves and time spent, and the bot's own voice monitor.
    Voice,
    /// Status, typing, commands, and member records.
    Presence,
    /// Any kind not listed in the others.
    Other,
}

/// The kinds in each group but `Other`, which is every kind not listed here.
const GROUPED: [(Group, &[&str]); 4] = [
    (
        Group::Said,
        &[
            "message",
            "message_edit",
            "message_delete",
            "message_delete_bulk",
            "heard",
            "reaction_add",
            "reaction_remove",
            "thread_create",
        ],
    ),
    (
        Group::Moderation,
        &[
            "judged",
            "judge_failed",
            "hostile",
            "timed_out",
            "timeout_released",
            "timeout_message",
            "timeout_reaction",
            "timeout_vc",
            "pardoned",
        ],
    ),
    (
        Group::Voice,
        &[
            "voice",
            "vc_session",
            "monitor_joined",
            "monitor_left",
            "monitor_failed",
        ],
    ),
    (
        Group::Presence,
        &[
            "presence",
            "typing",
            "command",
            "member_seen",
            "member_join",
            "member_leave",
            "member_update",
        ],
    ),
];

impl Group {
    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Said,
            Self::Moderation,
            Self::Voice,
            Self::Presence,
            Self::Other,
        ]
        .into_iter()
        .find(|g| g.name() == s)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Said => "said",
            Self::Moderation => "moderation",
            Self::Voice => "voice",
            Self::Presence => "presence",
            Self::Other => "other",
        }
    }

    pub fn of(kind: &str) -> Self {
        GROUPED
            .iter()
            .find(|(_, kinds)| kinds.contains(&kind))
            .map_or(Self::Other, |(g, _)| *g)
    }
}

/// Which of some people's entries to show, newest first, all of them mixed together.
#[derive(Clone, Debug, PartialEq)]
pub struct LogQuery {
    /// Whose entries; none matches nothing.
    pub people: Vec<Who>,
    /// Only these groups; empty means all of them.
    pub groups: Vec<Group>,
    /// Only entries whose kind or payload contains this, ignoring ASCII case.
    pub text: Option<String>,
    /// Only entries older than this id (the last one on the page before).
    pub before: Option<i64>,
    pub limit: i64,
}

/// The group filter as SQL parameters: (no filter, kinds wanted, other wanted, kinds not other).
fn group_params(groups: &[Group]) -> (bool, String, bool, String) {
    let wanted: Vec<&str> = GROUPED
        .iter()
        .filter(|(g, _)| groups.contains(g))
        .flat_map(|(_, kinds)| kinds.iter().copied())
        .collect();
    let known: Vec<&str> = GROUPED
        .iter()
        .flat_map(|(_, k)| k.iter().copied())
        .collect();
    (
        groups.is_empty(),
        Value::from(wanted).to_string(),
        groups.contains(&Group::Other),
        Value::from(known).to_string(),
    )
}

/// One person's totals, from the log.
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct Person {
    pub user_id: Option<i64>,
    pub total: i64,
    /// Messages sent and things heard said in voice.
    pub said: i64,
    pub judged: i64,
    pub avg_score: Option<f64>,
    pub hostile: i64,
    pub timeouts: i64,
    /// Whether they held the participant role, as of their latest entry that says.
    pub participant: Option<bool>,
    pub first: String,
    pub last: String,
}

impl Person {
    pub fn who(&self) -> Who {
        Who::of_row(self.user_id)
    }
}

/// How many of one person's entries fall on a day, and how many of those were hostile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct DayCount {
    pub day: String,
    pub n: i64,
    pub hostile: i64,
}

/// The name a logged member payload gives its user: nick, else display name, else username.
pub fn name_in(kind: &str, data: &Value) -> Option<String> {
    let (member, user) = match kind {
        "member_seen" | "member_join" => (data, &data["user"]),
        "member_update" => (&data["new"], &data["new"]["user"]),
        "member_leave" => (&data["member"], &data["user"]),
        "message" => (&data["member"], &data["author"]),
        _ => return None,
    };
    [&member["nick"], &user["global_name"], &user["username"]]
        .into_iter()
        .find_map(|v| v.as_str().filter(|s| !s.is_empty()))
        .map(str::to_owned)
}

impl Db {
    /// Everyone in a guild's log with their totals, busiest first, server-wide entries last.
    pub async fn people(&self, guild_id: u64) -> Result<Vec<Person>> {
        Ok(sqlx::query_as(
            "SELECT user_id,
                COUNT(*) AS total,
                SUM(kind IN ('message', 'heard')) AS said,
                SUM(kind = 'judged') AS judged,
                AVG(CASE WHEN kind = 'judged' THEN json_extract(data, '$.score') END) AS avg_score,
                SUM(kind = 'hostile') AS hostile,
                SUM(kind = 'timed_out') AS timeouts,
                (SELECT b.participant FROM activity_log b
                 WHERE b.guild_id = a.guild_id AND b.user_id IS a.user_id
                   AND b.participant IS NOT NULL
                 ORDER BY b.id DESC LIMIT 1) AS participant,
                MIN(at) AS first,
                MAX(at) AS last
             FROM activity_log a WHERE guild_id = ?
             GROUP BY user_id
             ORDER BY user_id IS NULL, total DESC, user_id",
        )
        .bind(guild_id as i64)
        .fetch_all(&self.0)
        .await?)
    }

    /// Names from the log, for people the bot can't see any more: user id -> latest name.
    pub async fn logged_names(&self, guild_id: u64) -> Result<HashMap<u64, String>> {
        // The kinds `name_in` reads a name from.
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT user_id, kind, data FROM activity_log WHERE id IN (
                SELECT MAX(id) FROM activity_log
                WHERE guild_id = ? AND user_id IS NOT NULL
                  AND kind IN ('member_seen', 'member_join', 'member_update', 'member_leave', 'message')
                GROUP BY user_id)",
        )
        .bind(guild_id as i64)
        .fetch_all(&self.0)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(user, kind, data)| {
                let data = serde_json::from_str(&data).ok()?;
                Some((user as u64, name_in(&kind, &data)?))
            })
            .collect())
    }

    /// Some people's entries, newest first, filtered and paged by `q`.
    pub async fn person_log(&self, guild_id: u64, q: &LogQuery) -> Result<Vec<LogRow>> {
        let (all, wanted, other, known) = group_params(&q.groups);
        let (users, server) = people_params(&q.people);
        Ok(sqlx::query_as(
            "SELECT id, user_id, channel_id, kind, participant, data, day, at FROM activity_log
             WHERE guild_id = ?1
               AND (user_id IN (SELECT value FROM json_each(?2)) OR (?10 AND user_id IS NULL))
               AND (?3 IS NULL OR id < ?3)
               AND (?4 OR kind IN (SELECT value FROM json_each(?5))
                    OR (?6 AND kind NOT IN (SELECT value FROM json_each(?7))))
               AND (?8 IS NULL OR instr(lower(kind || ' ' || data), lower(?8)) > 0)
             ORDER BY id DESC LIMIT ?9",
        )
        .bind(guild_id as i64)
        .bind(users)
        .bind(q.before)
        .bind(all)
        .bind(wanted)
        .bind(other)
        .bind(known)
        .bind(q.text.as_deref())
        .bind(q.limit)
        .bind(server)
        .fetch_all(&self.0)
        .await?)
    }

    /// How many entries some people have per day between them, oldest day first.
    pub async fn person_days(&self, guild_id: u64, people: &[Who]) -> Result<Vec<DayCount>> {
        let (users, server) = people_params(people);
        Ok(sqlx::query_as(
            "SELECT day, COUNT(*) AS n, SUM(kind = 'hostile') AS hostile FROM activity_log
             WHERE guild_id = ?1
               AND (user_id IN (SELECT value FROM json_each(?2)) OR (?3 AND user_id IS NULL))
             GROUP BY day ORDER BY day",
        )
        .bind(guild_id as i64)
        .bind(users)
        .bind(server)
        .fetch_all(&self.0)
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::Log;
    use chrono::NaiveDate;
    use poise::serenity_prelude::UserId;
    use serde_json::json;

    async fn db_with(entries: &[(u64, Option<u64>, &'static str, &str, Value)]) -> Db {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        for (guild, user, kind, day, data) in entries {
            let mut log = Log::new(kind, data);
            if let Some(u) = user {
                log = log.user(UserId::new(*u)).participant(Some(*u != 2));
            }
            let day: NaiveDate = day.parse().unwrap();
            db.log_activity(*guild, &log, day).await.unwrap();
        }
        db
    }

    fn query(who: Who) -> LogQuery {
        LogQuery {
            people: vec![who],
            groups: vec![],
            text: None,
            before: None,
            limit: 100,
        }
    }

    #[test]
    fn who_and_group_parse_from_the_url() {
        assert_eq!(Who::parse("123"), Some(Who::User(123)));
        assert_eq!(Who::parse("server"), Some(Who::Server));
        assert_eq!(Who::parse("bob"), None);
        assert_eq!(Who::parse("0"), None, "not a Discord id");
        assert_eq!(Who::User(5).to_string(), "5");
        assert_eq!(Who::Server.to_string(), "server");

        for g in ["said", "moderation", "voice", "presence", "other"] {
            assert_eq!(Group::parse(g).map(Group::name), Some(g));
        }
        assert_eq!(Group::parse("nope"), None);
    }

    #[test]
    fn every_kind_has_a_group() {
        assert_eq!(Group::of("message"), Group::Said);
        assert_eq!(Group::of("heard"), Group::Said);
        assert_eq!(Group::of("hostile"), Group::Moderation);
        assert_eq!(Group::of("vc_session"), Group::Voice);
        assert_eq!(Group::of("member_update"), Group::Presence);
        assert_eq!(Group::of("something_new"), Group::Other);
    }

    #[test]
    fn names_from_logged_members_and_messages() {
        let user = json!({"username": "al", "global_name": "Al"});
        let name = |kind, data: Value| name_in(kind, &data);
        assert_eq!(
            name("member_seen", json!({"nick": null, "user": user})).as_deref(),
            Some("Al")
        );
        assert_eq!(
            name("member_join", json!({"nick": "Big Al", "user": user})).as_deref(),
            Some("Big Al")
        );
        assert_eq!(
            name(
                "member_update",
                json!({"new": {"nick": null, "user": {"username": "al"}}})
            )
            .as_deref(),
            Some("al")
        );
        assert_eq!(
            name("member_leave", json!({"user": user, "member": null})).as_deref(),
            Some("Al")
        );
        assert_eq!(
            name(
                "message",
                json!({"author": {"username": "bo", "global_name": ""}, "member": {"nick": null}})
            )
            .as_deref(),
            Some("bo")
        );
        assert_eq!(name("typing", json!({"user": user})), None);
        assert_eq!(name("member_seen", json!({})), None);
    }

    #[tokio::test]
    async fn people_are_summed_per_user_busiest_first_server_last() {
        let db = db_with(&[
            (1, Some(1), "message", "2026-11-01", json!({})),
            (1, Some(1), "judged", "2026-11-01", json!({"score": 0.2})),
            (1, Some(1), "judged", "2026-11-02", json!({"score": 0.8})),
            (1, Some(1), "hostile", "2026-11-02", json!({})),
            (1, Some(1), "timed_out", "2026-11-02", json!({"secs": 10})),
            (1, Some(2), "heard", "2026-11-03", json!({})),
            (1, None, "monitor_joined", "2026-11-03", json!({})),
            (1, None, "monitor_left", "2026-11-03", json!({})),
            (1, None, "monitor_left", "2026-11-03", json!({})),
            (2, Some(1), "message", "2026-11-03", json!({})),
        ])
        .await;
        let people = db.people(1).await.unwrap();
        assert_eq!(
            people.iter().map(Person::who).collect::<Vec<_>>(),
            [Who::User(1), Who::User(2), Who::Server],
            "busiest first, server-wide last even with more entries"
        );
        let al = &people[0];
        assert_eq!(
            (al.total, al.said, al.judged, al.hostile, al.timeouts),
            (5, 1, 2, 1, 1)
        );
        assert_eq!(al.avg_score, Some(0.5));
        assert_eq!(al.participant, Some(true));
        let rows = db.activity(1, 0, 100).await.unwrap();
        let al_at: Vec<_> = rows
            .iter()
            .filter(|r| r.user_id == Some(1))
            .map(|r| &r.at)
            .collect();
        assert_eq!((&al.first, &al.last), (al_at[0], al_at[4]));
        assert_eq!((people[1].said, people[1].participant), (1, Some(false)));
        assert_eq!((people[2].total, people[2].participant), (3, None));
        assert!(db.people(3).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn logged_names_are_the_latest_naming_record() {
        let db = db_with(&[
            (
                1,
                Some(1),
                "member_seen",
                "2026-11-01",
                json!({"user": {"username": "al"}}),
            ),
            (
                1,
                Some(1),
                "member_update",
                "2026-11-02",
                json!({"new": {"nick": "Big Al", "user": {}}}),
            ),
            (1, Some(1), "typing", "2026-11-03", json!({})),
            (1, Some(3), "typing", "2026-11-03", json!({})),
            (
                2,
                Some(4),
                "member_seen",
                "2026-11-01",
                json!({"user": {"username": "elsewhere"}}),
            ),
        ])
        .await;
        let names = db.logged_names(1).await.unwrap();
        assert_eq!(names, HashMap::from([(1, "Big Al".to_owned())]));
    }

    #[tokio::test]
    async fn person_log_filters_by_person_group_and_text_newest_first() {
        let db = db_with(&[
            (
                1,
                Some(1),
                "message",
                "2026-11-01",
                json!({"content": "good game"}),
            ),
            (
                1,
                Some(1),
                "heard",
                "2026-11-01",
                json!({"text": "GG everyone"}),
            ),
            (
                1,
                Some(1),
                "presence",
                "2026-11-01",
                json!({"status": "online"}),
            ),
            (1, Some(1), "brand_new_kind", "2026-11-01", json!({})),
            (
                1,
                Some(2),
                "message",
                "2026-11-01",
                json!({"content": "gg"}),
            ),
            (1, None, "monitor_left", "2026-11-01", json!({})),
            (
                2,
                Some(1),
                "message",
                "2026-11-01",
                json!({"content": "gg"}),
            ),
        ])
        .await;
        let kinds = |q: LogQuery| {
            let db = db.clone();
            async move {
                let rows = db.person_log(1, &q).await.unwrap();
                rows.into_iter().map(|r| r.kind).collect::<Vec<_>>()
            }
        };
        let al = query(Who::User(1));
        assert_eq!(
            kinds(al.clone()).await,
            ["brand_new_kind", "presence", "heard", "message"]
        );
        let said = LogQuery {
            groups: vec![Group::Said],
            ..al.clone()
        };
        assert_eq!(kinds(said).await, ["heard", "message"]);
        let other_and_presence = LogQuery {
            groups: vec![Group::Other, Group::Presence],
            ..al.clone()
        };
        assert_eq!(
            kinds(other_and_presence).await,
            ["brand_new_kind", "presence"]
        );
        let gg = LogQuery {
            text: Some("gG".into()),
            ..al.clone()
        };
        assert_eq!(kinds(gg).await, ["heard"]);
        let by_kind = LogQuery {
            text: Some("PRESENCE".into()),
            ..al.clone()
        };
        assert_eq!(kinds(by_kind).await, ["presence"]);
        assert_eq!(kinds(query(Who::Server)).await, ["monitor_left"]);
    }

    #[tokio::test]
    async fn a_log_of_several_people_interleaves_them_newest_first() {
        let db = db_with(&[
            (
                1,
                Some(1),
                "message",
                "2026-11-01",
                json!({"content": "you there?"}),
            ),
            (
                1,
                Some(2),
                "message",
                "2026-11-01",
                json!({"content": "yep"}),
            ),
            (
                1,
                Some(3),
                "message",
                "2026-11-01",
                json!({"content": "not asked"}),
            ),
            (
                1,
                Some(1),
                "message",
                "2026-11-01",
                json!({"content": "cool"}),
            ),
            (1, None, "monitor_left", "2026-11-01", json!({})),
        ])
        .await;
        let ids = |people: Vec<Who>| {
            let db = db.clone();
            async move {
                let q = LogQuery {
                    people,
                    ..query(Who::User(1))
                };
                let rows = db.person_log(1, &q).await.unwrap();
                rows.into_iter().map(|r| r.id).collect::<Vec<_>>()
            }
        };
        assert_eq!(ids(vec![Who::User(1), Who::User(2)]).await, [4, 2, 1]);
        assert_eq!(ids(vec![Who::User(2), Who::Server]).await, [5, 2]);
        assert!(ids(vec![]).await.is_empty());
        assert_eq!(
            db.person_days(1, &[Who::User(1), Who::User(2)])
                .await
                .unwrap(),
            [DayCount {
                day: "2026-11-01".into(),
                n: 3,
                hostile: 0
            }]
        );
    }

    #[tokio::test]
    async fn person_log_pages_with_before() {
        let entries: Vec<_> = (0..5)
            .map(|i| (1, Some(1), "message", "2026-11-01", json!({ "n": i })))
            .collect();
        let db = db_with(&entries).await;
        let page = |before| {
            let db = db.clone();
            async move {
                let q = LogQuery {
                    before,
                    limit: 2,
                    ..query(Who::User(1))
                };
                let rows = db.person_log(1, &q).await.unwrap();
                rows.into_iter().map(|r| r.id).collect::<Vec<_>>()
            }
        };
        assert_eq!(page(None).await, [5, 4]);
        assert_eq!(page(Some(4)).await, [3, 2]);
        assert_eq!(page(Some(2)).await, [1]);
        assert!(page(Some(1)).await.is_empty());
    }

    #[tokio::test]
    async fn person_days_count_entries_and_hostile_per_day() {
        let db = db_with(&[
            (1, Some(1), "message", "2026-11-02", json!({})),
            (1, Some(1), "hostile", "2026-11-02", json!({})),
            (1, Some(1), "message", "2026-11-01", json!({})),
            (1, Some(2), "message", "2026-11-01", json!({})),
            (1, None, "monitor_left", "2026-11-03", json!({})),
        ])
        .await;
        let day = |day: &str, n, hostile| DayCount {
            day: day.into(),
            n,
            hostile,
        };
        assert_eq!(
            db.person_days(1, &[Who::User(1)]).await.unwrap(),
            [day("2026-11-01", 1, 0), day("2026-11-02", 2, 1)]
        );
        assert_eq!(
            db.person_days(1, &[Who::Server]).await.unwrap(),
            [day("2026-11-03", 1, 0)]
        );
    }
}
