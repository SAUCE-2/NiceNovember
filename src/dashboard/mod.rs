//! A web dashboard of the activity log, person by person, served by the bot straight from its
//! database, so there's nothing to export or upload.
//!
//! `/log dashboard` gives an admin a link with a key for their server ([`keys`]). The page
//! (`dashboard/` at the repo root, built into the binary) sends that key with each API call:
//!
//! | route | what |
//! |---|---|
//! | `GET /api/people` | the server's name, and everyone in its log with their totals |
//! | `GET /api/people/{who}/log?groups=&q=&before=` | one person's entries, newest first, a page at a time |
//! | `GET /api/people/{who}/days` | how many entries (and hostile ones) they have each day |
//!
//! `{who}` is a user id, or `server` for server-wide entries.

pub mod keys;
pub mod store;

use std::net::SocketAddr;
use std::num::NonZeroU64;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use poise::serenity_prelude::{self as serenity, ChannelId, GuildId, UserId};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::Db;
use crate::stats::LogRow;
use store::{Group, LogQuery, Person, Who};

/// Entries per page of someone's log.
const PAGE: i64 = 100;

/// The link an admin opens: the key goes after `#`, so it's never sent to the server in the URL.
pub fn link(base_url: &str, key: &str) -> String {
    format!("{}/#key={key}", base_url.trim_end_matches('/'))
}

/// Names the bot can see right now (Discord's cache), so the dashboard shows names, not ids.
pub trait Directory: Send + Sync {
    fn guild_name(&self, guild: u64) -> Option<String>;
    fn member_name(&self, guild: u64, user: u64) -> Option<String>;
    fn channel_name(&self, guild: u64, channel: u64) -> Option<String>;
}

impl Directory for serenity::Cache {
    fn guild_name(&self, guild: u64) -> Option<String> {
        let g = self.guild(GuildId::from(NonZeroU64::new(guild)?))?;
        Some(g.name.clone())
    }

    fn member_name(&self, guild: u64, user: u64) -> Option<String> {
        let g = self.guild(GuildId::from(NonZeroU64::new(guild)?))?;
        let m = g.members.get(&UserId::from(NonZeroU64::new(user)?))?;
        Some(m.display_name().to_owned())
    }

    fn channel_name(&self, guild: u64, channel: u64) -> Option<String> {
        let g = self.guild(GuildId::from(NonZeroU64::new(guild)?))?;
        let id = ChannelId::from(NonZeroU64::new(channel)?);
        let from_threads = || g.threads.iter().find(|t| t.id == id);
        let c = g.channels.get(&id).or_else(from_threads)?;
        Some(c.name.clone())
    }
}

#[derive(Clone)]
struct Dash {
    db: Db,
    names: Arc<dyn Directory>,
}

/// Listen on `port` until the process ends. Never fails: the bot runs on without its dashboard.
pub async fn serve(port: u16, db: Db, names: Arc<dyn Directory>) {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("dashboard can't listen on port {port}: {e}");
            return;
        }
    };
    tracing::info!("dashboard on port {port}");
    if let Err(e) = axum::serve(listener, router(db, names)).await {
        tracing::warn!("dashboard stopped: {e}");
    }
}

fn router(db: Db, names: Arc<dyn Directory>) -> Router {
    Router::new()
        .route(
            "/",
            get(page(
                "text/html",
                include_str!("../../dashboard/index.html"),
            )),
        )
        .route(
            "/style.css",
            get(page("text/css", include_str!("../../dashboard/style.css"))),
        )
        .route(
            "/logs.js",
            get(page(
                "text/javascript",
                include_str!("../../dashboard/logs.js"),
            )),
        )
        .route(
            "/app.js",
            get(page(
                "text/javascript",
                include_str!("../../dashboard/app.js"),
            )),
        )
        .route("/api/people", get(people))
        .route("/api/people/{who}/log", get(person_log))
        .route("/api/people/{who}/days", get(person_days))
        .layer(axum::middleware::map_response(lock_down))
        .with_state(Dash { db, names })
}

/// A static file of the page.
fn page(
    content_type: &'static str,
    body: &'static str,
) -> impl Fn() -> std::future::Ready<Response> + Clone {
    move || std::future::ready(([(header::CONTENT_TYPE, content_type)], body).into_response())
}

/// The page shows message content from the whole server: load nothing from elsewhere but Discord's
/// images, don't let other sites frame it, and don't cache any of it.
async fn lock_down(mut res: Response) -> Response {
    let h = res.headers_mut();
    let set = [
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; img-src 'self' https://cdn.discordapp.com \
             https://media.discordapp.net; frame-ancestors 'none'",
        ),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::CACHE_CONTROL, "no-store"),
    ];
    for (name, value) in set {
        h.insert(name, HeaderValue::from_static(value));
    }
    res
}

/// Someone holding a valid key, and the guild it opens.
struct Viewer {
    guild: u64,
}

impl FromRequestParts<Dash> for Viewer {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, dash: &Dash) -> Result<Self, ApiError> {
        let key = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(ApiError::Unauthorized)?;
        let guild = dash.db.dashboard_guild(key, chrono::Utc::now()).await?;
        guild
            .map(|guild| Viewer { guild })
            .ok_or(ApiError::Unauthorized)
    }
}

enum ApiError {
    /// No key, or one that's wrong or expired.
    Unauthorized,
    BadRequest(&'static str),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "this link has expired or isn't right: run /log dashboard for a new one",
            ),
            Self::BadRequest(why) => (StatusCode::BAD_REQUEST, why),
            Self::Internal(e) => {
                tracing::warn!("dashboard: {e:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "something went wrong reading the log",
                )
            }
        };
        (status, Json(json!({ "error": error }))).into_response()
    }
}

/// The server's name, and everyone in its log.
async fn people(viewer: Viewer, State(dash): State<Dash>) -> Result<Json<Value>, ApiError> {
    let g = viewer.guild;
    let people = dash.db.people(g).await?;
    let logged = dash.db.logged_names(g).await?;
    let name = |who| match who {
        Who::Server => "Server-wide".to_owned(),
        Who::User(u) => dash
            .names
            .member_name(g, u)
            .or_else(|| logged.get(&u).cloned())
            .unwrap_or_else(|| format!("User {u}")),
    };
    let people: Vec<Value> = people
        .iter()
        .map(|p| person_json(p, &name(p.who())))
        .collect();
    Ok(Json(json!({
        "guild": dash.names.guild_name(g).unwrap_or_else(|| format!("Server {g}")),
        "people": people,
    })))
}

fn person_json(p: &Person, name: &str) -> Value {
    json!({
        "id": p.who().to_string(),
        "name": name,
        "total": p.total,
        "said": p.said,
        "judged": p.judged,
        "avg_score": p.avg_score,
        "hostile": p.hostile,
        "timeouts": p.timeouts,
        "participant": p.participant,
        "first": p.first,
        "last": p.last,
    })
}

/// A page of someone's log, as the URL asks for it.
#[derive(Deserialize)]
struct LogParams {
    /// Comma-separated groups; none means all.
    groups: Option<String>,
    q: Option<String>,
    before: Option<i64>,
}

fn who(path: &str) -> Result<Who, ApiError> {
    Who::parse(path).ok_or(ApiError::BadRequest("no such person"))
}

impl LogParams {
    fn into_query(self, who: Who) -> Result<LogQuery, ApiError> {
        let groups = self.groups.unwrap_or_default();
        let groups = groups
            .split(',')
            .filter(|g| !g.is_empty())
            .map(|g| Group::parse(g).ok_or(ApiError::BadRequest("no such group")))
            .collect::<Result<_, _>>()?;
        let text = self
            .q
            .map(|q| q.trim().to_owned())
            .filter(|q| !q.is_empty());
        Ok(LogQuery {
            who,
            groups,
            text,
            before: self.before,
            // One more than a page, to know whether there's more.
            limit: PAGE + 1,
        })
    }
}

async fn person_log(
    viewer: Viewer,
    State(dash): State<Dash>,
    Path(path): Path<String>,
    Query(params): Query<LogParams>,
) -> Result<Json<Value>, ApiError> {
    let q = params.into_query(who(&path)?)?;
    let mut rows = dash.db.person_log(viewer.guild, &q).await?;
    let more = rows.len() as i64 > PAGE;
    rows.truncate(PAGE as usize);
    let channel = |c: i64| dash.names.channel_name(viewer.guild, c as u64);
    let entries: Vec<Value> = rows.iter().map(|r| entry_json(r, channel)).collect();
    Ok(Json(json!({ "entries": entries, "more": more })))
}

fn entry_json(r: &LogRow, channel_name: impl Fn(i64) -> Option<String>) -> Value {
    let data: Value = serde_json::from_str(&r.data).unwrap_or(Value::String(r.data.clone()));
    json!({
        "id": r.id,
        "at": r.at,
        "day": r.day,
        "kind": r.kind,
        "group": Group::of(&r.kind),
        // As strings: snowflakes don't fit in a JS number.
        "channel_id": r.channel_id.map(|c| c.to_string()),
        "channel": r.channel_id.and_then(channel_name),
        "participant": r.participant,
        "data": data,
    })
}

async fn person_days(
    viewer: Viewer,
    State(dash): State<Dash>,
    Path(path): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let days = dash.db.person_days(viewer.guild, who(&path)?).await?;
    Ok(Json(json!(days)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::Log;
    use axum::body::Body;
    use axum::http::Request;
    use std::collections::HashMap;
    use tower::ServiceExt;

    /// Discord's cache in miniature: names by id.
    struct Names(HashMap<u64, &'static str>);

    impl Directory for Names {
        fn guild_name(&self, guild: u64) -> Option<String> {
            self.0.get(&guild).map(|s| s.to_string())
        }
        fn member_name(&self, _: u64, user: u64) -> Option<String> {
            self.0.get(&user).map(|s| s.to_string())
        }
        fn channel_name(&self, _: u64, channel: u64) -> Option<String> {
            self.0.get(&channel).map(|s| s.to_string())
        }
    }

    const GUILD: u64 = 1000;

    /// A dashboard over a log with Al (1, cached), Bo (2, left: named by the log), Cy (3, nameless)
    /// and a server-wide entry; and a key for it.
    async fn dashboard() -> (Router, String) {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let day = "2026-11-05".parse().unwrap();
        let al = || Log::new("message", json!({"content": "hi"})).user(UserId::new(1));
        let entries = [
            al().channel(ChannelId::new(77)),
            al(),
            Log::new("hostile", json!({"said": "ugh", "score": 0.9})).user(UserId::new(1)),
            Log::new("member_leave", json!({"user": {"username": "bo"}})).user(UserId::new(2)),
            Log::new("typing", json!({})).user(UserId::new(3)),
            Log::new("monitor_joined", json!({})),
        ];
        for log in &entries {
            db.log_activity(GUILD, log, day).await.unwrap();
        }
        // Another server's entries never show.
        db.log_activity(2000, &al(), day).await.unwrap();
        let key = db
            .new_dashboard_key(GUILD, chrono::Utc::now())
            .await
            .unwrap();
        let names = Names(HashMap::from([
            (1, "Al"),
            (GUILD, "Nice Club"),
            (77, "general"),
        ]));
        (router(db, Arc::new(names)), key)
    }

    async fn get(app: &Router, uri: &str, key: Option<&str>) -> (StatusCode, Response) {
        let mut req = Request::get(uri);
        if let Some(k) = key {
            req = req.header("authorization", format!("Bearer {k}"));
        }
        let res = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        (res.status(), res)
    }

    async fn get_json(app: &Router, uri: &str, key: &str) -> (StatusCode, Value) {
        let (status, res) = get(app, uri, Some(key)).await;
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    /// One guild as Discord sends it when the bot connects.
    const GUILD_CREATE: &str = r#"{
        "id": "1000", "name": "Nice Club", "icon": null, "splash": null,
        "discovery_splash": null, "owner_id": "1", "afk_channel_id": null, "afk_timeout": 300,
        "verification_level": 0, "default_message_notifications": 0,
        "explicit_content_filter": 0, "roles": [], "emojis": [], "features": [],
        "mfa_level": 0, "application_id": null, "system_channel_id": null,
        "system_channel_flags": 0, "rules_channel_id": null, "vanity_url_code": null,
        "description": null, "banner": null, "premium_tier": 0, "preferred_locale": "en-US",
        "public_updates_channel_id": null, "nsfw_level": 0, "premium_progress_bar_enabled": false,
        "joined_at": "2026-11-01T00:00:00Z", "large": false, "member_count": 1,
        "voice_states": [], "presences": [], "stage_instances": [],
        "guild_scheduled_events": [], "stickers": [],
        "members": [{
            "user": {"id": "1", "username": "al", "global_name": "Al", "discriminator": "0",
                     "avatar": null},
            "nick": "Big Al", "roles": [], "joined_at": "2026-11-01T00:00:00Z",
            "deaf": false, "mute": false, "flags": 0
        }],
        "channels": [{"id": "77", "type": 0, "name": "general", "position": 0,
                      "permission_overwrites": []}],
        "threads": [{"id": "78", "type": 11, "name": "a thread", "parent_id": "77",
                     "guild_id": "1000",
                     "thread_metadata": {"archived": false, "auto_archive_duration": 60,
                                         "locked": false}}]
    }"#;

    /// Discord's real cache, holding one guild as Discord sends it.
    fn discord_cache() -> serenity::Cache {
        let cache = serenity::Cache::new();
        let mut event: serenity::GuildCreateEvent = serde_json::from_str(GUILD_CREATE).unwrap();
        cache.update(&mut event);
        cache
    }

    #[test]
    fn names_come_from_discords_cache() {
        let cache = discord_cache();
        let names: &dyn Directory = &cache;
        assert_eq!(names.guild_name(GUILD).as_deref(), Some("Nice Club"));
        assert_eq!(names.member_name(GUILD, 1).as_deref(), Some("Big Al"));
        assert_eq!(names.channel_name(GUILD, 77).as_deref(), Some("general"));
        assert_eq!(names.channel_name(GUILD, 78).as_deref(), Some("a thread"));
        assert_eq!(names.member_name(GUILD, 2), None, "not in the server");
        assert_eq!(names.channel_name(GUILD, 79), None);
        assert_eq!(names.guild_name(2000), None, "not a server the bot's in");
        assert_eq!(names.member_name(2000, 1), None);
        assert_eq!(names.channel_name(2000, 77), None);
        assert_eq!(names.member_name(GUILD, 0), None, "never a Discord id");
        assert_eq!(names.guild_name(0), None);
    }

    #[tokio::test]
    async fn serves_on_its_port_and_gives_up_on_a_taken_one() {
        let port = {
            let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            free.local_addr().unwrap().port()
        };
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let names: Arc<dyn Directory> = Arc::new(Names(HashMap::new()));
        let server = tokio::spawn(serve(port, db.clone(), names.clone()));
        let url = format!("http://127.0.0.1:{port}/api/people");
        let mut status = None;
        for _ in 0..50 {
            if let Ok(res) = reqwest::get(&url).await {
                status = Some(res.status().as_u16());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(status, Some(401));
        // A second dashboard can't have the port, and returns instead of taking the bot down.
        tokio::time::timeout(std::time::Duration::from_secs(5), serve(port, db, names))
            .await
            .expect("gave up");
        server.abort();
    }

    #[test]
    fn links_carry_the_key_after_the_hash() {
        assert_eq!(
            link("http://deck.local:8080/", "abc"),
            "http://deck.local:8080/#key=abc"
        );
        assert_eq!(
            link("http://deck.local:8080", "abc"),
            "http://deck.local:8080/#key=abc"
        );
    }

    #[tokio::test]
    async fn the_api_needs_a_valid_key() {
        let (app, _) = dashboard().await;
        for uri in ["/api/people", "/api/people/1/log", "/api/people/1/days"] {
            assert_eq!(
                get(&app, uri, None).await.0,
                StatusCode::UNAUTHORIZED,
                "{uri}"
            );
            assert_eq!(
                get(&app, uri, Some("wrong")).await.0,
                StatusCode::UNAUTHORIZED,
                "{uri}"
            );
        }
        let (_, body) = get_json(&app, "/api/people", "wrong").await;
        assert!(body["error"].as_str().unwrap().contains("/log dashboard"));
    }

    #[tokio::test]
    async fn the_page_is_served_locked_down() {
        let (app, _) = dashboard().await;
        for (uri, ty) in [
            ("/", "text/html"),
            ("/style.css", "text/css"),
            ("/logs.js", "text/javascript"),
            ("/app.js", "text/javascript"),
        ] {
            let (status, res) = get(&app, uri, None).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert_eq!(res.headers()[header::CONTENT_TYPE], ty);
            let csp = res.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap();
            assert!(csp.starts_with("default-src 'self'"));
            assert_eq!(res.headers()[header::REFERRER_POLICY], "no-referrer");
            assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
        }
    }

    #[tokio::test]
    async fn people_are_named_from_discord_then_the_log() {
        let (app, key) = dashboard().await;
        let (status, body) = get_json(&app, "/api/people", &key).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["guild"], "Nice Club");
        let people = body["people"].as_array().unwrap();
        let summary: Vec<_> = people
            .iter()
            .map(|p| {
                (
                    p["id"].as_str().unwrap(),
                    p["name"].as_str().unwrap(),
                    p["total"].as_i64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("1", "Al", 3),
                ("2", "bo", 1),
                ("3", "User 3", 1),
                ("server", "Server-wide", 1)
            ]
        );
        assert_eq!(people[0]["hostile"], 1);
        assert_eq!(people[0]["said"], 2);
    }

    #[tokio::test]
    async fn a_persons_log_comes_a_page_at_a_time_with_channel_names() {
        let (app, key) = dashboard().await;
        let (status, body) = get_json(&app, "/api/people/1/log", &key).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["more"], false);
        let entries = body["entries"].as_array().unwrap();
        let kinds: Vec<_> = entries
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["hostile", "message", "message"]);
        assert_eq!(entries[0]["group"], "moderation");
        assert_eq!(entries[0]["data"]["said"], "ugh");
        assert_eq!(entries[2]["channel"], "general");
        assert_eq!(entries[2]["channel_id"], "77");
        assert_eq!(entries[1]["channel"], Value::Null);

        let (_, body) = get_json(&app, "/api/people/1/log?groups=said&q=HI", &key).await;
        assert_eq!(body["entries"].as_array().unwrap().len(), 2);
        let first_id = entries[0]["id"].as_i64().unwrap();
        let (_, body) = get_json(
            &app,
            &format!("/api/people/1/log?before={first_id}&groups="),
            &key,
        )
        .await;
        assert_eq!(body["entries"].as_array().unwrap().len(), 2);

        let (_, body) = get_json(&app, "/api/people/server/log", &key).await;
        assert_eq!(body["entries"][0]["kind"], "monitor_joined");

        for bad in ["/api/people/al/log", "/api/people/1/log?groups=said,nope"] {
            let (status, body) = get_json(&app, bad, &key).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
            assert!(body["error"].is_string());
        }
    }

    #[tokio::test]
    async fn a_page_says_when_theres_more() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let day = "2026-11-05".parse().unwrap();
        for _ in 0..PAGE + 1 {
            let log = Log::new("typing", json!({})).user(UserId::new(1));
            db.log_activity(GUILD, &log, day).await.unwrap();
        }
        let key = db
            .new_dashboard_key(GUILD, chrono::Utc::now())
            .await
            .unwrap();
        let app = router(db, Arc::new(Names(HashMap::new())));
        let (_, body) = get_json(&app, "/api/people/1/log", &key).await;
        assert_eq!(body["entries"].as_array().unwrap().len(), PAGE as usize);
        assert_eq!(body["more"], true);
        let (_, body) = get_json(&app, "/api/people", &key).await;
        assert_eq!(body["guild"], format!("Server {GUILD}"));
    }

    #[tokio::test]
    async fn days_count_a_persons_entries() {
        let (app, key) = dashboard().await;
        let (status, body) = get_json(&app, "/api/people/1/days", &key).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!([{"day": "2026-11-05", "n": 3, "hostile": 1}]));
        let (status, _) = get_json(&app, "/api/people/x/days", &key).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
