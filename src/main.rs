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
use clap::Parser;

mod cli;
mod coords;
mod dem;
mod export;
/// A radargram built from arrays: `ridal.write_netcdf` in Python. Only the
/// binding in `lib.rs` calls it; it is declared here for its tests, which
/// run in this target.
#[allow(dead_code)]
mod external;
mod formats;
mod gpr;
mod id_pattern;
mod identity;
mod inspect;
mod interp;
mod io;
mod job;
mod positions;
mod processing;
mod project;
mod recipe;
mod render;
#[cfg(feature = "server")]
mod server;
mod site;
mod source;
#[cfg(feature = "server")]
mod survey;
mod tools;
mod user_metadata;

const PROGRAM_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROGRAM_NAME: &str = env!("CARGO_PKG_NAME");
const PROGRAM_AUTHORS: &str = env!("CARGO_PKG_AUTHORS");

fn main() {
    let args = cli::Args::parse();
    let return_code = cli::main(args);

    std::process::exit(return_code);
}
