-- There's no log channel any more: everything, shadow mode included, goes to the announce channel
-- with Jev's score, and setup notices go there too.
ALTER TABLE guild_config DROP COLUMN log_channel_id;
