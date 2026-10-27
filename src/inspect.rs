//! `ridal info`: what a raw recording or a processed file contains.

use std::error::Error;
use std::path::{Path, PathBuf};

use ndarray::Array1;
use serde::Serialize;

use crate::formats::{self, ResolvedInput};
use crate::gpr::{GPRLocation, GPRMeta, LocationCorrection};
use crate::processing::input;
use crate::tools;

#[derive(Debug, Clone)]
pub struct InfoParams {
    pub filepaths: Vec<PathBuf>,
    pub dem_path: Option<PathBuf>,
    pub cor_path: Option<PathBuf>,
    pub medium_velocity: f32,
    pub crs: Option<String>,
    pub override_antenna_mhz: Option<f32>,
    pub override_antenna_separation: Option<f32>,
    pub override_date: Option<chrono::NaiveDate>,
    pub sidecar: crate::sidecar::Choice,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct NamedFormat {
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct MetadataSummary {
    pub samples: u32,
    pub sampling_frequency_mhz: f32,
    pub frequency_steps: u32,
    pub time_interval_s: f32,
    pub antenna_name: String,
    pub antenna_mhz: f32,
    pub antenna_separation_m: f32,
    pub time_window_ns: f32,
    pub last_trace: u32,
    pub medium_velocity_m_per_ns: f32,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct CentroidSummary {
    pub easting: f64,
    pub northing: f64,
    pub altitude: f64,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct CorrectionSummary {
    pub kind: String,
    #[cfg_attr(feature = "server", schema(required = true))]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct LocationSummary {
    pub n_points: usize,
    pub crs: String,
    pub start_time: String,
    pub stop_time: String,
    pub duration_s: f64,
    pub track_length_m: f64,
    pub altitude_min_m: f64,
    pub altitude_max_m: f64,
    pub centroid: CentroidSummary,
    pub correction: CorrectionSummary,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct RelatedFiles {
    pub header: String,
    pub data: String,
    pub coordinates: String,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct InfoRecord {
    pub input: String,
    pub format: NamedFormat,
    pub metadata: MetadataSummary,
    pub location: LocationSummary,
    pub related_files: RelatedFiles,
    /// What the recording's sidecar overrode, or `null` when it was read
    /// without one or the sidecar decided nothing (#527).
    #[cfg_attr(feature = "server", schema(required = true))]
    pub sidecar: Option<SidecarSummary>,
}

/// What a recording's sidecar overrode, beside what its header says.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct SidecarSummary {
    pub path: String,
    /// The values the sidecar decided: those no argument gave instead.
    /// `metadata` already holds them.
    pub applied: crate::sidecar::Overrides,
    /// What the header says of each value in `applied`, left out where
    /// the format does not record it.
    pub header: crate::sidecar::Overrides,
}

/// Whether `records`, in merge order, can be merged into one radargram, from
/// their headers alone: the checks [`GPR::merge`] makes that do not need the
/// data, so a mismatch can be refused before anything is processed (#382).
/// Keep the two in step.
///
/// The message counts the recordings with each value of the first property
/// that differs, and names the files in every group but a clear majority,
/// so that one odd file among many stands out.
pub fn check_mergeable(records: &[InfoRecord]) -> Result<(), String> {
    type Property = (&'static str, &'static str, fn(&InfoRecord) -> String);
    let properties: [Property; 4] = [
        (
            "their positions are in different CRSs",
            "; give one CRS for all of them",
            |r| r.location.crs.clone(),
        ),
        ("their antenna frequencies differ", "", |r| {
            format!("{} MHz", r.metadata.antenna_mhz)
        }),
        ("their time windows differ", "", |r| {
            format!("{} ns", r.metadata.time_window_ns)
        }),
        ("their antenna separations differ", "", |r| {
            format!("{} m", r.metadata.antenna_separation_m)
        }),
    ];
    let name = |record: &InfoRecord| {
        Path::new(&record.input)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| record.input.clone())
    };
    for (what, hint, value_of) in properties {
        // Groups in order of first appearance, then largest first.
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        for record in records {
            let value = value_of(record);
            match groups.iter_mut().find(|(v, _)| *v == value) {
                Some((_, names)) => names.push(name(record)),
                None => groups.push((value, vec![name(record)])),
            }
        }
        if groups.len() < 2 {
            continue;
        }
        groups.sort_by_key(|(_, names)| std::cmp::Reverse(names.len()));
        let clear_majority = groups[0].1.len() > groups[1].1.len();
        let described: Vec<String> = groups
            .iter()
            .enumerate()
            .map(|(i, (value, names))| {
                let verb = if names.len() == 1 { "is" } else { "are" };
                let count = format!("{} {verb} {value}", names.len());
                if i == 0 && clear_majority {
                    return count;
                }
                const SHOWN: usize = 3;
                let mut listed = names
                    .iter()
                    .take(SHOWN)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                if names.len() > SHOWN {
                    listed.push_str(&format!(" and {} more", names.len() - SHOWN));
                }
                format!("{count} ({listed})")
            })
            .collect();
        return Err(format!(
            "These recordings cannot be merged into one radargram: {what}{hint}. {}.",
            described.join("; ")
        ));
    }
    Ok(())
}

/// What `ridal info` reports for a NetCDF file Ridal processed (#11).
///
/// Identity rather than contents: enough to match a local file to a radargram
/// a server already has. A file from a Ridal too old to have radargram ids
/// is reported rather than refused, with `reprocess_reason` set and the
/// identity fields `None`, so a caller sorting many files does not have to
/// tell that apart from a failure.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessedInfoRecord {
    pub input: String,
    pub format: NamedFormat,
    /// The Ridal version that wrote the file.
    pub ridal_version: String,
    /// Why the file has to be reprocessed before Ridal can use it, or
    /// `None` when it does not.
    pub reprocess_reason: Option<String>,
    pub radargram_id: Option<String>,
    pub display_name: Option<String>,
    pub group_name: Option<String>,
    pub group_id: Option<String>,
    /// Verbatim from the file, since the revision id of a file without a
    /// recipe hashes this string.
    pub processing_datetime: Option<String>,
    /// The hash of what the file was made from (#382), or `None` for a
    /// file written before recipes were recorded.
    pub recipe_hash: Option<String>,
    /// The id a server gives this revision of the radargram (#117): from
    /// `recipe_hash` when there is one, otherwise from
    /// `processing_datetime`.
    pub revision_id: Option<String>,
    pub samples: Option<usize>,
    pub traces: Option<usize>,
}

/// One inspected input: a raw recording, or a file Ridal processed.
///
/// Untagged, so each serializes as its own record; `format.name` tells them
/// apart (`ridal` for a processed file).
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Inspected {
    Raw(InfoRecord),
    Processed(ProcessedInfoRecord),
}

fn metadata_summary(meta: &GPRMeta) -> MetadataSummary {
    MetadataSummary {
        samples: meta.samples,
        sampling_frequency_mhz: meta.frequency,
        frequency_steps: meta.frequency_steps,
        time_interval_s: meta.time_interval,
        antenna_name: meta.antenna.clone(),
        antenna_mhz: meta.antenna_mhz,
        antenna_separation_m: meta.antenna_separation,
        time_window_ns: meta.time_window,
        last_trace: meta.last_trace,
        medium_velocity_m_per_ns: meta.medium_velocity,
    }
}

fn correction_summary(location: &GPRLocation) -> CorrectionSummary {
    match &location.correction {
        LocationCorrection::None => CorrectionSummary {
            kind: "none".to_string(),
            source: None,
        },
        LocationCorrection::Dem { path, .. } => CorrectionSummary {
            kind: "dem".to_string(),
            source: Some(path.to_string_lossy().to_string()),
        },
        LocationCorrection::InCoordinates { path } => CorrectionSummary {
            kind: "dem_in_coordinates".to_string(),
            source: Some(path.to_string_lossy().to_string()),
        },
    }
}

fn location_summary(location: &GPRLocation) -> Result<LocationSummary, String> {
    if location.cor_points.is_empty() {
        return Err("No coordinate points were available for the input.".to_string());
    }
    let altitudes = Array1::from_iter(location.cor_points.iter().map(|point| point.altitude));
    let eastings = Array1::from_iter(location.cor_points.iter().map(|point| point.easting));
    let northings = Array1::from_iter(location.cor_points.iter().map(|point| point.northing));
    let length = location.length();
    let duration = location.cor_points[location.cor_points.len() - 1].time_seconds
        - location.cor_points[0].time_seconds;

    Ok(LocationSummary {
        n_points: location.cor_points.len(),
        crs: location.crs.clone(),
        start_time: tools::seconds_to_rfc3339(location.cor_points[0].time_seconds),
        stop_time: tools::seconds_to_rfc3339(
            location.cor_points[location.cor_points.len() - 1].time_seconds,
        ),
        duration_s: duration,
        track_length_m: length,
        altitude_min_m: altitudes.iter().cloned().fold(f64::INFINITY, f64::min),
        altitude_max_m: altitudes.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        centroid: CentroidSummary {
            easting: eastings.mean().unwrap(),
            northing: northings.mean().unwrap(),
            altitude: altitudes.mean().unwrap(),
        },
        correction: correction_summary(location),
    })
}

fn info_record(
    resolved: &ResolvedInput,
    meta: &GPRMeta,
    location: &GPRLocation,
    sidecar: Option<SidecarSummary>,
) -> Result<InfoRecord, String> {
    let fmt = formats::format_info(resolved.kind);
    Ok(InfoRecord {
        input: resolved.input.to_string_lossy().to_string(),
        format: NamedFormat {
            name: fmt.name,
            description: fmt.description,
        },
        metadata: metadata_summary(meta),
        location: location_summary(location)?,
        related_files: RelatedFiles {
            header: resolved.header.to_string_lossy().to_string(),
            data: resolved.data.to_string_lossy().to_string(),
            coordinates: resolved.coordinates.to_string_lossy().to_string(),
        },
        sidecar,
    })
}

/// What the sidecar of `resolved` decided, beside what its header says,
/// or `None` when it decided nothing. `meta` is the recording as read,
/// with every override.
fn sidecar_summary(
    resolved: &ResolvedInput,
    meta: &GPRMeta,
    params: &InfoParams,
) -> Result<Option<SidecarSummary>, Box<dyn std::error::Error>> {
    let applied = crate::sidecar::applied(
        resolved,
        params.override_antenna_mhz,
        params.override_antenna_separation,
    )?;
    let Some(path) = resolved.sidecar.as_ref().filter(|_| !applied.is_empty()) else {
        return Ok(None);
    };
    let bare = ResolvedInput {
        sidecar: None,
        ..resolved.clone()
    };
    let read = |mhz| input::load_meta(&bare, meta.medium_velocity, mhz, None, params.override_date);
    // A format that does not record the frequency cannot be read without
    // one; the one the recording was read with stands in, and only the
    // separation is taken from that read.
    let (header_mhz, header) = match read(None) {
        Ok(header) => (Some(header.antenna_mhz), header),
        Err(_) => (None, read(Some(meta.antenna_mhz))?),
    };
    let header_separation =
        formats::records_antenna_separation(resolved.kind).then_some(header.antenna_separation);
    Ok(Some(SidecarSummary {
        path: path.to_string_lossy().to_string(),
        applied,
        header: crate::sidecar::Overrides {
            antenna_mhz: header_mhz.filter(|_| applied.antenna_mhz.is_some()),
            antenna_separation_m: header_separation
                .filter(|_| applied.antenna_separation_m.is_some()),
        },
    }))
}

/// Inspect a NetCDF file as Ridal output, without loading its amplitudes.
fn processed_info_record(path: &Path) -> Result<ProcessedInfoRecord, String> {
    let record = |ridal_version: String| ProcessedInfoRecord {
        input: path.to_string_lossy().to_string(),
        format: NamedFormat {
            name: "ridal",
            description: "NetCDF processed by Ridal",
        },
        ridal_version,
        reprocess_reason: None,
        radargram_id: None,
        display_name: None,
        group_name: None,
        group_id: None,
        processing_datetime: None,
        recipe_hash: None,
        revision_id: None,
        samples: None,
        traces: None,
    };
    match crate::io::inspect_ridal_netcdf(path)? {
        crate::io::RidalNetcdfKind::Supported(meta) => {
            let revision_id = meta.revision_id();
            let (samples, traces) = meta.shape;
            Ok(ProcessedInfoRecord {
                radargram_id: Some(meta.radargram_id.to_string()),
                display_name: meta.display_name.map(|name| name.to_string()),
                group_name: meta.group_name.map(|name| name.to_string()),
                group_id: meta.group_id.map(|id| id.to_string()),
                processing_datetime: Some(meta.processing_datetime),
                recipe_hash: meta.recipe_hash,
                revision_id: Some(revision_id.to_string()),
                samples: Some(samples),
                traces: Some(traces),
                ..record(meta.ridal_version)
            })
        }
        crate::io::RidalNetcdfKind::Legacy(version) => Ok(ProcessedInfoRecord {
            reprocess_reason: Some(format!(
                "This file was {}",
                crate::io::legacy_reason(&version)
            )),
            ..record(version)
        }),
        crate::io::RidalNetcdfKind::NotRidal => Err(format!(
            "{} is a NetCDF file but not one Ridal processed.",
            path.display()
        )),
    }
}

pub fn inspect(params: InfoParams) -> Result<Vec<Inspected>, Box<dyn Error>> {
    let expanded = input::expand_input_paths(&params.filepaths)?;
    let mut records = Vec::<Inspected>::new();

    // These describe how to read a raw recording. A processed file already
    // carries the result, so given one they would be silently ignored.
    let raw_only = [
        ("cor", params.cor_path.is_some()),
        ("dem", params.dem_path.is_some()),
        ("crs", params.crs.is_some()),
        (
            "override_antenna_mhz",
            params.override_antenna_mhz.is_some(),
        ),
        (
            "override_antenna_separation",
            params.override_antenna_separation.is_some(),
        ),
        ("override_date", params.override_date.is_some()),
    ];
    if let Some(netcdf) = expanded.iter().find(|input| input::is_netcdf(input)) {
        let given: Vec<&str> = raw_only
            .iter()
            .filter(|(_, set)| *set)
            .map(|(name, _)| *name)
            .collect();
        if !given.is_empty() {
            return Err(format!(
                "{} only apply to raw recordings, and {} was processed already.",
                given.join(", "),
                netcdf.display()
            )
            .into());
        }
    }

    let mut raw = expanded
        .iter()
        .filter(|input| !input::is_netcdf(input))
        .map(|input| formats::resolve_input(input))
        .collect::<Result<Vec<_>, String>>()?;
    formats::with_coordinates(&mut raw, params.cor_path.as_deref())?;
    crate::sidecar::with_sidecar(&mut raw, &params.sidecar)?;
    let mut raw = raw.into_iter();

    for input in expanded {
        if input::is_netcdf(&input) {
            records.push(Inspected::Processed(processed_info_record(&input)?));
            continue;
        }
        let resolved = raw.next().expect("one resolved recording per raw input");
        let (meta, location) = input::load_meta_and_location(
            &resolved,
            params.medium_velocity,
            params.crs.as_ref(),
            params.dem_path.as_ref(),
            params.override_antenna_mhz,
            params.override_antenna_separation,
            params.override_date,
        )?;
        let sidecar = sidecar_summary(&resolved, &meta, &params)?;
        records.push(Inspected::Raw(info_record(
            &resolved, &meta, &location, sidecar,
        )?));
    }

    Ok(records)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use crate::gpr::test_fixtures::*;

    pub(crate) fn info_params(inputs: Vec<PathBuf>) -> super::InfoParams {
        super::InfoParams {
            filepaths: inputs,
            dem_path: None,
            cor_path: None,
            medium_velocity: 0.168,
            crs: None,
            override_antenna_mhz: None,
            override_antenna_separation: None,
            override_date: None,
            sidecar: crate::sidecar::Choice::Beside,
        }
    }

    pub(crate) fn processed(records: Vec<super::Inspected>) -> super::ProcessedInfoRecord {
        match records.into_iter().next() {
            Some(super::Inspected::Processed(record)) => record,
            other => panic!("expected a processed record, got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn info_reports_a_processed_file_by_its_ids() {
        // #11: a local file is matched to a served radargram by its ids, so
        // the revision id must be the one the server computes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("line.nc");
        let gpr = make_dummy_gpr(20, 10, Some(1.));
        gpr.export(&path).unwrap();

        let record = processed(super::inspect(info_params(vec![path])).unwrap());
        let datetime = record.processing_datetime.clone().unwrap();
        let id = crate::identity::RadargramId::new("test-radargram").unwrap();
        assert_eq!(record.format.name, "ridal");
        assert_eq!(record.radargram_id.as_deref(), Some("test-radargram"));
        assert_eq!(
            record.revision_id,
            Some(crate::identity::RevisionId::fingerprint_v1(&id, &datetime).to_string())
        );
        assert_eq!((record.samples, record.traces), (Some(10), Some(20)));
        assert_eq!(record.reprocess_reason, None);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_file_is_named_by_its_recipe_so_processing_again_is_the_same_revision() {
        // #382: the revision id comes from what the file was made from, not
        // from when, so running the same processing twice gives one
        // revision and changing a step gives another.
        let dir = tempfile::tempdir().unwrap();
        let run = |name: &str, steps: &[&str]| {
            let output = dir.path().join(name);
            let mut params = info_run_params(&output);
            params.options.steps = steps.iter().map(|s| s.to_string()).collect();
            crate::processing::run(params).unwrap();
            processed(super::inspect(info_params(vec![output])).unwrap())
        };
        let first = run("first.nc", &["subset(0 200 0 300)", "dewow"]);
        let again = run("again.nc", &["subset(0 200 0 300)", "dewow"]);
        let other = run("other.nc", &["subset(0 200 0 300)", "dewow(method=mean)"]);

        let hash = first
            .recipe_hash
            .clone()
            .expect("a run from raw files records its recipe");
        assert_ne!(first.processing_datetime, again.processing_datetime);
        assert_eq!(again.recipe_hash.as_deref(), Some(hash.as_str()));
        assert_eq!(first.revision_id, again.revision_id);
        let id = crate::identity::RadargramId::new("line-01").unwrap();
        assert_eq!(
            first.revision_id,
            Some(crate::identity::RevisionId::fingerprint_v2(&id, &hash).to_string())
        );
        assert_ne!(other.recipe_hash, first.recipe_hash);
        assert_ne!(other.revision_id, first.revision_id);

        // The stored recipe is the one that hashes to the stored hash, and
        // says what was read and run.
        let file = netcdf::open(dir.path().join("first.nc")).unwrap();
        let text = match file.attribute("ridal_recipe").unwrap().value().unwrap() {
            netcdf::AttributeValue::Str(text) => text,
            other => panic!("ridal_recipe is {other:?}"),
        };
        let recipe: crate::recipe::Recipe = serde_json::from_str(&text).unwrap();
        assert_eq!(recipe.hash(), hash);
        assert_eq!(recipe.core_version, crate::recipe::CORE_PROCESSING_VERSION);
        assert_eq!(recipe.bundles.len(), 1);
        assert_eq!(recipe.bundles[0].format, "ramac");
        assert_eq!(recipe.bundles[0].files.len(), 3);
        let steps: Vec<&str> = recipe.steps.iter().map(|s| s.step.as_str()).collect();
        assert_eq!(steps.len(), 2);
        assert!(steps[1].starts_with("dewow("), "{steps:?}");
        assert!(recipe.steps.iter().all(|s| s.version >= 1));
    }

    fn info_run_params(output: &std::path::Path) -> crate::processing::RunParams {
        crate::processing::RunParams {
            filepaths: vec!["assets/mala/dronbreen-20250327-DAT_0066_A1.rad".into()],
            output_path: Some(output.to_path_buf()),
            track_path: None,
            render_path: None,
            radargram_id: Some("line-01".to_string()),
            display_name: None,
            options: crate::processing::ProcessingOptions {
                quiet: true,
                ..Default::default()
            },
        }
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn info_reports_a_legacy_file_rather_than_failing() {
        // A caller sorting many files sorts this one as "reprocess", which it
        // could only do from an error by reading its message.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.nc");
        {
            let mut file = netcdf::create(&path).unwrap();
            file.add_attribute("program_version", "ridal version 0.3.0")
                .unwrap();
        }

        let record = processed(super::inspect(info_params(vec![path])).unwrap());
        assert_eq!(record.ridal_version, "ridal version 0.3.0");
        assert!(record
            .reprocess_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("Reprocess it")));
        assert_eq!(record.radargram_id, None);
        assert_eq!(record.revision_id, None);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn info_refuses_a_netcdf_file_ridal_did_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.nc");
        {
            let mut file = netcdf::create(&path).unwrap();
            file.add_dimension("x", 1).unwrap();
        }
        let error = super::inspect(info_params(vec![path])).unwrap_err();
        assert!(
            error.to_string().contains("not one Ridal processed"),
            "{error}"
        );
    }

    #[test]
    fn info_refuses_raw_file_options_with_a_processed_file() {
        // Refused before any file is opened: they would be ignored, and an
        // ignored `--crs` is a silent wrong answer about where a line is.
        let mut params = info_params(vec![PathBuf::from("line.nc")]);
        params.crs = Some("EPSG:32633".to_string());
        let error = super::inspect(params).unwrap_err();
        assert!(error.to_string().contains("crs only apply"), "{error}");
    }

    fn header(name: &str, antenna_mhz: f32) -> super::InfoRecord {
        super::InfoRecord {
            input: format!("/somewhere/{name}"),
            format: super::NamedFormat {
                name: "ramac",
                description: "",
            },
            metadata: super::MetadataSummary {
                samples: 500,
                sampling_frequency_mhz: 1000.0,
                frequency_steps: 1,
                time_interval_s: 0.1,
                antenna_name: String::new(),
                antenna_mhz,
                antenna_separation_m: 1.0,
                time_window_ns: 500.0,
                last_trace: 99,
                medium_velocity_m_per_ns: 0.168,
            },
            location: super::LocationSummary {
                n_points: 100,
                crs: "EPSG:32633".to_string(),
                start_time: String::new(),
                stop_time: String::new(),
                duration_s: 0.0,
                track_length_m: 0.0,
                altitude_min_m: 0.0,
                altitude_max_m: 0.0,
                centroid: super::CentroidSummary {
                    easting: 0.0,
                    northing: 0.0,
                    altitude: 0.0,
                },
                correction: super::CorrectionSummary {
                    kind: String::new(),
                    source: None,
                },
            },
            related_files: super::RelatedFiles {
                header: String::new(),
                data: String::new(),
                coordinates: String::new(),
            },
            sidecar: None,
        }
    }

    #[test]
    fn an_odd_recording_is_named_and_the_rest_counted() {
        let mut records = vec![header("DAT_0001.rad", 100.0)];
        for i in 2..=10 {
            records.push(header(&format!("DAT_{i:04}.rad"), 25.0));
        }
        let message = super::check_mergeable(&records).unwrap_err();
        assert_eq!(
            message,
            "These recordings cannot be merged into one radargram: their antenna \
             frequencies differ. 9 are 25 MHz; 1 is 100 MHz (DAT_0001.rad)."
        );
    }

    #[test]
    fn without_a_majority_every_group_is_named() {
        let records = [header("a.rad", 100.0), header("b.rad", 25.0)];
        let message = super::check_mergeable(&records).unwrap_err();
        assert!(
            message.ends_with("1 is 100 MHz (a.rad); 1 is 25 MHz (b.rad)."),
            "{message}"
        );
        assert!(super::check_mergeable(&[header("a.rad", 25.0), header("b.rad", 25.0)]).is_ok());
    }
}

/// Positions from a Ridal coordinate CSV and how `--cor` finds them (#13).
#[cfg(test)]
mod coordinate_tests {
    use std::path::{Path, PathBuf};

    use crate::formats;

    const LINE: &str = "dronbreen-20250327-DAT_0066_A1";

    /// The fixture's `.cor` as a coordinate CSV, row for row.
    fn csv_of_cor(cor: &Path, with_alt: bool) -> String {
        let mut csv = String::from(if with_alt {
            "trace,time,lon,lat,alt\n"
        } else {
            "trace,time,lon,lat\n"
        });
        for line in std::fs::read_to_string(cor).unwrap().lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            assert_eq!((f[4], f[6]), ("N", "E"));
            let trace = f[0].parse::<u32>().unwrap() - 1;
            let time = format!("{}T{:0>8}Z", f[1], f[2]);
            csv += &format!("{trace},{time},{},{}", f[5], f[3]);
            if with_alt {
                csv += &format!(",{}", f[7]);
            }
            csv += "\n";
        }
        csv
    }

    /// The fixture's header and data in a directory of their own, with no
    /// positions file beside them.
    fn recording_without_positions(dir: &Path) -> PathBuf {
        for ext in ["rad", "rd3"] {
            std::fs::copy(
                format!("assets/mala/{LINE}.{ext}"),
                dir.join(format!("{LINE}.{ext}")),
            )
            .unwrap();
        }
        dir.join(format!("{LINE}.rad"))
    }

    fn info_params(filepaths: Vec<PathBuf>) -> super::InfoParams {
        super::InfoParams {
            filepaths,
            dem_path: None,
            cor_path: None,
            medium_velocity: 0.168,
            crs: None,
            override_antenna_mhz: None,
            override_antenna_separation: None,
            override_date: None,
            sidecar: crate::sidecar::Choice::Beside,
        }
    }

    fn read(resolved: &formats::ResolvedInput) -> Result<super::GPRLocation, String> {
        crate::processing::input::load_meta_and_location(
            resolved, 0.168, None, None, None, None, None,
        )
        .map(|(_, location)| location)
        .map_err(|e| e.to_string())
    }

    #[test]
    fn a_csv_of_the_cor_reads_as_the_cor_does() {
        let dir = tempfile::tempdir().unwrap();
        let cor = PathBuf::from(format!("assets/mala/{LINE}.cor"));
        let csv = dir.path().join("positions.csv");
        std::fs::write(&csv, csv_of_cor(&cor, true)).unwrap();

        let original = formats::resolve_input(&cor).unwrap();
        let mut replaced = [original.clone()];
        formats::with_coordinates(&mut replaced, Some(&csv)).unwrap();
        let (from_cor, from_csv) = (read(&original).unwrap(), read(&replaced[0]).unwrap());

        assert_eq!(from_csv.crs, from_cor.crs);
        assert_eq!(from_csv.cor_points.len(), from_cor.cor_points.len());
        for (a, b) in from_csv.cor_points.iter().zip(&from_cor.cor_points) {
            assert_eq!(
                (a.trace_n, a.time_seconds, a.easting, a.northing, a.altitude),
                (b.trace_n, b.time_seconds, b.easting, b.northing, b.altitude)
            );
        }
        // And the recipe names the file that was read.
        let bundle = crate::recipe::bundle_of(&replaced[0]).unwrap();
        assert_eq!(bundle.files[2].file.name, "positions.csv");
    }

    #[test]
    fn a_coords_csv_beside_the_recording_is_found_and_stands_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let rad = recording_without_positions(dir.path());
        let csv = dir.path().join(format!("{LINE}.coords.csv"));
        std::fs::write(
            &csv,
            csv_of_cor(Path::new(&format!("assets/mala/{LINE}.cor")), true),
        )
        .unwrap();
        // Preferred to the format's own, when both are there.
        std::fs::copy(
            format!("assets/mala/{LINE}.cor"),
            dir.path().join(format!("{LINE}.cor")),
        )
        .unwrap();

        for input in [&rad, &csv] {
            let resolved = formats::resolve_input(input).unwrap();
            assert_eq!(resolved.header, rad);
            assert_eq!(resolved.coordinates, csv);
            assert!(read(&resolved).is_ok());
        }

        let records = super::inspect(info_params(vec![rad])).unwrap();
        let Some(super::Inspected::Raw(record)) = records.first() else {
            panic!("expected a raw record, got {records:?}");
        };
        assert_eq!(record.related_files.coordinates, csv.display().to_string());
    }

    #[test]
    fn missing_positions_say_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let rad = recording_without_positions(dir.path());
        let error = read(&formats::resolve_input(&rad).unwrap()).unwrap_err();
        assert!(error.contains("No positions for"), "{error}");
        assert!(error.contains("--cor"), "{error}");
        assert!(error.contains(".coords.csv"), "{error}");
    }

    #[test]
    fn a_csv_without_alt_needs_a_dem() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("positions.csv");
        std::fs::write(
            &csv,
            csv_of_cor(Path::new(&format!("assets/mala/{LINE}.cor")), false),
        )
        .unwrap();
        let mut resolved =
            [formats::resolve_input(Path::new(&format!("assets/mala/{LINE}.rad"))).unwrap()];
        formats::with_coordinates(&mut resolved, Some(&csv)).unwrap();
        let error = read(&resolved[0]).unwrap_err();
        assert!(error.contains("gives no altitude"), "{error}");
        assert!(error.contains("--dem"), "{error}");
    }

    #[test]
    fn another_formats_positions_file_is_refused() {
        let mut resolved =
            [formats::resolve_input(Path::new(&format!("assets/mala/{LINE}.rad"))).unwrap()];
        let dir = tempfile::tempdir().unwrap();
        let dzg = dir.path().join("track.DZG");
        std::fs::write(&dzg, "").unwrap();
        formats::with_coordinates(&mut resolved, Some(&dzg)).unwrap();
        let error = read(&resolved[0]).unwrap_err();
        assert!(error.contains("a ramac recording's positions"), "{error}");
    }

    #[test]
    fn one_positions_file_for_several_recordings_is_refused() {
        let line = |name: &str| PathBuf::from(format!("assets/mala/{name}.rad"));
        let cor = PathBuf::from(format!("assets/mala/{LINE}.cor"));
        let two = vec![line(LINE), line("dronbreen-20220329-DAT_0237_A1")];

        let mut params = info_params(two.clone());
        params.cor_path = Some(cor.clone());
        let error = super::inspect(params).unwrap_err().to_string();
        assert!(error.contains("2 recordings were given"), "{error}");

        // A directory works for both, each by its stem.
        let mut params = info_params(two);
        params.cor_path = Some(PathBuf::from("assets/mala"));
        assert_eq!(super::inspect(params).unwrap().len(), 2);
    }
}
