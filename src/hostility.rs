//! What counts as not nice ("hostile" in the code and the activity log), and how it's
//! reported. Pure: no Discord or network calls.

use poise::serenity_prelude::{ChannelId, MessageId, UserId};
use serde::Serialize;

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

/// One thing said in the voice call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Line {
    pub speaker: String,
    pub said: String,
}

/// The voice call around something said: who said it, who's there, and what came just before.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Call {
    pub speaker: String,
    /// Display names of everyone in the call (bots excluded), speaker included.
    pub in_call: Vec<String>,
    /// What participants said just before, oldest first.
    pub recent: Vec<Line>,
}

/// Something a participant said, ready to judge.
#[derive(Clone, Debug)]
pub struct Utterance {
    pub user: UserId,
    pub channel: ChannelId,
    pub source: Source,
    pub said: String,
    /// The chat message, for text; for a reaction, the message reacted to.
    pub message: Option<MessageId>,
    /// What the message replies to (or the reaction is on), as context for the judge.
    pub in_reply_to: Option<String>,
    /// For voice: the call it was said in.
    pub call: Option<Call>,
}

const TEXT_QUESTION: &str = "The state is a Discord chat message (possibly with attached images, \
    and the message it replies to for context) from a member of a community server that has a \
    be-nice-to-each-other rule this month. Is the author of `message_text` being not nice to \
    someone?";
const VOICE_QUESTION: &str = "The state is a moment in a Discord voice call on a community server \
    that has a be-nice-to-each-other rule this month. `speaker` just said `said_now`. \
    `people_in_call` is who's there, and `recent_conversation` is what some of them said just \
    before (it may be missing lines). Everything is speech-to-text and may contain mishearings. \
    In `said_now`, was `speaker` not nice to someone?";
const REACTION_QUESTION: &str = "The state is an emoji `reaction` that a member of a community \
    server with a be-nice-to-each-other rule this month put on someone else's Discord message \
    (`on_message`, possibly with images). Custom emoji are given by name, like `:clown:`. Is the \
    member who reacted being not nice to someone with this reaction?";
pub const YES: &str = "Not nice: insulting, mocking, belittling or putting someone down, \
    name-calling, slurs, threats, telling someone to shut up, go away or die, rudely dismissing \
    them, or contempt aimed at a person or group, including mean comments passed off as jokes";
pub const NO: &str = "Nice or neutral: friendly talk and banter that isn't at anyone's expense, \
    swearing that isn't aimed at anyone (\"this game is shit\"), respectful disagreement, \
    self-deprecation, or quoting or talking about someone else being mean without joining in";

pub fn question(source: Source) -> &'static str {
    match source {
        Source::Message | Source::Edit => TEXT_QUESTION,
        Source::Voice => VOICE_QUESTION,
        Source::Reaction => REACTION_QUESTION,
    }
}

pub fn is_hostile(score: f32, threshold: f32) -> bool {
    score >= threshold
}

/// Longest quote posted, in characters; longer ones are cut with an ellipsis.
const MAX_QUOTE: usize = 1500;

/// `said` as a Discord block quote, cut to [`MAX_QUOTE`].
pub fn quote(said: &str) -> String {
    let said = said.trim();
    let mut text: String = said.chars().take(MAX_QUOTE).collect();
    if said.chars().count() > MAX_QUOTE {
        text.push('…');
    }
    if text.is_empty() {
        return "> …".into();
    }
    text.lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The public call-out posted to the announce channel, with Jev's score as a percentage. In
/// shadow mode it says nobody was timed out.
pub fn announcement(u: &Utterance, score: f32, shadow: bool) -> String {
    let (did, quoted) = match u.source {
        Source::Voice => (format!("said in 🔊 <#{}>", u.channel), u.said.as_str()),
        Source::Message => (format!("said in <#{}>", u.channel), u.said.as_str()),
        Source::Edit => (
            format!("said in an edit in <#{}>", u.channel),
            u.said.as_str(),
        ),
        Source::Reaction => (
            format!("reacted {} in <#{}> to", u.said, u.channel),
            u.in_reply_to.as_deref().unwrap_or_default(),
        ),
    };
    let percent = format!("{:.0}% hostile", score * 100.0);
    let verdict = if shadow {
        format!("👻 **Not nice** ({percent}, shadow mode: not timed out)")
    } else {
        format!("🚫 **Not nice!** ({percent})")
    };
    format!("{verdict} <@{}> {did}:\n{}", u.user, quote(quoted))
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
mod tests {
    use super::*;

    fn utterance(source: Source, said: &str) -> Utterance {
        Utterance {
            user: UserId::new(7),
            channel: ChannelId::new(9),
            source,
            said: said.into(),
            message: None,
            in_reply_to: None,
            call: None,
        }
    }

    #[test]
    fn threshold_is_inclusive() {
        assert!(is_hostile(0.7, 0.7));
        assert!(is_hostile(0.9, 0.7));
        assert!(!is_hostile(0.69, 0.7));
    }

    #[test]
    fn questions_match_the_source() {
        assert_eq!(question(Source::Message), TEXT_QUESTION);
        assert_eq!(question(Source::Edit), TEXT_QUESTION);
        assert_eq!(question(Source::Voice), VOICE_QUESTION);
        assert_eq!(question(Source::Reaction), REACTION_QUESTION);
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
            "🚫 **Not nice!** (87% hostile) <@7> said in 🔊 <#9>:\n> shut up"
        );
        let say = |source| announcement(&utterance(source, "x"), 0.7, false);
        assert!(say(Source::Message).contains("said in <#9>:"));
        assert!(say(Source::Edit).contains("in an edit in <#9>"));
        let mut r = utterance(Source::Reaction, "🖕");
        r.in_reply_to = Some("I won".into());
        assert_eq!(
            announcement(&r, 1.0, false),
            "🚫 **Not nice!** (100% hostile) <@7> reacted 🖕 in <#9> to:\n> I won"
        );
    }

    #[test]
    fn shadow_announcements_say_nobody_was_timed_out() {
        assert_eq!(
            announcement(&utterance(Source::Message, "you suck"), 0.716, true),
            "👻 **Not nice** (72% hostile, shadow mode: not timed out) <@7> said in <#9>:\n\
             > you suck"
        );
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
