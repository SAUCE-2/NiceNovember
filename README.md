# NiceNovember 🤝

A Discord bot that keeps November nice.

The bot sits in your voice channel and listens. When a member with the participant role says
something not nice to someone else in the call, they're timed out for 10 seconds and what they said
is quoted in the announce channel. Chat messages are judged the same way. Nobody's exempt: members Discord won't
let a bot time out (the owner, admins, anyone above the bot's role) get a forced timeout instead.
Everything that happens is logged, so you can build stats from it at the end of the month.

## Features

- **AI judging.** Messages (with the message they reply to, plus images and GIFs), edits, emoji
  reactions (with the message they're on) and speech are judged by [Jev](https://typesafe.ai), so "hostile" means more than a list of swear words.
  Swearing at a game is fine. Swearing at a person isn't.
- **Always listening in voice.** The bot sits in one voice channel all the time and transcribes
  each thing a participant says locally with [Whisper](https://github.com/ggerganov/whisper.cpp).
- **Judged in context.** Each line is judged with who said it, who's in the call, and the last few
  lines of the conversation (up to 8, from the last 45 seconds), so "shut up" answering a joke
  reads differently from "shut up" aimed at someone.
- **Public call-outs.** Each hostile message or sentence is posted to the announce channel as a
  quote, with who said it and where.
- **Timeouts for everyone.** Native Discord timeouts where possible, forced ones otherwise. Both
  are also enforced by the bot: until the timeout ends, the user's messages are deleted, their
  reactions are removed, and they're server-muted in VC.
- **Loads of metrics.** Every message, reaction, voice join, presence change, transcript, Jev score,
  timeout and pardon goes into an activity log you can export.

## How it works

| Step | Chat | Voice |
|---|---|---|
| Who | Participants only (the participant role) | Participants in the monitored channel |
| What's judged | Every new message (with the message it replies to), edits that change the text, and reactions on someone else's message (with that message) | Each thing they say, with the recent conversation: audio is cut when they go quiet for 0.8s (or after 15s) and transcribed locally |
| Not nice when | Jev's score is at or above the threshold (default 0.7, set with `/nice setup threshold:`) | Same |
| Then | 10s timeout, quote in the announce channel, details in the log channel | Same, and server-muted until it ends |

**Native or forced?** Discord won't let a bot time out the server owner, anyone with Administrator,
or anyone whose top role is at or above the bot's. Those members get a forced timeout instead. Put
the bot's role near the top of the role list so as many people as possible get the native kind.
Either way the bot deletes their messages, removes their reactions and server-mutes them in VC, so
the two behave the same. If someone leaves VC while muted, they're unmuted when they come back.

## Privacy

Only members with the participant role are judged, and only their audio is transcribed. Audio from
everyone else in the channel is dropped without being transcribed.

- **Sent to [Jev](https://typesafe.ai):** participants' message text, the text of the message they
  replied to, attached images and embed previews (GIFs, link thumbnails), edits, their reactions
  with the message (and images) they reacted to, and transcripts of
  what they said in VC, with the display names of everyone in the call and the last few lines
  participants said. Audio never leaves your server.
- **Posted publicly:** each hostile message or transcript, quoted in the announce channel. In
  [shadow mode](#tuning-the-threshold) they're quoted in the log channel instead.
- **Stored in the bot's database:** the activity log (see below), which includes message content
  and transcripts from the whole server. Treat the database and exports as private.

## Commands

Anyone can use the read-only commands. Everything else needs **Administrator**.

| Command | What it does |
|---|---|
| `/nice setup [role] [announce_channel] [log_channel] [voice_channel] [threshold] [shadow]` | Setup checklist, fixing things in one go |
| `/nice unset <log channel\|voice channel\|threshold>` | Clear an optional setting |
| `/nice pardon <user>` | End someone's timeout early |
| `/nice check <text> [spoken]` | See Jev's score for some text, without timing anyone out (for tuning the threshold) |
| `/nice recent [count]` | The latest hostile things people said |
| `/status` | Uptime, latency, memory, whether it's listening, and how much was heard, judged and timed out |
| `/stats summary [month]` | How much has been logged, by kind |
| `/stats export [month]` | Download the activity log as gzipped JSON Lines |
| `/stats wipe confirm:true [user]` | Delete logged activity, e.g. after testing |

`month` takes `2026-11`, `november`, `last` or `all` and defaults to the current month.

## Self-hosting

### 1. Create the Discord bot

1. Create an application at <https://discord.com/developers/applications>. Under **Bot**, enable the
   **Presence**, **Server Members** and **Message Content** intents, then copy the token.
2. Invite the bot with the `bot` and `applications.commands` scopes and the **Administrator**
   permission.
3. In Server Settings → Roles, drag the bot's role as high as you can (see
   [native or forced](#how-it-works)).

### 2. Run it on the Steam Machine

The Steam Machine runs SteamOS, which ships rootless Podman. Watchtower can't update containers
there, so the install script runs the bot as a [Quadlet](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html)
service, `nicenovember.service`, and Podman's own `podman-auto-update.timer` checks every 2 minutes
and restarts it if a new image was published. Run this in Desktop Mode's terminal (Konsole):

```bash
curl -fsSL https://raw.githubusercontent.com/SAUCE-2/NiceNovember/main/deploy/steam-machine/install.sh | bash
```

The first run creates `~/nicenovember/.env`. Fill in `DISCORD_TOKEN` and `JEV_API_KEY`:

```bash
nano ~/nicenovember/.env
```

Then run the same install command again. It writes the systemd units, enables linger so the bot
keeps running in Game Mode and after logout (it asks for your sudo password once; set one with
`passwd` if you never have), and starts the bot. You can run it again at any time.

Useful commands:

```bash
podman logs -f nicenovember
```

```bash
systemctl --user status nicenovember.service
```

```bash
systemctl --user list-timers podman-auto-update.timer
```

```bash
podman cp nicenovember:/data/nicenovember.db .
```

The data lives in the `nicenovember-data` Podman volume, so it survives updates. systemd restarts
the bot if it exits and starts it after a reboot.

If the pull fails with `unauthorized` or `denied`, the image on GHCR is private. Make it public in
GitHub under the repo's **Packages → nicenovember → Package settings → Change visibility**.

### Next to SpiritBot

NiceNovember runs as its own container and doesn't touch SpiritBot's. Its container, volume and
service are all called `nicenovember`, while SpiritBot keeps its compose setup and
`spiritbot-update.timer`. Two things to know:

- **It needs its own Discord bot.** Create a second application (step 1 above) and use its token.
  Two processes on one token would fight over the same gateway session and slash commands.
- **The Jev key can be the same one** SpiritBot uses.

Both can sit in voice at the same time. SpiritBot only listens for 10 seconds when someone joins,
while NiceNovember listens all the time, so expect it to use more CPU (one Whisper run per thing said).

### Or run it with Docker

```bash
mkdir nicenovember && cd nicenovember
curl -O https://raw.githubusercontent.com/SAUCE-2/NiceNovember/main/docker-compose.yml
curl -o .env https://raw.githubusercontent.com/SAUCE-2/NiceNovember/main/.env.example
# Fill in DISCORD_TOKEN and JEV_API_KEY in .env
docker compose up -d
```

Update with `docker compose pull && docker compose up -d`.

### 3. Configure

The bot posts a setup checklist when it joins your server. To set it up in one go, run:

```
/nice setup role:@Participants announce_channel:#hall-of-shame log_channel:#mod-log voice_channel:🔊General
```

It joins the voice channel straight away and stays there. If it's moved or kicked out, it comes back
within 30 seconds.

### Tuning the threshold

To find the right threshold on real conversation before anyone gets timed out, turn on shadow mode:
`/nice setup shadow:true`. Everything is judged and logged as usual, but instead of a timeout and a
call-out, the log channel gets `🔎 @user · Voice in 🔊General · jev 0.74 (threshold 0.70) · 👻 shadow
mode, would have been timed out` with the quote. Too many false alarms? Raise the bar with
`/nice setup threshold:0.8`. Letting too much through? Lower it. `/nice check` scores any text you
type. Turn it off with `/nice setup shadow:false`.

### Configuration

All settings are environment variables in `.env`:

| Variable | Description |
|---|---|
| `DISCORD_TOKEN` | **Required.** Your bot token |
| `JEV_API_KEY` | **Required to judge anything.** Without it the bot only logs |
| `BOT_TZ` | Timezone that decides which day each activity log entry is filed under (default `Europe/London`) |
| `DEV_GUILD_ID` | Register commands in one server only, so they update instantly. Leave empty in production |
| `RUST_LOG` | Log level |

## Activity log

Rather than deciding up front what to count, the bot logs everything into the `activity_log` table
and you work out the stats afterwards. Each entry has the server, the user, the channel, a `kind`,
whether the user was a participant at the time, the day (in `BOT_TZ`), a UTC timestamp, and a JSON
`data` payload. Bots aren't logged.

| kind | data |
|---|---|
| `message` | The whole Discord message: content, attachments, embeds, stickers, mentions, reply |
| `message_edit` / `message_delete` / `message_delete_bulk` | The edit event / the deleted message's id (join it to `message`) |
| `reaction_add` / `reaction_remove` | Emoji, message id, and who wrote the message |
| `voice` | `join`/`leave`/`move`/`update`, with old and new mute, deafen, stream and camera state |
| `vc_session` | Seconds spent in VC, logged on leaving |
| `presence` | Status, devices and activities (games, Spotify, custom status), logged when they change |
| `typing`, `command`, `thread_create` | Who typed where, which slash command was used, new threads |
| `member_seen` / `member_join` / `member_leave` / `member_update` | Names, nick and roles (snapshot at startup, then changes) |
| `heard` | A participant's transcribed speech: `text`, Whisper's `raw` output, `audio_ms`, `transcribe_ms` |
| `judged` | A Jev verdict: `source` (message/edit/voice/reaction), `said`, `score`, `hostile`, `threshold`, `shadow`, `jev_ms`, `images`, and for voice `context_lines` and `in_call` |
| `judge_failed` | Jev couldn't be reached, so it wasn't judged |
| `hostile` | Something hostile: what was `said`, `score`, timeout `method` (native/forced), `muted`, `announced` |
| `timed_out` / `timeout_released` | Each timeout: `method`, `secs`, `until`, why a native one was refused; and when it was lifted |
| `timeout_message` / `timeout_reaction` / `timeout_vc` | Tried to talk, react or join VC while timed out |
| `pardoned` | An admin ended a timeout early (`by`) |
| `monitor_joined` / `monitor_failed` | The bot joined the monitored voice channel, or couldn't |

To get the data out, run `/stats export month:november`. It gives you a `.jsonl.gz` file with one
JSON object per line. If it's over Discord's 10 MB upload limit, copy the database instead (see
above) and query it with SQLite's JSON functions. Some ideas:

```sql
-- Meanest people this month
SELECT user_id, COUNT(*) FROM activity_log
WHERE kind = 'hostile' AND day LIKE '2026-11-%' GROUP BY user_id ORDER BY 2 DESC;

-- Average hostility score per person, chat vs voice
SELECT user_id, json_extract(data, '$.source') AS source, AVG(json_extract(data, '$.score')), COUNT(*)
FROM activity_log WHERE kind = 'judged' GROUP BY 1, 2 ORDER BY 3 DESC;

-- Who talks most in VC (minutes of speech heard)
SELECT user_id, SUM(json_extract(data, '$.audio_ms')) / 60000.0 FROM activity_log
WHERE kind = 'heard' GROUP BY user_id ORDER BY 2 DESC;

-- Who'd have been timed out natively vs forced
SELECT json_extract(data, '$.method'), COUNT(*) FROM activity_log
WHERE kind = 'timed_out' GROUP BY 1;
```

## Development

Builds run in Docker, so you don't need cmake, clang or opus installed locally.

```bash
cp .env.example .env   # set DISCORD_TOKEN, JEV_API_KEY and DEV_GUILD_ID (your test server)
docker compose -f docker-compose.dev.yml up -d --build
```

To run cargo in a toolchain image with the native dependencies:

```bash
./dev.sh cargo test
```

```bash
./dev.sh cargo clippy --all-targets
```

The live tests are ignored by default. With a Whisper model and a 16 kHz mono WAV (for example
`samples/jfk.wav` from whisper.cpp), this streams the WAV through the voice pipeline in 20 ms ticks:

```bash
STT_TEST_MODEL=models/ggml-base.en.bin STT_TEST_WAV=models/jfk.wav ./dev.sh cargo test -- --ignored --nocapture
```

`deploy/steam-machine/test.sh` checks the install script against a fake Podman and systemd.

### Where things are

| Path | What |
|---|---|
| `src/hostility.rs` | What counts as hostile (Jev's question), the call-out text, transcript cleanup |
| `src/judge.rs` | Judge an utterance, then time out, announce and log |
| `src/timeout.rs` | Native vs forced timeouts, enforcement, and the one release path |
| `src/voice/monitor.rs` | Sitting in the monitored channel and turning speech into utterances |
| `src/voice/segmenter.rs` | Cutting each speaker's audio into one segment per thing said |
| `src/stats/` | The activity log, its capture of Discord events, and the export |
| `deploy/steam-machine/` | Install script for rootless Podman + systemd (Quadlet) |

Made with the same stack as [SpiritBot](https://github.com/SAUCE-2/spritbot-slop). CI runs
`cargo fmt`, `clippy`, tests and the deploy script tests on every PR, and builds the image.
