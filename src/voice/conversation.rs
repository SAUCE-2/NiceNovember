//! The last few things said in the monitored channel, so each new line is judged in context:
//! "shut up" answering a joke reads differently from "shut up" out of nowhere. Pure.

use std::collections::VecDeque;

use chrono::TimeDelta;

use crate::hostility::Line;

/// Lines older than this aren't context any more.
pub const WINDOW: TimeDelta = TimeDelta::seconds(45);
/// At most this many lines of context.
pub const MAX_LINES: usize = 8;

#[derive(Default)]
pub struct Conversation {
    lines: VecDeque<Line>,
}

impl Conversation {
    /// What was said in the [`WINDOW`] before `line` (up to [`MAX_LINES`], oldest first), then
    /// remember `line`.
    pub fn next(&mut self, line: Line) -> Vec<Line> {
        self.lines.retain(|l| line.at - l.at <= WINDOW);
        let skip = self.lines.len().saturating_sub(MAX_LINES);
        let context = self.lines.iter().skip(skip).cloned().collect();
        self.lines.push_back(line);
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
        context
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn line(said: &str, at: DateTime<Utc>) -> Line {
        Line {
            speaker: "sam".into(),
            said: said.into(),
            at,
        }
    }

    fn saids(lines: &[Line]) -> Vec<&str> {
        lines.iter().map(|l| l.said.as_str()).collect()
    }

    #[test]
    fn context_is_what_came_before() {
        let mut c = Conversation::default();
        let t = DateTime::UNIX_EPOCH;
        assert!(c.next(line("a", t)).is_empty());
        assert_eq!(saids(&c.next(line("b", t))), ["a"]);
        assert_eq!(
            saids(&c.next(line("c", t + WINDOW))),
            ["a", "b"],
            "window is inclusive"
        );
        let later = t + WINDOW + TimeDelta::seconds(1);
        assert_eq!(
            saids(&c.next(line("d", later))),
            ["c"],
            "a and b are too old"
        );
    }

    #[test]
    fn keeps_only_the_latest_lines() {
        let mut c = Conversation::default();
        let t = DateTime::UNIX_EPOCH;
        for i in 0..20 {
            c.next(line(&i.to_string(), t));
        }
        let context = c.next(line("x", t));
        assert_eq!(context.len(), MAX_LINES);
        assert_eq!(context.first().unwrap().said, "12");
        assert_eq!(context.last().unwrap().said, "19");
        assert_eq!(c.lines.len(), MAX_LINES, "memory stays bounded");
    }
}
