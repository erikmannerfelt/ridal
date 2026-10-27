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
//! A file holding several recordings (an IceRadar file of several lines)
//! may give a line its own tables under `[lines.<N>]`, with the same shape
//! as the top level. A line's value takes precedence over the file's:
//! line > file > header. A value given as an argument (`--override-*`,
//! `override_*=` in Python) takes precedence over both.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::formats::{self, FormatKind, ResolvedInput};

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
}

/// One key of `[overrides]`, for the reference page.
#[cfg(test)]
pub struct OverrideKey {
    pub name: &'static str,
    pub unit: &'static str,
    pub description: &'static str,
    /// The argument that takes precedence over it.
    pub argument: &'static str,
}

/// Every key of `[overrides]`. `every_override_key_is_listed` checks this
/// against [`Overrides`], and `docs/reference/sidecar.md` is generated
/// from it.
#[cfg(test)]
pub const OVERRIDE_KEYS: &[OverrideKey] = &[
    OverrideKey {
        name: "antenna_mhz",
        unit: "MHz",
        description: "The antenna's center frequency. Needed for an IceRadar file, which does \
                      not record it.",
        argument: "--override-antenna-mhz",
    },
    OverrideKey {
        name: "antenna_separation_m",
        unit: "m",
        description: "The horizontal distance between the transmitter and the receiver. GSSI \
                      and IceRadar files do not record it, and it is taken as 0 m without \
                      one.",
        argument: "--override-antenna-separation",
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
    fn under(self, other: Overrides) -> Overrides {
        Overrides {
            antenna_mhz: other.antenna_mhz.or(self.antenna_mhz),
            antenna_separation_m: other.antenna_separation_m.or(self.antenna_separation_m),
        }
    }

    fn check(&self) -> Result<(), String> {
        if let Some(mhz) = self.antenna_mhz {
            if !(mhz.is_finite() && mhz > 0.) {
                return Err(format!("antenna_mhz must be above 0, not {mhz}"));
            }
        }
        if let Some(m) = self.antenna_separation_m {
            if !(m.is_finite() && m >= 0.) {
                return Err(format!("antenna_separation_m must be 0 or more, not {m}"));
            }
        }
        Ok(())
    }

    /// `key = value` for each value set, for messages.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(mhz) = self.antenna_mhz {
            parts.push(format!("antenna_mhz = {mhz}"));
        }
        if let Some(m) = self.antenna_separation_m {
            parts.push(format!("antenna_separation_m = {m}"));
        }
        parts.join(", ")
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
}

/// Read and check a sidecar, without regard to the recording it is for.
fn read(path: &Path) -> Result<Sidecar, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let sidecar: Sidecar = toml::from_str(&text)
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

/// What the sidecar of `resolved` sets for it, line > file, or nothing
/// without one.
///
/// # Errors
/// A sidecar that cannot be read, is invalid, or has `[lines]` tables
/// that do not fit the recording's file: any at all for a format without
/// lines, and a line the file does not have.
pub fn overrides_of(resolved: &ResolvedInput) -> Result<Overrides, String> {
    let Some(path) = &resolved.sidecar else {
        return Ok(Overrides::default());
    };
    let sidecar = read(path)?;
    if sidecar.lines.is_empty() {
        return Ok(sidecar.overrides);
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
    // A file of one line is read without `part.line` set (`with_lines`).
    let line = match resolved.part.line {
        Some(line) => line,
        None => crate::processing::input::iceradar_line(resolved).map_err(|e| e.to_string())?,
    };
    Ok(match sidecar.lines.get(&line.to_string()) {
        Some(table) => sidecar.overrides.under(table.overrides),
        None => sidecar.overrides,
    })
}

/// What the sidecar of `resolved` sets that the arguments do not: the
/// values it actually decides, which the recipe records.
pub fn applied(
    resolved: &ResolvedInput,
    antenna_mhz: Option<f32>,
    antenna_separation_m: Option<f32>,
) -> Result<Overrides, String> {
    let from_sidecar = overrides_of(resolved)?;
    Ok(Overrides {
        antenna_mhz: from_sidecar.antenna_mhz.filter(|_| antenna_mhz.is_none()),
        antenna_separation_m: from_sidecar
            .antenna_separation_m
            .filter(|_| antenna_separation_m.is_none()),
    })
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
                antenna_separation_m: Some(4.5)
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
    fn the_arguments_take_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "[overrides]\nantenna_mhz = 50\nantenna_separation_m = 4\n",
        );
        let resolved = ramac(dir.path(), Some(path));
        assert_eq!(
            applied(&resolved, Some(100.), None).unwrap(),
            Overrides {
                antenna_mhz: None,
                antenna_separation_m: Some(4.)
            }
        );
        assert!(applied(&resolved, Some(100.), Some(1.)).unwrap().is_empty());
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
                override_antenna_separation: separation,
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
                override_antenna_separation: separation,
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
        let mut table = String::from(
            "| Key | Unit | Option that takes precedence | Description |\n|---|---|---|---|\n",
        );
        for key in OVERRIDE_KEYS {
            table += &format!(
                "| `{}` | {} | `{}` | {} |\n",
                key.name, key.unit, key.argument, key.description
            );
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
        };
        let table = toml::Table::try_from(all).unwrap();
        let mut fields = table.keys().map(String::as_str).collect::<Vec<_>>();
        fields.sort();
        let mut listed = OVERRIDE_KEYS.iter().map(|k| k.name).collect::<Vec<_>>();
        listed.sort();
        assert_eq!(fields, listed);
    }
}
