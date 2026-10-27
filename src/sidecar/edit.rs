//! Writing sidecars: `ridal override` and `ridal.override()` (#527).
//!
//! A sidecar may have been written by hand, so it is edited in place with
//! `toml_edit`, keeping its comments and layout, rather than read and
//! written out again. Every recording's new sidecar is checked before any
//! is written, so a mistake in one leaves them all as they were, and each
//! is written to a temporary sibling and renamed over the old one.

use std::path::{Path, PathBuf};

use super::{Overrides, OVERRIDE_KEYS, SCHEMA, SIDECAR_SUFFIX};
use crate::formats::{self, FormatKind, LineSelection, ResolvedInput};

/// What to change in each recording's sidecar.
#[derive(Debug, Clone, Default)]
pub struct Edit {
    /// The values to set.
    pub set: Overrides,
    /// The keys to remove, by their names in a sidecar.
    pub unset: Vec<String>,
    /// For an IceRadar file, the lines whose own tables to change instead
    /// of the file's.
    pub lines: Option<LineSelection>,
}

/// What happened to one recording's sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Created(PathBuf),
    Changed(PathBuf),
    /// Already as asked, so not written.
    Unchanged(PathBuf),
    /// `--template` leaves an existing sidecar alone.
    Exists(PathBuf),
}

impl Outcome {
    pub fn path(&self) -> &Path {
        match self {
            Outcome::Created(path)
            | Outcome::Changed(path)
            | Outcome::Unchanged(path)
            | Outcome::Exists(path) => path,
        }
    }

    /// Whether the file was written.
    pub fn written(&self) -> bool {
        matches!(self, Outcome::Created(_) | Outcome::Changed(_))
    }
}

/// The recordings `inputs` name, by any of their files or by glob, each
/// once, in the order first named.
fn recordings(inputs: &[PathBuf]) -> Result<Vec<ResolvedInput>, String> {
    let mut out: Vec<ResolvedInput> = Vec::new();
    for path in crate::processing::input::expand_input_paths(inputs)? {
        let resolved = formats::resolve_input(&path)?;
        if !resolved.header.is_file() {
            return Err(format!("File not found: {}", resolved.header.display()));
        }
        if !out.iter().any(|r| r.header == resolved.header) {
            out.push(resolved);
        }
    }
    Ok(out)
}

/// Where the sidecar of `resolved` is, or would be.
fn sidecar_path(resolved: &ResolvedInput) -> Result<PathBuf, String> {
    if let Some(path) = &resolved.sidecar {
        return Ok(path.clone());
    }
    let stem = resolved
        .header
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{} has no usable file name", resolved.header.display()))?;
    Ok(resolved
        .header
        .with_file_name(format!("{stem}{SIDECAR_SUFFIX}")))
}

/// The line numbers `selection` picks of the file of `resolved`.
fn chosen_lines(resolved: &ResolvedInput, selection: &LineSelection) -> Result<Vec<u32>, String> {
    if resolved.kind != FormatKind::IceRadar {
        return Err(format!(
            "--lines (lines= in Python) applies to IceRadar files only; {} is a {} file",
            resolved.header.display(),
            formats::display_name(resolved.kind)
        ));
    }
    let available = crate::io::iceradar_lines(&resolved.header)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(n, _)| n)
        .collect::<Vec<_>>();
    match selection {
        LineSelection::All => Ok(available),
        LineSelection::Some(wanted) => {
            if let Some(missing) = wanted.iter().find(|n| !available.contains(n)) {
                return Err(format!(
                    "{} has no line {missing}; its lines are {}",
                    resolved.header.display(),
                    available
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            Ok(wanted.clone())
        }
    }
}

/// A sidecar's number as TOML writes it: the shortest decimal that reads
/// back as the same `f32`, so `0.1` is not written as `0.10000000149`.
fn number(value: f32) -> toml_edit::Item {
    let shortest: f64 = value.to_string().parse().expect("a float's own text");
    toml_edit::value(shortest)
}

/// The `overrides` table at `place` in `document`, made where it is
/// missing: the top level for `None`, a line's for `Some`. Tables made only
/// to hold another are implicit, so a new line's values are written under
/// one `[lines.N.overrides]` header.
fn overrides_table(
    document: &mut toml_edit::DocumentMut,
    line: Option<u32>,
) -> Result<&mut toml_edit::Table, String> {
    fn child<'a>(
        table: &'a mut toml_edit::Table,
        key: &str,
        implicit: bool,
        where_: &str,
    ) -> Result<&'a mut toml_edit::Table, String> {
        let item = table.entry(key).or_insert_with(|| {
            let mut new = toml_edit::Table::new();
            new.set_implicit(implicit);
            toml_edit::Item::Table(new)
        });
        item.as_table_mut()
            .ok_or_else(|| format!("{where_}{key} is not a table"))
    }
    let root = document.as_table_mut();
    match line {
        None => child(root, "overrides", false, ""),
        Some(line) => {
            let lines = child(root, "lines", true, "")?;
            let this = child(lines, &line.to_string(), true, "lines.")?;
            child(this, "overrides", false, &format!("lines.{line}."))
        }
    }
}

/// `text`, the sidecar at `path`, with `edit` made in the tables of each
/// of `lines` (the file's for `None`).
fn edited(text: &str, path: &Path, edit: &Edit, lines: &[Option<u32>]) -> Result<String, String> {
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not a valid sidecar: {e}", path.display()))?;
    if !document.contains_key("schema") {
        // Written before every table, as top-level values are.
        document["schema"] = toml_edit::value(i64::from(SCHEMA));
    }
    for &line in lines {
        let table = overrides_table(&mut document, line)?;
        for (key, value) in edit.set.entries() {
            if let Some(value) = value {
                table[key] = number(value);
            }
        }
        for key in &edit.unset {
            table.remove(key);
        }
        if table.is_empty() {
            remove_empty(&mut document, line);
        }
    }
    Ok(document.to_string())
}

/// Remove the `overrides` table at `place` and whatever only held it.
fn remove_empty(document: &mut toml_edit::DocumentMut, line: Option<u32>) {
    let Some(line) = line else {
        document.remove("overrides");
        return;
    };
    let key = line.to_string();
    let Some(lines) = document.get_mut("lines").and_then(|l| l.as_table_mut()) else {
        return;
    };
    if let Some(this) = lines.get_mut(&key).and_then(|t| t.as_table_mut()) {
        this.remove("overrides");
        if this.is_empty() {
            lines.remove(&key);
        }
    }
    if lines.is_empty() {
        document.remove("lines");
    }
}

/// Write `text` to `path` by way of a temporary sibling, so that `path` is
/// the old file or the new one, never part of one.
fn write_replacing(path: &Path, text: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} is not a file name", path.display()))?
        .to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let written = std::fs::write(&temporary, text)
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|e| format!("could not write {}: {e}", path.display()));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

fn check_keys(edit: &Edit) -> Result<(), String> {
    let known = OVERRIDE_KEYS.iter().map(|k| k.name).collect::<Vec<_>>();
    if let Some(unknown) = edit.unset.iter().find(|k| !known.contains(&k.as_str())) {
        return Err(format!(
            "{unknown} is not an override; they are {}",
            known.join(", ")
        ));
    }
    for (key, value) in edit.set.entries() {
        if value.is_some() && edit.unset.iter().any(|k| k == key) {
            return Err(format!("{key} is both set and unset"));
        }
    }
    if edit.set.is_empty() && edit.unset.is_empty() {
        return Err(format!(
            "Nothing to change: give a value to set ({}), a key to --unset, or --template",
            OVERRIDE_KEYS
                .iter()
                .map(|k| format!("--{}", k.name.replace('_', "-")))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    edit.set.check()
}

/// Make `edit` in the sidecar of every recording `inputs` name, creating
/// those that do not exist.
///
/// # Errors
/// An input that is not a recording, a value out of range, an unknown key,
/// `lines` for a format without them or a line a file does not have, or an
/// existing sidecar that is not valid. Nothing is written then.
pub fn apply(inputs: &[PathBuf], edit: &Edit) -> Result<Vec<Outcome>, String> {
    check_keys(edit)?;
    let mut planned = Vec::new();
    for resolved in recordings(inputs)? {
        let path = sidecar_path(&resolved)?;
        let exists = path.is_file();
        let before = if exists {
            std::fs::read_to_string(&path)
                .map_err(|e| format!("Could not read {}: {e}", path.display()))?
        } else {
            String::new()
        };
        // The file as it is must be valid, or its mistakes would be kept.
        super::parse(&before, &path)?;
        let lines = match &edit.lines {
            None => vec![None],
            Some(selection) => chosen_lines(&resolved, selection)?
                .into_iter()
                .map(Some)
                .collect(),
        };
        let after = edited(&before, &path, edit, &lines)?;
        // As every reader will read it.
        let sidecar = super::parse(&after, &path)?;
        super::check_lines(&sidecar, &path, &resolved)?;
        let outcome = match (exists, before == after) {
            (true, true) => Outcome::Unchanged(path),
            (true, false) => Outcome::Changed(path),
            (false, _) => Outcome::Created(path),
        };
        planned.push((outcome, after));
    }
    for (outcome, text) in &planned {
        if outcome.written() {
            write_replacing(outcome.path(), text)?;
        }
    }
    Ok(planned.into_iter().map(|(outcome, _)| outcome).collect())
}

/// What the header of `resolved` says of each override, or `None` where it
/// does not record it.
fn header_values(resolved: &ResolvedInput) -> Overrides {
    let bare = ResolvedInput {
        sidecar: None,
        ..resolved.clone()
    };
    let read = |mhz| crate::processing::input::load_meta(&bare, 0.168, mhz, None, None);
    let (antenna_mhz, separation) = match read(None) {
        Ok(meta) => (Some(meta.antenna_mhz), Some(meta.antenna_separation)),
        // IceRadar: no frequency, so any stands in to read the rest.
        Err(_) => (None, read(Some(1.)).ok().map(|m| m.antenna_separation)),
    };
    Overrides {
        antenna_mhz,
        antenna_separation_m: separation
            .filter(|_| formats::records_antenna_separation(resolved.kind)),
    }
}

/// A sidecar for `resolved` that sets nothing: every key commented out,
/// with what the header says of it.
fn template_text(resolved: &ResolvedInput) -> String {
    let header = header_values(resolved);
    let name = resolved
        .header
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut text = format!(
        "# Overrides for {name}, which take precedence over what its header says.\n\
         # Uncomment a key to set it. The --override-* options of a run take\n\
         # precedence over this file.\n\
         schema = {SCHEMA}\n\n[overrides]\n"
    );
    for (key, said) in header.entries() {
        let about = OVERRIDE_KEYS
            .iter()
            .find(|k| k.name == key)
            .expect("every key is listed");
        text += &format!("# {key} ({}): {}\n", about.unit, about.description);
        match said {
            Some(value) => text += &format!("# The header says {value}.\n# {key} = {value}\n"),
            None => text += &format!("# The header does not record it.\n# {key} =\n"),
        }
    }
    if resolved.kind == FormatKind::IceRadar {
        text += "\n# One line's own values, which take precedence over the file's:\n\
                 # [lines.0.overrides]\n\
                 # antenna_separation_m =\n";
    }
    text
}

/// Write a template sidecar (every key commented out, with what the header
/// says of it) for every recording `inputs` name that has none.
pub fn template(inputs: &[PathBuf]) -> Result<Vec<Outcome>, String> {
    let mut outcomes = Vec::new();
    for resolved in recordings(inputs)? {
        let path = sidecar_path(&resolved)?;
        if path.is_file() {
            outcomes.push(Outcome::Exists(path));
            continue;
        }
        write_replacing(&path, &template_text(&resolved))?;
        outcomes.push(Outcome::Created(path));
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = "dronbreen-20250327-DAT_0066_A1";

    /// The 2025 Drønbreen fixture in a directory of its own.
    fn dronbreen() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        for ext in ["rad", "rd3", "cor"] {
            std::fs::copy(
                format!("assets/mala/{LINE}.{ext}"),
                dir.path().join(format!("{LINE}.{ext}")),
            )
            .unwrap();
        }
        let rad = dir.path().join(format!("{LINE}.rad"));
        (dir, rad)
    }

    fn separation(m: f32) -> Edit {
        Edit {
            set: Overrides {
                antenna_separation_m: Some(m),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn unset(key: &str) -> Edit {
        Edit {
            unset: vec![key.to_string()],
            ..Default::default()
        }
    }

    fn read_back(rad: &Path) -> Overrides {
        super::super::overrides_of(&formats::resolve_input(rad).unwrap()).unwrap()
    }

    #[test]
    fn a_sidecar_is_created_changed_and_left_alone() {
        let (dir, rad) = dronbreen();
        let path = dir.path().join(format!("{LINE}.ridal.toml"));
        // Named by any of its files, more than once: one recording.
        let rd3 = rad.with_extension("rd3");
        assert_eq!(
            apply(&[rad.clone(), rd3], &separation(4.5)).unwrap(),
            [Outcome::Created(path.clone())]
        );
        assert_eq!(read_back(&rad).antenna_separation_m, Some(4.5));
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with("schema = 1\n"), "{written}");

        assert_eq!(
            apply(std::slice::from_ref(&rad), &separation(4.5)).unwrap(),
            [Outcome::Unchanged(path.clone())]
        );
        assert_eq!(
            apply(std::slice::from_ref(&rad), &separation(0.1)).unwrap(),
            [Outcome::Changed(path.clone())]
        );
        // The shortest text that reads back as the value.
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("antenna_separation_m = 0.1\n"),
            "{written}"
        );

        apply(std::slice::from_ref(&rad), &unset("antenna_separation_m")).unwrap();
        assert!(read_back(&rad).is_empty());
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("[overrides]"), "{written}");
    }

    #[test]
    fn what_a_person_wrote_is_kept() {
        let (dir, rad) = dronbreen();
        let path = dir.path().join(format!("{LINE}.ridal.toml"));
        let by_hand = "# Measured on site, 2025-03-27.\n\
                       [overrides]\n\
                       antenna_mhz = 25 # as the label says\n\
                       \n\
                       [native.ramac]\n\
                       OPERATOR = \"EM\"\n";
        std::fs::write(&path, by_hand).unwrap();
        apply(std::slice::from_ref(&rad), &separation(4.)).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        for kept in [
            "# Measured on site, 2025-03-27.",
            "antenna_mhz = 25 # as the label says",
            "[native.ramac]\nOPERATOR = \"EM\"",
            "antenna_separation_m = 4.0",
        ] {
            assert!(written.contains(kept), "{kept:?} in\n{written}");
        }
        assert!(written.starts_with("schema = 1"), "{written}");
    }

    #[test]
    fn a_mistake_writes_nothing() {
        let (dir, rad) = dronbreen();
        let (other_dir, other) = dronbreen();
        let broken = other_dir.path().join(format!("{LINE}.ridal.toml"));
        std::fs::write(&broken, "[overrides]\nantenna = 1\n").unwrap();
        let inputs = [rad.clone(), other];
        let error = apply(&inputs, &separation(4.)).unwrap_err();
        assert!(error.contains("not a valid sidecar"), "{error}");
        // The first was not written either.
        assert!(!dir.path().join(format!("{LINE}.ridal.toml")).exists());

        for (edit, expected) in [
            (separation(-1.), "must be 0 or more"),
            (unset("antenna"), "antenna is not an override"),
            (Edit::default(), "Nothing to change"),
            (
                Edit {
                    unset: vec!["antenna_separation_m".to_string()],
                    ..separation(1.)
                },
                "both set and unset",
            ),
            (
                Edit {
                    lines: Some(LineSelection::All),
                    ..separation(1.)
                },
                "applies to IceRadar files only",
            ),
        ] {
            let error = apply(std::slice::from_ref(&rad), &edit).unwrap_err();
            assert!(error.contains(expected), "{expected}: {error}");
        }
        assert!(!dir.path().join(format!("{LINE}.ridal.toml")).exists());
    }

    #[test]
    #[cfg(not(target_os = "windows"))] // PROJ, for the positions.
    fn a_line_gets_its_own_table() {
        let dir = tempfile::tempdir().unwrap();
        let h5 = dir.path().join("stor.h5");
        let line = || {
            vec![
                crate::io::iceradar::tests::trace(40, 256, Some("103045.6")),
                crate::io::iceradar::tests::trace(40, 256, Some("103046.8")),
            ]
        };
        crate::io::iceradar::tests::write_fixture(
            &h5,
            crate::io::iceradar::Layout::V1,
            &[line(), line(), line()],
            "6/18/2024 1:30:00 PM",
        );
        let path = dir.path().join("stor.ridal.toml");
        let edit = |lines: &str, edit: Edit| Edit {
            lines: Some(lines.parse().unwrap()),
            ..edit
        };
        apply(std::slice::from_ref(&h5), &separation(4.)).unwrap();
        apply(std::slice::from_ref(&h5), &edit("1,2", separation(6.))).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            written,
            "schema = 1\n\n[overrides]\nantenna_separation_m = 4.0\n\n\
             [lines.1.overrides]\nantenna_separation_m = 6.0\n\n\
             [lines.2.overrides]\nantenna_separation_m = 6.0\n"
        );
        let error = apply(std::slice::from_ref(&h5), &edit("7", separation(6.))).unwrap_err();
        assert!(error.contains("has no line 7"), "{error}");

        // Unset, a line's table goes, and the others stay.
        apply(
            std::slice::from_ref(&h5),
            &edit("2", unset("antenna_separation_m")),
        )
        .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("lines.2"), "{written}");
        assert!(written.contains("[lines.1.overrides]"), "{written}");
        apply(
            std::slice::from_ref(&h5),
            &edit("all", unset("antenna_separation_m")),
        )
        .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("lines"), "{written}");
        assert!(written.contains("antenna_separation_m = 4.0"), "{written}");
    }

    #[test]
    fn a_template_shows_the_header_and_sets_nothing() {
        let (dir, rad) = dronbreen();
        let path = dir.path().join(format!("{LINE}.ridal.toml"));
        assert_eq!(
            template(std::slice::from_ref(&rad)).unwrap(),
            [Outcome::Created(path.clone())]
        );
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("# antenna_mhz = 25\n"), "{written}");
        assert!(
            written.contains("# antenna_separation_m = 6.2\n"),
            "{written}"
        );
        assert!(read_back(&rad).is_empty());
        // Uncommenting a line sets it.
        std::fs::write(
            &path,
            written.replace("# antenna_separation_m = 6.2", "antenna_separation_m = 4"),
        )
        .unwrap();
        assert_eq!(read_back(&rad).antenna_separation_m, Some(4.));
        // An existing one is left alone.
        assert_eq!(
            template(std::slice::from_ref(&rad)).unwrap(),
            [Outcome::Exists(path)]
        );
    }
}
