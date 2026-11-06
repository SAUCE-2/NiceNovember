//! The last few things said in the monitored channel, so each new line is judged in context:
//! "shut up" answering a joke reads differently from "shut up" out of nowhere. Pure.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::hostility::Line;

/// Lines older than this aren't context any more.
pub const WINDOW: Duration = Duration::from_secs(45);
/// At most this many lines of context.
pub const MAX_LINES: usize = 8;

#[derive(Default)]
pub struct Conversation {
    lines: VecDeque<(Instant, Line)>,
}

impl Conversation {
    /// What was said in the [`WINDOW`] before `now` (up to [`MAX_LINES`], oldest first), then
    /// remember `line` as said at `now`.
    pub fn next(&mut self, now: Instant, line: Line) -> Vec<Line> {
        self.lines
            .retain(|(at, _)| now.saturating_duration_since(*at) <= WINDOW);
        let skip = self.lines.len().saturating_sub(MAX_LINES);
        let context = self
            .lines
            .iter()
            .skip(skip)
            .map(|(_, l)| l.clone())
            .collect();
        self.lines.push_back((now, line));
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
        context
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(said: &str) -> Line {
        Line {
            speaker: "sam".into(),
            said: said.into(),
        }
    }

    fn saids(lines: &[Line]) -> Vec<&str> {
        lines.iter().map(|l| l.said.as_str()).collect()
    }

    #[test]
    fn context_is_what_came_before() {
        let mut c = Conversation::default();
        let t = Instant::now();
        assert!(c.next(t, line("a")).is_empty());
        assert_eq!(saids(&c.next(t, line("b"))), ["a"]);
        assert_eq!(
            saids(&c.next(t + WINDOW, line("c"))),
            ["a", "b"],
            "window is inclusive"
        );
        let later = t + WINDOW + Duration::from_secs(1);
        assert_eq!(
            saids(&c.next(later, line("d"))),
            ["c"],
            "a and b are too old"
        );
    }

    #[test]
    fn keeps_only_the_latest_lines() {
        let mut c = Conversation::default();
        let t = Instant::now();
        for i in 0..20 {
            c.next(t, line(&i.to_string()));
        }
        let context = c.next(t, line("x"));
        assert_eq!(context.len(), MAX_LINES);
        assert_eq!(context.first().unwrap().said, "12");
        assert_eq!(context.last().unwrap().said, "19");
        assert_eq!(c.lines.len(), MAX_LINES, "memory stays bounded");
    }
}
