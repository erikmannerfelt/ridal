//! 2D Kirchhoff migration.

use ndarray::{Array1, Array2};
use rayon::prelude::*;

use crate::gpr::GPR;
use crate::processing::steps::StepReport;

pub(crate) fn run_kirchhoff_migration2d(gpr: &mut GPR) -> Result<StepReport, String> {
    let x_coords = gpr.location.distances().mapv(|v| v as f32);
    // The Fresnel zone is measured in traces by dividing by the spacing,
    // so a single trace, or traces recorded standing still, leave nothing
    // to migrate across.
    let track_length =
        x_coords.last().copied().unwrap_or(0.) - x_coords.first().copied().unwrap_or(0.);
    if gpr.width() < 2 || track_length.is_nan() || track_length <= 0. {
        return Ok(StepReport::warning(
            "kirchhoff_migration2d",
            format!(
                "The {} traces span {track_length} m, so there is no track to migrate along. \
                 The data were left as they are.",
                gpr.width()
            ),
        ));
    }
    let mut x_diff = 0_f32;
    for i in 1..x_coords.shape()[0] {
        x_diff += x_coords[i] - x_coords[i - 1];
    }
    x_diff /= (x_coords.shape()[0] - 1) as f32;

    // The z-coords will be negative height in relation to the highest point (all values are
    // positive).
    let mut z_coords = Array1::from_iter(
        gpr.location
            .cor_points
            .iter()
            .map(|point| point.altitude as f32),
    );
    z_coords -= z_coords.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    z_coords *= -1.0;

    // The vertical resolution in ns
    let t_diff = gpr.vertical_resolution_ns();

    // The vertical resolution in m
    let z_diff = t_diff * gpr.metadata.medium_velocity;

    // The minimum logical resolution, assuming just one wavelength
    let logical_res = (gpr.metadata.antenna_mhz / gpr.metadata.medium_velocity) * (1e-9 * 1e6);

    let old_data = gpr.data.iter().collect::<Vec<&f32>>();

    let height = gpr.height();
    let width = gpr.width();

    let output: Vec<f32> = (0..(width * height))
        .into_par_iter()
        .map(|sample_idx| {
            let row = sample_idx / width;
            let trace_n = sample_idx - (row * width);
            let trace_top_z = z_coords[trace_n];
            let trace_x = x_coords[trace_n];

            // The expected two-way time of the sample (assuming it is straight down)
            let t_0 = 2. * row as f32 * t_diff;
            let t_0_px = row;

            // Derive the Fresnel zone
            // This is the radius in which the sample may be affected horizontally
            // Pérez-Gracia et al. (2008) Horizontal resolution in a non-destructive
            // shallow GPR survey: An experimental evaluation. NDT & E International,
            // 41(8): 611–620. doi:10.1016/j.ndteint.2008.06.002
            let fresnel_radius = 0.5 * (logical_res * 2. * z_diff * row as f32).sqrt();

            // Derive the radius in pixel space
            let fresnel_width = fresnel_radius / x_diff;

            // If the fresnel width is zero pixels, no neighbors will change the sample. Therefore, just
            // take the old value and put it in the topographically correct place.
            if fresnel_width < 0.1 {
                return old_data[(t_0_px * width) + trace_n].to_owned();
            };

            // Derive all of the neighboring columns that may affect the sample
            let min_neighbor = (trace_n as f32 - fresnel_width)
                .floor()
                .clamp(0_f32, width as f32) as usize;
            let max_neighbor = (trace_n + fresnel_width.ceil() as usize + 1).clamp(0, width);

            let mut ampl = 0_f32;
            let mut n_ampl = 0_f32;

            // Pixels that are entirely within the fresnel width should be included fully. Pixels
            // outside should be excluded. Pixels that border the fresnel width should be included
            // in a weighted average. Here, these weights are created. If the neighbour-trace
            // distance is larger than the fresnel width, the weight is first assigned 1.0.
            // The ones bordering the fresnel width are given the fraction how how much is covered
            // , e.g. 24% (0.24). With a fresnel width of e.g. 1.2, there would be three weights:
            // [0.2, 1.0, 0.2]
            let n_neighbors = (max_neighbor - min_neighbor) as f32;
            let in_fresnel_weight = n_neighbors / (n_neighbors - 2.);
            let out_fresnel_weight = fresnel_width.fract();

            for neighbor_n in min_neighbor..max_neighbor {
                // Get the vertical component of the two-way time to the neighboring sample.
                let t_top =
                    t_0 - 2. * (z_coords[neighbor_n] - trace_top_z) / gpr.metadata.medium_velocity;

                // Get the travel time to the sample accounting for the x distance
                let t_x = (t_top.powi(2)
                    + (2. * (x_coords[neighbor_n] - trace_x) / gpr.metadata.medium_velocity)
                        .powi(2))
                .sqrt();
                // The sample will be in either the pixel when rounding down or when rounding up
                // ... so these will both be evaluated
                // These values have pixel units, as they are normalized to pixel resolution
                let t_1_px = (0.5 * t_x / t_diff).floor() as usize;
                let mut t_2_px = (0.5 * t_x / t_diff).ceil() as usize;

                if t_2_px >= height {
                    t_2_px = t_1_px;
                };

                // If the travel times are within the bounds of the data and the pixel displacement is not zero,
                // ... append a weighted amplitude accounting for the displacement distance
                if t_1_px < height {
                    let weight = match t_1_px == t_2_px {
                        true => 0_f32,
                        false => ((t_1_px as f32 - (0.5 * t_x / t_diff))
                            / (t_1_px as f32 - t_2_px as f32))
                            .abs(),
                    };

                    ampl += (x_diff / (2. * std::f32::consts::PI * t_x * gpr.metadata.medium_velocity).sqrt()) // Account for the horizontal distance
                    * (t_top / t_x) // Account for the vertical distance
                    * ((1. - weight) * old_data[(t_1_px * width) + neighbor_n] + weight * old_data[(t_2_px * width) + neighbor_n]) // Account for the neigbour's value
                    * if (neighbor_n == min_neighbor) | (neighbor_n == max_neighbor - 1)
                     {out_fresnel_weight} else {in_fresnel_weight};
                    n_ampl += 1.0;
                };
            }

            if n_ampl > 0. {
                ampl / n_ampl
            } else {
                0.
            }
        })
        .collect();

    gpr.replace_samples(Array2::from_shape_vec((height, width), output).unwrap())?;
    Ok(StepReport::new(
        "kirchhoff_migration2d",
        format!(
            "Ran 2D Kirchhoff migration with a velocity of {} m/ns",
            gpr.metadata.medium_velocity
        ),
    ))
}
