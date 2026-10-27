use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    Ramac,
    PulseEkko,
    Gssi,
    IceRadar,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FormatCapabilities {
    pub read: bool,
    pub write: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FormatFiles {
    pub header: &'static str,
    pub data: &'static str,
    pub coordinates: &'static str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FormatInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub capabilities: FormatCapabilities,
    pub files: FormatFiles,
}

#[derive(Debug, Clone)]
pub struct ResolvedInput {
    pub input: PathBuf,
    pub kind: FormatKind,
    pub header: PathBuf,
    pub data: PathBuf,
    pub coordinates: PathBuf,
    /// Which part of the file is the recording: all of it, unless the
    /// format's files can hold several recordings. Set by [`with_lines`].
    pub part: Part,
}

/// Which part of a file holding several recordings one recording is.
///
/// Every field `None` (the default) is the whole file, which is every
/// recording of a format whose files hold one. Lines (IceRadar) are
/// recorded one after another and may be merged in order; a field for
/// channels, recorded side by side, would sit beside `line` and never be
/// merged. Internal: users choose with the format's own word (`--lines`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct Part {
    /// The line, of an IceRadar file.
    // Defaulted, so a field added beside it later still reads a recipe
    // written before it.
    #[serde(default)]
    #[cfg_attr(feature = "server", schema(required = true))]
    pub line: Option<u32>,
}

impl Part {
    /// Whether this is the whole file.
    pub fn is_whole(&self) -> bool {
        *self == Part::default()
    }
}

pub fn all_formats() -> Vec<FormatInfo> {
    vec![
        FormatInfo {
            name: "ramac",
            description: "Malå RAMAC format",
            capabilities: FormatCapabilities {
                read: true,
                write: false,
            },
            files: FormatFiles {
                header: ".rad",
                data: ".rd3",
                coordinates: ".cor",
            },
        },
        FormatInfo {
            name: "pulseekko",
            description: "Sensors & Software pulseEKKO format",
            capabilities: FormatCapabilities {
                read: true,
                write: false,
            },
            files: FormatFiles {
                header: ".hd",
                data: ".dt1",
                coordinates: ".gp2",
            },
        },
        FormatInfo {
            name: "gssi",
            description: "GSSI DZT/DZG format",
            capabilities: FormatCapabilities {
                read: true,
                write: false,
            },
            files: FormatFiles {
                header: ".DZT",
                data: ".DZT",
                coordinates: ".DZG",
            },
        },
        FormatInfo {
            name: "iceradar",
            description: "Blue System Integration IceRadar HDF5 format",
            capabilities: FormatCapabilities {
                read: true,
                write: false,
            },
            files: FormatFiles {
                header: ".h5",
                data: ".h5",
                coordinates: ".h5",
            },
        },
    ]
}

pub fn format_info(kind: FormatKind) -> FormatInfo {
    match kind {
        FormatKind::Ramac => all_formats()
            .into_iter()
            .find(|fmt| fmt.name == "ramac")
            .unwrap(),
        FormatKind::PulseEkko => all_formats()
            .into_iter()
            .find(|fmt| fmt.name == "pulseekko")
            .unwrap(),
        FormatKind::Gssi => all_formats()
            .into_iter()
            .find(|fmt| fmt.name == "gssi")
            .unwrap(),
        FormatKind::IceRadar => all_formats()
            .into_iter()
            .find(|fmt| fmt.name == "iceradar")
            .unwrap(),
    }
}

pub fn find_neighbor_case_insensitive(base: &Path, target_extension: &str) -> Option<PathBuf> {
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    let stem = base.file_stem().or_else(|| base.file_name())?.to_str()?;
    let target_extension = target_extension.trim_start_matches('.');

    std::fs::read_dir(parent)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case(stem))
                && path
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case(target_extension))
        })
}

fn resolve_sidecar(base: &Path, target_extension: &str) -> PathBuf {
    find_neighbor_case_insensitive(base, target_extension)
        .unwrap_or_else(|| base.with_extension(target_extension))
}

/// What a Ridal coordinate CSV beside a recording is called, after the
/// recording's stem: `DAT_0001_A1.coords.csv` (#13).
pub const COORDS_CSV_SUFFIX: &str = ".coords.csv";

/// Whether `path` is to be read as a Ridal coordinate CSV rather than as a
/// format's own positions file.
pub fn is_coords_csv(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
}

/// The recording stem of a `<stem>.coords.csv`.
fn coords_csv_stem(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    let split = name.len().checked_sub(COORDS_CSV_SUFFIX.len())?;
    (split > 0
        && name.is_char_boundary(split)
        && name[split..].eq_ignore_ascii_case(COORDS_CSV_SUFFIX))
    .then(|| &name[..split])
}

/// `<stem>.coords.csv` in `dir`, whatever the case of its name.
fn find_coords_csv(dir: &Path, stem: &str) -> Option<PathBuf> {
    let wanted = format!("{stem}{COORDS_CSV_SUFFIX}");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(&wanted))
        })
}

fn header_stem(resolved: &ResolvedInput) -> Result<&str, String> {
    resolved
        .header
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{} has no usable file name", resolved.header.display()))
}

/// Resolve a recording's files from any one of them.
///
/// Its positions are a `<stem>.coords.csv` beside the header when there is
/// one, and the format's own positions file otherwise.
pub fn resolve_input(input: &Path) -> Result<ResolvedInput, String> {
    // A coordinate CSV stands for its recording, as a `.cor` does.
    let mut resolved = match coords_csv_stem(input) {
        Some(stem) => ResolvedInput {
            input: input.to_path_buf(),
            ..resolve_files(&input.with_file_name(stem))?
        },
        None => resolve_files(input)?,
    };
    let dir = resolved.header.parent().unwrap_or_else(|| Path::new("."));
    if let Some(csv) = find_coords_csv(dir, header_stem(&resolved)?) {
        resolved.coordinates = csv;
    }
    Ok(resolved)
}

/// The format's own positions file of a recording, beside its header,
/// whether or not it exists, and whatever [`resolve_input`] or `--cor`
/// chose instead.
pub fn own_positions(resolved: &ResolvedInput) -> PathBuf {
    if resolved.kind == FormatKind::IceRadar {
        return resolved.header.clone();
    }
    let native = format_info(resolved.kind).files.coordinates;
    resolve_sidecar(&resolved.header, native.trim_start_matches('.'))
}

/// Refuse one positions file for several recordings: trace numbers start
/// over in each, so it could not be right for more than one.
pub fn check_cor_count(n_recordings: usize, cor: Option<&Path>) -> Result<(), String> {
    match cor {
        Some(cor) if n_recordings > 1 && !cor.is_dir() => Err(format!(
            "{} is one positions file, but {n_recordings} recordings were given. \
             Give a directory instead, holding a <stem>{COORDS_CSV_SUFFIX} or the \
             format's own positions file for each recording.",
            cor.display()
        )),
        _ => Ok(()),
    }
}

/// Apply `--cor` (`cor=` in Python) to resolved recordings.
///
/// - `None`: each keeps the positions [`resolve_input`] found.
/// - A file: the positions of the one recording given.
/// - A directory: for each recording, `<stem>.coords.csv` in it, or else
///   the format's own positions file with that stem (`<stem>.cor`), where
///   `<stem>` is the header's.
///
/// # Errors
/// A file for more than one recording, or a directory without positions
/// for one of them.
pub fn with_coordinates(resolved: &mut [ResolvedInput], cor: Option<&Path>) -> Result<(), String> {
    let Some(cor) = cor else {
        return Ok(());
    };
    check_cor_count(resolved.len(), Some(cor))?;
    if !cor.is_dir() {
        if let [one] = resolved {
            one.coordinates = cor.to_path_buf();
        }
        return Ok(());
    }
    for recording in resolved.iter_mut() {
        let stem = header_stem(recording)?;
        let native = format_info(recording.kind).files.coordinates;
        recording.coordinates = find_coords_csv(cor, stem)
            .or_else(|| {
                find_neighbor_case_insensitive(&cor.join(format!("{stem}{native}")), native)
            })
            .ok_or_else(|| {
                format!(
                    "No positions for {} in {}: neither {stem}{COORDS_CSV_SUFFIX} \
                     nor {stem}{native} is there.",
                    recording.header.display(),
                    cor.display()
                )
            })?;
    }
    Ok(())
}

/// Whether an extension is that of an IceRadar autosave: `x1` in
/// `name.hdf5.x1`.
fn is_iceradar_autosave(extension: &str) -> bool {
    extension.len() > 1
        && extension.starts_with('x')
        && extension[1..].bytes().all(|b| b.is_ascii_digit())
}

/// Which lines of a multi-line file to read: `--lines`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineSelection {
    All,
    /// In the order given, which is the order they are merged in.
    Some(Vec<u32>),
}

impl std::str::FromStr for LineSelection {
    type Err = String;

    /// `all`, or a comma-separated list of line numbers and ranges:
    /// `3`, `0,2-4`.
    fn from_str(text: &str) -> Result<Self, String> {
        if text.trim().eq_ignore_ascii_case("all") {
            return Ok(LineSelection::All);
        }
        let bad = |part: &str| {
            format!(
                "{part:?} in lines {text:?} is not a line number or a range: give \"all\", or \
                 numbers and ranges separated by commas, such as 0,2-4"
            )
        };
        let mut lines = Vec::new();
        for part in text.split(',').map(str::trim) {
            match part.split_once('-') {
                Some((from, to)) => {
                    let from = from.trim().parse::<u32>().map_err(|_| bad(part))?;
                    let to = to.trim().parse::<u32>().map_err(|_| bad(part))?;
                    if to < from {
                        return Err(bad(part));
                    }
                    lines.extend(from..=to);
                }
                None => lines.push(part.parse::<u32>().map_err(|_| bad(part))?),
            }
        }
        if let Some(line) = lines
            .iter()
            .enumerate()
            .find_map(|(i, line)| lines[..i].contains(line).then_some(line))
        {
            return Err(format!("Line {line} is given twice in lines {text:?}"));
        }
        Ok(LineSelection::Some(lines))
    }
}

/// Expand each multi-line recording into one input per selected line, in
/// the order given, for the caller to merge as it merges several files.
///
/// A file of one line needs no selection. A file of several needs one, and
/// the error lists its lines. A selection for inputs that hold no lines is
/// refused rather than ignored.
pub fn with_lines(
    resolved: Vec<ResolvedInput>,
    selection: Option<&LineSelection>,
) -> Result<Vec<ResolvedInput>, String> {
    if selection.is_some() && resolved.iter().all(|r| r.kind != FormatKind::IceRadar) {
        return Err(
            "--lines (lines= in Python) applies to IceRadar files only; none was given".to_string(),
        );
    }
    let mut out = Vec::new();
    for recording in resolved {
        if recording.kind != FormatKind::IceRadar {
            out.push(recording);
            continue;
        }
        let lines = crate::io::iceradar_lines(&recording.header).map_err(|e| e.to_string())?;
        let available = lines.iter().map(|(n, _)| *n).collect::<Vec<_>>();
        let chosen = match (selection, &lines[..]) {
            // Left unset, so the output and the recipe are those of a
            // file without lines; the reader takes the only one.
            (None, [_]) => {
                out.push(recording);
                continue;
            }
            (None, _) => {
                let listing = lines
                    .iter()
                    .map(|(n, traces)| format!("line_{n} ({traces} traces)"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "{} holds {} lines: {listing}. Choose which to process with --lines (lines= in Python): \
                     one (--lines {first}), several, merged in the order given (--lines \
                     {first},{second}), or all of them (--lines all). Lines recorded with \
                     different settings cannot be merged.",
                    recording.header.display(),
                    lines.len(),
                    first = available[0],
                    second = available[1],
                ));
            }
            (Some(LineSelection::All), _) => available.clone(),
            (Some(LineSelection::Some(wanted)), _) => {
                if let Some(missing) = wanted.iter().find(|n| !available.contains(n)) {
                    return Err(format!(
                        "{} has no line {missing}; its lines are {}",
                        recording.header.display(),
                        available
                            .iter()
                            .map(u32::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                wanted.clone()
            }
        };
        out.extend(chosen.into_iter().map(|line| ResolvedInput {
            part: Part { line: Some(line) },
            ..recording.clone()
        }));
    }
    Ok(out)
}

/// A recording's files from any one of them, with the format's own
/// positions file.
fn resolve_files(input: &Path) -> Result<ResolvedInput, String> {
    let ext = input
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());

    match ext.as_deref() {
        Some("rad") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::Ramac,
            header: resolve_sidecar(input, "rad"),
            data: resolve_sidecar(input, "rd3"),
            coordinates: resolve_sidecar(input, "cor"),
            part: Part::default(),
        }),
        Some("rd3") | Some("cor") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::Ramac,
            header: resolve_sidecar(input, "rad"),
            data: resolve_sidecar(input, "rd3"),
            coordinates: resolve_sidecar(input, "cor"),
            part: Part::default(),
        }),
        Some("hd") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::PulseEkko,
            header: resolve_sidecar(input, "hd"),
            data: resolve_sidecar(input, "dt1"),
            coordinates: resolve_sidecar(input, "gp2"),
            part: Part::default(),
        }),
        Some("dt1") | Some("gp2") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::PulseEkko,
            header: resolve_sidecar(input, "hd"),
            data: resolve_sidecar(input, "dt1"),
            coordinates: resolve_sidecar(input, "gp2"),
            part: Part::default(),
        }),
        Some("dzt") | Some("dzg") | Some("dzx") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::Gssi,
            header: resolve_sidecar(input, "dzt"),
            data: resolve_sidecar(input, "dzt"),
            coordinates: resolve_sidecar(input, "dzg"),
            part: Part::default(),
        }),
        // One file is header, data and positions at once.
        Some("h5") | Some("hdf5") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::IceRadar,
            header: input.to_path_buf(),
            data: input.to_path_buf(),
            coordinates: input.to_path_buf(),
            part: Part::default(),
        }),
        Some(autosave) if is_iceradar_autosave(autosave) => Err(format!(
            "{:?} looks like an IceRadar autosave, a copy of the first part of the file it is named after. Give that file ({:?}) instead.",
            input,
            input.with_extension("")
        )),
        Some(other) => Err(format!(
            "Unsupported input extension '.{other}' for {:?}. Supported formats are RAMAC (.rad/.rd3/.cor), pulseEKKO (.hd/.dt1/.gp2), GSSI (.dzt/.dzg/.dzx) and IceRadar (.h5/.hdf5), and a recording's <stem>{COORDS_CSV_SUFFIX}.",
            input
        )),
        None => {
            let ramac = find_neighbor_case_insensitive(input, "rad");
            let pulseekko = find_neighbor_case_insensitive(input, "hd");
            let gssi = find_neighbor_case_insensitive(input, "dzt");
            match (ramac, pulseekko, gssi) {
                (Some(ramac), None, None) => Ok(ResolvedInput {
                    input: input.to_path_buf(),
                    kind: FormatKind::Ramac,
                    header: ramac.clone(),
                    data: resolve_sidecar(&ramac, "rd3"),
                    coordinates: resolve_sidecar(&ramac, "cor"),
                    part: Part::default(),
                }),
                (None, Some(pulseekko), None) => Ok(ResolvedInput {
                    input: input.to_path_buf(),
                    kind: FormatKind::PulseEkko,
                    header: pulseekko.clone(),
                    data: resolve_sidecar(&pulseekko, "dt1"),
                    coordinates: resolve_sidecar(&pulseekko, "gp2"),
                    part: Part::default(),
                }),
                (None, None, Some(gssi)) => Ok(ResolvedInput {
                    input: input.to_path_buf(),
                    kind: FormatKind::Gssi,
                    header: gssi.clone(),
                    data: gssi.clone(),
                    coordinates: resolve_sidecar(&gssi, "dzg"),
                    part: Part::default(),
                }),
                (Some(ramac), Some(pulseekko), None) => Err(format!(
                    "Ambiguous extension-less input {:?}: both {:?} and {:?} exist.",
                    input, ramac, pulseekko
                )),
                (Some(ramac), None, Some(gssi)) => Err(format!(
                    "Ambiguous extension-less input {:?}: both {:?} and {:?} exist.",
                    input, ramac, gssi
                )),
                (None, Some(pulseekko), Some(gssi)) => Err(format!(
                    "Ambiguous extension-less input {:?}: both {:?} and {:?} exist.",
                    input, pulseekko, gssi
                )),
                (Some(ramac), Some(pulseekko), Some(gssi)) => Err(format!(
                    "Ambiguous extension-less input {:?}: {:?}, {:?}, and {:?} exist.",
                    input, ramac, pulseekko, gssi
                )),
                (None, None, None) => Err(format!(
                    "Could not infer format for extension-less input {:?}. Tried {:?}, {:?}, and {:?}.",
                    input,
                    input.with_extension("rad"),
                    input.with_extension("hd"),
                    input.with_extension("dzt")
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_formats_names() {
        let names = all_formats()
            .into_iter()
            .map(|fmt| fmt.name.to_string())
            .collect::<Vec<String>>();
        assert_eq!(
            names,
            vec![
                "ramac".to_string(),
                "pulseekko".to_string(),
                "gssi".to_string(),
                "iceradar".to_string()
            ]
        );
    }

    #[test]
    fn an_iceradar_file_is_its_own_header_data_and_positions() {
        for name in ["stor.h5", "heli.HDF5"] {
            let resolved = resolve_input(Path::new(name)).unwrap();
            assert_eq!(resolved.kind, FormatKind::IceRadar);
            assert_eq!(resolved.data, PathBuf::from(name));
            assert_eq!(resolved.coordinates, PathBuf::from(name));
            assert_eq!(own_positions(&resolved), PathBuf::from(name));
            assert!(resolved.part.is_whole());
        }
        let error = resolve_input(Path::new("heli.hdf5.x1")).unwrap_err();
        assert!(error.contains("autosave"), "{error}");
        assert!(error.contains("heli.hdf5\""), "{error}");

        // --lines is refused rather than ignored for formats without lines.
        let rad = vec![resolve_input(Path::new("line01.rad")).unwrap()];
        let all = Some(LineSelection::All);
        assert!(with_lines(rad.clone(), all.as_ref()).is_err());
        assert_eq!(with_lines(rad, None).unwrap().len(), 1);
    }

    #[test]
    fn test_resolve_ramac_extensions() {
        let resolved = resolve_input(Path::new("line01.rad")).unwrap();
        assert_eq!(resolved.kind, FormatKind::Ramac);
        assert_eq!(resolved.data, PathBuf::from("line01.rd3"));
        assert_eq!(resolved.coordinates, PathBuf::from("line01.cor"));

        let resolved = resolve_input(Path::new("line01.rd3")).unwrap();
        assert_eq!(resolved.kind, FormatKind::Ramac);
        assert_eq!(resolved.header, PathBuf::from("line01.rad"));
    }

    #[test]
    fn test_resolve_pulseekko_extensions() {
        let resolved = resolve_input(Path::new("line01.hd")).unwrap();
        assert_eq!(resolved.kind, FormatKind::PulseEkko);
        assert_eq!(resolved.data, PathBuf::from("line01.dt1"));
        assert_eq!(resolved.coordinates, PathBuf::from("line01.gp2"));

        let resolved = resolve_input(Path::new("line01.dt1")).unwrap();
        assert_eq!(resolved.kind, FormatKind::PulseEkko);
        assert_eq!(resolved.header, PathBuf::from("line01.hd"));
    }

    #[test]
    fn test_resolve_gssi_extensions() {
        let resolved = resolve_input(Path::new("line01.dzt")).unwrap();
        assert_eq!(resolved.kind, FormatKind::Gssi);
        assert_eq!(resolved.data, PathBuf::from("line01.dzt"));
        assert_eq!(resolved.coordinates, PathBuf::from("line01.dzg"));

        let resolved = resolve_input(Path::new("line01.dzg")).unwrap();
        assert_eq!(resolved.kind, FormatKind::Gssi);
        assert_eq!(resolved.header, PathBuf::from("line01.dzt"));
    }

    #[test]
    fn test_resolve_case_insensitive_sidecars() {
        let temp_dir = tempfile::tempdir().unwrap();
        let rad = temp_dir.path().join("LINE01.RAD");
        let rd3 = temp_dir.path().join("LINE01.RD3");
        let cor = temp_dir.path().join("LINE01.COR");
        std::fs::write(&rad, "").unwrap();
        std::fs::write(&rd3, "").unwrap();
        std::fs::write(&cor, "").unwrap();

        let resolved = resolve_input(&temp_dir.path().join("LINE01")).unwrap();
        assert_eq!(resolved.kind, FormatKind::Ramac);
        assert_eq!(resolved.header, rad);
        assert_eq!(resolved.data, rd3);
        assert_eq!(resolved.coordinates, cor);

        let dzt = temp_dir.path().join("TRACK.DZT");
        let dzg = temp_dir.path().join("TRACK.DZG");
        std::fs::write(&dzt, "").unwrap();
        std::fs::write(&dzg, "").unwrap();

        let resolved = resolve_input(&temp_dir.path().join("TRACK")).unwrap();
        assert_eq!(resolved.kind, FormatKind::Gssi);
        assert_eq!(resolved.header, dzt);
        assert_eq!(resolved.coordinates, dzg);
    }

    #[test]
    fn cor_applies_a_file_to_one_recording_and_a_directory_by_stem() {
        let recordings = tempfile::tempdir().unwrap();
        let positions = tempfile::tempdir().unwrap();
        let rad = recordings.path().join("LINE01.rad");
        let dzt = recordings.path().join("track.DZT");
        for path in [&rad, &dzt] {
            std::fs::write(path, "").unwrap();
        }
        let csv = positions.path().join("line01.COORDS.csv");
        let dzg = positions.path().join("track.dzg");
        for path in [&csv, &dzg] {
            std::fs::write(path, "").unwrap();
        }
        let resolve = || vec![resolve_input(&rad).unwrap(), resolve_input(&dzt).unwrap()];

        let mut both = resolve();
        with_coordinates(&mut both, None).unwrap();
        assert_eq!(both[0].coordinates, recordings.path().join("LINE01.cor"));

        let mut both = resolve();
        with_coordinates(&mut both, Some(positions.path())).unwrap();
        assert_eq!((&both[0].coordinates, &both[1].coordinates), (&csv, &dzg));

        let mut both = resolve();
        let error = with_coordinates(&mut both, Some(&csv)).unwrap_err();
        assert!(error.contains("2 recordings were given"), "{error}");

        let mut one = vec![resolve_input(&dzt).unwrap()];
        with_coordinates(&mut one, Some(&csv)).unwrap();
        assert_eq!(one[0].coordinates, csv);

        std::fs::remove_file(&dzg).unwrap();
        let mut both = resolve();
        let error = with_coordinates(&mut both, Some(positions.path())).unwrap_err();
        assert!(
            error.contains("neither track.coords.csv nor track.DZG"),
            "{error}"
        );
    }

    #[test]
    fn coords_csv_stem_needs_the_whole_suffix_and_a_stem() {
        assert_eq!(coords_csv_stem(Path::new("a/L1.coords.csv")), Some("L1"));
        assert_eq!(coords_csv_stem(Path::new("L1.Coords.CSV")), Some("L1"));
        assert_eq!(coords_csv_stem(Path::new(".coords.csv")), None);
        assert_eq!(coords_csv_stem(Path::new("L1.csv")), None);
        assert!(resolve_input(Path::new("L1.csv")).is_err());
    }
}
