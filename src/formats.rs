use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    Ramac,
    PulseEkko,
    Gssi,
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

pub fn resolve_input(input: &Path) -> Result<ResolvedInput, String> {
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
        }),
        Some("rd3") | Some("cor") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::Ramac,
            header: resolve_sidecar(input, "rad"),
            data: resolve_sidecar(input, "rd3"),
            coordinates: resolve_sidecar(input, "cor"),
        }),
        Some("hd") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::PulseEkko,
            header: resolve_sidecar(input, "hd"),
            data: resolve_sidecar(input, "dt1"),
            coordinates: resolve_sidecar(input, "gp2"),
        }),
        Some("dt1") | Some("gp2") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::PulseEkko,
            header: resolve_sidecar(input, "hd"),
            data: resolve_sidecar(input, "dt1"),
            coordinates: resolve_sidecar(input, "gp2"),
        }),
        Some("dzt") | Some("dzg") | Some("dzx") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::Gssi,
            header: resolve_sidecar(input, "dzt"),
            data: resolve_sidecar(input, "dzt"),
            coordinates: resolve_sidecar(input, "dzg"),
        }),
        Some(other) => Err(format!(
            "Unsupported input extension '.{other}' for {:?}. Supported formats are RAMAC (.rad/.rd3/.cor), pulseEKKO (.hd/.dt1/.gp2), and GSSI (.dzt/.dzg/.dzx).",
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
                }),
                (None, Some(pulseekko), None) => Ok(ResolvedInput {
                    input: input.to_path_buf(),
                    kind: FormatKind::PulseEkko,
                    header: pulseekko.clone(),
                    data: resolve_sidecar(&pulseekko, "dt1"),
                    coordinates: resolve_sidecar(&pulseekko, "gp2"),
                }),
                (None, None, Some(gssi)) => Ok(ResolvedInput {
                    input: input.to_path_buf(),
                    kind: FormatKind::Gssi,
                    header: gssi.clone(),
                    data: gssi.clone(),
                    coordinates: resolve_sidecar(&gssi, "dzg"),
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
                "gssi".to_string()
            ]
        );
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
}
