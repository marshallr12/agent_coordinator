//! Keeping a service's log readable while one thing keeps failing the same
//! way. A loop that looks at a launch every second and is refused every time
//! would otherwise write the same line every second: in the third phase 4c
//! machine run that was 43,847 lines in 12 hours.
//!
//! Each kind of line has its own key. The first line of a key is written.
//! The same text again under that key is held back and counted; once every
//! [`SUMMARY_EVERY`] the line is written again with the count held back
//! since. A different text under the key says how often the previous one
//! was held back, then is written itself.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How often a line that keeps repeating is written again with its count.
pub const SUMMARY_EVERY: Duration = Duration::from_secs(60);

/// The last line written under one key and what was held back since.
#[derive(Debug)]
struct Kept {
    text: String,
    held: u64,
    since: Instant,
}

/// The lines of a log, by key, deciding which to write.
#[derive(Debug, Default)]
pub struct Quiet {
    kept: BTreeMap<String, Kept>,
}

impl Quiet {
    /// The lines to write now for `text` under `key` at `now`; empty when
    /// the line is held back.
    pub fn lines(&mut self, key: &str, text: &str, now: Instant) -> Vec<String> {
        let Some(kept) = self.kept.get_mut(key) else {
            self.keep(key, text, now);
            return vec![text.to_owned()];
        };
        if kept.text != text {
            let mut lines = held_back(kept).into_iter().collect::<Vec<_>>();
            lines.push(text.to_owned());
            self.keep(key, text, now);
            return lines;
        }
        kept.held += 1;
        if now.duration_since(kept.since) < SUMMARY_EVERY {
            return Vec::new();
        }
        let line = repeated(kept, now);
        (kept.held, kept.since) = (0, now);
        vec![line]
    }

    /// Forgets what was written under `key`, so its next line is written
    /// at once: the condition it reported is over.
    pub fn clear(&mut self, key: &str) {
        self.kept.remove(key);
    }

    /// Starts counting `text` under `key` from `now`.
    fn keep(&mut self, key: &str, text: &str, now: Instant) {
        let kept = Kept {
            text: text.to_owned(),
            held: 0,
            since: now,
        };
        self.kept.insert(key.to_owned(), kept);
    }
}

/// The line written again after a [`SUMMARY_EVERY`] of repeats.
fn repeated(kept: &Kept, now: Instant) -> String {
    let seconds = now.duration_since(kept.since).as_secs();
    format!(
        "{} (repeated {} more times in the last {seconds} s)",
        kept.text, kept.held
    )
}

/// What a line that is being replaced was held back, if anything.
fn held_back(kept: &Kept) -> Option<String> {
    (kept.held > 0).then(|| {
        format!(
            "the previous line was repeated {} more times: {}",
            kept.held, kept.text
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_line_is_written_once_then_once_a_minute_with_its_count() {
        let (mut quiet, start) = (Quiet::default(), Instant::now());
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        assert_eq!(quiet.lines("observe", "refused", at(0)), ["refused"]);
        for second in 1..60 {
            assert!(quiet.lines("observe", "refused", at(second)).is_empty());
        }
        assert_eq!(
            quiet.lines("observe", "refused", at(60)),
            ["refused (repeated 60 more times in the last 60 s)"]
        );
        assert!(quiet.lines("observe", "refused", at(61)).is_empty());
    }

    #[test]
    fn a_new_line_says_what_was_held_back_and_keys_do_not_mix() {
        let (mut quiet, start) = (Quiet::default(), Instant::now());
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        quiet.lines("observe", "refused", at(0));
        assert!(quiet.lines("observe", "refused", at(1)).is_empty());
        assert_eq!(quiet.lines("stop", "refused", at(2)), ["refused"]);
        assert_eq!(
            quiet.lines("observe", "the pipe broke", at(3)),
            [
                "the previous line was repeated 1 more times: refused",
                "the pipe broke"
            ]
        );
        quiet.clear("observe");
        assert_eq!(
            quiet.lines("observe", "the pipe broke", at(4)),
            ["the pipe broke"]
        );
    }
}
