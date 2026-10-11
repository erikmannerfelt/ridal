//! Radargram id and display name patterns (#368).
//!
//! A pattern is literal text with placeholders, rendered once per output of
//! a batch: `dronbreen-2026-{freq}mhz-{n:02, per=freq}` gives
//! `dronbreen-2026-100mhz-01`, `dronbreen-2026-100mhz-02` and
//! `dronbreen-2026-800mhz-01` for two 100 MHz outputs and one at 800 MHz.
//! `ridal batch-process --id-pattern`/`--display-name-pattern` and, later,
//! the server's ingest planner (#382) both render through here, so the two
//! cannot disagree.
//!
//! | Placeholder | Value |
//! |---|---|
//! | `{stem}` | the first input's file stem (slugified in an id) |
//! | `{n}` | a counter over the outputs in chronological order, from 1 |
//! | `{freq}` | antenna frequency in MHz, rounded |
//! | `{nfiles}` | how many inputs were merged into the output |
//! | `{date}` | the start date, `YYYYMMDD` |
//! | `{group}` | the group id |
//! | `{id}`, `{group_name}` | display names only: the rendered id, and the group's name |
//!
//! `{n}`, `{freq}` and `{nfiles}` take a zero-padded width, `{n:02}`. `{n}`
//! also takes `from=15` (start at 15), `from=next` (continue after the
//! highest number among existing ids that match), and `per=freq` or
//! `per=freq+date` (one counter for each value of those). `{{` and `}}` are
//! literal braces.
//!
//! An id pattern is checked when it is parsed: its literal text may only
//! hold what an id may (`[a-z0-9_-]`; uppercase is refused, not lowercased),
//! so that every rendered id is valid by construction except for its length
//! and ends, which are checked per output. A display name pattern may hold
//! any text, and its `{n}` is the number in the output's id unless it is
//! given options of its own.

use std::collections::HashMap;

use crate::identity::{DisplayName, GroupId, GroupName, RadargramId};

/// Which of the two a pattern renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    Id,
    DisplayName,
}

impl PatternKind {
    fn flag(self) -> &'static str {
        match self {
            PatternKind::Id => "id pattern",
            PatternKind::DisplayName => "display name pattern",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Field {
    Stem,
    N,
    Freq,
    NFiles,
    Date,
    Group,
    Id,
    GroupName,
}

impl Field {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "stem" => Field::Stem,
            "n" => Field::N,
            "freq" => Field::Freq,
            "nfiles" => Field::NFiles,
            "date" => Field::Date,
            "group" => Field::Group,
            "id" => Field::Id,
            "group_name" => Field::GroupName,
            _ => return None,
        })
    }

    fn padded(self) -> bool {
        matches!(self, Field::N | Field::Freq | Field::NFiles)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    At(u64),
    Next,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Placeholder {
    field: Field,
    width: Option<usize>,
    from: Option<Start>,
    per: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Placeholder(Placeholder),
}

/// A parsed pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    kind: PatternKind,
    segments: Vec<Segment>,
}

/// Why a pattern was refused, with where in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError {
    pub message: String,
    /// Byte range in the pattern.
    pub span: (usize, usize),
}

impl PatternError {
    /// The message with the offending part of `source` underlined, as step
    /// errors are.
    pub fn render(&self, source: &str) -> String {
        let start = source[..self.span.0].chars().count();
        let width = source[self.span.0..self.span.1].chars().count().max(1);
        format!(
            "{}\n  {}\n  {}{}",
            self.message,
            source,
            " ".repeat(start),
            "^".repeat(width)
        )
    }
}

fn error(message: impl Into<String>, start: usize, end: usize) -> PatternError {
    PatternError {
        message: message.into(),
        span: (start, end),
    }
}

impl Pattern {
    pub fn parse(source: &str, kind: PatternKind) -> Result<Self, PatternError> {
        let mut segments = Vec::new();
        let mut literal = String::new();
        let mut chars = source.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            match c {
                '{' if chars.peek().map(|(_, c)| *c) == Some('{') => {
                    chars.next();
                    literal.push('{');
                }
                '}' if chars.peek().map(|(_, c)| *c) == Some('}') => {
                    chars.next();
                    literal.push('}');
                }
                '}' => {
                    return Err(error(
                        "a `}` that closes nothing; write `}}` for a literal brace",
                        at,
                        at + 1,
                    ))
                }
                '{' => {
                    let Some(end) = source[at..].find('}').map(|i| at + i) else {
                        return Err(error("a `{` that is never closed", at, at + 1));
                    };
                    if !literal.is_empty() {
                        segments.push(Segment::Literal(std::mem::take(&mut literal)));
                    }
                    let placeholder = parse_placeholder(source, at, end, kind)?;
                    segments.push(Segment::Placeholder(placeholder));
                    while chars.peek().is_some_and(|(i, _)| *i <= end) {
                        chars.next();
                    }
                }
                c => {
                    if kind == PatternKind::Id {
                        check_id_literal(source, c, at)?;
                    }
                    literal.push(c);
                }
            }
        }
        if !literal.is_empty() {
            segments.push(Segment::Literal(literal));
        }
        let counters = segments
            .iter()
            .filter(|s| matches!(s, Segment::Placeholder(p) if p.field == Field::N))
            .count();
        if counters > 1 {
            return Err(error("a pattern can have only one `{n}`", 0, source.len()));
        }
        if segments.is_empty() {
            return Err(error(format!("the {} is empty", kind.flag()), 0, 0));
        }
        Ok(Self { kind, segments })
    }

    fn counter(&self) -> Option<&Placeholder> {
        self.segments.iter().find_map(|s| match s {
            Segment::Placeholder(p) if p.field == Field::N => Some(p),
            _ => None,
        })
    }

    /// That `batch` has what the pattern uses of it: a group, for
    /// `{group}` and `{group_name}`. Said once for the pattern rather than
    /// once per output.
    fn check_batch(&self, batch: &BatchFacts) -> Result<(), String> {
        for segment in &self.segments {
            let Segment::Placeholder(p) = segment else {
                continue;
            };
            let fields = std::iter::once(p.field).chain(p.per.iter().copied());
            for field in fields {
                if field == Field::Group && batch.group_id.is_none()
                    || field == Field::GroupName && batch.group_name.is_none()
                {
                    return Err(format!(
                        "the pattern uses `{{{}}}`, but no group was given",
                        if field == Field::Group {
                            "group"
                        } else {
                            "group_name"
                        }
                    ));
                }
            }
        }
        Ok(())
    }

    /// Whether rendering needs the ids that already exist, for `from=next`.
    pub fn needs_existing_ids(&self) -> bool {
        self.counter().is_some_and(|p| p.from == Some(Start::Next))
    }
}

fn check_id_literal(source: &str, c: char, at: usize) -> Result<(), PatternError> {
    if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' {
        return Ok(());
    }
    let end = at + c.len_utf8();
    if c.is_uppercase() {
        // The whole word it is in, lowercased, is the fix to suggest:
        // `mhz` for `MHz`, not `m`.
        let word_start = source[..at]
            .rfind(|c: char| !c.is_alphanumeric())
            .map_or(0, |i| i + 1);
        let word_end = source[at..]
            .find(|c: char| !c.is_alphanumeric())
            .map_or(source.len(), |i| at + i);
        return Err(error(
            format!(
                "an id cannot contain uppercase letters; write `{}`",
                source[word_start..word_end].to_lowercase()
            ),
            word_start,
            word_end,
        ));
    }
    Err(error(
        format!("an id cannot contain `{c}`: only lowercase ASCII letters, digits, `-` and `_`"),
        at,
        end,
    ))
}

/// One `{...}`, from the `{` at `open` to the `}` at `close`.
fn parse_placeholder(
    source: &str,
    open: usize,
    close: usize,
    kind: PatternKind,
) -> Result<Placeholder, PatternError> {
    let whole = |message: String| error(message, open, close + 1);
    let inner = &source[open + 1..close];
    let mut parts = inner.split(',');
    let head = parts.next().unwrap_or_default().trim();
    let (name, spec) = match head.split_once(':') {
        Some((name, spec)) => (name.trim(), Some(spec.trim())),
        None => (head, None),
    };
    let field = Field::parse(name).ok_or_else(|| {
        whole(format!(
            "`{{{name}}}` is not a placeholder; there are `{{stem}}`, `{{n}}`, `{{freq}}`, \
             `{{nfiles}}`, `{{date}}` and `{{group}}`{}",
            if kind == PatternKind::DisplayName {
                ", and `{id}` and `{group_name}` in a display name"
            } else {
                ""
            }
        ))
    })?;
    if kind == PatternKind::Id && matches!(field, Field::Id | Field::GroupName) {
        return Err(whole(format!(
            "`{{{name}}}` is only for display name patterns"
        )));
    }
    let width = match spec {
        None => None,
        Some(spec) => {
            if !field.padded() {
                return Err(whole(format!("`{{{name}}}` takes no width")));
            }
            let digits = spec.strip_prefix('0').filter(|d| !d.is_empty());
            match digits.and_then(|d| d.parse::<usize>().ok()) {
                Some(width) if (1..=12).contains(&width) => Some(width),
                _ => {
                    return Err(whole(format!(
                        "`:{spec}` is not a width; write a zero and the number of digits, \
                         as in `{{{name}:02}}`"
                    )))
                }
            }
        }
    };
    let mut placeholder = Placeholder {
        field,
        width,
        from: None,
        per: Vec::new(),
    };
    for option in parts {
        let option = option.trim();
        let Some((key, value)) = option.split_once('=') else {
            return Err(whole(format!(
                "`{option}` is not an option; write `key=value`"
            )));
        };
        let (key, value) = (key.trim(), value.trim());
        if field != Field::N {
            return Err(whole(format!(
                "only `{{n}}` takes options, and `{{{name}}}` was given `{key}`"
            )));
        }
        match key {
            "from" => {
                placeholder.from = Some(match value {
                    "next" if kind == PatternKind::Id => Start::Next,
                    "next" => {
                        return Err(whole(
                            "`from=next` continues existing ids, so it is for id patterns".into(),
                        ))
                    }
                    number => Start::At(number.parse().map_err(|_| {
                        whole(format!("`from={number}` is neither a number nor `next`"))
                    })?),
                });
            }
            "per" => {
                for name in value.split('+').map(str::trim) {
                    match Field::parse(name) {
                        Some(Field::N) | None | Some(Field::Id) => {
                            return Err(whole(format!(
                                "`per={name}` is not something to count by; use `stem`, \
                                 `freq`, `nfiles`, `date` or `group`"
                            )))
                        }
                        Some(Field::GroupName) if kind == PatternKind::Id => {
                            return Err(whole(
                                "`per=group_name` is for display names; use `per=group`".into(),
                            ))
                        }
                        Some(other) => placeholder.per.push(other),
                    }
                }
            }
            other => {
                return Err(whole(format!(
                    "`{other}` is not an option of `{{n}}`; there are `from` and `per`"
                )))
            }
        }
    }
    Ok(placeholder)
}

/// What a pattern is rendered from, for one output.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputFacts {
    /// What error messages call the output: its first input's file name.
    pub label: String,
    /// The first input's file stem, as written.
    pub stem: String,
    /// The first trace's time, seconds since the Unix epoch.
    pub start_seconds: f64,
    pub antenna_mhz: f32,
    pub n_files: usize,
}

/// What every output of a batch shares.
#[derive(Debug, Clone, Default)]
pub struct BatchFacts {
    pub group_id: Option<GroupId>,
    pub group_name: Option<GroupName>,
}

fn pad(value: impl std::fmt::Display, width: Option<usize>) -> String {
    match width {
        Some(width) => format!("{value:0>width$}"),
        None => value.to_string(),
    }
}

/// One placeholder's value for one output, other than `{n}`.
fn value(
    field: Field,
    width: Option<usize>,
    kind: PatternKind,
    output: &OutputFacts,
    batch: &BatchFacts,
    id: Option<&RadargramId>,
) -> Result<String, String> {
    Ok(match field {
        Field::Stem => match kind {
            PatternKind::Id => {
                let slug = crate::identity::slug_fragment(&output.stem);
                if slug.is_empty() {
                    return Err(format!(
                        "{}: `{{stem}}` has no letters or digits to put in an id",
                        output.label
                    ));
                }
                slug
            }
            PatternKind::DisplayName => output.stem.clone(),
        },
        Field::Freq => pad(output.antenna_mhz.round() as i64, width),
        Field::NFiles => pad(output.n_files, width),
        Field::Date => chrono::DateTime::from_timestamp(output.start_seconds.floor() as i64, 0)
            .ok_or_else(|| format!("{}: its start time is not a date", output.label))?
            .format("%Y%m%d")
            .to_string(),
        Field::Group => batch
            .group_id
            .as_ref()
            .map(|id| id.to_string())
            .ok_or("the pattern uses `{group}`, but no group was given")?,
        Field::GroupName => batch
            .group_name
            .as_ref()
            .map(|name| name.to_string())
            .ok_or("the pattern uses `{group_name}`, but no group was given")?,
        Field::Id => id
            .map(|id| id.to_string())
            .ok_or("`{id}` is only for display name patterns")?,
        Field::N => unreachable!("the counter is assigned by `counters`"),
    })
}

/// The values a `per` counter is kept separately for.
fn partition(
    per: &[Field],
    kind: PatternKind,
    output: &OutputFacts,
    batch: &BatchFacts,
) -> Result<Vec<String>, String> {
    per.iter()
        .map(|field| value(*field, None, kind, output, batch, None))
        .collect()
}

/// The counter for each output, in output order: numbered in chronological
/// order, one sequence per `per` partition.
fn counters(
    pattern: &Pattern,
    outputs: &[OutputFacts],
    batch: &BatchFacts,
    existing: &[String],
) -> Result<Vec<Option<u64>>, String> {
    let Some(counter) = pattern.counter() else {
        return Ok(vec![None; outputs.len()]);
    };
    let mut order: Vec<usize> = (0..outputs.len()).collect();
    order.sort_by(|a, b| {
        outputs[*a]
            .start_seconds
            .total_cmp(&outputs[*b].start_seconds)
    });
    let mut next = HashMap::<Vec<String>, u64>::new();
    let mut out = vec![None; outputs.len()];
    for index in order {
        let output = &outputs[index];
        let key = partition(&counter.per, pattern.kind, output, batch)?;
        let n = match next.get(&key) {
            Some(n) => *n,
            None => match counter.from {
                None => 1,
                Some(Start::At(n)) => n,
                Some(Start::Next) => highest_existing(pattern, counter, output, batch, existing)?
                    .map_or(1, |n| n + 1),
            },
        };
        next.insert(key, n + 1);
        out[index] = Some(n);
    }
    Ok(out)
}

/// The highest counter among `existing` ids that match `pattern` with this
/// output's `per` values, for `from=next`.
///
/// Every other placeholder matches any run of id characters, since it
/// differs from output to output anyway.
fn highest_existing(
    pattern: &Pattern,
    counter: &Placeholder,
    output: &OutputFacts,
    batch: &BatchFacts,
    existing: &[String],
) -> Result<Option<u64>, String> {
    let mut pieces = Vec::new();
    for segment in &pattern.segments {
        pieces.push(match segment {
            Segment::Literal(text) => Piece::Exact(text.clone()),
            Segment::Placeholder(p) if p.field == Field::N => Piece::Number,
            Segment::Placeholder(p) if counter.per.contains(&p.field) => {
                Piece::Exact(value(p.field, p.width, pattern.kind, output, batch, None)?)
            }
            Segment::Placeholder(_) => Piece::Any,
        });
    }
    Ok(existing
        .iter()
        .filter_map(|id| matched_number(&pieces, id))
        .max())
}

#[derive(Debug)]
enum Piece {
    Exact(String),
    Number,
    Any,
}

/// The number `pieces` reads out of `text`, if `pieces` matches all of it.
/// A match without a number in it reads as 0; a pattern with `from=next`
/// always has one.
fn matched_number(pieces: &[Piece], text: &str) -> Option<u64> {
    match pieces.split_first() {
        None => text.is_empty().then_some(0),
        Some((Piece::Exact(literal), rest)) => {
            matched_number(rest, text.strip_prefix(literal.as_str())?)
        }
        Some((Piece::Number, rest)) => {
            let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            // Shortest first, so that a literal digit after the counter can
            // still match.
            (1..=digits).find_map(|end| {
                matched_number(rest, &text[end..])?;
                text[..end].parse().ok()
            })
        }
        // At least one character: a placeholder always renders to some.
        Some((Piece::Any, rest)) => text
            .char_indices()
            .map(|(i, _)| i)
            .skip(1)
            .chain((!text.is_empty()).then_some(text.len()))
            .find_map(|end| matched_number(rest, &text[end..])),
    }
}

fn render_one(
    pattern: &Pattern,
    output: &OutputFacts,
    batch: &BatchFacts,
    n: Option<u64>,
    id: Option<&RadargramId>,
) -> Result<String, String> {
    let mut out = String::new();
    for segment in &pattern.segments {
        match segment {
            Segment::Literal(text) => out.push_str(text),
            Segment::Placeholder(p) if p.field == Field::N => out.push_str(&pad(
                n.expect("assigned whenever there is a counter"),
                p.width,
            )),
            Segment::Placeholder(p) => {
                out.push_str(&value(p.field, p.width, pattern.kind, output, batch, id)?)
            }
        }
    }
    Ok(out)
}

/// Every output's id, in output order.
///
/// All of them are rendered and checked before any is returned: an id that
/// is not valid, or two outputs with the same id, is an error naming the
/// outputs, so that nothing is processed under a pattern that cannot work.
pub fn render_ids(
    pattern: &Pattern,
    outputs: &[OutputFacts],
    batch: &BatchFacts,
    existing: &[String],
) -> Result<Vec<RadargramId>, String> {
    let ids = render_each_id(pattern, outputs, batch, existing)?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    let mut seen = HashMap::<&str, &str>::new();
    for (output, id) in outputs.iter().zip(&ids) {
        if let Some(other) = seen.insert(id.as_str(), &output.label) {
            return Err(format!(
                "{other} and {} would both be '{id}'. Add something that tells them apart \
                 to the id pattern, such as `{{n}}`, or a `per=` that matches what it shows.",
                output.label
            ));
        }
    }
    Ok(ids)
}

/// Every output's id, in output order, each checked on its own but not
/// against the others: for the ingest planner (#382), which shows each
/// problem where it is rather than stopping at the first. The outer error
/// is one the whole pattern has, such as `{group}` with no group.
pub fn render_each_id(
    pattern: &Pattern,
    outputs: &[OutputFacts],
    batch: &BatchFacts,
    existing: &[String],
) -> Result<Vec<Result<RadargramId, String>>, String> {
    debug_assert_eq!(pattern.kind, PatternKind::Id);
    pattern.check_batch(batch)?;
    let numbers = counters(pattern, outputs, batch, existing)?;
    Ok(outputs
        .iter()
        .zip(numbers)
        .map(|(output, n)| {
            let text = render_one(pattern, output, batch, n, None)?;
            RadargramId::new(&text)
                .map_err(|e| format!("{}: the id pattern gives {e}", output.label))
        })
        .collect())
}

/// Every output's display name, in output order, given the ids they got
/// from `id_pattern` and the `existing` ids it was rendered against.
///
/// A bare `{n}` is the number in the output's id, so that "line 7" is
/// `line-07`. Given `from=` or `per=`, it is a counter of its own.
pub fn render_display_names(
    pattern: &Pattern,
    id_pattern: &Pattern,
    outputs: &[OutputFacts],
    batch: &BatchFacts,
    ids: &[RadargramId],
    existing: &[String],
) -> Result<Vec<Option<DisplayName>>, String> {
    debug_assert_eq!(pattern.kind, PatternKind::DisplayName);
    pattern.check_batch(batch)?;
    let own = pattern
        .counter()
        .is_some_and(|n| n.from.is_some() || !n.per.is_empty());
    let numbers = match id_pattern.counter() {
        Some(_) if !own => counters(id_pattern, outputs, batch, existing)?,
        _ => counters(pattern, outputs, batch, &[])?,
    };
    outputs
        .iter()
        .zip(numbers)
        .zip(ids)
        .map(|((output, n), id)| {
            render_one(pattern, output, batch, n, Some(id)).map(DisplayName::from_input)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(stem: &str, start: f64, mhz: f32, n_files: usize) -> OutputFacts {
        OutputFacts {
            label: format!("{stem}.rad"),
            stem: stem.to_string(),
            start_seconds: start,
            antenna_mhz: mhz,
            n_files,
        }
    }

    fn ids(
        pattern: &str,
        outputs: &[OutputFacts],
        existing: &[&str],
    ) -> Result<Vec<String>, String> {
        let pattern = Pattern::parse(pattern, PatternKind::Id).map_err(|e| e.render(pattern))?;
        let existing: Vec<String> = existing.iter().map(|s| s.to_string()).collect();
        let batch = BatchFacts {
            group_id: Some(GroupId::new("dronbreen").unwrap()),
            group_name: GroupName::from_input("Drønbreen"),
        };
        Ok(render_ids(&pattern, outputs, &batch, &existing)?
            .into_iter()
            .map(|id| id.to_string())
            .collect())
    }

    /// Two 100 MHz lines and an 800 MHz one, given out of time order.
    fn survey() -> Vec<OutputFacts> {
        vec![
            output("DAT_0142_A1", 3000., 100., 2),
            output("DAT_0140_A1", 1000., 100., 1),
            output("DAT_0141_A1", 2000., 800., 3),
        ]
    }

    #[test]
    fn the_examples_from_the_spec_render() {
        assert_eq!(
            ids("attempt-{n:02}-{freq}mhz", &survey(), &[]).unwrap(),
            [
                "attempt-03-100mhz",
                "attempt-01-100mhz",
                "attempt-02-800mhz"
            ]
        );
        assert_eq!(
            ids("dronbreen-2026-{freq}mhz-{n:02, per=freq}", &survey(), &[]).unwrap(),
            [
                "dronbreen-2026-100mhz-02",
                "dronbreen-2026-100mhz-01",
                "dronbreen-2026-800mhz-01"
            ]
        );
        assert_eq!(
            ids("dronbreen-2026-{stem}_{nfiles:02}", &survey(), &[]).unwrap(),
            [
                "dronbreen-2026-dat_0142_a1_02",
                "dronbreen-2026-dat_0140_a1_01",
                "dronbreen-2026-dat_0141_a1_03"
            ]
        );
        assert_eq!(
            ids("{group}-{date}-{n}", &survey()[..1], &[]).unwrap(),
            ["dronbreen-19700101-1"]
        );
    }

    #[test]
    fn a_counter_can_start_anywhere_or_continue() {
        assert_eq!(
            ids("line-{n:02, from=15}", &survey(), &[]).unwrap(),
            ["line-17", "line-15", "line-16"]
        );
        let existing = [
            "line-100mhz-04",
            "line-100mhz-07",
            "line-800mhz-02",
            "other-100mhz-30",
            "line-100mhz-x",
        ];
        assert_eq!(
            ids(
                "line-{freq}mhz-{n:02, per=freq, from=next}",
                &survey(),
                &existing
            )
            .unwrap(),
            ["line-100mhz-09", "line-100mhz-08", "line-800mhz-03"]
        );
        // Without `per`, one sequence after the highest of all of them.
        assert_eq!(
            ids("line-{freq}mhz-{n:02, from=next}", &survey(), &existing).unwrap(),
            ["line-100mhz-10", "line-100mhz-08", "line-800mhz-09"]
        );
        // Nothing to continue: from 1.
        assert_eq!(
            ids("new-{n, from=next}", &survey()[..1], &existing).unwrap(),
            ["new-1"]
        );
    }

    #[test]
    fn a_number_next_to_digits_is_still_found() {
        // `{n}` followed by literal digits: the counter is the shortest run
        // that lets the rest match.
        assert_eq!(
            ids("l{n, from=next}0", &survey()[..1], &["l70", "l120"]).unwrap(),
            ["l130"]
        );
    }

    #[test]
    fn colliding_ids_are_refused_before_anything_runs() {
        let refused = ids("line-{freq}mhz", &survey(), &[]).unwrap_err();
        assert!(
            refused.contains("DAT_0142_A1.rad and DAT_0140_A1.rad"),
            "{refused}"
        );
        assert!(refused.contains("'line-100mhz'"), "{refused}");
        // A `per` on something the id does not show collides too.
        assert!(ids("line-{n, per=freq}", &survey(), &[]).is_err());
    }

    #[test]
    fn literal_text_must_be_what_an_id_can_hold() {
        let refused = ids("line-{freq}MHz", &survey(), &[]).unwrap_err();
        assert!(refused.contains("write `mhz`"), "{refused}");
        assert!(
            refused.contains("\n  line-{freq}MHz\n             ^^^"),
            "{refused}"
        );
        assert!(ids("line {n}", &survey(), &[])
            .unwrap_err()
            .contains("cannot contain ` `"));
        assert!(ids("drønbreen-{n}", &survey(), &[]).is_err());
    }

    #[test]
    fn mistakes_in_placeholders_are_named() {
        for (pattern, says) in [
            ("{frequency}", "not a placeholder"),
            ("{id}", "only for display name patterns"),
            ("{stem:02}", "takes no width"),
            ("{n:2}", "not a width"),
            ("{freq, from=2}", "only `{n}` takes options"),
            ("{n, from=soon}", "neither a number nor `next`"),
            ("{n, per=n}", "not something to count by"),
            ("{n, start=2}", "not an option"),
            ("{n}-{n}", "only one `{n}`"),
            ("line-{n", "never closed"),
            ("line}", "closes nothing"),
            ("", "empty"),
        ] {
            let refused = ids(pattern, &survey(), &[]).unwrap_err();
            assert!(refused.contains(says), "{pattern}: {refused}");
        }
    }

    #[test]
    fn rendered_ids_are_still_checked() {
        // Only what the literal text cannot rule out: ends and length.
        assert!(ids("-{n}", &survey(), &[])
            .unwrap_err()
            .contains("must not start or end"));
        let long = format!("{}{{n}}", "a".repeat(128));
        assert!(ids(&long, &survey(), &[]).unwrap_err().contains("too long"));
        let unnamed = [output("___", 0., 100., 1)];
        assert!(ids("{stem}", &unnamed, &[])
            .unwrap_err()
            .contains("no letters or digits"));
    }

    #[test]
    fn display_names_take_any_text_and_the_id() {
        let outputs = survey();
        let id_pattern = Pattern::parse("line-{n:02}", PatternKind::Id).unwrap();
        let batch = BatchFacts {
            group_id: Some(GroupId::new("dronbreen").unwrap()),
            group_name: GroupName::from_input("Drønbreen"),
        };
        let ids = render_ids(&id_pattern, &outputs, &batch, &[]).unwrap();
        let names = Pattern::parse(
            "{group_name} {freq} MHz, line {n} ({id}, {stem}) {{draft}}",
            PatternKind::DisplayName,
        )
        .unwrap();
        let names = render_display_names(&names, &id_pattern, &outputs, &batch, &ids, &[]).unwrap();
        assert_eq!(
            names[1].as_ref().unwrap().as_str(),
            "Drønbreen 100 MHz, line 1 (line-01, DAT_0140_A1) {draft}"
        );

        // A bare `{n}` is the id's number; with options it counts alone.
        let id_pattern = Pattern::parse("line-{n:02, from=7}", PatternKind::Id).unwrap();
        let ids = render_ids(&id_pattern, &outputs, &batch, &[]).unwrap();
        let render = |text: &str| {
            let pattern = Pattern::parse(text, PatternKind::DisplayName).unwrap();
            render_display_names(&pattern, &id_pattern, &outputs, &batch, &ids, &[])
                .unwrap()
                .into_iter()
                .map(|name| name.unwrap().as_str().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(render("Line {n}"), ["Line 9", "Line 7", "Line 8"]);
        assert_eq!(render("Line {n, from=1}"), ["Line 3", "Line 1", "Line 2"]);
        assert!(Pattern::parse("{n, from=next}", PatternKind::DisplayName).is_err());
    }

    #[test]
    fn a_group_placeholder_needs_a_group() {
        let pattern = Pattern::parse("{group}-{n}", PatternKind::Id).unwrap();
        let refused = render_ids(&pattern, &survey(), &BatchFacts::default(), &[]).unwrap_err();
        assert!(refused.contains("no group was given"), "{refused}");
    }
}
