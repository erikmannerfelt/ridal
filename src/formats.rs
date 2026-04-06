use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    Ramac,
    PulseEkko,
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
    }
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
            header: input.to_path_buf(),
            data: input.with_extension("rd3"),
            coordinates: input.with_extension("cor"),
        }),
        Some("rd3") | Some("cor") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::Ramac,
            header: input.with_extension("rad"),
            data: input.with_extension("rd3"),
            coordinates: input.with_extension("cor"),
        }),
        Some("hd") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::PulseEkko,
            header: input.to_path_buf(),
            data: input.with_extension("dt1"),
            coordinates: input.with_extension("gp2"),
        }),
        Some("dt1") | Some("gp2") => Ok(ResolvedInput {
            input: input.to_path_buf(),
            kind: FormatKind::PulseEkko,
            header: input.with_extension("hd"),
            data: input.with_extension("dt1"),
            coordinates: input.with_extension("gp2"),
        }),
        Some(other) => Err(format!(
            "Unsupported input extension '.{other}' for {:?}. Supported formats are RAMAC (.rad/.rd3/.cor) and pulseEKKO (.hd/.dt1/.gp2).",
            input
        )),
        None => {
            let ramac = input.with_extension("rad");
            let pulseekko = input.with_extension("hd");
            match (ramac.is_file(), pulseekko.is_file()) {
                (true, false) => Ok(ResolvedInput {
                    input: input.to_path_buf(),
                    kind: FormatKind::Ramac,
                    header: ramac.clone(),
                    data: ramac.with_extension("rd3"),
                    coordinates: ramac.with_extension("cor"),
                }),
                (false, true) => Ok(ResolvedInput {
                    input: input.to_path_buf(),
                    kind: FormatKind::PulseEkko,
                    header: pulseekko.clone(),
                    data: pulseekko.with_extension("dt1"),
                    coordinates: pulseekko.with_extension("gp2"),
                }),
                (true, true) => Err(format!(
                    "Ambiguous extension-less input {:?}: both {:?} and {:?} exist.",
                    input, ramac, pulseekko
                )),
                (false, false) => Err(format!(
                    "Could not infer format for extension-less input {:?}. Tried {:?} and {:?}.",
                    input, ramac, pulseekko
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
        assert_eq!(names, vec!["ramac".to_string(), "pulseekko".to_string()]);
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
}
