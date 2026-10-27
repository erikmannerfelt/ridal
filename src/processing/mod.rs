//! Processing raw recordings into radargrams: reading the inputs,
//! running the steps, and naming and writing the outputs.

pub mod batch;
pub mod grouping;
pub mod input;
pub mod steps;

use std::path::PathBuf;

use crate::formats::{self, ResolvedInput};
use crate::gpr::{RidalIdentity, GPR};
use crate::{tools, user_metadata};

/// How to process, shared by a single run ([`RunParams`]) and a batch
/// ([`BatchRunParams`]): everything but where the inputs come from and
/// where the outputs go.
#[derive(Debug, Clone)]
pub struct ProcessingOptions {
    pub dem_path: Option<PathBuf>,
    pub cor_path: Option<PathBuf>,
    pub medium_velocity: f32,
    pub crs: Option<String>,
    pub quiet: bool,
    pub steps: Vec<String>,
    pub no_export: bool,
    /// Render profile for the rendered image: a built-in name or a TOML
    /// file. `None` means the built-in `default`.
    pub render_profile: Option<String>,
    /// Width of the rendered image. `None` means one pixel per trace.
    pub render_width: Option<usize>,
    /// Render the topographically corrected view (#289).
    pub render_topo: bool,
    pub override_antenna_mhz: Option<f32>,
    pub override_antenna_separation: Option<f32>,
    /// The UTC date of recordings whose files give only a time of day, or a
    /// date that is wrong: IceRadar and pulseEKKO.
    pub override_date: Option<chrono::NaiveDate>,
    /// Which lines of a multi-line file (IceRadar) to merge, in order.
    pub lines: Option<formats::LineSelection>,
    /// Which sidecar each recording is read with (#527).
    pub sidecar: crate::sidecar::Choice,
    pub user_metadata: user_metadata::UserMetadata,
    /// The group's display name. A group applies uniformly to a whole batch,
    /// since batch runs typically process one survey or campaign.
    pub group: Option<String>,
    pub group_id: Option<String>,
}

impl Default for ProcessingOptions {
    /// No steps, no DEM, no rendering, at the typical velocity in glacier ice.
    fn default() -> Self {
        Self {
            dem_path: None,
            cor_path: None,
            medium_velocity: DEFAULT_MEDIUM_VELOCITY,
            crs: None,
            quiet: false,
            steps: Vec::new(),
            no_export: false,
            render_profile: None,
            render_width: None,
            render_topo: false,
            override_antenna_mhz: None,
            override_antenna_separation: None,
            override_date: None,
            lines: None,
            sidecar: crate::sidecar::Choice::Beside,
            user_metadata: user_metadata::UserMetadata::new(),
            group: None,
            group_id: None,
        }
    }
}

/// The velocity of the medium, in m/ns, when none is given: typical of most
/// glacier ice.
pub const DEFAULT_MEDIUM_VELOCITY: f32 = 0.168;

#[derive(Debug, Clone)]
pub struct RunParams {
    pub filepaths: Vec<PathBuf>,
    pub output_path: Option<PathBuf>,
    /// `Some(None)` derives a sidecar path from the output path.
    pub track_path: Option<Option<PathBuf>>,
    /// `Some(None)` derives a sidecar path from the output path.
    pub render_path: Option<Option<PathBuf>>,
    pub radargram_id: Option<String>,
    pub display_name: Option<String>,
    pub options: ProcessingOptions,
}

#[derive(Debug, Clone)]
pub struct ProcessResult {
    pub output_path: PathBuf,
}

/// `<stem>.nc`, or `<stem>_line_<N>.nc` for a line of a multi-line file,
/// so processing the lines of one file one by one does not overwrite the
/// same output. A merge of several lines is named after its first.
fn default_output_name(first: &ResolvedInput) -> String {
    let stem = first
        .header
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    match first.part.line {
        Some(line) => format!("{stem}_line_{line}.nc"),
        None => format!("{stem}.nc"),
    }
}

/// A recording as messages name it: its header, and its line if it has one.
fn describe_input(resolved: &ResolvedInput) -> String {
    match resolved.part.line {
        Some(line) => format!("{:?} line_{line}", resolved.header),
        None => format!("{:?}", resolved.header),
    }
}

/// What the input files did not record and Ridal assumed instead, as
/// warnings to show: the antenna separation, for the formats whose headers
/// do not give it, and an IceRadar line's date taken from the PC clock.
/// Empty when nothing was assumed, or the user gave it.
pub(crate) fn assumptions(
    resolved: &[ResolvedInput],
    options: &ProcessingOptions,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut warnings = Vec::new();
    let given = crate::sidecar::Overrides {
        antenna_mhz: options.override_antenna_mhz,
        antenna_separation_m: options.override_antenna_separation,
    };
    for r in resolved {
        let sidecar = crate::sidecar::overrides_of(r)?;
        for ((key, given), (_, beside)) in given.entries().into_iter().zip(sidecar.entries()) {
            let (Some(given), Some(beside)) = (given, beside) else {
                continue;
            };
            if given != beside {
                let argument = crate::sidecar::OVERRIDE_KEYS
                    .iter()
                    .find(|k| k.name == key)
                    .expect("every key is listed")
                    .argument;
                warnings.push(format!(
                    "Warning: {argument} {given} takes precedence over {key} = {beside} in {}.",
                    r.sidecar
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
                ));
            }
        }
    }
    if options.override_antenna_separation.is_none() {
        let mut unrecorded = Vec::new();
        for r in resolved {
            if !formats::records_antenna_separation(r.kind)
                && !unrecorded.contains(&r.kind)
                && crate::sidecar::overrides_of(r)?
                    .antenna_separation_m
                    .is_none()
            {
                unrecorded.push(r.kind);
            }
        }
        if !unrecorded.is_empty() {
            let names = unrecorded
                .iter()
                .map(|&kind| formats::display_name(kind))
                .collect::<Vec<_>>()
                .join(" and ");
            warnings.push(format!(
                "Warning: {names} files do not record the antenna separation, so it is taken \
                 as 0 m. Give it with --override-antenna-separation \
                 (override_antenna_separation= in Python), or as antenna_separation_m in a \
                 <stem>{} beside each recording.",
                crate::sidecar::SIDECAR_SUFFIX
            ));
        }
    }
    if options.override_date.is_none() {
        for r in resolved
            .iter()
            .filter(|r| r.kind == formats::FormatKind::IceRadar)
        {
            let line = input::iceradar_line(r)?;
            if let crate::io::IceRadarDate::PcClock(date) =
                crate::io::iceradar_date(&r.header, line, None)?
            {
                warnings.push(format!(
                    "Warning: {} is dated {date} by the acquisition PC's clock, which is local \
                     time and may be wrong; the GPS records only the time of day. Give the UTC \
                     date with --override-date (override_date= in Python) if it is not {date}.",
                    describe_input(r)
                ));
            }
        }
    }
    Ok(warnings)
}

fn derive_output_path(
    output_path: Option<&PathBuf>,
    first_resolved: &ResolvedInput,
) -> Result<PathBuf, String> {
    let default_output = first_resolved
        .header
        .with_file_name(default_output_name(first_resolved));
    match output_path {
        Some(path) => {
            if path.is_dir() {
                Ok(path.join(default_output_name(first_resolved)))
            } else {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() && !parent.is_dir() {
                        return Err(format!(
                            "Output directory of path is not a directory: {:?}",
                            path
                        ));
                    }
                }
                Ok(path.clone())
            }
        }
        None => Ok(default_output),
    }
}

pub fn build_processed_gpr(
    params: RunParams,
) -> Result<(GPR, PathBuf), Box<dyn std::error::Error>> {
    build_processed_gpr_with_progress(params, &mut |_, _, _| {})
}

/// [`build_processed_gpr`], calling `on_step(index, of, step)` before each
/// step runs, for a caller that reports progress other than by printing
/// (`ridal process-job`, #382).
pub fn build_processed_gpr_with_progress(
    params: RunParams,
    on_step: &mut dyn FnMut(usize, usize, &str),
) -> Result<(GPR, PathBuf), Box<dyn std::error::Error>> {
    let options = &params.options;
    // Before reading anything. Covers lists that did not come through
    // `resolve_steps`, such as a radargram's pre-steps followed by its
    // profile's.
    steps::check_step_order(&options.steps)?;
    let expanded = input::expand_input_paths(&params.filepaths)?;
    if expanded.is_empty() {
        return Err("No input files were provided.".into());
    }
    let resolved_inputs = expanded
        .iter()
        .map(|input| crate::formats::resolve_input(input))
        .collect::<Result<Vec<crate::formats::ResolvedInput>, String>>()?;
    if resolved_inputs.is_empty() {
        return Err("No input files were resolved.".into());
    }
    // Before `--cor`, so one positions file is refused for several lines as
    // it is for several files: trace numbers start over in each line.
    let mut resolved_inputs = formats::with_lines(resolved_inputs, options.lines.as_ref())?;
    formats::with_coordinates(&mut resolved_inputs, options.cor_path.as_deref())?;
    crate::sidecar::with_sidecar(&mut resolved_inputs, &options.sidecar)?;
    if !options.quiet {
        for warning in assumptions(&resolved_inputs, options)? {
            eprintln!("{warning}");
        }
    }
    // What each recording's sidecar decides, which the recipe records.
    let applied = resolved_inputs
        .iter()
        .map(|resolved| {
            crate::sidecar::applied(
                resolved,
                options.override_antenna_mhz,
                options.override_antenna_separation,
            )
        })
        .collect::<Result<Vec<_>, String>>()?;
    if !options.quiet {
        for (resolved, applied) in resolved_inputs.iter().zip(&applied) {
            println!(
                "Positions for {:?} from {:?}",
                resolved.header, resolved.coordinates
            );
            if let (Some(sidecar), false) = (&resolved.sidecar, applied.is_empty()) {
                println!(
                    "Overrides for {} from {:?}: {}",
                    describe_input(resolved),
                    sidecar,
                    applied.describe()
                );
            }
        }
    }

    let output_path = derive_output_path(params.output_path.as_ref(), &resolved_inputs[0])?;
    // Hashed here, from the same resolution the readers use, so the recipe
    // names exactly the files that were read, `--cor` applied.
    let inputs = crate::recipe::Inputs {
        bundles: resolved_inputs
            .iter()
            .zip(applied)
            .map(|(resolved, applied)| {
                Ok(crate::recipe::Bundle {
                    overrides: (!applied.is_empty()).then_some(applied),
                    ..crate::recipe::bundle_of(resolved)?
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
        dem: options
            .dem_path
            .as_deref()
            .map(crate::recipe::hash_file)
            .transpose()?,
        antenna_mhz_override: options.override_antenna_mhz,
        antenna_separation_override: options.override_antenna_separation,
        date_override: options.override_date,
    };
    let first_resolved = resolved_inputs.remove(0);
    let (first_meta, first_location) = input::load_meta_and_location(
        &first_resolved,
        options.medium_velocity,
        options.crs.as_ref(),
        options.dem_path.as_ref(),
        options.override_antenna_mhz,
        options.override_antenna_separation,
        options.override_date,
    )?;
    let mut gpr = input::load_gpr(
        &first_resolved,
        first_meta,
        first_location,
        options.override_date,
    )?;
    gpr.user_metadata = options.user_metadata.clone();
    gpr.inputs = Some(inputs);

    for resolved in resolved_inputs {
        let (meta, location) = input::load_meta_and_location(
            &resolved,
            options.medium_velocity,
            options.crs.as_ref(),
            options.dem_path.as_ref(),
            options.override_antenna_mhz,
            options.override_antenna_separation,
            options.override_date,
        )?;
        let other = input::load_gpr(&resolved, meta, location, options.override_date)?;
        if !options.quiet {
            println!(
                "Merging {} -> {}",
                describe_input(&resolved),
                describe_input(&first_resolved)
            );
        }
        gpr.merge(&other).map_err(|e| {
            format!(
                "Cannot merge {} into {}: {e}",
                describe_input(&resolved),
                describe_input(&first_resolved)
            )
        })?;
    }

    let start_time = std::time::SystemTime::now();
    if !options.quiet {
        println!("Processing {:?}", gpr.metadata.data_filepath);
    }
    for (i, step) in options.steps.iter().enumerate() {
        on_step(i, options.steps.len(), step);
        if !options.quiet {
            println!(
                "{}/{}, t+{:.2} s, Running step {}.",
                i + 1,
                options.steps.len(),
                std::time::SystemTime::now()
                    .duration_since(start_time)
                    .unwrap()
                    .as_secs_f32(),
                step,
            );
        }
        let report = gpr
            .process(step)
            .map_err(|e| format!("Error on step {}: {:?}", step, e))?;
        if report.is_warning() && !options.quiet {
            eprintln!("{}: {}", report.name(), report.event());
        }
    }

    // Resolved once, here, from CLI arguments and the output stem. There is
    // no "inherited" tier yet because Ridal cannot currently read its own
    // processed NetCDF outputs back in as input (#123 adds metadata-only
    // inspection; full reading into GPR is explicitly future work there).
    // Passing `None` for `inherited` keeps `identity::resolve_*` already
    // shaped for that follow-up rather than requiring a redesign then.
    let output_stem = output_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("Output path has no valid file stem: {output_path:?}"))?;
    let (radargram_id, id_note) =
        crate::identity::resolve_radargram_id(params.radargram_id.as_deref(), None, output_stem)?;
    if let Some(note) = id_note {
        if !options.quiet {
            println!("{note}");
        }
    }
    let display_name = crate::identity::resolve_display_name(params.display_name.as_deref(), None);
    let group = crate::identity::resolve_group(
        options.group.as_deref(),
        options.group_id.as_deref(),
        None,
        None,
    )?;
    let (group_name, group_id) = match group {
        Some((name, id)) => (Some(name), Some(id)),
        None => (None, None),
    };
    gpr.identity = RidalIdentity {
        radargram_id: Some(radargram_id),
        display_name,
        group_name,
        group_id,
    };

    Ok((gpr, output_path))
}

pub fn run(params: RunParams) -> Result<ProcessResult, Box<dyn std::error::Error>> {
    let options = &params.options;
    let (gpr, output_path) = build_processed_gpr(params.clone())?;

    if !options.no_export {
        if !options.quiet {
            println!("Exporting to {:?}", output_path);
        }
        gpr.export(&output_path)
            .map_err(|e| format!("Error exporting data: {:?}", e))?;
    }

    if let Some(potential_fp) = &params.render_path {
        let render_filepath = match potential_fp {
            Some(fp) => {
                if fp.is_dir() {
                    fp.join(output_path.file_stem().unwrap())
                        .with_extension("jpg")
                } else {
                    fp.clone()
                }
            }
            None => output_path.with_extension("jpg"),
        };
        let profile = crate::render::profile::RenderProfile::resolve(
            options.render_profile.as_deref().unwrap_or("default"),
        )?;
        if !options.quiet {
            println!(
                "Rendering image to {:?} with the '{}' profile",
                render_filepath, profile.name
            );
        }
        gpr.render(
            &render_filepath,
            &profile,
            options.render_width,
            options.render_topo,
        )
        .map_err(|e| format!("Error writing image: {:?}", e))?;
    }

    if let Some(potential_track_path) = &params.track_path {
        crate::io::export_locations(
            &gpr.location,
            potential_track_path.into(),
            &output_path,
            !options.quiet,
        )?;
    }

    Ok(ProcessResult { output_path })
}

pub fn default_processing_profile() -> Vec<String> {
    vec![
        "remove_empty_traces".to_string(),
        "zero_corr".to_string(),
        "correct_antenna_separation".to_string(),
        "dewow".to_string(),
        "background_removal".to_string(),
        format!("auto_gain({})", steps::DEFAULT_AUTOGAIN_N_BINS),
    ]
}

/// The steps a run applies, from the three ways of choosing them: the
/// default profile, the default profile plus `correct_topography`, or a step
/// list (comma-separated, or a file of one step per line). At most one may
/// be given, and none means no steps. Every step is validated.
pub fn resolve_steps(
    default: bool,
    default_with_topo: bool,
    steps: Option<&str>,
) -> Result<Vec<String>, String> {
    if usize::from(default) + usize::from(default_with_topo) + usize::from(steps.is_some()) > 1 {
        return Err("Only one of steps, default and default_with_topo may be given".to_string());
    }
    let resolved = if default_with_topo {
        let mut profile = default_processing_profile();
        profile.push("correct_topography".to_string());
        profile
    } else if default {
        default_processing_profile()
    } else if let Some(steps) = steps {
        tools::parse_step_list(steps)?
    } else {
        vec![]
    };
    steps::validate_steps(&resolved)?;
    Ok(resolved)
}

#[cfg(test)]
mod tests {

    #[test]
    fn an_unrecorded_antenna_separation_is_warned_about_once_per_format() {
        use crate::formats::FormatKind;
        // Nothing is read for the separation, so no file exists.
        let input = |kind, name: &str| crate::formats::ResolvedInput {
            input: name.into(),
            kind,
            header: name.into(),
            data: name.into(),
            coordinates: name.into(),
            part: Default::default(),
            sidecar: None,
        };
        let warnings = |inputs: &[_], separation| {
            let options = super::ProcessingOptions {
                override_antenna_separation: separation,
                ..Default::default()
            };
            super::assumptions(inputs, &options).unwrap()
        };

        assert_eq!(
            warnings(&[input(FormatKind::Ramac, "a.rad")], None),
            [""; 0]
        );
        let mixed = [
            input(FormatKind::Gssi, "a.DZT"),
            input(FormatKind::Ramac, "b.rad"),
            input(FormatKind::Gssi, "c.DZT"),
        ];
        let [warning] = &warnings(&mixed, None)[..] else {
            panic!("expected one warning");
        };
        assert!(
            warning.starts_with("Warning: GSSI files do not record"),
            "{warning}"
        );
        assert_eq!(warnings(&mixed, Some(0.5)), [""; 0]);

        // A sidecar that gives it silences the warning for its recording
        // alone, and an argument that disagrees with it is pointed out.
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("a.ridal.toml");
        std::fs::write(&sidecar, "[overrides]\nantenna_separation_m = 2\n").unwrap();
        let with_sidecar = crate::formats::ResolvedInput {
            sidecar: Some(sidecar),
            ..input(FormatKind::Gssi, "a.DZT")
        };
        assert_eq!(warnings(&[with_sidecar.clone()], None), [""; 0]);
        let [warning] = &warnings(
            &[with_sidecar.clone(), input(FormatKind::Gssi, "c.DZT")],
            None,
        )[..] else {
            panic!("expected one warning");
        };
        assert!(warning.contains("GSSI files do not record"), "{warning}");
        let [warning] = &warnings(&[with_sidecar.clone()], Some(0.5))[..] else {
            panic!("expected one warning");
        };
        assert!(
            warning.contains(
                "--override-antenna-separation 0.5 takes precedence over antenna_separation_m = 2"
            ),
            "{warning}"
        );
        assert_eq!(warnings(&[with_sidecar], Some(2.)), [""; 0]);
    }

    #[test]
    fn steps_are_resolved_from_exactly_one_choice() {
        let default = super::default_processing_profile();
        assert_eq!(
            super::resolve_steps(false, false, None).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(super::resolve_steps(true, false, None).unwrap(), default);

        let with_topo = super::resolve_steps(false, true, None).unwrap();
        assert_eq!(with_topo[..default.len()], default[..]);
        assert_eq!(with_topo[default.len()..], ["correct_topography"]);

        assert_eq!(
            super::resolve_steps(false, false, Some("zero_corr, dewow")).unwrap(),
            ["zero_corr", "dewow"]
        );

        for (default, with_topo, steps) in [
            (true, true, None),
            (true, false, Some("zero_corr")),
            (false, true, Some("zero_corr")),
        ] {
            let error = super::resolve_steps(default, with_topo, steps).unwrap_err();
            assert!(error.contains("Only one of"), "{error}");
        }
        assert!(super::resolve_steps(false, false, Some("no_such_step")).is_err());
        assert!(super::resolve_steps(false, false, Some("subset(1 2 3 4 5 6)")).is_err());
    }
}
