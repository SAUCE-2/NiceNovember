-- Discord snowflakes are stored as INTEGER (i64); times as RFC3339 UTC text, days as YYYY-MM-DD.

CREATE TABLE guild_config (
    guild_id             INTEGER PRIMARY KEY,
    participant_role_id  INTEGER,            -- only members with this role are judged
    announce_channel_id  INTEGER,            -- hostile quotes are posted here
    log_channel_id       INTEGER,            -- details + setup notices (falls back to announce)
    voice_channel_id     INTEGER,            -- the one VC the bot always sits in and listens to
    threshold            REAL,               -- Jev score counted as hostile; NULL = default
    last_setup_hash      TEXT
);

-- Every timeout, native or forced. Until `until`, the bot deletes the user's messages, removes
-- their reactions and server-mutes them in VC. A row outlives `until` only while the bot still
-- owes the user an unmute (`voice_muted`), e.g. because they left VC while muted.
CREATE TABLE timeouts (
    guild_id    INTEGER NOT NULL,
    user_id     INTEGER NOT NULL,
    until       TEXT    NOT NULL,
    method      TEXT    NOT NULL,            -- 'native' or 'forced'
    voice_muted INTEGER NOT NULL DEFAULT 0,  -- the bot server-muted them and must unmute
    PRIMARY KEY (guild_id, user_id)
);

-- Raw record of everything that happens, so stats can be worked out after the fact. See src/stats/.
CREATE TABLE activity_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id    INTEGER NOT NULL,
    user_id     INTEGER,             -- who it's about; NULL for server-wide events
    channel_id  INTEGER,
    kind        TEXT    NOT NULL,    -- message, voice, judged, hostile, timed_out, ...
    participant INTEGER,             -- did the user have the participant role (NULL = unknown)
    data        TEXT    NOT NULL,    -- JSON: the Discord payload, or the bot's own details
    day         TEXT    NOT NULL,    -- YYYY-MM-DD in the bot's timezone
    at          TEXT    NOT NULL     -- RFC3339 UTC
);

CREATE INDEX activity_log_kind ON activity_log (guild_id, kind, day);
CREATE INDEX activity_log_user ON activity_log (guild_id, user_id, day);
