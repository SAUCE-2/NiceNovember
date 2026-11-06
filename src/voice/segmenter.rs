//! Cuts each speaker's continuous audio into segments, one per thing they said, by waiting for
//! them to go quiet. Pure: fed one 20 ms tick at a time, with whoever sent audio in it.

use std::collections::HashMap;
use std::hash::Hash;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// A segment ends after this many quiet ticks in a row.
    pub end_after_quiet: u32,
    /// A segment is cut once it holds this many samples, even mid-sentence.
    pub max_samples: usize,
    /// Segments with fewer loud ticks than this (a click, a cough) are dropped.
    pub min_loud: u32,
    /// A tick whose mean absolute amplitude is below this counts as quiet.
    pub quiet_level: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            end_after_quiet: 40,      // 0.8 s
            max_samples: 16_000 * 15, // 15 s
            min_loud: 10,             // 0.2 s
            quiet_level: 150,
        }
    }
}

#[derive(Default)]
struct Open {
    samples: Vec<i16>,
    quiet_run: u32,
    loud: u32,
}

pub struct Segmenter<K> {
    limits: Limits,
    open: HashMap<K, Open>,
}

pub fn is_quiet(pcm: &[i16], level: u32) -> bool {
    if pcm.is_empty() {
        return true;
    }
    let total: u64 = pcm.iter().map(|s| s.unsigned_abs() as u64).sum();
    total / (pcm.len() as u64) < level as u64
}

impl<K: Copy + Eq + Hash> Segmenter<K> {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            open: HashMap::new(),
        }
    }

    /// One tick: the audio of everyone who sent some. Returns the segments that finished.
    pub fn tick(&mut self, frames: &[(K, &[i16])]) -> Vec<(K, Vec<i16>)> {
        let quiet_level = self.limits.quiet_level;
        for (k, pcm) in frames {
            let quiet = is_quiet(pcm, quiet_level);
            if quiet && !self.open.contains_key(k) {
                continue; // silence doesn't start a segment
            }
            let seg = self.open.entry(*k).or_default();
            seg.samples.extend_from_slice(pcm);
            if quiet {
                seg.quiet_run += 1;
            } else {
                seg.quiet_run = 0;
                seg.loud += 1;
            }
        }
        for (k, seg) in self.open.iter_mut() {
            if !frames.iter().any(|(f, _)| f == k) {
                seg.quiet_run += 1; // sent nothing at all: Discord stops sending when you stop
            }
        }
        let done: Vec<K> = self
            .open
            .iter()
            .filter(|(_, s)| {
                s.quiet_run >= self.limits.end_after_quiet
                    || s.samples.len() >= self.limits.max_samples
            })
            .map(|(k, _)| *k)
            .collect();
        done.into_iter().filter_map(|k| self.finish(k)).collect()
    }

    /// The speaker left: whatever they were saying is finished now.
    pub fn flush(&mut self, k: K) -> Option<(K, Vec<i16>)> {
        self.finish(k)
    }

    fn finish(&mut self, k: K) -> Option<(K, Vec<i16>)> {
        let seg = self.open.remove(&k)?;
        (seg.loud >= self.limits.min_loud).then_some((k, seg.samples))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples in one 20 ms tick of 16 kHz mono.
    const TICK_SAMPLES: usize = 320;
    const LOUD: [i16; TICK_SAMPLES] = [1000; TICK_SAMPLES];
    const QUIET: [i16; TICK_SAMPLES] = [10; TICK_SAMPLES];

    fn seg() -> Segmenter<u32> {
        Segmenter::new(Limits {
            end_after_quiet: 3,
            max_samples: TICK_SAMPLES * 10,
            min_loud: 2,
            quiet_level: 150,
        })
    }

    fn run(s: &mut Segmenter<u32>, ticks: &[&[(u32, &[i16])]]) -> Vec<(u32, usize)> {
        ticks
            .iter()
            .flat_map(|t| s.tick(t))
            .map(|(k, v)| (k, v.len() / TICK_SAMPLES))
            .collect()
    }

    #[test]
    fn quietness() {
        assert!(is_quiet(&[], 150));
        assert!(is_quiet(&QUIET, 150));
        assert!(!is_quiet(&LOUD, 150));
        assert!(!is_quiet(&[-1000; 4], 150), "negative samples count too");
        assert!(!is_quiet(&[150; 4], 150), "level itself is loud");
    }

    #[test]
    fn ends_after_going_quiet_or_silent() {
        let mut s = seg();
        let l: &[i16] = &LOUD;
        let q: &[i16] = &QUIET;
        // Speak 2 ticks, then 3 quiet frames: one 5-tick segment.
        let out = run(&mut s, &[&[(1, l)], &[(1, l)], &[(1, q)], &[(1, q)]]);
        assert!(out.is_empty());
        assert_eq!(s.open.len(), 1);
        assert_eq!(run(&mut s, &[&[(1, q)]]), vec![(1, 5)]);
        assert_eq!(s.open.len(), 0);

        // Speak 2 ticks, then send nothing: ends 3 ticks later, holding only the audio.
        let out = run(&mut s, &[&[(1, l)], &[(1, l)], &[], &[], &[]]);
        assert_eq!(out, vec![(1, 2)]);
    }

    #[test]
    fn talking_again_keeps_the_segment_open() {
        let mut s = seg();
        let (l, q): (&[i16], &[i16]) = (&LOUD, &QUIET);
        let out = run(
            &mut s,
            &[&[(1, l)], &[(1, q)], &[(1, q)], &[(1, l)], &[], &[], &[]],
        );
        assert_eq!(out, vec![(1, 4)]);
    }

    #[test]
    fn silence_alone_and_blips_produce_nothing() {
        let mut s = seg();
        let (l, q): (&[i16], &[i16]) = (&LOUD, &QUIET);
        assert!(run(&mut s, &[&[(1, q)], &[(1, q)]]).is_empty());
        assert_eq!(s.open.len(), 0, "quiet doesn't open a segment");
        assert!(
            run(&mut s, &[&[(1, l)], &[], &[], &[]]).is_empty(),
            "one loud tick"
        );
        assert_eq!(s.open.len(), 0);
    }

    #[test]
    fn long_speech_is_cut_at_the_limit() {
        let mut s = seg();
        let l: &[i16] = &LOUD;
        let tick = [(1, l)];
        let ticks: Vec<&[(u32, &[i16])]> = vec![&tick; 10];
        let out = run(&mut s, &ticks);
        assert_eq!(out, vec![(1, 10)]);
    }

    #[test]
    fn speakers_are_separate_and_flush_on_leaving() {
        let mut s = seg();
        let l: &[i16] = &LOUD;
        let out = run(&mut s, &[&[(1, l), (2, l)], &[(1, l), (2, l)], &[(1, l)]]);
        assert!(out.is_empty());
        assert_eq!(s.open.len(), 2);
        assert_eq!(
            s.flush(2).map(|(k, v)| (k, v.len())),
            Some((2, 2 * TICK_SAMPLES))
        );
        assert_eq!(s.flush(2), None);
        assert_eq!(run(&mut s, &[&[], &[], &[]]), vec![(1, 3)]);
    }
}
