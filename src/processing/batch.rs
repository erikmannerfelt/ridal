//! Processing many recordings at once: planning which inputs become
//! which radargrams, and naming them.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::formats;
use crate::processing::grouping::{self, GroupingEdits, GroupingRecording};
use crate::processing::{self, input, ProcessingOptions, RunParams};

#[derive(Debug, Clone)]
pub struct BatchRunParams {
    pub filepaths: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub track_dir: Option<PathBuf>,
    pub render_dir: Option<PathBuf>,
    pub merge: Option<String>,
    // Batch mode derives a radargram ID and (absent) display name per output
    // from each output's own stem -- an explicit single ID or display name
    // would collide across outputs -- unless patterns say otherwise (#368).
    /// An id pattern (see [`crate::id_pattern`]). The outputs are then named
    /// after their ids, `<id>.nc`.
    pub id_pattern: Option<String>,
    pub display_name_pattern: Option<String>,
    pub options: ProcessingOptions,
}

#[derive(Debug, Clone)]
pub struct BatchProcessResult {
    pub output_paths: Vec<PathBuf>,
}

pub fn run_batch(params: BatchRunParams) -> Result<BatchProcessResult, String> {
    if !params.output_dir.is_dir() {
        return Err(format!(
            "output_dir must be an existing directory: {}",
            params.output_dir.display()
        ));
    }
    if let Some(render_dir) = &params.render_dir {
        if !render_dir.is_dir() {
            return Err(format!(
                "render_dir must be an existing directory: {}",
                render_dir.display()
            ));
        }
    }
    if let Some(track_dir) = &params.track_dir {
        if !track_dir.is_dir() {
            return Err(format!(
                "track_dir must be an existing directory: {}",
                track_dir.display()
            ));
        }
    }

    let plan = plan_batch(&params)?;
    if !params.options.quiet {
        for warning in &plan.warnings {
            eprintln!("{warning}");
        }
    }
    let mut output_paths = Vec::<PathBuf>::new();

    for planned in plan.outputs {
        let group = planned.inputs;
        let output_nc = planned.output;
        let radargram_id = Some(planned.radargram_id);
        let display_name = planned.display_name;
        let stem = output_nc
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("Could not derive output stem from {}", output_nc.display()))?
            .to_string();

        let render_path = params
            .render_dir
            .as_ref()
            .map(|dir| Some(dir.join(format!("{stem}.jpg"))));

        let track_path = params
            .track_dir
            .as_ref()
            .map(|dir| Some(dir.join(format!("{stem}_track.csv"))));

        let run_params = RunParams {
            filepaths: group.clone(),
            output_path: Some(output_nc.clone()),
            track_path,
            render_path,
            // Always given, from the plan: without a pattern it is the id
            // process mode would fall back to, and giving it here keeps that
            // mode's advice to pass --radargram-id, which batch mode does not
            // have, from being printed once per output (#368).
            radargram_id,
            display_name,
            options: ProcessingOptions {
                // The parts the plan chose, which a run's own
                // `--lines` and `--channel` would otherwise choose.
                parts: Some(planned.parts.clone()),
                lines: None,
                channel: None,
                ..params.options.clone()
            },
        };

        // process-mode semantics: one group -> one output, in the order we pass here.
        // For merge groups, group_batch_inputs_chronologically() already ordered them chronologically.
        let result = processing::run(run_params).map_err(|e| e.to_string())?;
        output_paths.push(result.output_path);
    }

    Ok(BatchProcessResult { output_paths })
}

/// Why an output of a merging batch starts a new radargram instead of
/// continuing the one before it, in time order (#382 phase 2).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct Split {
    /// From the end of the previous output to the start of this one.
    pub gap_seconds: f64,
    /// Whether the gap alone is more than `--merge` allows.
    pub over_threshold: bool,
    /// Why the two cannot be one radargram, when they cannot: the first
    /// thing `GPR::merge` refused.
    #[cfg_attr(feature = "server", schema(required = true))]
    pub incompatible: Option<String>,
    /// Whether a person asked for a new radargram here, in the ingest
    /// planner (#382); never in `batch-process`.
    pub manual: bool,
}

/// One output of a batch, as it would be written.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlannedOutput {
    pub radargram_id: String,
    pub display_name: Option<String>,
    /// The `.nc` file it is written to.
    pub output: PathBuf,
    /// In the order they are merged.
    pub inputs: Vec<PathBuf>,
    /// The part of each input that is read, in the order of `inputs`: a
    /// line of an IceRadar file or a channel of a GSSI file of several, or
    /// the whole file.
    pub parts: Vec<formats::Part>,
    /// Why this output is not part of the one before it; `None` for the
    /// first output, and for every output of a batch that does not merge.
    pub split: Option<Split>,
}

/// What a batch would do, before it does any of it: which inputs make which
/// output, under which id, and why the inputs were split where they were.
///
/// `run_batch` processes exactly this, and `batch-process --dry-run` prints
/// it, so the two cannot disagree. Every id is checked here, so a plan that
/// exists is one that can run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BatchPlan {
    /// `--merge`, in seconds, or `None` when the batch does not merge.
    pub merge_threshold_seconds: Option<f64>,
    pub outputs: Vec<PlannedOutput>,
    /// What the batch would do that may not be what was meant, to show
    /// before it runs.
    pub warnings: Vec<String>,
}

pub fn plan_batch(params: &BatchRunParams) -> Result<BatchPlan, String> {
    let expanded_filepaths = input::expand_cli_inputs(&params.filepaths)?;
    formats::check_cor_count(expanded_filepaths.len(), params.options.cor_path.as_deref())?;
    // Each output is processed on its own, where one sidecar for one
    // recording would pass; refused here for the batch as a whole.
    if let crate::sidecar::Choice::File(path) = &params.options.sidecar {
        if expanded_filepaths.len() > 1 {
            return Err(format!(
                "{} is one sidecar, but {} recordings were given. Put a <stem>{} beside each \
                 recording instead.",
                path.display(),
                expanded_filepaths.len(),
                crate::sidecar::SIDECAR_SUFFIX
            ));
        }
    }
    if params.options.channel.is_some() && !expanded_filepaths.iter().any(|p| is_gssi(p)) {
        return Err(
            "--channel (channel= in Python) applies to GSSI files only; none was given".to_string(),
        );
    }
    let recordings = batch_recordings(&expanded_filepaths, params.options.channel)?;
    let threshold = params
        .merge
        .as_deref()
        .map(|merge| {
            parse_duration::parse(merge)
                .map(|d| d.as_secs_f64())
                .map_err(|e| format!("Failed to parse merge duration '{merge}': {e}"))
        })
        .transpose()?;
    if params.display_name_pattern.is_some() && params.id_pattern.is_none() {
        return Err(
            "--display-name-pattern needs --id-pattern too: each output's display name \
             is rendered beside its id"
                .to_string(),
        );
    }

    // Merging orders the inputs by time, and a pattern renders from what
    // the headers say: either way each header and its positions are read,
    // once, and the data is not.
    let read = (threshold.is_some() || params.id_pattern.is_some())
        .then(|| {
            recordings
                .iter()
                .map(|recording| read_for_grouping(recording, params))
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?;

    let groups: Vec<(Vec<usize>, Option<Split>)> = match (threshold, &read) {
        (Some(threshold), Some(read)) => group_each_channel(&recordings, read, threshold),
        _ => (0..recordings.len()).map(|i| (vec![i], None)).collect(),
    };
    let names = match &read {
        Some(read) => batch_names(params, &groups, &recordings, read)?,
        None => None,
    };
    let warnings = match (&read, params.id_pattern.as_deref()) {
        (Some(read), Some(pattern)) => channel_warning(params, pattern, &recordings, read)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    };

    let mut outputs = Vec::with_capacity(groups.len());
    let mut written = std::collections::HashMap::<PathBuf, PathBuf>::new();
    for (index, (members, split)) in groups.into_iter().enumerate() {
        let group: Vec<PathBuf> = members
            .iter()
            .map(|i| recordings[*i].path.clone())
            .collect();
        let parts: Vec<formats::Part> = members.iter().map(|i| recordings[*i].part).collect();
        let first = &group[0];
        let file_stem = first
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("Could not derive output stem from {}", first.display()))?;
        // As `ridal process` names a line's or a channel's output.
        let file_stem = format!("{file_stem}{}", processing::part_suffix(&parts[0]));
        let (radargram_id, display_name, stem) = match &names {
            Some((ids, display_names)) => (
                ids[index].to_string(),
                display_names
                    .as_ref()
                    .and_then(|names| names[index].as_ref())
                    .map(|name| name.as_str().to_string()),
                ids[index].to_string(),
            ),
            None => (
                crate::identity::RadargramId::from_fallback(&file_stem)?.to_string(),
                None,
                file_stem,
            ),
        };
        let output = params.output_dir.join(format!("{stem}.nc"));
        // Two inputs with one file name in different directories used to
        // overwrite each other's output without a word.
        if let Some(other) = written.insert(output.clone(), first.clone()) {
            return Err(format!(
                "{} and {} would both be written to {}. Rename one, or name the outputs \
                 with --id-pattern{}.",
                other.display(),
                first.display(),
                output.display(),
                match (parts[0].line, parts[0].channel) {
                    (Some(_), _) => ", using {line} for the lines of an IceRadar file",
                    (None, Some(_)) => ", using {channel} for the channels of a GSSI file",
                    (None, None) => "",
                }
            ));
        }
        outputs.push(PlannedOutput {
            radargram_id,
            display_name,
            output,
            inputs: group,
            parts,
            split,
        });
    }
    Ok(BatchPlan {
        merge_threshold_seconds: threshold,
        outputs,
        warnings,
    })
}

/// Whether `path` is a GSSI recording, which `--channel` applies to.
fn is_gssi(path: &Path) -> bool {
    formats::resolve_input(path).is_ok_and(|r| r.kind == formats::FormatKind::Gssi)
}

/// One recording of a batch: a file, one line of an IceRadar file, or one
/// channel of a GSSI file.
#[derive(Debug, Clone, PartialEq)]
struct Recording {
    path: PathBuf,
    /// The whole file, or the line or channel read.
    part: formats::Part,
    /// The IceRadar line it is, for `{line}`: the part's, or the only line
    /// of a file of one, which is read as the whole file.
    line: Option<u32>,
}

/// The recordings the batch's files hold: each file, each line of an
/// IceRadar file of several, and each channel of a GSSI file of several, or
/// only `channel` of every GSSI file when it is given. A channel a file
/// does not have is an error, before anything is processed.
fn batch_recordings(paths: &[PathBuf], channel: Option<u32>) -> Result<Vec<Recording>, String> {
    let mut out = Vec::new();
    for path in paths {
        let whole = Recording {
            path: path.clone(),
            part: formats::Part::default(),
            line: None,
        };
        let resolved = match formats::resolve_input(path) {
            Ok(resolved) if resolved.kind == formats::FormatKind::IceRadar => {
                let lines = crate::io::iceradar_lines(&resolved.header)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                match &lines[..] {
                    // Read as the whole file, as `ridal process` reads it.
                    [(only, _)] => out.push(Recording {
                        line: Some(*only),
                        ..whole
                    }),
                    _ => out.extend(lines.iter().map(|(line, _)| Recording {
                        part: formats::Part {
                            line: Some(*line),
                            ..Default::default()
                        },
                        line: Some(*line),
                        ..whole.clone()
                    })),
                }
                continue;
            }
            Ok(resolved) if resolved.kind == formats::FormatKind::Gssi => resolved,
            // Anything else, or an input that does not resolve, which is
            // reported with its context when it is read.
            _ => {
                out.push(whole);
                continue;
            }
        };
        if let Some(channel) = channel {
            formats::with_channel(vec![resolved], Some(channel))?;
            out.push(Recording {
                part: formats::Part {
                    channel: Some(channel),
                    ..Default::default()
                },
                ..whole
            });
            continue;
        }
        let channels = crate::io::gssi_channels(&resolved.header)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        match channels.len() {
            1 => out.push(whole),
            _ => out.extend(channels.iter().map(|c| Recording {
                part: formats::Part {
                    channel: Some(c.number),
                    ..Default::default()
                },
                ..whole.clone()
            })),
        }
    }
    Ok(out)
}

/// Group each channel's recordings in time on its own, so that a file's
/// channels, recorded side by side, are never merged, while the same
/// channel of consecutive files is. The outputs are in the order their
/// first recording started. Recordings without a channel are one stream,
/// as every batch without GSSI channels is: IceRadar lines, recorded one
/// after another, merge in time as files do.
fn group_each_channel(
    recordings: &[Recording],
    read: &[GroupingRecording],
    threshold: f64,
) -> Vec<(Vec<usize>, Option<Split>)> {
    let mut channels: Vec<Option<u32>> = Vec::new();
    for recording in recordings {
        if !channels.contains(&recording.part.channel) {
            channels.push(recording.part.channel);
        }
    }
    let mut groups = Vec::new();
    for channel in channels {
        let indices: Vec<usize> = (0..recordings.len())
            .filter(|&i| recordings[i].part.channel == channel)
            .collect();
        let these: Vec<GroupingRecording> = indices.iter().map(|&i| read[i].clone()).collect();
        let grouping =
            grouping::group_chronologically(&these, Some(threshold), &GroupingEdits::default());
        groups.extend(grouping.groups.into_iter().map(|group| {
            let members = group
                .members
                .iter()
                .map(|&m| indices[m])
                .collect::<Vec<_>>();
            (members, group.split)
        }));
    }
    // Stable, so one stream keeps the order grouping gave it.
    groups.sort_by(|a, b| {
        read[a.0[0]]
            .start_seconds
            .total_cmp(&read[b.0[0]].start_seconds)
    });
    groups
}

/// A warning when an id pattern may not tell the channels of a GSSI file
/// apart: it uses neither `{channel}` nor, for a file whose channels have
/// different frequencies, `{freq}`. Two outputs with one id are refused
/// anyway; this is for ids that differ only by a counter, say, and so do
/// not say which channel is which.
fn channel_warning(
    params: &BatchRunParams,
    id_pattern: &str,
    recordings: &[Recording],
    read: &[GroupingRecording],
) -> Option<String> {
    let pattern =
        crate::id_pattern::Pattern::parse(id_pattern, crate::id_pattern::PatternKind::Id).ok()?;
    if params.options.channel.is_some() || pattern.uses_channel() {
        return None;
    }
    let mut unclear: Vec<&PathBuf> = Vec::new();
    for recording in recordings {
        if recording.part.channel.is_none() || unclear.contains(&&recording.path) {
            continue;
        }
        let frequencies: Vec<f32> = (0..recordings.len())
            .filter(|&j| recordings[j].path == recording.path)
            .map(|j| read[j].key.antenna_mhz.round())
            .collect();
        let distinct = frequencies
            .iter()
            .enumerate()
            .all(|(k, f)| !frequencies[..k].contains(f));
        if !(pattern.uses_freq() && distinct) {
            unclear.push(&recording.path);
        }
    }
    let first = unclear.first()?;
    Some(format!(
        "Warning: --id-pattern {id_pattern:?} uses neither {{channel}} nor a {{freq}} that \
         differs between the channels, so the ids of {}{} may not say which channel each is. \
         Add {{channel}} to the pattern, or read one channel with --channel.",
        first.display(),
        match unclear.len() {
            1 => String::new(),
            n => format!(" and {} other files", n - 1),
        }
    ))
}

/// One input's header and positions, as planning a batch reads them.
fn read_for_grouping(
    recording: &Recording,
    params: &BatchRunParams,
) -> Result<GroupingRecording, String> {
    let path = &recording.path;
    let resolved = formats::resolve_input(path)
        .map_err(|e| format!("Failed to resolve input '{}': {e}", path.display()))?;
    let mut resolved = formats::ResolvedInput {
        part: recording.part,
        ..resolved
    };
    formats::with_coordinates(
        std::slice::from_mut(&mut resolved),
        params.options.cor_path.as_deref(),
    )?;
    crate::sidecar::with_sidecar(std::slice::from_mut(&mut resolved), &params.options.sidecar)?;
    let (meta, location) = input::load_meta_and_location(
        &resolved,
        params.options.medium_velocity,
        params.options.crs.as_ref(),
        None,
        params.options.override_antenna_mhz,
        params.options.override_antenna_separation,
        params.options.override_date,
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    GroupingRecording::of(&meta, &location)
        .ok_or_else(|| format!("No location points found in '{}'", path.display()))
}

/// What a pattern is rendered from, for an output made of `members`, the
/// first of which is `first` and reads as `recording`.
fn output_facts(
    first: &Recording,
    recording: &GroupingRecording,
    n_files: usize,
) -> crate::id_pattern::OutputFacts {
    let first_path = &first.path;
    crate::id_pattern::OutputFacts {
        label: first_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        stem: first_path
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        start_seconds: recording.start_seconds,
        antenna_mhz: recording.key.antenna_mhz,
        n_files,
        channel: first.part.channel.unwrap_or(1),
        line: first.line,
    }
}

/// The ids and display names the patterns give each output of a batch, in
/// output order, or `None` without an id pattern (#368).
///
/// Rendered from each output's first input as `read` has it: the start time
/// orders `{n}` and gives `{date}`, and the antenna gives `{freq}`.
/// `from=next` continues after the ids of the processed files already in
/// the output directory.
#[allow(clippy::type_complexity)]
fn batch_names(
    params: &BatchRunParams,
    groups: &[(Vec<usize>, Option<Split>)],
    recordings: &[Recording],
    read: &[GroupingRecording],
) -> Result<
    Option<(
        Vec<crate::identity::RadargramId>,
        Option<Vec<Option<crate::identity::DisplayName>>>,
    )>,
    String,
> {
    use crate::id_pattern::{BatchFacts, Pattern, PatternKind};

    let parse = |text: &str, kind: PatternKind, flag: &str| {
        Pattern::parse(text, kind).map_err(|e| format!("--{flag}: {}", e.render(text)))
    };
    let Some(id_text) = params.id_pattern.as_deref() else {
        return Ok(None);
    };
    let id_pattern = parse(id_text, PatternKind::Id, "id-pattern")?;
    let display_pattern = params
        .display_name_pattern
        .as_deref()
        .map(|text| parse(text, PatternKind::DisplayName, "display-name-pattern"))
        .transpose()?;

    let group = crate::identity::resolve_group(
        params.options.group.as_deref(),
        params.options.group_id.as_deref(),
        None,
        None,
    )?;
    let batch = match group {
        Some((name, id)) => BatchFacts {
            group_id: Some(id),
            group_name: Some(name),
        },
        None => BatchFacts::default(),
    };

    let outputs: Vec<_> = groups
        .iter()
        .map(|(members, _)| output_facts(&recordings[members[0]], &read[members[0]], members.len()))
        .collect();

    let existing = if id_pattern.needs_existing_ids() {
        existing_radargram_ids(&params.output_dir)
    } else {
        Vec::new()
    };
    let ids = crate::id_pattern::render_ids(&id_pattern, &outputs, &batch, &existing)?;
    let display_names = display_pattern
        .map(|pattern| {
            crate::id_pattern::render_display_names(
                &pattern,
                &id_pattern,
                &outputs,
                &batch,
                &ids,
                &existing,
            )
        })
        .transpose()?;
    Ok(Some((ids, display_names)))
}

/// The radargram ids of the processed files directly in `dir`. A file that
/// is not one is skipped: this only finds where a counter left off.
fn existing_radargram_ids(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| input::is_netcdf(path))
        .filter_map(|path| match crate::io::inspect_ridal_netcdf(&path) {
            Ok(crate::io::RidalNetcdfKind::Supported(meta)) => Some(meta.radargram_id.to_string()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::inspect::tests::{info_params, processed};

    fn batch_params(output: &std::path::Path, id_pattern: Option<&str>) -> super::BatchRunParams {
        super::BatchRunParams {
            filepaths: vec![
                "assets/mala/dronbreen-20250327-DAT_0066_A1.rad".into(),
                "assets/mala/dronbreen-20220329-DAT_0237_A1.rad".into(),
                "assets/mala/scott_turnerbreen-20240207-DAT_0454_A1_2500-4000.rad".into(),
            ],
            output_dir: output.to_path_buf(),
            track_dir: None,
            render_dir: None,
            merge: None,
            id_pattern: id_pattern.map(str::to_string),
            display_name_pattern: None,
            options: super::ProcessingOptions {
                quiet: true,
                steps: vec!["subset(0 50 0 100)".to_string()],
                group: Some("Drønbreen".to_string()),
                ..Default::default()
            },
        }
    }

    fn ids_in(dir: &std::path::Path) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .map(|path| {
                let record =
                    processed(crate::inspect::inspect(info_params(vec![path.clone()])).unwrap());
                (
                    path.file_name().unwrap().to_string_lossy().to_string(),
                    record.radargram_id.unwrap(),
                )
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_batch_names_its_outputs_after_the_id_pattern() {
        // #368. The 2022 and 2024 lines are 100 MHz, the 2025 one 25 MHz,
        // and the counter runs in time order within each frequency.
        let dir = tempfile::tempdir().unwrap();
        let mut params = batch_params(dir.path(), Some("{group}-{freq}mhz-{n:02, per=freq}"));
        params.display_name_pattern = Some("{group_name}, {freq} MHz line {n}".to_string());
        super::run_batch(params).unwrap();
        assert_eq!(
            ids_in(dir.path()),
            [
                (
                    "dronbreen-100mhz-01.nc".to_string(),
                    "dronbreen-100mhz-01".to_string()
                ),
                (
                    "dronbreen-100mhz-02.nc".to_string(),
                    "dronbreen-100mhz-02".to_string()
                ),
                (
                    "dronbreen-25mhz-01.nc".to_string(),
                    "dronbreen-25mhz-01".to_string()
                ),
            ]
        );
        let record = processed(
            crate::inspect::inspect(info_params(vec![dir.path().join("dronbreen-100mhz-02.nc")]))
                .unwrap(),
        );
        assert_eq!(
            record.display_name.as_deref(),
            Some("Drønbreen, 100 MHz line 2")
        );

        // Again, continuing: one more of each frequency.
        super::run_batch(batch_params(
            dir.path(),
            Some("{group}-{freq}mhz-{n:02, per=freq, from=next}"),
        ))
        .unwrap();
        let names: Vec<String> = ids_in(dir.path()).into_iter().map(|(_, id)| id).collect();
        assert!(
            names.contains(&"dronbreen-100mhz-04".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"dronbreen-25mhz-02".to_string()),
            "{names:?}"
        );
        assert_eq!(names.len(), 6);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_plan_says_why_each_output_is_its_own_radargram() {
        let dir = tempfile::tempdir().unwrap();

        // Not merging: one output per input, and no splits to explain.
        let plan = super::plan_batch(&batch_params(dir.path(), None)).unwrap();
        assert_eq!(plan.merge_threshold_seconds, None);
        assert_eq!(plan.outputs.len(), 3);
        assert!(plan
            .outputs
            .iter()
            .all(|o| o.split.is_none() && o.inputs.len() == 1));
        assert_eq!(
            plan.outputs[0].radargram_id,
            "dronbreen-20250327-dat_0066_a1"
        );
        assert_eq!(
            plan.outputs[0].output,
            dir.path().join("dronbreen-20250327-DAT_0066_A1.nc")
        );

        // Merging within ten minutes: years apart, and not compatible
        // either, so both reasons are given, in time order.
        let mut params = batch_params(dir.path(), None);
        params.merge = Some("10 min".to_string());
        let plan = super::plan_batch(&params).unwrap();
        assert_eq!(plan.merge_threshold_seconds, Some(600.));
        let first = &plan.outputs[0];
        assert!(first.inputs[0].ends_with("dronbreen-20220329-DAT_0237_A1.rad"));
        assert_eq!(first.split, None);
        for later in &plan.outputs[1..] {
            let split = later.split.as_ref().expect("a later output says why");
            assert!(
                split.over_threshold && split.gap_seconds > 600.,
                "{split:?}"
            );
            assert!(split.incompatible.is_some(), "{split:?}");
        }

        // A threshold of years: only the incompatibility is left.
        params.merge = Some("3 years".to_string());
        let plan = super::plan_batch(&params).unwrap();
        let split = plan.outputs[2].split.as_ref().unwrap();
        assert!(!split.over_threshold);
        assert!(
            split
                .incompatible
                .as_deref()
                .is_some_and(|reason| reason.contains("Antenna frequencies")),
            "{split:?}"
        );

        // Nothing was processed.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn two_inputs_that_would_write_one_output_are_refused() {
        // They used to overwrite each other without a word.
        let dir = tempfile::tempdir().unwrap();
        let mut params = batch_params(dir.path(), None);
        params.filepaths = vec![params.filepaths[0].clone(), params.filepaths[0].clone()];
        let refused = super::plan_batch(&params).unwrap_err();
        assert!(refused.contains("would both be written to"), "{refused}");
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_pattern_that_collides_processes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let refused =
            super::run_batch(batch_params(dir.path(), Some("{group}-{freq}mhz"))).unwrap_err();
        assert!(
            refused.contains("would both be 'dronbreen-100mhz'"),
            "{refused}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

        let mut params = batch_params(dir.path(), None);
        params.display_name_pattern = Some("{n}".to_string());
        assert!(super::run_batch(params)
            .unwrap_err()
            .contains("needs --id-pattern"));
    }

    fn two_channel_batch(
        input: &std::path::Path,
        output: &std::path::Path,
        channel: Option<u32>,
        merge: Option<&str>,
        id_pattern: Option<&str>,
    ) -> super::BatchRunParams {
        use crate::io::test_dzt;
        super::BatchRunParams {
            filepaths: vec![
                test_dzt::write_two_channel_recording(input, "first"),
                test_dzt::write_two_channel_recording(input, "second"),
            ],
            output_dir: output.to_path_buf(),
            track_dir: None,
            render_dir: None,
            merge: merge.map(str::to_string),
            id_pattern: id_pattern.map(str::to_string),
            display_name_pattern: None,
            options: super::ProcessingOptions {
                quiet: true,
                channel,
                ..Default::default()
            },
        }
    }

    fn planned(plan: &super::BatchPlan) -> Vec<(String, Vec<String>, Option<u32>)> {
        plan.outputs
            .iter()
            .map(|o| {
                (
                    o.output.file_name().unwrap().to_string_lossy().to_string(),
                    o.inputs
                        .iter()
                        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
                        .collect(),
                    o.parts[0].channel,
                )
            })
            .collect()
    }

    #[test]
    fn a_batch_makes_each_channel_its_own_output_or_reads_the_one_given() {
        let input = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let plan = |channel, merge| {
            super::plan_batch(&two_channel_batch(
                input.path(),
                output.path(),
                channel,
                merge,
                None,
            ))
            .unwrap()
        };
        let one = |name: &str, channel| {
            (
                format!("{name}_ch_{channel}.nc"),
                vec![format!("{name}.DZT")],
                Some(channel),
            )
        };

        assert_eq!(
            planned(&plan(None, None)),
            [
                one("first", 1),
                one("first", 2),
                one("second", 1),
                one("second", 2)
            ]
        );
        assert_eq!(
            planned(&plan(Some(1), None)),
            [one("first", 1), one("second", 1)]
        );
        let error = super::plan_batch(&two_channel_batch(
            input.path(),
            output.path(),
            Some(3),
            None,
            None,
        ))
        .unwrap_err();
        assert!(error.contains("has no channel 3"), "{error}");

        // Merged, each channel of the two files is one radargram, recorded
        // at the same time as the other channel's.
        let merged = plan(None, Some("10 min"));
        let both = || vec!["first.DZT".to_string(), "second.DZT".to_string()];
        assert_eq!(
            planned(&merged),
            [
                ("first_ch_1.nc".to_string(), both(), Some(1)),
                ("first_ch_2.nc".to_string(), both(), Some(2)),
            ]
        );
        assert!(merged.warnings.is_empty());
    }

    #[test]
    fn an_id_pattern_names_the_channels_or_is_warned_about() {
        let input = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let plan = |channel, pattern| {
            super::plan_batch(&two_channel_batch(
                input.path(),
                output.path(),
                channel,
                None,
                Some(pattern),
            ))
        };
        let ids = |plan: &super::BatchPlan| {
            plan.outputs
                .iter()
                .map(|o| o.radargram_id.clone())
                .collect::<Vec<_>>()
        };

        let named = plan(None, "{stem}-ch{channel}").unwrap();
        assert_eq!(
            ids(&named),
            ["first-ch1", "first-ch2", "second-ch1", "second-ch2"]
        );
        assert!(named.warnings.is_empty(), "{:?}", named.warnings);
        // The channels of a 300/800D file differ in frequency.
        let by_freq = plan(None, "{stem}-{freq}mhz").unwrap();
        assert_eq!(ids(&by_freq)[..2], ["first-800mhz", "first-300mhz"]);
        assert!(by_freq.warnings.is_empty(), "{:?}", by_freq.warnings);

        let counted = plan(None, "line-{n:02}").unwrap();
        let [warning] = &counted.warnings[..] else {
            panic!("expected one warning: {:?}", counted.warnings);
        };
        assert!(warning.contains("uses neither {channel}"), "{warning}");
        assert!(warning.contains("first.DZT and 1 other files"), "{warning}");
        // One channel of each needs no telling apart.
        assert!(plan(Some(2), "line-{n:02}").unwrap().warnings.is_empty());
        // Two outputs with one id are refused, as ever.
        let error = plan(None, "{stem}").unwrap_err();
        assert!(error.contains("first"), "{error}");
    }

    #[test]
    fn a_batch_processes_every_channel() {
        let input = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let params = two_channel_batch(
            input.path(),
            output.path(),
            None,
            None,
            Some("{stem}-ch{channel}"),
        );
        let result = super::run_batch(params).unwrap();
        let mut written = result
            .output_paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        written.sort();
        assert_eq!(
            written,
            [
                "first-ch1.nc",
                "first-ch2.nc",
                "second-ch1.nc",
                "second-ch2.nc"
            ]
        );
    }

    /// A two-line IceRadar file, its second line recorded a few seconds
    /// after the first, in a batch at 20 MHz.
    #[cfg(not(target_os = "windows"))]
    fn two_line_batch(
        input: &std::path::Path,
        output: &std::path::Path,
        merge: Option<&str>,
        id_pattern: Option<&str>,
    ) -> super::BatchRunParams {
        use crate::io::iceradar::tests::{trace, write_fixture};
        use crate::io::iceradar::Layout;
        let path = input.join("stor.h5");
        let line = |times: [&'static str; 2]| times.map(|t| trace(40, 256, Some(t))).to_vec();
        write_fixture(
            &path,
            Layout::V1,
            &[
                line(["103045.6", "103046.8"]),
                line(["103050.0", "103051.2"]),
            ],
            "6/18/2024 1:30:00 PM",
        );
        super::BatchRunParams {
            filepaths: vec![path],
            output_dir: output.to_path_buf(),
            track_dir: None,
            render_dir: None,
            merge: merge.map(str::to_string),
            id_pattern: id_pattern.map(str::to_string),
            display_name_pattern: None,
            options: super::ProcessingOptions {
                quiet: true,
                override_antenna_mhz: Some(20.),
                crs: Some("EPSG:32632".to_string()),
                ..Default::default()
            },
        }
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn a_batch_makes_each_iceradar_line_a_recording() {
        let input = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let plan = |merge, pattern| {
            super::plan_batch(&two_line_batch(input.path(), output.path(), merge, pattern))
        };
        let line = |n| crate::formats::Part {
            line: Some(n),
            ..Default::default()
        };

        let apart = plan(None, None).unwrap();
        assert_eq!(
            planned_parts(&apart),
            [
                ("stor_line_0.nc".to_string(), vec![line(0)]),
                ("stor_line_1.nc".to_string(), vec![line(1)]),
            ]
        );
        // Recorded one after another, the lines merge as files do.
        let merged = plan(Some("10 min"), None).unwrap();
        assert_eq!(
            planned_parts(&merged),
            [("stor_line_0.nc".to_string(), vec![line(0), line(1)])]
        );
        assert_eq!(merged.outputs[0].inputs.len(), 2);

        let named = plan(None, Some("{stem}-l{line:02}")).unwrap();
        let ids: Vec<_> = named
            .outputs
            .iter()
            .map(|o| o.radargram_id.as_str())
            .collect();
        assert_eq!(ids, ["stor-l00", "stor-l01"]);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn a_batch_processes_merged_lines_of_one_file() {
        let input = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let params = two_line_batch(input.path(), output.path(), Some("10 min"), None);
        let result = super::run_batch(params).unwrap();
        assert_eq!(result.output_paths, [output.path().join("stor_line_0.nc")]);
        let info = crate::io::inspect_ridal_netcdf(&result.output_paths[0]).unwrap();
        let crate::io::RidalNetcdfKind::Supported(meta) = info else {
            panic!("not a Ridal file");
        };
        // Both lines' two traces.
        assert_eq!(meta.shape.1, 4, "{:?}", meta.shape);
    }

    #[test]
    fn the_line_placeholder_is_refused_for_other_formats() {
        let output = tempfile::tempdir().unwrap();
        let mut params = batch_params(output.path(), Some("x-{line}"));
        params.filepaths.truncate(1);
        let error = super::plan_batch(&params).unwrap_err();
        assert!(
            error.contains("`{line}` is the line of an IceRadar file"),
            "{error}"
        );
    }

    fn planned_parts(plan: &super::BatchPlan) -> Vec<(String, Vec<crate::formats::Part>)> {
        plan.outputs
            .iter()
            .map(|o| {
                (
                    o.output.file_name().unwrap().to_string_lossy().to_string(),
                    o.parts.clone(),
                )
            })
            .collect()
    }
}
