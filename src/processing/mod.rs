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

fn derive_output_path(
    output_path: Option<&PathBuf>,
    first_resolved: &ResolvedInput,
) -> Result<PathBuf, String> {
    let default_output = first_resolved.header.with_extension("nc");
    match output_path {
        Some(path) => {
            if path.is_dir() {
                Ok(path
                    .join(first_resolved.header.file_stem().unwrap())
                    .with_extension("nc"))
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
    let expanded = input::expand_input_paths(&params.filepaths)?;
    if expanded.is_empty() {
        return Err("No input files were provided.".into());
    }
    let mut resolved_inputs = expanded
        .iter()
        .map(|input| crate::formats::resolve_input(input))
        .collect::<Result<Vec<crate::formats::ResolvedInput>, String>>()?;
    if resolved_inputs.is_empty() {
        return Err("No input files were resolved.".into());
    }
    formats::with_coordinates(&mut resolved_inputs, options.cor_path.as_deref())?;
    if !options.quiet {
        for resolved in &resolved_inputs {
            println!(
                "Positions for {:?} from {:?}",
                resolved.header, resolved.coordinates
            );
        }
    }

    let output_path = derive_output_path(params.output_path.as_ref(), &resolved_inputs[0])?;
    // Hashed here, from the same resolution the readers use, so the recipe
    // names exactly the files that were read, `--cor` applied.
    let inputs = crate::recipe::Inputs {
        bundles: resolved_inputs
            .iter()
            .map(crate::recipe::bundle_of)
            .collect::<Result<Vec<_>, String>>()?,
        dem: options
            .dem_path
            .as_deref()
            .map(crate::recipe::hash_file)
            .transpose()?,
        antenna_mhz_override: options.override_antenna_mhz,
        antenna_separation_override: options.override_antenna_separation,
    };
    let first_resolved = resolved_inputs.remove(0);
    let (first_meta, first_location) = input::load_meta_and_location(
        &first_resolved,
        options.medium_velocity,
        options.crs.as_ref(),
        options.dem_path.as_ref(),
        options.override_antenna_mhz,
        options.override_antenna_separation,
    )?;
    let mut gpr = GPR::from_meta_and_loc(first_location, first_meta).map_err(|e| {
        format!(
            "Error loading GPR data from {:?}: {:?}",
            first_resolved.data, e
        )
    })?;
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
        )?;
        let other = GPR::from_meta_and_loc(location, meta)
            .map_err(|e| format!("Error loading GPR data from {:?}: {:?}", resolved.data, e))?;
        if !options.quiet {
            println!(
                "Merging {:?} -> {:?}",
                resolved.header, first_resolved.header
            );
        }
        gpr.merge(&other)?;
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
