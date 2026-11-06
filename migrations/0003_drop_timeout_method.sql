-- Discord's native timeout is no longer used: every timeout is the bot's own (messages deleted,
-- reactions removed, server-muted and deafened in VC), so there's no method to record.
-- `voice_muted` now means the bot server-muted and deafened them and must undo both.
ALTER TABLE timeouts DROP COLUMN method;
