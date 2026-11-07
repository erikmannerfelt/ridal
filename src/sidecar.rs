//! A recording's sidecar (#527): `<stem>.ridal.toml` beside its header,
//! holding values that take precedence over what the header says.
//!
//! It is an overlay, not a replacement: the reader still parses the
//! header, and the sidecar sets only what it names. Two layers:
//!
//! - `[overrides]`, corrections that change processing. Strict: an
//!   unknown key is an error, so a typo is never silently ignored.
//! - `[native.<format>]`, fields of one format that `GPRMeta` has nowhere
//!   to put, kept for that format's writer (#99). Read here only to check
//!   the format's name; their contents are the writer's business.
//!
//! A file holding several recordings may give each its own tables, with
//! the same shape as the top level: a line of an IceRadar file under
//! `[lines.<N>]`, and a channel of a GSSI file under `[channels.<N>]`. A
//! line's or channel's value takes precedence over the file's: line or
//! channel > file > header. A value given as an argument (`--override
//! KEY=VALUE`, `overrides=` in Python) takes precedence over both, and is
//! read by [`from_assignments`] into the same [`Overrides`], so the two
//! cannot disagree about what is valid (#559).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::formats::{self, FormatKind, ResolvedInput};

pub mod edit;

/// What a sidecar beside a recording is called, after the recording's
/// stem: `DAT_0001_A1.ridal.toml`.
pub const SIDECAR_SUFFIX: &str = ".ridal.toml";

/// The layout of a sidecar this Ridal reads. A sidecar without `schema`
/// is read as this one.
pub const SCHEMA: u32 = 1;

/// Values that take precedence over the header's. Each is `None` where
/// the sidecar does not set it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    /// The antenna's center frequency, in MHz.
    #[serde(default)]
    #[cfg_attr(feature = "server", schema(required = true))]
    pub antenna_mhz: Option<f32>,
    /// The horizontal distance between the transmitter and the receiver,
    /// in m.
    #[serde(default)]
    #[cfg_attr(feature = "server", schema(required = true))]
    pub antenna_separation_m: Option<f32>,
    /// The digitizer's sampling frequency, in MHz: not the antenna's.
    #[serde(default)]
    #[cfg_attr(feature = "server", schema(required = true))]
    pub sampling_mhz: Option<f32>,
    /// The UTC date of a recording whose file records only a time of day,
    /// as `YYYY-MM-DD`.
    #[serde(default, with = "date_serde")]
    #[cfg_attr(
        feature = "server",
        schema(value_type = Option<String>, format = Date, required = true)
    )]
    pub date: Option<chrono::NaiveDate>,
    /// m, added to every altitude read from the recording's own positions
    /// (#558).
    #[serde(default)]
    #[cfg_attr(feature = "server", schema(required = true))]
    pub altitude_offset_m: Option<f32>,
}

/// `date` as `YYYY-MM-DD`: a string in JSON, and in TOML either a string
/// or a local date (`date = 2014-04-05`), which is what one would write.
pub(crate) mod date_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    const FORMAT: &str = "%Y-%m-%d";

    pub fn serialize<S: Serializer>(
        date: &Option<chrono::NaiveDate>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match date {
            Some(date) => serializer.serialize_str(&date.format(FORMAT).to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<chrono::NaiveDate>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Text(String),
            Toml(toml::value::Datetime),
        }
        let text = match Option::<Written>::deserialize(deserializer)? {
            None => return Ok(None),
            Some(Written::Text(text)) => text,
            Some(Written::Toml(datetime)) => {
                if datetime.time.is_some() || datetime.offset.is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "date must be a date alone (YYYY-MM-DD), not {datetime}"
                    )));
                }
                datetime.to_string()
            }
        };
        chrono::NaiveDate::parse_from_str(&text, FORMAT)
            .map(Some)
            .map_err(|_| serde::de::Error::custom(format!("date must be YYYY-MM-DD, not {text:?}")))
    }
}

/// One key of `[overrides]`: everything that is said of it outside
/// [`Overrides`] itself. `ridal override`, the reference page and both
/// raw-data forms of the GUI (#564) are built from these, so a new key
/// appears in all of them once it is listed here, and every field below
/// is a decision a new key has to make.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct OverrideKey {
    pub name: &'static str,
    /// What a form calls it, in sentence case and without the unit.
    pub label: &'static str,
    pub unit: &'static str,
    pub description: &'static str,
    /// The values it takes, which [`Overrides::check`] holds it to and
    /// a form picks its input by.
    pub kind: ValueKind,
    /// Which recordings it applies to.
    pub applies: Applies,
    /// The code reading refuses a recording with when its files do not
    /// say this, and nothing gives it: the refusal that makes it essential
    /// in a form. `None` for a key a recording can be read without.
    pub refusal: Option<&'static str>,
    /// Whether bulk ingest takes it per antenna, as the headers name it,
    /// rather than for the whole upload: what describes the antenna, so an
    /// upload that mixes antennas can give each its own.
    pub per_antenna: bool,
    /// Whether it usually differs between recordings (a date, a take-off
    /// point), so a form says that a sidecar per recording is the place for
    /// it, rather than one value for all of them.
    pub per_recording: bool,
}

/// The values one key takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    /// A number above 0.
    Positive,
    /// A number of 0 or more.
    NonNegative,
    /// Any finite number.
    Number,
    /// `YYYY-MM-DD`.
    Date,
}

/// Which recordings a key applies to: one it does not apply to is read as
/// though it were not given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Applies {
    Every,
    /// Those whose positions are read with a date: a format whose positions
    /// record a time of day ([`formats::takes_date`]), without a
    /// `.coords.csv`, which gives dates of its own.
    DatedPositions,
    /// Those whose positions are their own rather than a `.coords.csv`.
    OwnPositions,
}

/// What decides whether a key applies to a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Traits {
    /// `None` for a format this Ridal does not know.
    pub kind: Option<FormatKind>,
    /// Whether a `.coords.csv` gives its positions.
    pub coords_csv: bool,
}

impl Applies {
    pub fn to(self, recording: Traits) -> bool {
        match self {
            Applies::Every => true,
            Applies::DatedPositions => {
                !recording.coords_csv && recording.kind.is_some_and(formats::takes_date)
            }
            Applies::OwnPositions => !recording.coords_csv,
        }
    }

    /// Why a key does not apply to a recording it does not apply to.
    pub fn reason(self) -> &'static str {
        match self {
            Applies::Every => "",
            Applies::DatedPositions => {
                "Only for recordings whose positions record a time of day (IceRadar, \
                 pulseEKKO and VIRL), without a .coords.csv."
            }
            Applies::OwnPositions => "Not for positions from a .coords.csv.",
        }
    }
}

/// The keys that do not apply to `recordings`, each with why not: what a
/// form shows disabled. `to_every`: a key that does not apply to one of
/// them is out, as for a source, which refuses it; otherwise only one
/// that applies to none of them, as for bulk ingest, which gives each
/// recording the keys that apply to it. Nothing when there are no
/// recordings yet, since nothing is known of them.
pub fn not_applicable(
    recordings: &[Traits],
    to_every: bool,
) -> BTreeMap<&'static str, &'static str> {
    if recordings.is_empty() {
        return BTreeMap::new();
    }
    OVERRIDE_KEYS
        .iter()
        .filter(|key| {
            let applies = |r: &Traits| key.applies.to(*r);
            if to_every {
                !recordings.iter().all(applies)
            } else {
                !recordings.iter().any(applies)
            }
        })
        .map(|key| (key.name, key.applies.reason()))
        .collect()
}

/// The key reading gives `code` for when nothing gives it.
pub fn key_refused_with(code: &str) -> Option<&'static OverrideKey> {
    OVERRIDE_KEYS.iter().find(|k| k.refusal == Some(code))
}

/// Every key of `[overrides]`. `every_override_key_is_listed` checks this
/// against [`Overrides`], and `docs/reference/sidecar.md` is generated
/// from it.
pub const OVERRIDE_KEYS: &[OverrideKey] = &[
    OverrideKey {
        name: "antenna_mhz",
        label: "Antenna frequency",
        unit: "MHz",
        description: "The antenna's center frequency. Needed for an IceRadar, VIRL or SkyHub \
                      file, which does not record it.",
        kind: ValueKind::Positive,
        applies: Applies::Every,
        refusal: Some("antenna_frequency_unknown"),
        per_antenna: true,
        per_recording: false,
    },
    OverrideKey {
        name: "antenna_separation_m",
        label: "Antenna separation",
        unit: "m",
        description: "The horizontal distance between the transmitter and the receiver. GSSI, \
                      IceRadar, VIRL and SkyHub files do not record it, and it is taken as 0 m \
                      without one.",
        kind: ValueKind::NonNegative,
        applies: Applies::Every,
        refusal: None,
        per_antenna: true,
        per_recording: false,
    },
    OverrideKey {
        name: "sampling_mhz",
        label: "Sampling frequency",
        unit: "MHz",
        description: "The digitizer's sampling frequency, not the antenna's: the time between \
                      samples is 1000 / sampling_mhz ns. Needed for a VIRL file, which does not \
                      record it. A wrong value scales every time and depth.",
        kind: ValueKind::Positive,
        applies: Applies::Every,
        refusal: Some("sampling_frequency_unknown"),
        per_antenna: true,
        per_recording: false,
    },
    OverrideKey {
        name: "date",
        label: "Date (UTC)",
        unit: "YYYY-MM-DD",
        description: "The UTC date of the recording, for an IceRadar, pulseEKKO or VIRL file, \
                      whose positions record only a time of day or a date that may be wrong. Written \
                      as a TOML date (date = 2014-04-05) or a string.",
        kind: ValueKind::Date,
        applies: Applies::DatedPositions,
        refusal: Some("date_unknown"),
        per_antenna: false,
        per_recording: true,
    },
    OverrideKey {
        name: "altitude_offset_m",
        label: "Altitude offset",
        unit: "m",
        description: "Added to every altitude the recording's own positions give, to make it the \
                      radar antenna's height in the vertical datum it will be compared with: the \
                      take-off point's elevation for altitudes relative to take-off, minus the \
                      height of a GNSS antenna mounted above the radar antenna, or a geoid \
                      separation. Not applied to a coordinate CSV's altitudes.",
        kind: ValueKind::Number,
        applies: Applies::OwnPositions,
        refusal: None,
        per_antenna: false,
        per_recording: true,
    },
];

// `check` refuses NaN, so a read value is equal to itself, as a recipe's
// bundles, which hold these, need.
impl Eq for Overrides {}

impl Overrides {
    /// Whether the sidecar sets nothing.
    pub fn is_empty(&self) -> bool {
        *self == Overrides::default()
    }

    /// `self`, with each value `other` sets in its place.
    pub fn under(self, other: Overrides) -> Overrides {
        Overrides {
            antenna_mhz: other.antenna_mhz.or(self.antenna_mhz),
            antenna_separation_m: other.antenna_separation_m.or(self.antenna_separation_m),
            sampling_mhz: other.sampling_mhz.or(self.sampling_mhz),
            date: other.date.or(self.date),
            altitude_offset_m: other.altitude_offset_m.or(self.altitude_offset_m),
        }
    }

    /// Hold each value set to its key's [`ValueKind`].
    pub fn check(&self) -> Result<(), String> {
        for (name, value) in self.entries() {
            let Some(Value::Number(n)) = value else {
                continue;
            };
            let key = OVERRIDE_KEYS
                .iter()
                .find(|k| k.name == name)
                .expect("every key is listed");
            let refused = match key.kind {
                ValueKind::Positive => (!(n.is_finite() && n > 0.)).then_some("be above 0"),
                ValueKind::NonNegative => (!(n.is_finite() && n >= 0.)).then_some("be 0 or more"),
                ValueKind::Number => (!n.is_finite()).then_some("be a number"),
                ValueKind::Date => unreachable!("{name} is a date, not a number"),
            };
            if let Some(must) = refused {
                return Err(format!("{name} must {must}, not {n}"));
            }
        }
        Ok(())
    }

    /// `self` with only the keys `keep` keeps.
    pub fn only(self, keep: impl Fn(&OverrideKey) -> bool) -> Overrides {
        let mut table = toml::Table::try_from(self).expect("overrides are a table");
        table.retain(|name, _| {
            OVERRIDE_KEYS
                .iter()
                .find(|k| k.name == name)
                .is_some_and(&keep)
        });
        table.try_into().expect("a subset of overrides reads back")
    }

    /// Each key, by its name in a sidecar, and its value.
    pub fn entries(&self) -> [(&'static str, Option<Value>); 5] {
        [
            ("antenna_mhz", self.antenna_mhz.map(Value::Number)),
            (
                "antenna_separation_m",
                self.antenna_separation_m.map(Value::Number),
            ),
            ("sampling_mhz", self.sampling_mhz.map(Value::Number)),
            ("date", self.date.map(Value::Date)),
            (
                "altitude_offset_m",
                self.altitude_offset_m.map(Value::Number),
            ),
        ]
    }

    /// `key = value` for each value set, for messages.
    pub fn describe(&self) -> String {
        self.entries()
            .iter()
            .filter_map(|(key, value)| value.map(|value| format!("{key} = {value}")))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `self`, without the values `other` sets.
    pub fn without(self, other: Overrides) -> Overrides {
        Overrides {
            antenna_mhz: self.antenna_mhz.filter(|_| other.antenna_mhz.is_none()),
            antenna_separation_m: self
                .antenna_separation_m
                .filter(|_| other.antenna_separation_m.is_none()),
            sampling_mhz: self.sampling_mhz.filter(|_| other.sampling_mhz.is_none()),
            date: self.date.filter(|_| other.date.is_none()),
            altitude_offset_m: self
                .altitude_offset_m
                .filter(|_| other.altitude_offset_m.is_none()),
        }
    }
}

/// One override given as an argument rather than in a sidecar: a
/// `--override KEY=VALUE`, an item of `overrides=` in Python, or one of
/// the per-key forms that came before them.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    /// The key, as `[overrides]` names it.
    pub key: String,
    pub value: toml::Value,
    /// How it was given, for messages: `--override date=2014-04-05`.
    pub given_as: String,
}

impl Assignment {
    /// `KEY=VALUE`, as `--override` and `ridal override --set` take it,
    /// split at the first `=`. The value is read as a TOML value, so
    /// `date=2014-04-05` is a date and `antenna_mhz=50` a number, as they
    /// would be in a sidecar; anything that is not one is a string.
    pub fn parse(text: &str, option: &str) -> Result<Assignment, String> {
        let (key, value) = text
            .split_once('=')
            .ok_or_else(|| "expected KEY=VALUE, such as antenna_mhz=50".to_string())?;
        let key = key.trim();
        let value = value.trim();
        let value = format!("value = {value}")
            .parse::<toml::Table>()
            .ok()
            .and_then(|mut table| table.remove("value"))
            .unwrap_or_else(|| toml::Value::String(value.to_string()));
        Ok(Assignment {
            key: key.to_string(),
            value,
            given_as: format!("{option} {text}"),
        })
    }
}

/// The overrides `assignments` give, each checked as it would be in a
/// sidecar.
///
/// # Errors
/// A key that `[overrides]` does not have, listing those it does; a value
/// a sidecar could not hold either; and a key given more than once, which
/// is refused rather than one taking precedence.
pub fn from_assignments(assignments: &[Assignment]) -> Result<Overrides, String> {
    let mut overrides = Overrides::default();
    for (i, assignment) in assignments.iter().enumerate() {
        let Assignment {
            key,
            value,
            given_as,
        } = assignment;
        if !OVERRIDE_KEYS.iter().any(|k| k.name == key) {
            return Err(format!(
                "{given_as}: there is no override {key:?}. The keys are {}.",
                OVERRIDE_KEYS
                    .iter()
                    .map(|k| k.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(earlier) = assignments[..i].iter().find(|a| &a.key == key) {
            return Err(format!(
                "{key} is given twice, as {} and as {given_as}; give it once",
                earlier.given_as
            ));
        }
        let table = toml::Table::from_iter([(key.clone(), value.clone())]);
        let one: Overrides = table
            .try_into()
            .map_err(|e: toml::de::Error| format!("{given_as}: {}", e.message()))?;
        one.check().map_err(|e| format!("{given_as}: {e}"))?;
        overrides = overrides.under(one);
    }
    Ok(overrides)
}

/// The value of one key of [`Overrides`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Number(f32),
    Date(chrono::NaiveDate),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Number(number) => write!(f, "{number}"),
            Value::Date(date) => write!(f, "{}", date.format("%Y-%m-%d")),
        }
    }
}

/// The tables of a sidecar that apply to one recording: the whole file,
/// or one line of it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tables {
    #[serde(default)]
    overrides: Overrides,
    /// By format name, as `ridal formats` lists it.
    #[serde(default)]
    native: BTreeMap<String, toml::Table>,
}

/// A sidecar as written.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sidecar {
    schema: Option<u32>,
    // Not `#[serde(flatten)] file: Tables`: serde ignores
    // `deny_unknown_fields` on a struct that flattens.
    #[serde(default)]
    overrides: Overrides,
    #[serde(default)]
    native: BTreeMap<String, toml::Table>,
    /// By line number. TOML keys are strings, so `[lines.3]` is `"3"`.
    #[serde(default)]
    lines: BTreeMap<String, Tables>,
    /// By channel number, from 1, as `--channel` takes it.
    #[serde(default)]
    channels: BTreeMap<String, Tables>,
}

/// Read and check a sidecar, without regard to the recording it is for.
fn read(path: &Path) -> Result<Sidecar, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    parse(&text, path)
}

/// Check the text of a sidecar, which messages call `path`, without
/// regard to the recording it is for.
fn parse(text: &str, path: &Path) -> Result<Sidecar, String> {
    let sidecar: Sidecar = toml::from_str(text)
        .map_err(|e| format!("{} is not a valid sidecar: {e}", path.display()))?;
    let invalid = |what: String| format!("{} is not a valid sidecar: {what}", path.display());
    if let Some(schema) = sidecar.schema {
        if schema != SCHEMA {
            return Err(invalid(format!(
                "it has schema = {schema}, and this Ridal reads schema = {SCHEMA}"
            )));
        }
    }
    let known = formats::all_formats()
        .iter()
        .map(|f| f.name)
        .collect::<Vec<_>>();
    let file = Tables {
        overrides: sidecar.overrides,
        native: sidecar.native.clone(),
    };
    let mut tables = vec![(String::new(), &file)];
    for (line, table) in &sidecar.lines {
        line.parse::<u32>().map_err(|_| {
            invalid(format!(
                "[lines.{line}] is not a line number; lines are numbered as --lines takes them"
            ))
        })?;
        tables.push((format!("[lines.{line}] "), table));
    }
    for (channel, table) in &sidecar.channels {
        channel
            .parse::<u32>()
            .ok()
            .filter(|&c| c >= 1)
            .ok_or_else(|| {
                invalid(format!(
                    "[channels.{channel}] is not a channel number; channels are numbered from 1, \
                 as --channel takes them"
                ))
            })?;
        tables.push((format!("[channels.{channel}] "), table));
    }
    for (place, table) in tables {
        table
            .overrides
            .check()
            .map_err(|e| invalid(format!("{place}{e}")))?;
        if let Some(unknown) = table
            .native
            .keys()
            .find(|name| !known.contains(&name.as_str()))
        {
            return Err(invalid(format!(
                "{place}[native.{unknown}] is not a format; the formats are {}",
                known.join(", ")
            )));
        }
    }
    Ok(sidecar)
}

/// What the sidecar of `resolved` sets for it, line or channel > file, or
/// nothing without one.
///
/// # Errors
/// A sidecar that cannot be read, is invalid, or has `[lines]` or
/// `[channels]` tables that do not fit the recording's file: any at all
/// for a format without lines or channels, and a line or channel the file
/// does not have.
pub fn overrides_of(resolved: &ResolvedInput) -> Result<Overrides, String> {
    let Some(path) = &resolved.sidecar else {
        return Ok(Overrides::default());
    };
    let sidecar = read(path)?;
    let mut overrides = sidecar.overrides;
    if !sidecar.lines.is_empty() {
        check_lines(&sidecar, path, resolved)?;
        // A file of one line is read without `part.line` set (`with_lines`).
        let line = match resolved.part.line {
            Some(line) => line,
            None => crate::processing::input::iceradar_line(resolved).map_err(|e| e.to_string())?,
        };
        if let Some(table) = sidecar.lines.get(&line.to_string()) {
            overrides = overrides.under(table.overrides);
        }
    }
    if !sidecar.channels.is_empty() {
        check_channels(&sidecar, path, resolved)?;
        let channel =
            crate::processing::input::gssi_channel(resolved).map_err(|e| e.to_string())?;
        if let Some(table) = sidecar.channels.get(&channel.to_string()) {
            overrides = overrides.under(table.overrides);
        }
    }
    Ok(overrides)
}

/// Refuse `[channels]` tables that do not fit the file of `resolved`: any
/// at all for a format without channels, and a channel the file does not
/// have.
fn check_channels(sidecar: &Sidecar, path: &Path, resolved: &ResolvedInput) -> Result<(), String> {
    if resolved.kind != FormatKind::Gssi {
        return Err(format!(
            "{} has [channels] tables, but {} is a {} file, which holds no channels",
            path.display(),
            resolved.header.display(),
            formats::display_name(resolved.kind),
        ));
    }
    let available = crate::io::gssi_channels(&resolved.header)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|c| c.number)
        .collect::<Vec<_>>();
    for channel in sidecar.channels.keys() {
        // Checked by `read`.
        let channel = channel.parse::<u32>().expect("a channel number");
        if !available.contains(&channel) {
            return Err(format!(
                "{} has [channels.{channel}], but {} has no channel {channel}; its channels \
                 are {}",
                path.display(),
                resolved.header.display(),
                available
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(())
}

/// Refuse `[lines]` tables that do not fit the file of `resolved`: any at
/// all for a format without lines, and a line the file does not have.
fn check_lines(sidecar: &Sidecar, path: &Path, resolved: &ResolvedInput) -> Result<(), String> {
    if sidecar.lines.is_empty() {
        return Ok(());
    }
    if resolved.kind != FormatKind::IceRadar {
        return Err(format!(
            "{} has [lines] tables, but {} is a {} file, which holds no lines",
            path.display(),
            resolved.header.display(),
            formats::display_name(resolved.kind),
        ));
    }
    let available = crate::io::iceradar_lines(&resolved.header)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(n, _)| n)
        .collect::<Vec<_>>();
    for line in sidecar.lines.keys() {
        // Checked by `read`.
        let line = line.parse::<u32>().expect("a line number");
        if !available.contains(&line) {
            return Err(format!(
                "{} has [lines.{line}], but {} has no line {line}; its lines are {}",
                path.display(),
                resolved.header.display(),
                available
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(())
}

/// What the sidecar of `resolved` sets that the arguments do not: the
/// values it actually decides, which the recipe records.
pub fn applied(resolved: &ResolvedInput, arguments: &Overrides) -> Result<Overrides, String> {
    Ok(overrides_of(resolved)?.without(*arguments))
}

/// A sidecar that sets `overrides` and nothing else, as a server writes
/// one beside a recording it lays out from values it stores.
pub fn to_toml(overrides: &Overrides) -> String {
    let table = toml::to_string(overrides).expect("overrides are plain numbers and a date");
    format!("schema = {SCHEMA}\n\n[overrides]\n{table}")
}

/// Which sidecar each recording is read with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Choice {
    /// `<stem>.ridal.toml` beside the header, when there is one.
    #[default]
    Beside,
    /// This file, for the one recording given (or every line given of one
    /// IceRadar file): `--sidecar`.
    File(PathBuf),
    /// None, whatever is beside the header: `--no-sidecar`.
    Ignore,
}

/// Apply `--sidecar` or `--no-sidecar` (`sidecar=` in Python) to resolved
/// recordings, which [`formats::resolve_input`] gave the sidecar beside
/// each.
///
/// # Errors
/// A file for more than one recording, or one that does not exist.
pub fn with_sidecar(resolved: &mut [ResolvedInput], choice: &Choice) -> Result<(), String> {
    match choice {
        Choice::Beside => Ok(()),
        Choice::Ignore => {
            for recording in resolved.iter_mut() {
                recording.sidecar = None;
            }
            Ok(())
        }
        Choice::File(path) => {
            // The lines of one IceRadar file are one file here, as the
            // sidecar holds them all.
            let mut files = resolved.iter().map(|r| &r.header).collect::<Vec<_>>();
            files.dedup();
            if files.len() > 1 {
                return Err(format!(
                    "{} is one sidecar, but {} recordings were given. Put a <stem>{SIDECAR_SUFFIX} \
                     beside each recording instead.",
                    path.display(),
                    files.len()
                ));
            }
            if !path.is_file() {
                return Err(format!("The sidecar {} does not exist", path.display()));
            }
            for recording in resolved.iter_mut() {
                recording.sidecar = Some(path.clone());
            }
            Ok(())
        }
    }
}

/// `<stem>.ridal.toml` beside `header`, whatever the case of its name.
pub fn find_beside(header: &Path) -> Option<PathBuf> {
    let dir = header.parent().unwrap_or_else(|| Path::new("."));
    let stem = header.file_stem()?.to_str()?;
    formats::find_named(dir, &format!("{stem}{SIDECAR_SUFFIX}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join(format!("a{SIDECAR_SUFFIX}"));
        std::fs::write(&path, text).unwrap();
        path
    }

    fn ramac(dir: &Path, sidecar: Option<PathBuf>) -> ResolvedInput {
        ResolvedInput {
            sidecar,
            ..formats::resolve_input(&dir.join("a.rad")).unwrap()
        }
    }

    fn error(text: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), text);
        overrides_of(&ramac(dir.path(), Some(path)))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn overrides_are_read_and_integers_are_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "schema = 1\n[overrides]\nantenna_mhz = 50\nantenna_separation_m = 4.5\n\
             [native.ramac]\nANYTHING = \"kept\"\n",
        );
        let read = overrides_of(&ramac(dir.path(), Some(path))).unwrap();
        assert_eq!(
            read,
            Overrides {
                antenna_mhz: Some(50.),
                antenna_separation_m: Some(4.5),
                ..Default::default()
            }
        );
        // Without a sidecar, nothing is overridden.
        assert!(overrides_of(&ramac(dir.path(), None)).unwrap().is_empty());
        // Nor with an empty one, which is read as schema 1.
        let empty = write(dir.path(), "");
        assert!(overrides_of(&ramac(dir.path(), Some(empty)))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_date_is_a_toml_date_or_a_string() {
        let dir = tempfile::tempdir().unwrap();
        for text in ["date = 2014-04-05", "date = \"2014-04-05\""] {
            let path = write(dir.path(), &format!("[overrides]\n{text}\n"));
            assert_eq!(
                overrides_of(&ramac(dir.path(), Some(path))).unwrap().date,
                chrono::NaiveDate::from_ymd_opt(2014, 4, 5),
                "{text}"
            );
        }
    }

    #[test]
    fn a_mistake_is_an_error_naming_the_file() {
        for (text, expected) in [
            (
                "[overrides]\nantena_separation_m = 4\n",
                "antena_separation_m",
            ),
            ("[override]\n", "unknown field `override`"),
            ("schema = 2\n", "schema = 2"),
            (
                "[overrides]\nantenna_mhz = 0\n",
                "antenna_mhz must be above 0",
            ),
            (
                "[overrides]\nantenna_separation_m = -1\n",
                "antenna_separation_m must be 0 or more",
            ),
            ("[overrides]\nantenna_mhz = \"50\"\n", "antenna_mhz"),
            (
                "[overrides]\nsampling_mhz = 0\n",
                "sampling_mhz must be above 0",
            ),
            (
                "[overrides]\ndate = 2014-04-05T10:00:00\n",
                "date must be a date alone",
            ),
            (
                "[overrides]\ndate = \"5 April 2014\"\n",
                "date must be YYYY-MM-DD",
            ),
            ("[overrides]\ndate = 20140405\n", "date"),
            ("[native.dzt]\n", "[native.dzt] is not a format"),
            ("[lines.first]\n", "[lines.first] is not a line number"),
            (
                "[lines.0.overrides]\nantenna_mhz = -5\n",
                "[lines.0] antenna_mhz must be above 0",
            ),
            ("[overrides\n", "not a valid sidecar"),
        ] {
            let error = error(text);
            assert!(error.contains(".ridal.toml"), "{text:?}: {error}");
            assert!(error.contains(expected), "{text:?}: {error}");
        }
    }

    #[test]
    fn lines_are_refused_for_a_format_without_them() {
        let error = error("[lines.0.overrides]\nantenna_mhz = 50\n");
        assert!(error.contains("which holds no lines"), "{error}");
    }

    #[test]
    fn a_written_sidecar_reads_back_as_its_values() {
        let dir = tempfile::tempdir().unwrap();
        for overrides in [
            Overrides::default(),
            Overrides {
                antenna_mhz: Some(25.),
                antenna_separation_m: None,
                ..Default::default()
            },
            Overrides {
                antenna_mhz: Some(100.),
                antenna_separation_m: Some(0.25),
                ..Default::default()
            },
            Overrides {
                sampling_mhz: Some(200.),
                date: chrono::NaiveDate::from_ymd_opt(2014, 4, 5),
                ..Default::default()
            },
        ] {
            let path = write(dir.path(), &to_toml(&overrides));
            assert_eq!(
                overrides_of(&ramac(dir.path(), Some(path))).unwrap(),
                overrides
            );
        }
    }

    #[test]
    fn the_arguments_take_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "[overrides]\nantenna_mhz = 50\nantenna_separation_m = 4\n",
        );
        let resolved = ramac(dir.path(), Some(path));
        let given = |antenna_mhz, antenna_separation_m| Overrides {
            antenna_mhz,
            antenna_separation_m,
            ..Default::default()
        };
        assert_eq!(
            applied(&resolved, &given(Some(100.), None)).unwrap(),
            given(None, Some(4.))
        );
        assert!(applied(&resolved, &given(Some(100.), Some(1.)))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_sidecar_beside_the_header_is_found_and_can_be_replaced_or_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let rad = dir.path().join("a.rad");
        assert_eq!(formats::resolve_input(&rad).unwrap().sidecar, None);
        // Whatever the case of its name, as a .cor is.
        let beside = dir.path().join("A.RIDAL.TOML");
        std::fs::write(&beside, "").unwrap();
        let mut resolved = [formats::resolve_input(&rad).unwrap()];
        assert_eq!(resolved[0].sidecar, Some(beside));

        let other = dir.path().join("other.toml");
        let error = with_sidecar(&mut resolved, &Choice::File(other.clone())).unwrap_err();
        assert!(error.contains("does not exist"), "{error}");
        std::fs::write(&other, "").unwrap();
        with_sidecar(&mut resolved, &Choice::File(other.clone())).unwrap();
        assert_eq!(resolved[0].sidecar, Some(other.clone()));
        with_sidecar(&mut resolved, &Choice::Ignore).unwrap();
        assert_eq!(resolved[0].sidecar, None);

        let mut two = [
            formats::resolve_input(&rad).unwrap(),
            formats::resolve_input(&dir.path().join("b.rad")).unwrap(),
        ];
        let error = with_sidecar(&mut two, &Choice::File(other)).unwrap_err();
        assert!(error.contains("2 recordings were given"), "{error}");
    }

    /// The 2025 Drønbreen fixture in `dir`, its header recording a 6.2 m
    /// separation and 25 MHz.
    fn dronbreen(dir: &Path) -> PathBuf {
        const LINE: &str = "dronbreen-20250327-DAT_0066_A1";
        for ext in ["rad", "rd3", "cor"] {
            std::fs::copy(
                format!("assets/mala/{LINE}.{ext}"),
                dir.join(format!("{LINE}.{ext}")),
            )
            .unwrap();
        }
        dir.join(format!("{LINE}.rad"))
    }

    #[test]
    #[cfg(not(target_os = "windows"))] // PROJ, for the .cor.
    fn a_recording_is_processed_and_inspected_with_its_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let rad = dronbreen(dir.path());
        let options =
            |sidecar: Choice, separation: Option<f32>| crate::processing::ProcessingOptions {
                quiet: true,
                sidecar,
                overrides: crate::sidecar::Overrides {
                    antenna_separation_m: separation,
                    ..Default::default()
                },
                ..Default::default()
            };
        let process = |options: crate::processing::ProcessingOptions| {
            let (gpr, _) = crate::processing::build_processed_gpr(crate::processing::RunParams {
                filepaths: vec![rad.clone()],
                output_path: None,
                track_path: None,
                render_path: None,
                radargram_id: Some("test".into()),
                display_name: None,
                options,
            })
            .unwrap();
            let hash = crate::recipe::Recipe::of(&gpr).unwrap().unwrap().hash();
            (gpr, hash)
        };
        let (bare, bare_hash) = process(options(Choice::Beside, None));
        assert_eq!(bare.metadata.antenna_separation, 6.2);
        assert_eq!(bare.inputs.as_ref().unwrap().bundles[0].overrides, None);

        std::fs::write(
            rad.with_extension("ridal.toml"),
            "# Measured on site.\n[overrides]\nantenna_separation_m = 4.5\n",
        )
        .unwrap();
        let (overridden, hash) = process(options(Choice::Beside, None));
        assert_eq!(overridden.metadata.antenna_separation, 4.5);
        assert_ne!(hash, bare_hash);
        // Only the values count, not how the file is written.
        std::fs::write(
            rad.with_extension("ridal.toml"),
            "schema = 1\n\n[overrides]\nantenna_separation_m = 4.50\n",
        )
        .unwrap();
        assert_eq!(process(options(Choice::Beside, None)).1, hash);
        // Ignored, it is the header alone again.
        assert_eq!(process(options(Choice::Ignore, None)).1, bare_hash);
        // An argument takes precedence, so the sidecar decides nothing.
        let (given, _) = process(options(Choice::Beside, Some(1.)));
        assert_eq!(given.metadata.antenna_separation, 1.);
        assert_eq!(given.inputs.as_ref().unwrap().bundles[0].overrides, None);

        let info = |sidecar: Choice, separation: Option<f32>| {
            let records = crate::inspect::inspect(crate::inspect::InfoParams {
                overrides: crate::sidecar::Overrides {
                    antenna_separation_m: separation,
                    ..Default::default()
                },
                sidecar,
                ..crate::inspect::tests::info_params(vec![rad.clone()])
            })
            .unwrap();
            let [crate::inspect::Inspected::Raw(record)] = &records[..] else {
                panic!("expected one raw record");
            };
            record.clone()
        };
        let record = info(Choice::Beside, None);
        assert_eq!(record.metadata.antenna_separation_m, 4.5);
        let summary = record.sidecar.unwrap();
        assert!(summary.path.ends_with(".ridal.toml"), "{}", summary.path);
        assert_eq!(summary.applied.antenna_separation_m, Some(4.5));
        assert_eq!(summary.header.antenna_separation_m, Some(6.2));
        // Not overridden, so not reported as the header's.
        assert_eq!(summary.header.antenna_mhz, None);
        assert!(info(Choice::Ignore, None).sidecar.is_none());
        assert!(info(Choice::Beside, Some(1.)).sidecar.is_none());
    }

    /// The key table of `docs/reference/sidecar.md`, between its markers.
    fn key_table() -> String {
        let mut table = String::from("| Key | Unit | Description |\n|---|---|---|\n");
        for key in OVERRIDE_KEYS {
            table += &format!("| `{}` | {} | {} |\n", key.name, key.unit, key.description);
        }
        table
    }

    #[test]
    fn sidecar_md_is_up_to_date() {
        const START: &str = "cargo test --no-default-features -F cli sidecar_md -->\n";
        const END: &str = "<!-- end of generated keys -->";
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/sidecar.md");
        // A Windows checkout may have converted the line endings.
        let actual = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\r\n", "\n");
        let (before, rest) = actual.split_once(START).expect("the start marker");
        let (_, after) = rest.split_once(END).expect("the end marker");
        let expected = format!("{before}{START}\n{}\n{END}{after}", key_table());
        if std::env::var_os("UPDATE_SIDECAR_MD").is_some() {
            // CI must check the committed file, never regenerate it.
            assert!(
                std::env::var_os("CI").is_none(),
                "UPDATE_SIDECAR_MD is set in CI"
            );
            std::fs::write(&path, &expected).unwrap();
        }
        assert!(
            actual == expected,
            "docs/reference/sidecar.md is stale. Regenerate it with\n  \
             UPDATE_SIDECAR_MD=1 cargo test --no-default-features -F cli sidecar_md"
        );
    }

    #[test]
    fn every_override_key_is_listed() {
        let all = Overrides {
            antenna_mhz: Some(1.),
            antenna_separation_m: Some(1.),
            sampling_mhz: Some(1.),
            date: chrono::NaiveDate::from_ymd_opt(2014, 4, 5),
            altitude_offset_m: Some(1.),
        };
        let table = toml::Table::try_from(all).unwrap();
        let mut fields = table.keys().map(String::as_str).collect::<Vec<_>>();
        fields.sort();
        let mut listed = OVERRIDE_KEYS.iter().map(|k| k.name).collect::<Vec<_>>();
        listed.sort();
        assert_eq!(fields, listed);
        let mut entries = all.entries().map(|(key, _)| key).to_vec();
        entries.sort();
        assert_eq!(entries, listed);
        // A form picks its input by the kind, and `check` holds a number to
        // it: a date is the only key that is not a number.
        for (name, value) in all.entries() {
            let key = OVERRIDE_KEYS.iter().find(|k| k.name == name).unwrap();
            assert_eq!(
                matches!(value, Some(Value::Date(_))),
                key.kind == ValueKind::Date,
                "{name}"
            );
        }
    }

    #[test]
    fn a_key_applies_by_where_the_positions_come_from() {
        use crate::formats::FormatKind;
        let virl = Traits {
            kind: Some(FormatKind::Virl),
            coords_csv: false,
        };
        let ramac = Traits {
            kind: Some(FormatKind::Ramac),
            coords_csv: false,
        };
        let csv = Traits {
            coords_csv: true,
            ..virl
        };
        assert!(not_applicable(&[], true).is_empty());
        assert_eq!(
            not_applicable(&[virl], true).keys().collect::<Vec<_>>(),
            Vec::<&&str>::new()
        );
        assert_eq!(
            not_applicable(&[ramac], true).keys().collect::<Vec<_>>(),
            [&"date"]
        );
        assert_eq!(
            not_applicable(&[csv], true).keys().collect::<Vec<_>>(),
            [&"altitude_offset_m", &"date"]
        );
        // A key out for one recording is out of a source, which refuses
        // it, but not of an upload, which gives it to those it applies to.
        assert_eq!(
            not_applicable(&[virl, ramac], true)
                .keys()
                .collect::<Vec<_>>(),
            [&"date"]
        );
        assert!(not_applicable(&[virl, ramac], false).is_empty());
    }

    #[test]
    fn a_value_is_held_to_its_key_s_kind() {
        let only = Overrides {
            antenna_mhz: Some(50.),
            date: chrono::NaiveDate::from_ymd_opt(2014, 4, 5),
            altitude_offset_m: Some(-3.),
            ..Default::default()
        };
        assert_eq!(
            only.only(|k| k.kind == ValueKind::Date),
            Overrides {
                date: only.date,
                ..Default::default()
            }
        );
        assert!(only.check().is_ok());
        for (overrides, expected) in [
            (
                Overrides {
                    antenna_separation_m: Some(-1.),
                    ..Default::default()
                },
                "antenna_separation_m must be 0 or more, not -1",
            ),
            (
                Overrides {
                    altitude_offset_m: Some(f32::INFINITY),
                    ..Default::default()
                },
                "altitude_offset_m must be a number, not inf",
            ),
        ] {
            assert_eq!(overrides.check().unwrap_err(), expected);
        }
        // Every refusal names a key that is essential when reading gives it.
        for key in OVERRIDE_KEYS {
            if let Some(code) = key.refusal {
                assert_eq!(key_refused_with(code).unwrap().name, key.name);
            }
        }
    }

    #[test]
    fn a_channel_takes_its_own_values_over_the_file_s() {
        use crate::io::test_dzt;
        let dir = tempfile::tempdir().unwrap();
        let dzt = test_dzt::write_two_channel_recording(dir.path(), "track");
        let sidecar = dir.path().join("track.ridal.toml");
        let channel = |n| {
            let resolved = crate::formats::resolve_input(&dzt).unwrap();
            crate::formats::with_channel(vec![resolved], Some(n))
                .unwrap()
                .remove(0)
        };

        std::fs::write(
            &sidecar,
            "[overrides]\nantenna_separation_m = 1\n\n\
             [channels.2.overrides]\nantenna_separation_m = 2.5\n",
        )
        .unwrap();
        assert_eq!(
            overrides_of(&channel(1)).unwrap().antenna_separation_m,
            Some(1.)
        );
        assert_eq!(
            overrides_of(&channel(2)).unwrap().antenna_separation_m,
            Some(2.5)
        );

        std::fs::write(&sidecar, "[channels.3.overrides]\nantenna_mhz = 500\n").unwrap();
        let error = overrides_of(&channel(1)).unwrap_err();
        assert!(
            error.contains("has no channel 3; its channels are 1, 2"),
            "{error}"
        );

        std::fs::write(&sidecar, "[channels.0.overrides]\nantenna_mhz = 500\n").unwrap();
        let error = overrides_of(&channel(1)).unwrap_err();
        assert!(error.contains("numbered from 1"), "{error}");

        let rad_sidecar = dir.path().join("x.ridal.toml");
        std::fs::write(&rad_sidecar, "[channels.1.overrides]\nantenna_mhz = 500\n").unwrap();
        let error = overrides_of(&ramac(dir.path(), Some(rad_sidecar))).unwrap_err();
        assert!(error.contains("which holds no channels"), "{error}");
    }
}
