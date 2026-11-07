//! What counts as not nice ("hostile" in the code and the activity log), and how it's
//! reported. Pure: no Discord or network calls.

use chrono::{DateTime, Utc};
use poise::serenity_prelude::{ChannelId, MessageId, UserId};
use serde::{Serialize, Serializer};

use crate::ai::jev::{Question, noul};

/// Where an utterance came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// A new chat message.
    Message,
    /// A chat message edited to say something new.
    Edit,
    /// Speech in the monitored voice channel, transcribed by Whisper.
    Voice,
    /// An emoji reaction on someone else's message.
    Reaction,
}

/// How times are shown to Jev.
pub const STAMP: &str = "%Y-%m-%d %H:%M:%S UTC";

fn stamp<S: Serializer>(at: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(&at.format(STAMP))
}

/// One thing said in a channel or the voice call, by display name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Line {
    pub speaker: String,
    pub said: String,
    #[serde(serialize_with = "stamp")]
    pub at: DateTime<Utc>,
}

/// Something a participant said, ready to judge.
#[derive(Clone, Debug)]
pub struct Utterance {
    pub user: UserId,
    pub channel: ChannelId,
    pub source: Source,
    /// Their display name.
    pub author: String,
    pub said: String,
    pub at: DateTime<Utc>,
    /// The chat message, for text; for a reaction, the message reacted to.
    pub message: Option<MessageId>,
    /// What the message replies to (or the reaction is on), as context for the judge.
    pub in_reply_to: Option<String>,
    /// The conversation: what was said just before in the same channel or call, oldest first.
    pub recent: Vec<Line>,
    /// For voice: display names of everyone in the call (bots excluded), speaker included.
    pub in_call: Vec<String>,
}

// Jev answers one condition per Noul best, so "not nice" is two questions asked together: is it
// aimed at someone, and is it a put-down. Words alone never decide it: a slur in a song title is a
// put-down word but not aimed, and "stop gaslighting me" is aimed but not a put-down.
const TEXT_SETTING: &str = "The state is a Discord chat message (`message_text`, possibly with \
    attached images and the message it replies to) that `author` sent at `sent_at`, on a \
    community server where everyone has agreed to be nice all month. `recent_messages` is what \
    was said in the channel just before, oldest first, with who said each and when. Use that \
    conversation and its timing to work out who or what `message_text` is about and what \
    `author` means by it.";
const VOICE_SETTING: &str = "The state is a moment in a Discord voice call on a community server \
    where everyone has agreed to be nice all month. `speaker` said `said_now` at `said_at`. \
    `people_in_call` is who's there, and `recent_conversation` is what some of them said just \
    before, oldest first, with when (it may be missing lines). Everything is speech-to-text and \
    may contain mishearings. Use that conversation and its timing to work out who or what \
    `said_now` is about and what `speaker` means by it.";
const REACTION_SETTING: &str = "The state is an emoji `reaction` that a member of a community \
    server where everyone has agreed to be nice all month put on someone else's Discord message \
    (`on_message`, possibly with images). Custom emoji are given by name, like `:clown:`.";

/// Jev's answer to "is it aimed at someone?"; see [`questions`].
pub const AIMED: &str = "aimed";
/// Jev's answer to "is it a put-down?"; see [`questions`].
pub const PUT_DOWN: &str = "put_down";

const AIMED_YES: &str = "Aimed at or about a person or group: whoever it's said to, someone \
    else in the server, someone who isn't there, or a group of people, or their looks, skills, \
    work, taste or ideas. Using an identity or a slur as the insult is aimed at the people it \
    names, even when it's said about a thing: \"that's so gay\" about a game is aimed at gay \
    people. The conversation can aim a short reply: \"cringe\" right after someone shares their \
    drawing is about their drawing. Examples: \"you're so bad at this\", \"Sam's cooking is \
    disgusting\", \"this map is retarded\", \"why are you stalking me, twin?\"";
const AIMED_NO: &str = "Not aimed at anyone: about a thing, a game, the situation or the speaker \
    themselves, or a word that's mentioned rather than used on someone. Naming an artist, song, \
    game or username, quoting a lyric, repeating someone else's words without joining in, or \
    naming or asking about meanness, the rules or this bot are all mentions, even when what's \
    mentioned has a swear word or slur in it. Examples: \"this boss is bullshit\", \"ugh, lag \
    again\", \"I'm so bad at this\", \"my favourite song is Fuck You by CeeLo Green\", \
    \"hostile\", \"does swearing count?\"";
const PUT_DOWN_YES: &str = "A put-down, even if it's mild, a joke or between friends: insults, \
    name-calling, slurs, mocking, teasing, roasting or trash talk, sarcasm, passive aggression, \
    backhanded compliments, rudely dismissing (\"nobody asked\", \"who cares\", \"ok and?\"), \
    telling someone to shut up or go away, threats, wishing harm, or contempt. Examples: \
    \"lmao your aim is trash\", \"imagine being that dumb\", \"wow, great job, genius\", \
    \"ok boomer\", \"cringe\", \"I'm not as nasty as you\"";
const PUT_DOWN_NO: &str = "Not a put-down: friendly, kind or neutral talk, compliments, \
    encouragement, questions, playful nicknames like \"twin\" or \"bro\", respectful \
    disagreement, saying what happened, asking someone to stop something, objecting to something \
    crude, rude or mean they just said or did, or saying how it makes you feel. Examples: \"gg, \
    well played\", \"I think you're wrong about that, here's why\", \"how are you making so much \
    money?\", \"I've told him off\", \"stop gaslighting me\", \"please stop\", \"I don't \
    appreciate your tone\", \"that's rude\", \"be nice\", \"that's disgusting\" right after a \
    crude sexual comment";

/// The [`AIMED`] and [`PUT_DOWN`] questions for an utterance from `source`, to ask in one call.
pub fn questions(source: Source) -> [(&'static str, Question); 2] {
    let (setting, it) = match source {
        Source::Message | Source::Edit => (TEXT_SETTING, "what `author` says in `message_text`"),
        Source::Voice => (VOICE_SETTING, "what `speaker` says in `said_now`"),
        Source::Reaction => (REACTION_SETTING, "this `reaction`"),
    };
    let ask = |q: &str, criteria| noul(format!("{setting} {q}"), Some(criteria));
    [
        (
            AIMED,
            ask(
                &format!("Is {it} aimed at or about a particular person or group?"),
                (AIMED_YES, AIMED_NO),
            ),
        ),
        (
            PUT_DOWN,
            ask(&format!("Is {it} a put-down?"), (PUT_DOWN_YES, PUT_DOWN_NO)),
        ),
    ]
}

/// Jev's answers to [`questions`] about one utterance.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Verdict {
    pub aimed: f32,
    pub put_down: f32,
}

impl Verdict {
    /// How not nice it is: only as sure as Jev is of both, so either one being low clears it.
    pub fn score(self) -> f32 {
        self.aimed.min(self.put_down)
    }
}

pub fn is_hostile(score: f32, threshold: f32) -> bool {
    score >= threshold
}

/// Longest quote posted, in characters; longer ones are cut with an ellipsis. With
/// [`QUOTE_CONTEXT`] lines of [`MAX_CONTEXT_LINE`], a call-out stays under Discord's 2000.
const MAX_QUOTE: usize = 1300;
/// How many lines of the call a voice call-out shows before what was said. Chat call-outs show
/// none, since anyone can scroll up the channel.
const QUOTE_CONTEXT: usize = 3;
/// Longest context line shown, in characters.
const MAX_CONTEXT_LINE: usize = 150;

/// `text` trimmed and cut to `max` characters, with an ellipsis if it was longer.
fn cut(text: &str, max: usize) -> String {
    let text = text.trim();
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

/// `said` as a Discord block quote, cut to [`MAX_QUOTE`].
fn quote(said: &str) -> String {
    let text = cut(said, MAX_QUOTE);
    if text.is_empty() {
        return "> …".into();
    }
    text.lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The last [`QUOTE_CONTEXT`] lines of a voice call, then what `u` said in bold. Transcripts
/// are single lines (see [`clean_transcript`]).
fn quote_in_context(u: &Utterance) -> String {
    let skip = u.recent.len().saturating_sub(QUOTE_CONTEXT);
    let before = u.recent[skip..]
        .iter()
        .map(|l| format!("> {}: {}", l.speaker, cut(&l.said, MAX_CONTEXT_LINE)));
    let said = cut(&u.said, MAX_QUOTE);
    let said = if said.is_empty() {
        "…".into()
    } else {
        format!("**{said}**")
    };
    let own = format!("> {}: {said}", u.author);
    before.chain([own]).collect::<Vec<_>>().join("\n")
}

/// The public call-out posted to the announce channel, with Jev's score as how sure it is that
/// this was hostile (a probability, not how hostile it was). In shadow mode it says nobody was
/// timed out.
pub fn announcement(u: &Utterance, score: f32, shadow: bool) -> String {
    let (did, quoted) = match u.source {
        Source::Voice => (format!("said in 🔊 <#{}>", u.channel), quote_in_context(u)),
        Source::Message => (format!("said in <#{}>", u.channel), quote(&u.said)),
        Source::Edit => (
            format!("said in an edit in <#{}>", u.channel),
            quote(&u.said),
        ),
        Source::Reaction => (
            format!("reacted {} in <#{}> to", u.said, u.channel),
            quote(u.in_reply_to.as_deref().unwrap_or_default()),
        ),
    };
    let percent = format!("{:.0}% sure it's hostile", score * 100.0);
    let verdict = if shadow {
        format!("👻 **Not nice** ({percent}, shadow mode: not timed out)")
    } else {
        format!("🚫 **Not nice!** ({percent})")
    };
    format!("{verdict} <@{}> {did}:\n{quoted}", u.user)
}

/// Whisper's non-speech markers like `[BLANK_AUDIO]`, `(music)` or `*coughs*`, and leftover
/// punctuation, removed. Empty means nothing was said.
pub fn clean_transcript(text: &str) -> String {
    let mut out = String::new();
    let mut close: Option<char> = None;
    for c in text.chars() {
        match (close, c) {
            (Some(end), c) if c == end => close = None,
            (Some(_), _) => {}
            (None, '[') => close = Some(']'),
            (None, '(') => close = Some(')'),
            (None, '*') => close = Some('*'),
            (None, c) => out.push(c),
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().any(char::is_alphanumeric) {
        out
    } else {
        String::new()
    }
}

#[cfg(test)]
pub fn utterance(source: Source, said: &str) -> Utterance {
    Utterance {
        user: UserId::new(7),
        channel: ChannelId::new(9),
        source,
        author: "Sam".into(),
        said: said.into(),
        at: DateTime::UNIX_EPOCH,
        message: None,
        in_reply_to: None,
        recent: vec![],
        in_call: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn lines_show_when_they_were_said() {
        let line = Line {
            speaker: "Alex".into(),
            said: "hi".into(),
            at: "2026-11-07T14:58:03.5Z".parse().unwrap(),
        };
        assert_eq!(
            serde_json::to_value(line).unwrap(),
            serde_json::json!({ "speaker": "Alex", "said": "hi", "at": "2026-11-07 14:58:03 UTC" })
        );
    }

    #[test]
    fn threshold_is_inclusive() {
        assert!(is_hostile(0.7, 0.7));
        assert!(is_hostile(0.9, 0.7));
        assert!(!is_hostile(0.69, 0.7));
    }

    #[test]
    fn not_nice_needs_both_aimed_and_a_put_down() {
        let score = |aimed, put_down| Verdict { aimed, put_down }.score();
        assert_eq!(score(0.1, 0.95), 0.1);
        assert_eq!(score(0.95, 0.2), 0.2);
        assert_eq!(score(0.9, 0.8), 0.8);
    }

    #[test]
    fn asks_both_questions_about_what_the_source_said() {
        let asked = |source| {
            let qs = serde_json::to_value(BTreeMap::from(questions(source))).unwrap();
            let instructions = |id: &str| qs[id]["instructions"].as_str().unwrap().to_owned();
            assert!(qs[AIMED]["criteria"]["true"].is_string());
            assert!(qs[PUT_DOWN]["criteria"]["false"].is_string());
            [instructions(AIMED), instructions(PUT_DOWN)]
        };
        for q in asked(Source::Message) {
            assert!(q.contains("`message_text`"), "{q}");
        }
        assert_eq!(asked(Source::Edit), asked(Source::Message));
        for q in asked(Source::Voice) {
            assert!(q.contains("`said_now`"), "{q}");
        }
        for q in asked(Source::Reaction) {
            assert!(q.contains("`reaction`"), "{q}");
        }
    }

    #[test]
    fn quotes_every_line_and_cuts_long_ones() {
        assert_eq!(quote("  you suck\nreally  "), "> you suck\n> really");
        assert_eq!(quote(""), "> …");
        let long = "a".repeat(MAX_QUOTE + 10);
        let q = quote(&long);
        assert!(q.ends_with('…'));
        assert_eq!(q.chars().count(), 2 + MAX_QUOTE + 1);
        assert_eq!(quote(&"a".repeat(MAX_QUOTE)).chars().count(), 2 + MAX_QUOTE);
    }

    #[test]
    fn announcements_say_where() {
        assert_eq!(
            announcement(&utterance(Source::Voice, "shut up"), 0.874, false),
            "🚫 **Not nice!** (87% sure it's hostile) <@7> said in 🔊 <#9>:\n> Sam: **shut up**"
        );
        let say = |source| announcement(&utterance(source, "x"), 0.7, false);
        assert!(say(Source::Message).contains("said in <#9>:"));
        assert!(say(Source::Edit).contains("in an edit in <#9>"));
        let mut r = utterance(Source::Reaction, "🖕");
        r.in_reply_to = Some("I won".into());
        assert_eq!(
            announcement(&r, 1.0, false),
            "🚫 **Not nice!** (100% sure it's hostile) <@7> reacted 🖕 in <#9> to:\n> I won"
        );
    }

    #[test]
    fn shadow_announcements_say_nobody_was_timed_out() {
        assert_eq!(
            announcement(&utterance(Source::Message, "you suck"), 0.716, true),
            "👻 **Not nice** (72% sure it's hostile, shadow mode: not timed out) <@7> said in <#9>:\n\
             > you suck"
        );
    }

    #[test]
    fn quotes_the_conversation_before_and_bolds_what_was_said() {
        let line = |speaker: &str, said: &str| Line {
            speaker: speaker.into(),
            said: said.into(),
            at: DateTime::UNIX_EPOCH,
        };
        let mut u = utterance(Source::Voice, "stop gaslighting me");
        u.recent = vec![
            line("Jo", "too early"),
            line("Alex", "I never said that"),
            line("Jo", "he did say it"),
            line("Alex", &"a".repeat(MAX_CONTEXT_LINE + 1)),
        ];
        let long = format!("{}…", "a".repeat(MAX_CONTEXT_LINE));
        assert_eq!(
            quote_in_context(&u),
            format!(
                "> Alex: I never said that\n> Jo: he did say it\n> Alex: {long}\n\
                 > Sam: **stop gaslighting me**"
            )
        );
        assert_eq!(quote_in_context(&utterance(Source::Voice, " ")), "> Sam: …");
    }

    #[test]
    fn transcripts_lose_non_speech() {
        assert_eq!(clean_transcript(" [BLANK_AUDIO] "), "");
        assert_eq!(clean_transcript("(music) ..."), "");
        assert_eq!(clean_transcript("*coughs* you  idiot [noise]"), "you idiot");
        assert_eq!(clean_transcript("Hello there."), "Hello there.");
        assert_eq!(clean_transcript("unclosed [bracket"), "unclosed");
    }
}
