-- Keys in `/log dashboard` links. Each lets whoever has the link read one server's activity log on
-- the dashboard until it expires. Kept here so links still work after the bot restarts.
CREATE TABLE dashboard_keys (
    key      TEXT    PRIMARY KEY,   -- random, 64 hex characters
    guild_id INTEGER NOT NULL,
    expires  TEXT    NOT NULL       -- RFC3339 UTC
);
