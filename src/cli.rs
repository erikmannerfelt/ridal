use crate::{gpr, tools};
/// Functions to handle the command line interface (CLI)
use clap::Parser;
use std::{path::PathBuf, time::Duration};

#[derive(Debug, Parser)]
#[clap(author, version, about, long_about = None)]
#[clap(group(
        clap::ArgGroup::new("step_choice")
        .required(false)
        .args(&["steps", "default", "default_with_topo"]),
    ))
]
#[clap(group(
        clap::ArgGroup::new("exit_choice")
        .required(false)
        .args(&["show_default", "info", "show_all_steps", "output"]),
    ))
]
pub struct Args {
    /// Filepath of the header file or a glob pattern of many files
    #[clap(short, long)]
    pub filepath: Option<String>,

    /// Velocity of the medium in m/ns. Defaults to the typical velocity of ice.
    #[clap(short, long, default_value = "0.168")]
    pub velocity: f32,

    /// Only show metadata for the file
    #[clap(short, long)]
    pub info: bool,

    /// Load a separate ".cor" file. If not given, it will be searched for automatically
    #[clap(short, long)]
    pub cor: Option<PathBuf>,

    /// Correct elevation values with a DEM
    #[clap(short, long)]
    pub dem: Option<PathBuf>,

    /// Which coordinate reference system to project coordinates in.
    #[clap(long)]
    pub crs: Option<String>,

    /// Export the location track to a comma separated values (CSV) file. Defaults to the output filename location and stem +
    /// "_track.csv"
    #[clap(short, long)]
    pub track: Option<Option<PathBuf>>,

    /// Process with the default profile. See "--show-default" to list the profile.
    #[clap(long)]
    pub default: bool,

    /// Process with the default profile plus topographic correction. See "--show-default" to list the profile.
    #[clap(long)]
    pub default_with_topo: bool,

    /// Show the default profile and exit
    #[clap(long)]
    pub show_default: bool,

    /// Show the available steps
    #[clap(long)]
    pub show_all_steps: bool,

    /// Processing steps to run, separated by commas. Can be a filepath to a newline separated step file.
    #[clap(long)]
    pub steps: Option<String>,

    /// Output filename or directory. Defaults to the input filename with a ".nc" extension
    #[clap(short, long)]
    pub output: Option<PathBuf>,

    /// Suppress progress messages
    #[clap(short, long)]
    pub quiet: bool,

    /// Render an image of the profile and save it to the specified path. Defaults to a jpg in the
    /// directory of the output filepath
    #[clap(short, long)]
    pub render: Option<Option<PathBuf>>,

    /// Don't export a nc file
    #[clap(long)]
    pub no_export: bool,

    /// Merge profiles closer in time than the given threshold when in batch mode (e.g. "10 min")
    #[clap(long)]
    pub merge: Option<String>,

    /// Override the antenna center frequency (in MHz) of the file metadata
    #[clap(long)]
    pub override_antenna_mhz: Option<f32>,
}

pub enum CliAction {
    Run(gpr::RunParams),
    Error(String),
    Done,
}
pub fn args_to_action(args: &Args) -> CliAction {
    if args.show_all_steps {
        for (name, description) in gpr::all_available_steps() {
            println!("{}\n{}\n{}\n", name, "-".repeat(name.len()), description);
        }
        return CliAction::Done;
    }

    if args.show_default {
        for line in gpr::default_processing_profile() {
            println!("{}", line);
        }
        return CliAction::Done;
    }

    let merge: Option<Duration> = match &args.merge {
        Some(merge_string) => match parse_duration::parse(merge_string) {
            Ok(d) => Some(d),
            Err(e) => return CliAction::Error(format!("Error parsing --merge string: {:?}", e)),
        },
        None => None,
    };

    let filepaths = match &args.filepath {
        Some(fp) => glob::glob(fp)
            .unwrap()
            .map(|v| v.unwrap())
            .collect::<Vec<PathBuf>>(),
        None => {
            return CliAction::Error(
                "No filepath given.\nUse the help text (\"-h\" or \"--help\") for assistance."
                    .to_string(),
            )
        }
    };

    let steps: Vec<String> = match args.info {
        true => Vec::new(),
        false => match args.default_with_topo {
            true => {
                let mut profile = gpr::default_processing_profile();
                profile.push("correct_topography".to_string());
                profile
            }
            false => match args.default {
                true => gpr::default_processing_profile(),
                false => match &args.steps {
                    Some(steps) => match tools::parse_step_list(steps) {
                        Ok(s) => s,
                        Err(e) => return CliAction::Error(e),
                    },
                    None => {
                        println!("No processing steps specified. Saving raw data.");
                        vec![]
                    }
                },
            },
        },
    };

    let allowed_steps = gpr::all_available_steps()
        .iter()
        .map(|s| s.0.clone())
        .collect::<Vec<String>>();
    for step in &steps {
        if !allowed_steps.iter().any(|allowed| step.contains(allowed)) {
            return CliAction::Error(format!("Unrecognized step: {}", step));
        }
    }

    let params = gpr::RunParams {
        filepaths,
        output_path: args.output.clone(),
        only_info: args.info,
        dem_path: args.dem.clone(),
        cor_path: args.cor.clone(),
        medium_velocity: args.velocity,
        crs: args.crs.clone(),
        quiet: args.quiet,
        track_path: args.track.clone(),
        steps,
        no_export: args.no_export,
        render_path: args.render.clone(),
        merge,
        override_antenna_mhz: args.override_antenna_mhz,
    };

    CliAction::Run(params)
}

#[cfg(feature = "cli")]
#[allow(dead_code)] // For maturin
pub fn main(arguments: Args) -> i32 {
    match args_to_action(&arguments) {
        CliAction::Run(params) => match gpr::run(params) {
            Ok(_) => 0,
            Err(e) => error(&format!("{e:?}"), 1),
        },
        CliAction::Error(message) => error(&message, 1),
        CliAction::Done => 0,
    }
}

/// Print an error to /dev/stderr and return an exit code
///
/// At the moment, it's quite barebones, but this allows for better handling later.
///
/// # Arguments
/// - `message`: The message to print to /dev/stderr
/// - `code`: The exit code
///
/// # Returns
/// The same exit code that was provided
#[cfg(feature = "cli")]
#[allow(dead_code)] // For maturin
fn error(message: &str, code: i32) -> i32 {
    eprintln!("{}", message);
    code
}
