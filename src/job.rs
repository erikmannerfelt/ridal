//! One processing job, as the server hands it to a child process (#382).
//!
//! The server never processes in its own process. It writes a [`JobSpec`]
//! to a file and starts `ridal process-job <spec>`, which runs it and
//! reports each step on stdout as one JSON [`Event`] per line. A job that
//! crashes, runs out of memory or is killed for its timeout takes nothing
//! with it but itself, and it does not share the server's NetCDF lock with
//! every render. `server::processing` is the other end.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Everything one job needs, read from raw files to a processed `.nc`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSpec {
    /// The raw recordings, in the order they are merged.
    pub inputs: Vec<PathBuf>,
    /// Where the processed file goes. Written to a temporary sibling and
    /// renamed into place, so it exists only once it is complete.
    pub output: PathBuf,
    /// Canonical or written-out steps, one per entry.
    pub steps: Vec<String>,
    /// m/ns.
    pub medium_velocity: f32,
    pub crs: Option<String>,
    pub dem: Option<PathBuf>,
    /// A RAMAC `.cor` in place of each recording's own.
    pub cor: Option<PathBuf>,
    pub antenna_mhz_override: Option<f32>,
    pub antenna_separation_override: Option<f32>,
    pub radargram_id: String,
    pub display_name: Option<String>,
    pub group_name: Option<String>,
    pub group_id: Option<String>,
}

/// What a job says while it runs, one per line of its stdout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// About to run step `index` (from 0) of `of`.
    Step {
        index: usize,
        of: usize,
        step: String,
    },
    /// Every step ran; writing the file.
    Exporting,
    /// The file is in place.
    Done {
        output: PathBuf,
        revision_id: String,
    },
    /// The job failed, and why. Nothing was written to `output`.
    Failed { message: String },
}

impl Event {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("an event is plain data and always serializes")
    }
}

/// Run `spec`, telling `emit` about each step, and return the revision the
/// file was written as.
pub fn run(spec: &JobSpec, emit: &mut dyn FnMut(Event)) -> Result<String, String> {
    let temporary = temporary_sibling(&spec.output)?;
    let params = crate::gpr::RunParams {
        filepaths: spec.inputs.clone(),
        output_path: Some(temporary.clone()),
        dem_path: spec.dem.clone(),
        cor_path: spec.cor.clone(),
        medium_velocity: spec.medium_velocity,
        crs: spec.crs.clone(),
        quiet: true,
        track_path: None,
        steps: spec.steps.clone(),
        no_export: false,
        render_path: None,
        render_profile: None,
        render_width: None,
        render_topo: false,
        override_antenna_mhz: spec.antenna_mhz_override,
        override_antenna_separation: spec.antenna_separation_override,
        user_metadata: crate::user_metadata::UserMetadata::new(),
        radargram_id: Some(spec.radargram_id.clone()),
        display_name: spec.display_name.clone(),
        group: spec.group_name.clone(),
        group_id: spec.group_id.clone(),
    };
    let result = (|| {
        let (gpr, _) =
            crate::gpr::build_processed_gpr_with_progress(params, &mut |index, of, step| {
                emit(Event::Step {
                    index,
                    of,
                    step: step.to_string(),
                })
            })
            .map_err(|e| e.to_string())?;
        emit(Event::Exporting);
        gpr.export(&temporary)?;
        let revision_id = match crate::io::inspect_ridal_netcdf(&temporary)? {
            crate::io::RidalNetcdfKind::Supported(meta) => meta.revision_id().to_string(),
            _ => return Err("the file just written is not one Ridal recognises".to_string()),
        };
        std::fs::rename(&temporary, &spec.output).map_err(|e| {
            format!(
                "could not move the result into place at {}: {e}",
                spec.output.display()
            )
        })?;
        Ok(revision_id)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// `output` with `.tmp-<pid>` before its extension, which the NetCDF writer
/// needs to keep.
fn temporary_sibling(output: &Path) -> Result<PathBuf, String> {
    let stem = output
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{} has no file name", output.display()))?;
    Ok(output.with_file_name(format!("{stem}.tmp-{}.nc", std::process::id())))
}

/// `ridal process-job <spec>`: run the job in `spec_path`, writing events
/// to stdout. Fails with the message also given as the last event.
pub fn run_from_file(spec_path: &Path) -> Result<(), String> {
    use std::io::Write;
    let mut stdout = std::io::stdout();
    let mut emit = |event: Event| {
        // A line per event, flushed, so the server sees each as it happens.
        let _ = writeln!(stdout, "{}", event.to_line());
        let _ = stdout.flush();
    };
    let outcome = std::fs::read_to_string(spec_path)
        .map_err(|e| format!("could not read the job {}: {e}", spec_path.display()))
        .and_then(|text| {
            serde_json::from_str::<JobSpec>(&text)
                .map_err(|e| format!("{} is not a job: {e}", spec_path.display()))
        })
        .and_then(|spec| run(&spec, &mut emit).map(|revision_id| (spec, revision_id)));
    match outcome {
        Ok((spec, revision_id)) => {
            emit(Event::Done {
                output: spec.output,
                revision_id,
            });
            Ok(())
        }
        Err(message) => {
            emit(Event::Failed {
                message: message.clone(),
            });
            Err(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(output: &Path, steps: &[&str]) -> JobSpec {
        JobSpec {
            inputs: vec!["assets/mala/dronbreen-20250327-DAT_0066_A1.rad".into()],
            output: output.to_path_buf(),
            steps: steps.iter().map(|s| s.to_string()).collect(),
            medium_velocity: 0.168,
            crs: None,
            dem: None,
            cor: None,
            antenna_mhz_override: None,
            antenna_separation_override: None,
            radargram_id: "line-01".to_string(),
            display_name: Some("Line 1".to_string()),
            group_name: None,
            group_id: None,
        }
    }

    #[test]
    fn events_are_one_line_of_json_each() {
        let event = Event::Step {
            index: 0,
            of: 2,
            step: "dewow".to_string(),
        };
        let line = event.to_line();
        assert_eq!(line, r#"{"event":"step","index":0,"of":2,"step":"dewow"}"#);
        assert_eq!(serde_json::from_str::<Event>(&line).unwrap(), event);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_job_reports_each_step_and_names_its_revision() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("line-01.nc");
        let spec = spec(&output, &["subset(0 100 0 200)", "dewow"]);
        let mut events = Vec::new();
        let revision = run(&spec, &mut |e| events.push(e)).unwrap();

        let steps: Vec<(usize, usize)> = events
            .iter()
            .filter_map(|e| match e {
                Event::Step { index, of, .. } => Some((*index, *of)),
                _ => None,
            })
            .collect();
        assert_eq!(steps, [(0, 2), (1, 2)]);
        assert_eq!(events.last(), Some(&Event::Exporting));
        match crate::io::inspect_ridal_netcdf(&output).unwrap() {
            crate::io::RidalNetcdfKind::Supported(meta) => {
                assert_eq!(meta.revision_id().to_string(), revision);
                assert_eq!(meta.radargram_id.as_str(), "line-01");
            }
            other => panic!("{other:?}"),
        }
        // Only the finished file is left.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_failed_job_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("line-01.nc");
        let spec = spec(&output, &["subset(0 100 0 200)", "subset(500 900)"]);
        let refused = run(&spec, &mut |_| {}).unwrap_err();
        assert!(refused.contains("subset"), "{refused}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_spec_that_cannot_be_read_fails_as_an_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("job.json");
        std::fs::write(&path, "{").unwrap();
        assert!(run_from_file(&path).unwrap_err().contains("is not a job"));
    }
}
