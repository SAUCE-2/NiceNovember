# NiceNovember 🤝

A Discord bot that keeps November nice.

The bot sits in your voice channel and listens. When a member with the participant role says
something not nice to someone else in the call, they're timed out for 10 seconds and what they said
is quoted in the announce channel. Chat messages are judged the same way. A timeout isn't Discord's
own: the bot server-mutes and deafens them and deletes what they post, so nobody's exempt, not even
the owner or admins.
Everything that happens is logged, so you can build stats from it at the end of the month.

## Features

- **AI judging.** Messages (with the message they reply to, plus images and GIFs), edits, emoji
  reactions (with the message they're on) and speech are judged by [Jev](https://typesafe.ai), so "hostile" means more than a list of swear words.
  Jev answers two questions in context: is it aimed at a person or group, and is it a put-down?
  It's only not nice when it's both. Swearing at a game, naming a song or artist with a slur in
  it, quoting a lyric, asking someone to stop something, or calling out something crude they said
  ("that's disgusting") is fine. A put-down aimed at someone
  isn't, even teasing, sarcasm or a joke between friends, and neither is using an identity as
  the insult ("that's so gay" about a game).
- **Always listening in voice.** The bot sits in one voice channel whenever anyone's in it (and
  leaves when it's empty), and transcribes each thing a participant says locally with
  [Whisper](https://github.com/ggerganov/whisper.cpp).
- **Judged in context.** Each message or line is judged with who said it and when, and the
  conversation just before it with who said each thing and when: in chat up to 10 messages from
  the 30 minutes before, in voice who's in the call and up to 8 lines from the last 45 seconds.
  So "cringe" right after someone shares their drawing is not nice, but just saying "hostile"
  (or answering "what does the bot call you?") isn't.
- **Public call-outs.** Each hostile message or sentence is posted to the announce channel as a
  quote, with who said it (pinging them, and nobody else), where, and how sure Jev is that it was
  hostile. Voice call-outs also show the last few lines of the call before it, with what was said
  in bold.
- **Timeouts for everyone.** Enforced by the bot, not Discord's timeout: until the timeout ends,
  the user's messages are deleted, their reactions are removed, and they're server-muted and
  deafened in VC.
- **Loads of metrics.** Every message, reaction, voice join, presence change, transcript, Jev score,
  timeout and pardon goes into an activity log you can export.
- **Dashboard.** Browse that log person by person in your browser, live from the bot's database:
  `/log dashboard` gives you the link.

## How it works

| Step | Chat | Voice |
|---|---|---|
| Who | Participants only (the participant role) | Participants in the monitored channel |
| What's judged | Every new message (with the recent conversation in the channel and the message it replies to), edits that change the text (with the conversation before the message), and reactions on someone else's message (with that message) | Each thing they say, with the recent conversation: audio is cut when they go quiet for 0.8s (or after 15s) and transcribed locally |
| Not nice when | Jev's score (the lower of how sure it is that it's aimed at someone and that it's a put-down) is at or above the threshold (default 0.7, set with `/nice setup threshold:`) | Same |
| How not nice | Every judged thing is also rated on how not nice it was (its *not niceness*, 0–100%, from friendly or neutral, through a gentle jab, a put-down that stings and harsh and personal, to cruel or hateful), for the `/stats` cards. Things judged before this rating existed are rated once, on the next start, with their conversation as Discord and the activity log still have it | Same |
| Then | 10s timeout, quote in the announce channel with how sure Jev is that it was hostile (e.g. `87% sure it's hostile`, a likelihood, not how hostile it was) | Same, and server-muted and deafened until it ends |

**Why not Discord's timeout?** Discord won't let a bot time out the server owner, anyone with
Administrator, or anyone above the bot's role, so the bot does it itself, the same for everyone: it
deletes their messages, removes their reactions and server-mutes and deafens them in VC. If someone
leaves VC while muted, they're unmuted and undeafened when they come back.

## Privacy

Only members with the participant role are judged, and only their audio is transcribed. Audio from
everyone else in the channel is dropped without being transcribed.

- **Sent to [Jev](https://typesafe.ai):** participants' message text, the text of the message they
  replied to, attached images and embed previews (GIFs, link thumbnails), edits, their reactions
  with the message (and images) they reacted to, and transcripts of
  what they said in VC, with the display names of everyone in the call and the last few lines
  participants said. Audio never leaves your server.
- **Posted publicly:** each hostile message or transcript, quoted in the announce channel, also in
  [shadow mode](#tuning-the-threshold).
- **Stored in the bot's database:** the activity log (see below), which includes message content
  and transcripts from participants. Treat the database and exports as private.
- **Shown on the [dashboard](#dashboard):** the same log, to whoever has a `/log dashboard` link
  (admins get one, and it works for 7 days).

## Commands

Anyone can use the read-only commands. Everything else needs **Administrator**.

| Command | What it does |
|---|---|
| `/nice setup [role] [announce_channel] [voice_channel] [threshold] [shadow]` | Setup checklist, fixing things in one go |
| `/nice unset <voice channel\|threshold>` | Clear an optional setting |
| `/nice pardon <user>` | End someone's timeout early |
| `/nice recent [count]` | The latest hostile things people said |
| `/status` | Uptime, latency, memory, whether it's listening, and how much was heard, judged and timed out |
| `/stats <user>` | Top Trumps card: meanest/friendliest lines and average not niceness (as a %) per source, how many weren't nice, plus timeouts (posted in-channel) |
| `/log summary` | How much has been logged, by kind (admin) |
| `/log dashboard` | A link to the [dashboard](#dashboard): everyone's log, person by person, live (admin) |
| `/log export` | Download the activity log as gzipped JSON Lines (admin) |
| `/log wipe confirm:true [user]` | Delete logged activity, e.g. after testing (admin) |

## Self-hosting

### 1. Create the Discord bot

1. Create an application at <https://discord.com/developers/applications>. Under **Bot**, enable the
   **Presence**, **Server Members** and **Message Content** intents, then copy the token.
2. Invite the bot with the `bot` and `applications.commands` scopes and the **Administrator**
   permission.

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

Podman keeps the image up to date but not the units, so when a release changes them (the
dashboard's port did), run the install command again to pick them up.

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
/nice setup role:@Participants announce_channel:#hall-of-shame voice_channel:🔊General
```

It joins the voice channel as soon as someone's in it, and leaves once nobody else is. If it's moved
or kicked out while people are there, it comes back within 30 seconds.

### Tuning the threshold

To find the right threshold on real conversation before anyone gets timed out, turn on shadow mode:
`/nice setup shadow:true`. Everything is judged, logged and quoted in the announce channel as usual,
but nobody is timed out, and the call-out says `👻 Not nice (74% sure it's hostile, shadow mode:
not timed out)`. Too many false alarms? Raise the bar with `/nice setup threshold:0.8`. Letting too much
through? Lower it. Turn it off with `/nice setup shadow:false`.

### Configuration

All settings are environment variables in `.env`:

| Variable | Description |
|---|---|
| `DISCORD_TOKEN` | **Required.** Your bot token |
| `JEV_API_KEY` | **Required to judge anything.** Without it the bot only logs |
| `DASHBOARD_URL` | Where you open the [dashboard](#dashboard), e.g. `http://steamdeck.local:8080`. `/log dashboard` links point here (default `http://localhost:8080`) |
| `DASHBOARD_PORT` | Port the dashboard listens on inside the container (default `8080`) |
| `BOT_TZ` | Timezone that decides which day each activity log entry is filed under (default `Europe/London`) |
| `DEV_GUILD_ID` | Register commands in one server only, so they update instantly. Leave empty in production |
| `RUST_LOG` | Log level |

## Activity log

Rather than deciding up front what to count, the bot logs everything participants do into the
`activity_log` table and you work out the stats afterwards. Each entry has the server, the user, the
channel, a `kind`, whether the user was a participant at the time, the day (in `BOT_TZ`), a UTC
timestamp, and a JSON `data` payload. Members without the participant role and bots aren't logged.
A `member_update` that gives or takes away the role is logged.

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
| `judged` | A Jev verdict: `source` (message/edit/voice/reaction), `said`, `score`, `aimed` and `put_down` (Jev's two answers; `score` is the lower), `meanness` (not niceness, 0 to 1; added on the next start to entries from before it was asked), `hostile`, `threshold`, `shadow`, `jev_ms`, `images`, `context_lines` (how much of the conversation it was judged with), and for voice `in_call` |
| `judge_failed` | Jev couldn't be reached, so it wasn't judged |
| `hostile` | Something hostile: what was `said`, `score`, `aimed`, `put_down`, `meanness`, `muted` (server-muted and deafened), `announced` |
| `timed_out` / `timeout_released` | Each timeout: `secs`, `until`, `muted`; and when it was lifted |
| `timeout_message` / `timeout_reaction` / `timeout_vc` | Tried to talk, react or join VC while timed out |
| `pardoned` | An admin ended a timeout early (`by`) |
| `monitor_joined` / `monitor_left` / `monitor_failed` | The bot joined the monitored voice channel, left voice, or couldn't join |

### Dashboard

The bot serves a dashboard of the activity log on port 8080, read live from its own database, so
there's no file to export or upload. Run `/log dashboard` (admins only) and open the link it gives
you. It shows:

- **Everyone in the log**, busiest first, with how many times each was not nice. Server-wide
  entries (the bot joining or leaving voice, bulk deletes) are listed on their own.
- **For each person:** their totals (entries, things said, judged, average hostility, not nice,
  timeouts), their entries per day, and their whole log, newest first: what they said in chat and
  in voice, how Jev judged it, timeouts, voice joins, presence and the rest. Filter it by
  said, moderation, voice, presence or other, search it, and open any entry for its raw data.

Each link carries a key for your server and works for 7 days; anyone with it can read the log, so
don't share it. For the links to work from other devices, `DASHBOARD_URL` in `.env` must say how you
reach the bot's machine, e.g. `http://steamdeck.local:8080` or `http://192.168.1.20:8080`. The Steam
Machine install fills it in with the machine's name (which also works over Tailscale's MagicDNS) if
it's empty. Keep port 8080 inside your home network (don't forward it from your router). The Steam
Machine install publishes it on port 8080 (`NICENOVEMBER_PORT=9090` picks another) and the Docker
Compose file does the same.

### Export

To get the data out, run `/log export`. It gives you a `.jsonl.gz` file with one JSON object per
line. If it's over Discord's 10 MB upload limit, copy the database instead (see above) and query it
with SQLite's JSON functions. Some ideas:

```sql
-- Meanest people
SELECT user_id, COUNT(*) FROM activity_log
WHERE kind = 'hostile' GROUP BY user_id ORDER BY 2 DESC;

-- Average not niceness per person, chat vs voice
SELECT user_id, json_extract(data, '$.source') AS source, AVG(json_extract(data, '$.meanness')), COUNT(*)
FROM activity_log WHERE kind = 'judged' GROUP BY 1, 2 ORDER BY 3 DESC;

-- Who talks most in VC (minutes of speech heard)
SELECT user_id, SUM(json_extract(data, '$.audio_ms')) / 60000.0 FROM activity_log
WHERE kind = 'heard' GROUP BY user_id ORDER BY 2 DESC;

-- Most timeouts served in VC (server-muted and deafened)
SELECT user_id, COUNT(*) FROM activity_log
WHERE kind = 'timed_out' AND json_extract(data, '$.muted') GROUP BY user_id ORDER BY 2 DESC;
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

`deploy/steam-machine/test.sh` checks the install script against a fake Podman and systemd, and
this tests the dashboard page's wording:

```bash
node --test dashboard/logs.test.js
```

### Where things are

| Path | What |
|---|---|
| `src/hostility.rs` | What counts as hostile (Jev's question), the call-out text, transcript cleanup |
| `src/judge.rs` | Judge an utterance, then time out, announce and log |
| `src/timeout.rs` | Timeouts: enforcement (delete, mute, deafen) and the one release path |
| `src/voice/monitor.rs` | Sitting in the monitored channel and turning speech into utterances |
| `src/voice/segmenter.rs` | Cutting each speaker's audio into one segment per thing said |
| `src/stats/` | The activity log, its capture of Discord events, and the export |
| `src/dashboard/` | The dashboard's web server: its API over the activity log, and its link keys |
| `dashboard/` | The dashboard page itself (built into the binary) |
| `deploy/steam-machine/` | Install script for rootless Podman + systemd (Quadlet) |

Made with the same stack as [SpiritBot](https://github.com/SAUCE-2/spritbot-slop). CI runs
`cargo fmt`, `clippy`, tests, the dashboard page's tests and the deploy script tests on every PR, and
builds the image.

Every push to `main` publishes the image as `:latest`, `:<commit>` and `:<version>`, and tags the
commit `v<version>`. The version is [semver](https://semver.org) worked out from the
[Conventional Commits](https://www.conventionalcommits.org) since the last release: a breaking
change (`feat!:` or a `BREAKING CHANGE:` footer) bumps the major, a `feat` the minor, anything else
the patch. `scripts/next-version.sh` prints the one the next push would get, and `/status` shows it.
