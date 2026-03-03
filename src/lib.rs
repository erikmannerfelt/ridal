//! # ridal  --- Speeding up Ground Penetrating Radar (GPR) processing
//! A Ground Penetrating Radar (GPR) processing tool written in rust.
//!
//! **This is a WIP.**
//!
//! The main aims of `ridal` are:
//! - **Ease of use**: A command line interface to process data or batches of data in one command.
//! - **Transparency**: All code is (or will be) thoroughly documented to show exactly how the data are modified.
//! - **Low memory usage and high speed**: While data are processed in-memory, they are usually no larger than an image (say 4000x2000 px). The functions of `ridal` avoid copying as much as possible, to keep memory usage to a minimum. Wherever possible, it is also multithreaded for fast processing times.
//! - **Reliability**: All functions will be tested in CI, meaning no crash or invalid behaviour should occur.
//!
#[cfg(feature = "python")]
use pyo3::prelude::*;

mod cli;
mod coords;
mod dem;
mod filters;
mod gpr;
mod io;
mod tools;

#[allow(dead_code)] // For maturin
const PROGRAM_VERSION: &str = env!("CARGO_PKG_VERSION");
#[allow(dead_code)] // For maturin
const PROGRAM_NAME: &str = env!("CARGO_PKG_NAME");
#[allow(dead_code)] // For maturin
const PROGRAM_AUTHORS: &str = env!("CARGO_PKG_AUTHORS");

#[cfg(feature = "python")]
#[pymodule]
pub mod ridal {
    use crate::{cli, gpr};
    use pyo3::prelude::*;
    use std::path::PathBuf;

    #[pymodule_init]
    fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add("version", crate::PROGRAM_VERSION)?;
        m.add("__version__", crate::PROGRAM_VERSION)
    }

    /// Call ridal with the given arguments (identical to the CLI).
    ///
    /// Parameters
    /// ----------
    /// filepath
    ///     Filepath of the header file or a glob pattern of many files
    /// velocity
    ///     Velocity of the medium in m/ns. Defaults to the typical velocity of ice.
    /// info
    ///     Only show metadata for the file
    /// cor
    ///     Load a separate ".cor" file. If not given, it will be searched for automatically
    /// dem
    ///     Correct elevation values with a DEM
    /// crs
    ///     Which coordinate reference system to project coordinates in.
    /// track
    ///     Export the location track to a comma separated values (CSV) file. Defaults to the output filename location and stem + "_track.csv"
    /// default
    ///     Process with the default profile. See "--show-default" to list the profile.
    /// default_with_topo
    ///     Process with the default profile plus topographic correction. See "--show-default" to list the profile.
    /// show_default
    ///     Show the default profile and exit
    /// show_all_steps
    ///     Show the available steps
    /// steps
    ///     Processing steps to run, separated by commas. Can be a filepath to a newline separated step file.
    /// output
    ///     Output filename or directory. Defaults to the input filename with a ".nc" extension
    /// quiet
    ///     Suppress progress messages
    /// render
    ///     Render an image of the profile and save it to the specified path. Defaults to a jpg in the directory of the output filepath
    /// no_export
    ///     Don't export a nc file
    /// merge
    ///     Merge profiles closer in time than the given threshold when in batch mode (e.g. "10 min")
    /// override_antenna_mhz
    ///     Override the antenna center frequency (in MHz) of the file metadata
    ///
    /// Returns
    /// -------
    /// The exit code of the CLI.
    #[pyfunction]
    #[pyo3(
        signature = (
            filepath=None,
            velocity=0.168,
            info=false,
            cor=None,
            dem=None,
            crs=None,
            track=None,
            default=false,
            default_with_topo=false,
            show_default=false,
            show_all_steps=false,
            steps=None,
            output=None,
            quiet=false,
            render=None,
            no_export=false,
            merge=None,
            override_antenna_mhz=None,
        )
    )]
    fn run_cli(
        filepath: Option<String>,
        velocity: f32,
        info: bool,
        cor: Option<PathBuf>,
        dem: Option<PathBuf>,
        crs: Option<String>,
        track: Option<PathBuf>,
        default: bool,
        default_with_topo: bool,
        show_default: bool,
        show_all_steps: bool,
        steps: Option<Vec<String>>,
        output: Option<PathBuf>,
        quiet: bool,
        render: Option<PathBuf>,
        no_export: bool,
        merge: Option<String>,
        override_antenna_mhz: Option<f32>,
        _py: Python<'_>,
    ) -> PyResult<i32> {
        let track_opt: Option<Option<PathBuf>> = match track {
            Some(s) => Some(Some(PathBuf::from(s))),
            None => None,
        };

        // render: CLI uses Option<Option<PathBuf>>
        let render_opt: Option<Option<PathBuf>> = match render {
            Some(s) => Some(Some(PathBuf::from(s))),
            None => None,
        };
        // Construct the same Args struct the CLI uses
        let args = cli::Args {
            filepath,
            velocity,
            info,
            cor,
            dem,
            crs,
            track: track_opt,
            default,
            default_with_topo,
            show_default,
            show_all_steps,
            steps: steps.and_then(|s| Some(s.join(","))),
            output,
            quiet,
            render: render_opt,
            no_export,
            merge,
            override_antenna_mhz,
        };

        // Use the shared core logic
        match cli::args_to_action(&args) {
            cli::CliAction::Run(params) => {
                // run the core processing
                match gpr::run(params) {
                    Ok(_) => Ok(0),
                    Err(e) => Err(pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}"))),
                }
            }
            cli::CliAction::Done => Ok(0),
            cli::CliAction::Error(msg) => Err(pyo3::exceptions::PyValueError::new_err(msg)),
        }
    }
}
