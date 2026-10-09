//! The recording clock of a radargram corrected for antenna separation
//! (#370).
//!
//! `correct_antenna_separation` resamples every trace onto an even depth
//! grid. Depth is not linear in travel time near the surface, so after it a
//! sample is no longer `dt` further along the recording clock than the one
//! above it -- and the regular `recording_time` axis an uncorrected revision
//! offers would, offered here too, relate the two revisions as though
//! nothing had been resampled. On Drønbreen that put carried picks five to
//! seven samples (1.2 to 1.7 m) too shallow while reporting that nothing had
//! visibly moved.
//!
//! The mapping is not lost, though. The correction is a closed-form,
//! monotone function of depth, and everything it needs is in the file: the
//! method and direct-wave velocity in `processing_steps`, the separation and
//! velocity as attributes, the sample interval and time zero on the axes.
//! [`recording_times`] inverts it, giving the recording-clock time of every
//! sample, which `anchors` offers as a `tiepoints` axis in place of the
//! regular one.
//!
//! The one number the file does not otherwise carry is the grid's spacing
//! for the `legacy` method, and for `slant` before #379 made it
//! `v * dt / 2`. That comes from the processing log, which records it.

use crate::gpr::SeparationMethod;

/// What a radargram says about its antenna geometry, read from its file.
#[derive(Debug, Clone, PartialEq)]
pub struct SeparationDeclarations {
    /// The acquisition separation, metres.
    pub antenna_separation_m: f64,
    /// The separation still to correct for, metres: zero once corrected.
    pub antenna_separation_effective_m: f64,
    /// The medium velocity depth was computed with, m/ns.
    pub medium_velocity: f64,
    /// The steps the radargram was processed with, as recorded.
    pub processing_steps: Vec<String>,
    /// The depth grid's spacing as the processing log recorded it, metres,
    /// when it did.
    pub logged_resolution_m: Option<f64>,
}

impl SeparationDeclarations {
    /// Whether the samples were resampled onto a depth grid.
    ///
    /// The same inference as `GPR::twtt_anchor_name`: the correction is the
    /// only thing that zeroes the effective separation, and it refuses to
    /// run when there is no separation to correct.
    pub fn is_corrected(&self) -> bool {
        self.antenna_separation_effective_m == 0.0 && self.antenna_separation_m > 0.0
    }

    /// The correction that ran, as `(method, direct_velocity)`.
    ///
    /// The first one: a second finds no separation left and does nothing.
    pub fn correction(&self) -> Option<(SeparationMethod, f64)> {
        self.processing_steps.iter().find_map(|step| {
            match crate::steps::parse_one(step).ok()?.step {
                crate::steps::Step::CorrectAntennaSeparation {
                    method,
                    direct_velocity,
                } => Some((method, f64::from(direct_velocity))),
                _ => None,
            }
        })
    }

    /// Whether the radargram was migrated.
    ///
    /// Migration moves reflections within the image rather than moving the
    /// grid, so no axis can account for it: a pick carried between a
    /// migrated and an unmigrated revision lands where it was drawn, on
    /// data that may have moved from under it.
    pub fn is_migrated(&self) -> bool {
        self.processing_steps.iter().any(|step| {
            matches!(
                crate::steps::parse_one(step).map(|parsed| parsed.step),
                Ok(crate::steps::Step::KirchhoffMigration2d)
            )
        })
    }
}

/// The grid spacing the last `correct_antenna_separation` logged.
///
/// "Standardized depths to 0.25669098 m (...) per pixel" is the only place
/// a legacy-method spacing is recorded.
pub fn logged_resolution(processing_log: &str) -> Option<f64> {
    processing_log
        .lines()
        .rev()
        .find_map(|line| line.split_once("Standardized depths to "))
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
}

/// Depth (m) of a return at `twtt` (ns from time zero), as the `slant`
/// correction computed it. See `tools::return_time_to_depth`.
pub(crate) fn slant_depth(twtt: f64, velocity: f64, separation: f64, direct_velocity: f64) -> f64 {
    if twtt < 0.0 {
        return twtt * velocity / 2.0;
    }
    let slant_leg = (twtt + separation / direct_velocity) * velocity / 2.0;
    let half_separation = separation / 2.0;
    if slant_leg > half_separation {
        (slant_leg.powi(2) - half_separation.powi(2)).sqrt()
    } else {
        0.0
    }
}

/// The inverse of [`slant_depth`].
///
/// Every return from time zero until the slant legs first reach below the
/// surface is at 0 m, so 0 m has no single time. It is given time zero,
/// which keeps the samples above it (negative depth, a straight path)
/// continuous with it; the samples below then start where the geometry
/// first gives a depth.
fn slant_twtt(depth: f64, velocity: f64, separation: f64, direct_velocity: f64) -> f64 {
    if depth <= 0.0 {
        return 2.0 * depth / velocity;
    }
    let half_separation = separation / 2.0;
    2.0 * (depth.powi(2) + half_separation.powi(2)).sqrt() / velocity - separation / direct_velocity
}

/// The separation the `legacy` correction used. It subtracted time zero
/// times the velocity, which is the pre-0.7 error it is kept to reproduce.
pub(crate) fn legacy_separation(separation: f64, time_zero: f64, velocity: f64) -> f64 {
    (separation.powi(2) - (time_zero * velocity).powi(2))
        .max(0.0)
        .sqrt()
}

/// Depth (m) of a return at `twtt`, as the `legacy` correction computed it.
pub(crate) fn legacy_depth(twtt: f64, velocity: f64, separation: f64) -> f64 {
    let two_way_distance = twtt * velocity;
    if twtt < 0.0 {
        two_way_distance / 2.0
    } else if two_way_distance > 2.0 * separation {
        (two_way_distance.powi(2) - 4.0 * separation.powi(2)).sqrt() / 2.0
    } else {
        0.0
    }
}

/// The inverse of [`legacy_depth`], with 0 m at time zero as for slant.
fn legacy_twtt(depth: f64, velocity: f64, separation: f64) -> f64 {
    if depth <= 0.0 {
        return 2.0 * depth / velocity;
    }
    2.0 * (depth.powi(2) + separation.powi(2)).sqrt() / velocity
}

/// One value for a per-trace variable that must be the same on every trace.
fn uniform(values: &[f64], scale: f64) -> Option<f64> {
    let first = *values.first()?;
    let tolerance =
        f64::from(f32::EPSILON) * values.iter().fold(scale, |m, v| m.max(v.abs())) * 8.0;
    values
        .iter()
        .all(|v| v.is_finite() && (v - first).abs() <= tolerance)
        .then_some(first)
}

/// The recording-clock time of every sample of a radargram corrected for
/// antenna separation, nanoseconds.
///
/// The grid is rebuilt from the file and each sample's depth taken back
/// through the correction's own depth function to the travel time it came
/// from, then put on the recording clock with time zero.
///
/// `Err` says why it cannot be, for a radargram that was corrected but
/// whose correction cannot be reconstructed. That is a revision offering no
/// recording clock, not one offering a wrong one.
pub fn recording_times(
    separation: &SeparationDeclarations,
    crop_ns: &[f64],
    time_zero_ns: &[f64],
    dt_ns: f64,
    n_samples: usize,
) -> Result<Vec<f64>, String> {
    let (method, direct_velocity) = separation.correction().ok_or(
        "the file says it was corrected for antenna separation but does not record \
         the correct_antenna_separation step that did it",
    )?;
    if !(dt_ns.is_finite() && dt_ns > 0.0) {
        return Err("the sample interval is not usable".into());
    }
    let velocity = separation.medium_velocity;
    if !(velocity.is_finite() && velocity > 0.0) {
        return Err("the medium velocity is not usable".into());
    }
    let crop = uniform(crop_ns, dt_ns).ok_or("the crop differs between traces")?;
    let time_zero = uniform(time_zero_ns, dt_ns).ok_or("the time zero differs between traces")?;
    // The travel time of the first sample, as `GPR::twtt_first_sample_ns`.
    let first = crop - time_zero;
    let s = separation.antenna_separation_m;

    let depth_of_twtt: Box<dyn Fn(f64) -> f64> = match method {
        SeparationMethod::Slant => {
            Box::new(move |twtt| slant_depth(twtt, velocity, s, direct_velocity))
        }
        SeparationMethod::Legacy => {
            let s = legacy_separation(s, time_zero, velocity);
            Box::new(move |twtt| legacy_depth(twtt, velocity, s))
        }
    };
    let twtt_of_depth: Box<dyn Fn(f64) -> f64> = match method {
        SeparationMethod::Slant => {
            Box::new(move |depth| slant_twtt(depth, velocity, s, direct_velocity))
        }
        SeparationMethod::Legacy => {
            let s = legacy_separation(s, time_zero, velocity);
            Box::new(move |depth| legacy_twtt(depth, velocity, s))
        }
    };

    // Where each sample sits on the depth grid. The `slant` grid since #379
    // is the one the file's coordinates describe, `twtt * v / 2`; any other
    // starts at the shallowest original sample and steps by what the log
    // recorded. The two agree whenever the first sample is at or above time
    // zero, which is every radargram not cropped further after zero_corr.
    let deterministic = velocity * dt_ns / 2.0;
    let grid: Box<dyn Fn(usize) -> f64> = match (method, separation.logged_resolution_m) {
        (SeparationMethod::Slant, None) => {
            Box::new(move |k| (first + k as f64 * dt_ns) * velocity / 2.0)
        }
        (SeparationMethod::Slant, Some(logged))
            if (logged - deterministic).abs() <= deterministic * 1e-5 =>
        {
            Box::new(move |k| (first + k as f64 * dt_ns) * velocity / 2.0)
        }
        (_, Some(logged)) => {
            let origin = depth_of_twtt(first);
            Box::new(move |k| origin + k as f64 * logged)
        }
        (SeparationMethod::Legacy, None) => {
            return Err(
                "the legacy correction's grid spacing is recorded only in the processing \
                 log, and this file's log does not have it"
                    .into(),
            )
        }
    };

    Ok((0..n_samples)
        .map(|k| twtt_of_depth(grid(k)) + time_zero)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inverses_undo_the_depth_functions() {
        for twtt in [-12.0, -0.5, 60.0, 100.0, 400.0, 2000.0] {
            let depth = slant_depth(twtt, 0.168, 6.2, 0.2997);
            assert!(
                (slant_twtt(depth, 0.168, 6.2, 0.2997) - twtt).abs() < 1e-9,
                "slant at {twtt}"
            );
            let depth = legacy_depth(twtt, 0.168, 3.0);
            assert!(
                (legacy_twtt(depth, 0.168, 3.0) - twtt).abs() < 1e-9,
                "legacy at {twtt}"
            );
        }
    }

    #[test]
    fn the_slant_depth_is_the_one_processing_uses() {
        for twtt in [0.0f32, 30.0, 100.0, 900.0] {
            let ours = slant_depth(f64::from(twtt), 0.168, 6.2, 0.2997);
            let theirs = crate::tools::return_time_to_depth(twtt, 0.168, 6.2, 0.2997);
            assert!(
                (ours - f64::from(theirs)).abs() < 1e-3,
                "{twtt}: {ours} vs {theirs}"
            );
        }
    }

    #[test]
    fn the_logged_resolution_is_the_last_one_logged() {
        let log = "zero_corr (duration: 0.1s):\tsomething\n\
                   correct_antenna_separation (duration: 0.15s):\tStandardized depths to \
                   0.25669098 m (0.08427165 ns) per pixel by accounting for ...";
        assert_eq!(logged_resolution(log), Some(0.25669098));
        assert_eq!(logged_resolution("zero_corr: nothing"), None);
    }

    fn declarations(steps: &[&str], logged: Option<f64>) -> SeparationDeclarations {
        SeparationDeclarations {
            antenna_separation_m: 6.2,
            antenna_separation_effective_m: 0.0,
            medium_velocity: 0.168,
            processing_steps: steps.iter().map(|s| s.to_string()).collect(),
            logged_resolution_m: logged,
        }
    }

    #[test]
    fn the_correction_is_read_from_the_recorded_steps() {
        let corrected = declarations(
            &[
                "zero_corr",
                "correct_antenna_separation(method=slant, direct_velocity=0.2)",
            ],
            None,
        );
        let (method, velocity) = corrected.correction().unwrap();
        assert_eq!(method, SeparationMethod::Slant);
        assert!((velocity - 0.2).abs() < 1e-6);
        assert!(corrected.is_corrected());
        assert!(!corrected.is_migrated());
        assert!(declarations(&["kirchhoff_migration2d"], None).is_migrated());
        assert!(declarations(&["zero_corr"], None).correction().is_none());
    }

    #[test]
    fn recording_times_increase_and_start_at_the_crop() {
        let corrected = declarations(&["correct_antenna_separation"], None);
        let times = recording_times(&corrected, &[100.0], &[100.0], 3.0, 600).unwrap();
        // Sample 0 is at time zero, on the clock at the crop.
        assert!((times[0] - 100.0).abs() < 1e-9);
        assert!(times.windows(2).all(|pair| pair[1] > pair[0]));
        // Deep down the geometry is nearly vertical: one sample is almost
        // exactly `dt` on the clock.
        assert!((times[599] - times[598] - 3.0).abs() < 0.01);
    }

    #[test]
    fn a_slant_grid_from_before_379_is_rebuilt_from_its_logged_spacing() {
        // The data-derived spacing was a little larger than `v * dt / 2`, so
        // the same sample index is deeper -- and later on the clock -- than
        // on the grid that replaced it.
        let steps = ["correct_antenna_separation"];
        let current =
            recording_times(&declarations(&steps, None), &[0.0], &[0.0], 3.0, 700).unwrap();
        let deterministic = 0.168 * 3.0 / 2.0;
        let logged = declarations(&steps, Some(deterministic));
        assert_eq!(
            recording_times(&logged, &[0.0], &[0.0], 3.0, 700).unwrap(),
            current,
            "a logged spacing equal to the deterministic one is the same grid"
        );
        let old = declarations(&steps, Some(deterministic * 1.0032));
        let before = recording_times(&old, &[0.0], &[0.0], 3.0, 700).unwrap();
        assert!(
            before[699] > current[699] + 3.0,
            "{} vs {}",
            before[699],
            current[699]
        );
    }

    #[test]
    fn a_legacy_correction_without_its_logged_spacing_cannot_be_reconstructed() {
        let corrected = declarations(&["correct_antenna_separation(legacy)"], None);
        assert!(recording_times(&corrected, &[0.0], &[0.0], 3.0, 10).is_err());
        let logged = declarations(&["correct_antenna_separation(legacy)"], Some(0.25));
        assert!(recording_times(&logged, &[0.0], &[0.0], 3.0, 10).is_ok());
    }
}
