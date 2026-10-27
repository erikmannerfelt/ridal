//! Grouping recordings into radargrams by time, and which can be merged.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::formats::ResolvedInput;
use crate::gpr::{GPRLocation, GPRMeta};
use crate::processing::batch::Split;
use crate::processing::input;

/// A raw recording's header and positions, without its data, with
/// positions in `crs` and elevations from `dem`: what the ingest planner
/// reads of each recording (#382 phase 7).
pub fn read_header_and_positions(
    resolved: &ResolvedInput,
    crs: Option<&String>,
    dem: Option<&PathBuf>,
    antenna_mhz: Option<f32>,
) -> Result<(GPRMeta, GPRLocation), HeaderReadError> {
    input::load_meta_and_location(resolved, 0.168, crs, dem, antenna_mhz, None, None).map_err(|e| {
        match e.downcast::<crate::io::UnknownAntennaFrequency>() {
            Ok(unknown) => HeaderReadError::UnknownFrequency(*unknown),
            Err(other) => HeaderReadError::Other(other.to_string()),
        }
    })
}

/// Why [`read_header_and_positions`] could not read a recording.
#[derive(Debug, Clone, PartialEq)]
pub enum HeaderReadError {
    /// Readable but for its antenna frequency, which an override gives.
    UnknownFrequency(crate::io::UnknownAntennaFrequency),
    Other(String),
}

/// What decides whether two recordings can be one radargram, read from
/// their headers and positions: the checks [`GPR::merge`] makes before it
/// touches the data, which it makes through this, so the two cannot drift.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeKey {
    pub crs: String,
    pub antenna_mhz: f32,
    pub time_window: f32,
    pub samples: u32,
    pub antenna_separation: f32,
    pub medium_velocity: f32,
}

impl MergeKey {
    pub fn of(meta: &GPRMeta, location: &GPRLocation) -> Self {
        Self {
            crs: location.crs.clone(),
            antenna_mhz: meta.antenna_mhz,
            time_window: meta.time_window,
            samples: meta.samples,
            antenna_separation: meta.antenna_separation,
            medium_velocity: meta.medium_velocity,
        }
    }

    /// Why a recording with `other` cannot follow one with `self`, or
    /// `None` when it can.
    pub fn refusal(&self, other: &MergeKey) -> Option<String> {
        if self.crs != other.crs {
            Some(format!("CRS are different: {} vs {}", self.crs, other.crs))
        } else if self.antenna_mhz != other.antenna_mhz {
            Some(format!(
                "Antenna frequencies are different: {} vs {}",
                self.antenna_mhz, other.antenna_mhz
            ))
        } else if self.time_window != other.time_window {
            Some(format!(
                "Time windows are different: {} vs {}",
                self.time_window, other.time_window
            ))
        // Traces of different lengths cannot be stacked side by side (#507).
        } else if self.samples != other.samples {
            Some(format!(
                "Sample counts are different: {} vs {}",
                self.samples, other.samples
            ))
        // The checks below do not stop the arrays concatenating; they stop
        // the result declaring one value for the whole file that is true of
        // only part of it (#151).
        } else if self.antenna_separation != other.antenna_separation {
            Some(format!(
                "Antenna separations are different: {} m vs {} m",
                self.antenna_separation, other.antenna_separation
            ))
        } else if self.medium_velocity != other.medium_velocity {
            Some(format!(
                "Medium velocities are different: {} m/ns vs {} m/ns",
                self.medium_velocity, other.medium_velocity
            ))
        } else {
            None
        }
    }
}

/// One recording as grouping sees it: when it ran, and what decides what
/// it can be merged with. Read from the header and positions, not the data,
/// so a few hundred recordings are cheap (#382).
#[derive(Debug, Clone, PartialEq)]
pub struct GroupingRecording {
    /// The first and last position's time, seconds since the Unix epoch.
    pub start_seconds: f64,
    pub stop_seconds: f64,
    pub key: MergeKey,
}

impl GroupingRecording {
    /// `None` when the recording has no positions to give it a time.
    pub fn of(meta: &GPRMeta, location: &GPRLocation) -> Option<Self> {
        let times = location.cor_points.iter().map(|p| p.time_seconds);
        let start_seconds = times.clone().fold(f64::INFINITY, f64::min);
        let stop_seconds = times.fold(f64::NEG_INFINITY, f64::max);
        start_seconds.is_finite().then(|| Self {
            start_seconds,
            stop_seconds,
            key: MergeKey::of(meta, location),
        })
    }
}

/// A person's changes to where a merging batch splits, by index into the
/// recordings grouped (#382 phase 7).
#[derive(Debug, Clone, Default)]
pub struct GroupingEdits {
    /// Recordings to keep with the one before them in time, whatever the
    /// gap. A join the headers refuse is not made, and is reported.
    pub joins: HashSet<usize>,
    /// Recordings that start a new output although they would merge. A
    /// recording in both is split.
    pub splits: HashSet<usize>,
}

/// One output: its recordings in merge order, and why it is not part of
/// the output before it.
#[derive(Debug, Clone, PartialEq)]
pub struct Grouped {
    pub members: Vec<usize>,
    pub split: Option<Split>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Grouping {
    /// In time order.
    pub groups: Vec<Grouped>,
    /// Each join that was asked for and refused, with why.
    pub refused_joins: Vec<(usize, String)>,
}

/// Group `recordings` into outputs: in time order, a recording continues
/// the output before it when the gap is at most `threshold` (never, with
/// `None`) and the two can be merged, unless `edits` say otherwise.
pub fn group_chronologically(
    recordings: &[GroupingRecording],
    threshold: Option<f64>,
    edits: &GroupingEdits,
) -> Grouping {
    let mut order: Vec<usize> = (0..recordings.len()).collect();
    order.sort_by(|a, b| {
        recordings[*a]
            .start_seconds
            .total_cmp(&recordings[*b].start_seconds)
            .then(a.cmp(b))
    });

    let mut groups = Vec::<Grouped>::new();
    let mut refused_joins = Vec::new();
    // The current output's key and the end of its last recording.
    let mut current: Option<(&MergeKey, f64)> = None;
    for index in order {
        let recording = &recordings[index];
        let Some((key, stop)) = current.as_mut() else {
            groups.push(Grouped {
                members: vec![index],
                split: None,
            });
            current = Some((&recording.key, recording.stop_seconds));
            continue;
        };
        let gap = (recording.start_seconds - *stop).abs();
        // Asked whatever the gap, so that a split can say both.
        let incompatible = key.refusal(&recording.key);
        let over_threshold = threshold.is_none_or(|t| gap > t);
        let manual = edits.splits.contains(&index);
        let joined = !manual && edits.joins.contains(&index);
        if joined {
            if let Some(reason) = &incompatible {
                refused_joins.push((index, reason.clone()));
            }
        }
        let continues = incompatible.is_none() && !manual && (joined || !over_threshold);
        if continues {
            groups
                .last_mut()
                .expect("current is set")
                .members
                .push(index);
            *stop = stop.max(recording.stop_seconds);
        } else {
            groups.push(Grouped {
                members: vec![index],
                split: Some(Split {
                    gap_seconds: gap,
                    over_threshold,
                    incompatible,
                    manual,
                }),
            });
            current = Some((&recording.key, recording.stop_seconds));
        }
    }
    Grouping {
        groups,
        refused_joins,
    }
}

#[cfg(test)]
mod tests {

    fn timed(start: f64, stop: f64, mhz: f32) -> super::GroupingRecording {
        super::GroupingRecording {
            start_seconds: start,
            stop_seconds: stop,
            key: super::MergeKey {
                crs: "EPSG:32633".to_string(),
                antenna_mhz: mhz,
                time_window: 500.,
                samples: 1024,
                antenna_separation: 4.,
                medium_velocity: 0.168,
            },
        }
    }

    #[test]
    fn grouping_follows_time_and_a_persons_joins_and_splits() {
        use super::{group_chronologically, GroupingEdits};
        // Given out of order: 0 and 2 are 50 s apart, 3 is an hour after
        // 2, and 1 is another antenna right after 3.
        let recordings = vec![
            timed(0., 100., 100.),
            timed(3900., 4000., 800.),
            timed(150., 200., 100.),
            timed(3800., 3850., 100.),
        ];
        let members = |grouping: &super::Grouping| -> Vec<Vec<usize>> {
            grouping.groups.iter().map(|g| g.members.clone()).collect()
        };

        let grouping = group_chronologically(&recordings, Some(600.), &GroupingEdits::default());
        assert_eq!(members(&grouping), [vec![0, 2], vec![3], vec![1]]);
        let split = grouping.groups[1].split.as_ref().unwrap();
        assert_eq!(split.gap_seconds, 3600.);
        assert!(split.over_threshold && split.incompatible.is_none() && !split.manual);
        let split = grouping.groups[2].split.as_ref().unwrap();
        assert!(!split.over_threshold);
        assert!(split
            .incompatible
            .as_deref()
            .unwrap()
            .contains("Antenna frequencies"));

        // Joined whatever the gap; split however close; the other
        // antenna cannot be joined, and says so.
        let edits = GroupingEdits {
            joins: [3, 1].into(),
            splits: [2].into(),
        };
        let grouping = group_chronologically(&recordings, Some(600.), &edits);
        assert_eq!(members(&grouping), [vec![0], vec![2, 3], vec![1]]);
        assert!(grouping.groups[1].split.as_ref().unwrap().manual);
        assert_eq!(grouping.refused_joins.len(), 1);
        assert_eq!(grouping.refused_joins[0].0, 1);

        // Never merging: only joins put two together.
        let edits = GroupingEdits {
            joins: [2].into(),
            splits: Default::default(),
        };
        let grouping = group_chronologically(&recordings, None, &edits);
        assert_eq!(members(&grouping), [vec![0, 2], vec![3], vec![1]]);
        assert!(grouping.groups[1].split.as_ref().unwrap().over_threshold);
    }

    #[test]
    fn recordings_differing_only_in_sample_count_are_two_radargrams() {
        use super::{group_chronologically, GroupingEdits};
        let mut shorter = timed(150., 200., 100.);
        shorter.key.samples = 512;
        let recordings = vec![timed(0., 100., 100.), shorter];

        let grouping = group_chronologically(&recordings, Some(600.), &GroupingEdits::default());
        assert_eq!(grouping.groups.len(), 2);
        let split = grouping.groups[1].split.as_ref().unwrap();
        assert!(!split.over_threshold);
        assert_eq!(
            split.incompatible.as_deref(),
            Some("Sample counts are different: 1024 vs 512")
        );
    }
}
