-- 1 = shadow mode: judge and log as usual, but only report would-be timeouts to the log channel.
ALTER TABLE guild_config ADD COLUMN shadow INTEGER NOT NULL DEFAULT 0;
