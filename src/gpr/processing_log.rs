//! The lines of a radargram's `processing_log`: one per step that ran, and
//! one per merge, in order.
//!
//! The log is written to every processed file and read back from files of
//! any age, so its line format is a file format. A line is
//!
//! ```text
//! {name} (duration: {seconds:.2}s):\t{event}
//! ```
//!
//! which it has been since 2022. `name` is the step's name, or a longer
//! form of it (`zero_corr(coppens, trace)`), and `event` is what the step
//! said it did. Files from before then have `duration: {:?}\t{event}`
//! lines, without a name.

use std::time::Duration;

/// Format one log line.
pub(crate) fn line(name: &str, duration: Duration, event: &str) -> String {
    format!(
        "{name} (duration: {:.2}s):\t{event}",
        duration.as_secs_f32()
    )
}

/// One log line, taken apart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry<'a> {
    /// Who wrote the line, or `None` for a line without a name: one from
    /// before 2022, or one that something other than Ridal wrote.
    pub name: Option<&'a str>,
    /// How long it took, when the line says so in seconds.
    pub seconds: Option<f32>,
    /// What happened, in words.
    pub event: &'a str,
}

/// Take a log line apart.
///
/// Never fails: a line in neither format is all event, so a reader that
/// looks for a name skips it and one that shows the log still has the text.
pub fn parse(line: &str) -> Entry<'_> {
    let unnamed = Entry {
        name: None,
        seconds: None,
        event: line,
    };
    let Some((head, event)) = line.split_once('\t') else {
        return unnamed;
    };
    if head.starts_with("duration: ") {
        return Entry { event, ..unnamed };
    }
    // The name may hold parentheses and spaces of its own, so the duration
    // is the suffix, not the first parenthesis.
    let Some((name, duration)) = head
        .strip_suffix("s):")
        .and_then(|head| head.rsplit_once(" (duration: "))
    else {
        return unnamed;
    };
    Entry {
        name: Some(name),
        seconds: duration.parse().ok(),
        event,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_line_parses_back() {
        let written = line("bandpass", Duration::from_millis(1234), "Applied a filter");
        assert_eq!(written, "bandpass (duration: 1.23s):\tApplied a filter");
        assert_eq!(
            parse(&written),
            Entry {
                name: Some("bandpass"),
                seconds: Some(1.23),
                event: "Applied a filter",
            }
        );
    }

    #[test]
    fn an_event_may_contain_tabs_and_the_format_itself() {
        let entry = parse("merge (duration: 0.00s):\tMerged \"a (duration: 1s):\tb.rd3\"");
        assert_eq!(entry.name, Some("merge"));
        assert_eq!(entry.event, "Merged \"a (duration: 1s):\tb.rd3\"");
    }

    #[test]
    fn lines_from_before_2022_have_no_name() {
        assert_eq!(
            parse("duration: 12.5ms\tApplied gain of 0.1 dB / ns"),
            Entry {
                name: None,
                seconds: None,
                event: "Applied gain of 0.1 dB / ns",
            }
        );
    }

    #[test]
    fn a_line_in_no_known_format_is_all_event() {
        for text in ["Elsewhere: bandpass", "no tab (duration: 1s): here", "x\ty"] {
            assert_eq!(parse(text).name, None, "{text}");
            assert_eq!(parse(text).event, text, "{text}");
        }
    }
}
