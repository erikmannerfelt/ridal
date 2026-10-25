//! Steps that change the geometry the samples describe: the antenna
//! separation and the topography.

use std::time::SystemTime;

use ndarray::{Array1, Array2, Axis, Slice};

use crate::gpr::{SeparationMethod, GPR};
use crate::tools;

/// The typical spacing of a depth axis, in metres: its 80th-percentile step.
fn vertical_resolution_of(depths: &Array1<f32>) -> f32 {
    let diffs = &depths.slice(ndarray::s![1..]) - &depths.slice(ndarray::s![..-1]);
    tools::quantiles(&diffs, &[0.8], None)[0]
}

impl GPR {
    /// Resample each trace onto an even depth grid, taking the antenna
    /// separation out of the geometry.
    ///
    /// `direct_velocity` is the velocity (m/ns) of the wave time zero was
    /// picked on; see [`tools::return_time_to_depth`]. [`SeparationMethod::Legacy`]
    /// ignores it.
    pub fn correct_antenna_separation(
        &mut self,
        method: SeparationMethod,
        direct_velocity: f32,
    ) -> Result<(), String> {
        let start_time = SystemTime::now();

        if self.antenna_separation_effective() == 0. {
            self.log_event(
                "correct_antenna_separation",
                "Skipping antenna separation correction since the antenna separation is 0 m.",
                start_time,
            );
            return Ok(());
        };

        let height_before = self.height();

        let depths = match method {
            SeparationMethod::Slant => self.depths_timed_from(direct_velocity),
            SeparationMethod::Legacy => self.legacy_depths(),
        };
        let max_depth = depths.iter().cloned().fold(0.0f32, f32::max);
        let min_depth = depths.iter().cloned().fold(f32::INFINITY, f32::min);

        if max_depth == 0.0 {
            eprintln!("correct_antenna_separation failed. Max depth after antenna correction ({} m) would be 0 m", self.antenna_separation_effective());
            panic!("");
        }

        // The grid the corrected radargram is resampled onto.
        //
        // Slant: the grid the file's own coordinates already describe
        // (#379). The sample interval is kept, so the exported `twtt` steps
        // by `dt` from the first sample's travel time, and `depth` is
        // `twtt * v / 2` -- so the grid has to start at the first sample's
        // `twtt * v / 2` and step by `v * dt / 2` for those coordinates to be
        // the depths of the samples they label. A spacing taken from the
        // data instead (the 0.8 quantile of the depth steps, which is
        // larger than `v * dt / 2` because the slant geometry stretches the
        // shallow part) left the `depth` coordinate short by 0.3 % on
        // Drønbreen, half a metre at 150 m. Being derivable from the file
        // is also what lets picks be carried across the correction (#370).
        //
        // Legacy keeps its own grid, which is what reproduces published
        // results to the micrometre (`derive_regression_tests`).
        let (resampler, resolution, origin_note) = match method {
            SeparationMethod::Slant => {
                let velocity = self.metadata.medium_velocity;
                let resolution = velocity * self.vertical_resolution_ns() / 2.;
                let origin = self.twtt_first_sample_ns() * velocity / 2.;
                let n_samples = ((max_depth - origin) / resolution).floor().max(0.) as usize + 1;
                let target =
                    Array1::from_iter((0..n_samples).map(|k| origin + k as f32 * resolution));
                // Only after a crop below time zero: then the first sample's
                // travel time is not the shallowest depth, and the grid
                // either leaves out or reaches above the shallowest data.
                let note = if (origin - min_depth).abs() > resolution / 2. {
                    format!(
                        " The grid starts at {origin} m, the depth of the first sample's travel \
                         time, while the shallowest sample was at {min_depth} m."
                    )
                } else {
                    String::new()
                };
                (
                    tools::Resampler::<f32>::new_with_target(depths, target),
                    resolution,
                    note,
                )
            }
            SeparationMethod::Legacy => {
                let resolution = vertical_resolution_of(&depths);
                (
                    tools::Resampler::<f32>::new(depths, resolution),
                    resolution,
                    String::new(),
                )
            }
        };

        self.replace_samples(resampler.resample_along_axis_par(&self.data, tools::Axis2D::Row))?;
        self.log_event("correct_antenna_separation", &format!("Standardized depths to {} m ({} ns) per pixel by accounting for an antenna separation of {} m with the {} geometry (height changed from {} px to {} px).{}", resolution, resolution / (self.metadata.time_window / self.height() as f32), self.antenna_separation_effective(), match method {
            SeparationMethod::Slant => format!("slant (direct wave at {direct_velocity} m/ns)"),
            SeparationMethod::Legacy => method.to_string(),
        }, height_before, self.height(), origin_note), start_time);

        self.mark_separation_corrected();
        Ok(())
    }

    pub fn correct_topography(&mut self) {
        let start_time = SystemTime::now();

        let mut altitudes = Array1::<f32>::from_iter(
            self.location
                .cor_points
                .iter()
                .map(|point| point.altitude as f32),
        );
        altitudes -= altitudes.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        altitudes *= -1.;

        // The same depths the exported `depth` axis has, so the grid and
        // the elevation axis written for it agree, including a first sample
        // above time zero.
        let depths = self.depths();
        let max_depth = depths[depths.len() - 1] - depths[0];

        let sample_per_meter = self.height() as f32 / max_depth;

        let start_indices = altitudes.mapv(|altitude| (altitude * sample_per_meter) as isize);

        let mut topo_data = Array2::<f32>::zeros((
            ((max_depth + altitudes.iter().cloned().fold(f32::NEG_INFINITY, f32::max))
                * sample_per_meter) as usize,
            self.width(),
        ));

        for (i, col) in self.data.columns().into_iter().enumerate() {
            topo_data
                .column_mut(i)
                .slice_axis_mut(
                    Axis(0),
                    Slice::new(
                        start_indices[i],
                        Some(self.height() as isize + start_indices[i]),
                        1,
                    ),
                )
                .assign(&col);
        }

        self.topo_data = Some(topo_data);

        self.log_event(
            "correct_topography",
            "Generated a profile that is corrected for topography (topo_data).",
            start_time,
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::gpr::test_fixtures::*;

    #[test]
    fn test_correct_antenna_separation() {
        let mut gpr = make_test_gpr(Some(10), Some(1024));

        set_separation_to_correct(&mut gpr, 30.);

        assert_eq!(gpr.data[[10, 0]], 10.);
        assert_eq!(gpr.log.len(), 0);
        gpr.correct_antenna_separation(
            super::SeparationMethod::Slant,
            crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
        )
        .unwrap();
        assert!(gpr
            .log
            .last()
            .unwrap()
            .contains("correct_antenna_separation"));

        assert_ne!(gpr.height(), 1024);

        assert!(gpr.data[[10, 0]] > 10.);
    }

    #[test]
    fn a_slant_corrected_radargram_is_where_its_depth_coordinate_says() {
        // #379. Every sample holds its own original index, so after the
        // correction each row says which original sample it came from, and
        // that sample's depth before the correction has to be the depth
        // the corrected radargram labels the row with. The data-derived
        // spacing this replaces fell 0.3 % short of that on Drønbreen.
        let mut gpr = make_test_gpr(Some(4), Some(1024));
        set_separation_to_correct(&mut gpr, 6.2);
        let before = gpr.depths_timed_from(crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS);
        gpr.correct_antenna_separation(
            super::SeparationMethod::Slant,
            crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
        )
        .unwrap();
        let after = gpr.depths();
        let resolution = after[1] - after[0];
        assert!(
            (resolution - gpr.metadata.medium_velocity * gpr.vertical_resolution_ns() / 2.).abs()
                < 1e-6
        );

        let depth_of_original = |index: f32| {
            let low = index.floor() as usize;
            let fraction = index - low as f32;
            before[low] + fraction * (before[(low + 1).min(before.len() - 1)] - before[low])
        };
        let deepest = before[before.len() - 1];
        let mut checked = 0;
        for (row, labelled) in after.iter().enumerate() {
            // Above the direct wave every sample collapses to 0 m, so no
            // single original sample is "the" one there.
            if *labelled < 1.0 || *labelled > deepest - resolution {
                continue;
            }
            let actual = depth_of_original(gpr.data[[row, 0]]);
            assert!(
                (actual - labelled).abs() < 1e-3,
                "row {row}: labelled {labelled} m, holds data from {actual} m"
            );
            checked += 1;
        }
        assert!(checked > 500, "{checked}");
    }
}
