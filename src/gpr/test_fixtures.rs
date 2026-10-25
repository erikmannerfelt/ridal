//! Radargrams for tests, shared by the modules that test steps, exports
//! and the pipeline.

use std::{path::PathBuf, str::FromStr};

use ndarray::Array1;

use super::{CorPoint, GPRLocation, LocationCorrection};

pub(crate) fn make_cor_points(n_points: usize, spacing: f64) -> Vec<CorPoint> {
    let eastings = Array1::range(0_f64, n_points as f64 * spacing, spacing);

    let northings = Array1::<f64>::zeros(n_points);

    let altitudes = eastings.clone();
    let seconds = eastings.clone();

    let trace_n = eastings.mapv(|v| v as u32);

    (0..n_points)
        .map(|i| CorPoint {
            trace_n: trace_n[i],
            time_seconds: seconds[i],
            easting: eastings[i],
            northing: northings[i],
            altitude: altitudes[i],
        })
        .collect::<Vec<CorPoint>>()
}

pub(crate) fn make_gpr_location(
    n_points: usize,
    spacing: Option<f64>,
    crs: Option<String>,
    correction: Option<LocationCorrection>,
) -> GPRLocation {
    let cor_points = make_cor_points(n_points, spacing.unwrap_or(1.));
    let crs = match crs {
        Some(c) => c,
        None => {
            let first_coord = crate::coords::Coord {
                x: cor_points[0].easting,
                y: cor_points[0].northing,
            };
            crate::coords::UtmCrs::optimal_crs(&first_coord).to_epsg_str()
        }
    };
    GPRLocation {
        cor_points,
        correction: correction.unwrap_or(LocationCorrection::None),
        time_source: super::TimeSource::Recorded,
        crs,
    }
}

pub(crate) fn make_dummy_gpr(
    n_traces: usize,
    n_samples: usize,
    spacing: Option<f64>,
) -> super::GPR {
    let gpr_location = make_gpr_location(n_traces, spacing, None, None);
    let metadata = super::GPRMeta {
        samples: n_samples as u32,
        frequency: 5000.,
        frequency_steps: 0,
        time_interval: 0.2,
        antenna: "500MHz".to_string(),
        antenna_mhz: 500.,
        antenna_separation: 1.,
        time_window: 2000.,
        last_trace: n_traces as u32,
        data_filepath: std::path::PathBuf::from_str("filepath.rd3").unwrap(),
        medium_velocity: 0.167,
    };

    let mut data = ndarray::Array2::<f32>::zeros((n_samples, n_traces));
    let new_row = ndarray::Array1::<f32>::range(0., n_traces as f32, 1.);
    for mut row in data.rows_mut() {
        row.assign(&new_row);
    }

    super::GPR {
        location: gpr_location,
        metadata,
        data,
        topo_data: None,
        steps: Vec::new(),
        crop_ns: vec![0.; n_traces],
        time_zero_ns: vec![0.; n_traces],
        antenna_separation_effective: 1.,
        log: Vec::new(),
        user_metadata: crate::user_metadata::UserMetadata::new(),
        // A real radargram_id is required for export() to succeed
        // (#116); test helpers give it one directly rather than routing
        // through build_processed_gpr()'s CLI-argument resolution.
        identity: super::RidalIdentity {
            radargram_id: Some(crate::identity::RadargramId::new("test-radargram").unwrap()),
            display_name: None,
            group_name: None,
            group_id: None,
        },
        inputs: None,
    }
}

pub(crate) fn make_test_metadata(width: Option<usize>, height: Option<usize>) -> super::GPRMeta {
    super::GPRMeta {
        samples: height.unwrap_or(1024) as u32,
        frequency: 8000.,
        frequency_steps: 1,
        time_interval: 1000.,
        antenna: "800MHz".into(),
        antenna_mhz: 800.,
        antenna_separation: 2.,
        time_window: 500.,
        last_trace: width.unwrap_or(2048) as u32,
        data_filepath: PathBuf::new(),
        medium_velocity: 0.168,
    }
}

pub(crate) fn make_test_gpr(width: Option<usize>, height: Option<usize>) -> super::GPR {
    let width = width.unwrap_or(2024);
    let height = height.unwrap_or(1024);

    let gpr_location = make_gpr_location(width, Some(1.), None, None);

    let meta = make_test_metadata(Some(width), Some(height));

    let antenna_separation = meta.antenna_separation;

    let mut data = ndarray::Array2::<f32>::zeros((height, width));

    for mut col in data.columns_mut() {
        col.assign(&Array1::<f32>::linspace(0., (height - 1) as f32, height));
    }

    super::GPR {
        data,
        topo_data: None,
        location: gpr_location,
        metadata: meta,
        steps: Vec::new(),
        log: Vec::new(),
        antenna_separation_effective: antenna_separation,
        crop_ns: vec![0.; width],
        time_zero_ns: vec![0.; width],
        user_metadata: crate::user_metadata::UserMetadata::new(),
        identity: super::RidalIdentity::default(),
        inputs: None,
    }
}

/// `make_test_gpr` plus the identity that `export_dataset` insists on,
/// which `build_processed_gpr` would normally have resolved by now.
pub(crate) fn make_exportable_gpr(width: usize, height: usize) -> super::GPR {
    let mut gpr = make_test_gpr(Some(width), Some(height));
    gpr.identity.radargram_id = Some(crate::identity::RadargramId::new("test_line").unwrap());
    gpr
}

/// A radargram with a detectable first break, which `make_test_gpr`'s
/// linear ramp does not have: `zero_corr` looks for a jump larger than
/// half the mean trace's standard deviation, and every step of a ramp
/// is the same size, so it finds nothing and returns immediately.
///
/// Silence, then the direct wave, then a weaker reflector further down
/// so a second correction also has something to crop.
pub(crate) fn make_gpr_with_first_break(width: usize, height: usize) -> super::GPR {
    let mut gpr = make_exportable_gpr(width, height);
    let direct = height / 10;
    let reflector = height / 2;
    gpr.data = ndarray::Array2::<f32>::from_shape_fn((height, width), |(row, _)| {
        if row == direct {
            100.
        } else if row == reflector {
            40.
        } else {
            0.
        }
    });
    gpr
}

pub(crate) fn f32_attr(ds: &crate::export::ExportDataset, name: &str) -> f32 {
    match ds.attrs.get(name) {
        Some(crate::export::ExportAttr::F32(v)) => *v,
        other => panic!("{name} should be an F32 attribute, got {other:?}"),
    }
}

pub(crate) fn str_attr(
    attrs: &std::collections::BTreeMap<String, crate::export::ExportAttr>,
    name: &str,
) -> String {
    match attrs.get(name) {
        Some(crate::export::ExportAttr::String(v)) => v.clone(),
        other => panic!("{name} should be a string attribute, got {other:?}"),
    }
}

pub(crate) fn per_trace_max_peak(gpr: &mut super::GPR) {
    crate::processing::steps::zero_corr::run_zero_corr(
        gpr,
        &crate::processing::steps::zero_corr::Settings {
            method: crate::processing::steps::zero_corr::Method::MaxPeak,
            scope: crate::processing::steps::zero_corr::Scope::Trace,
            sigma: 5.,
            time_zero: crate::processing::steps::zero_corr::Reference::Peak,
            margin: crate::processing::steps::zero_corr::Margin::Ns(0.),
            window: 51,
        },
        1.,
    )
    .unwrap();
}

/// A radargram whose first break arrives at a different sample in each
/// trace, which is the situation a per-trace zero correction exists for.
pub(crate) fn make_gpr_with_a_wandering_first_break(width: usize, height: usize) -> super::GPR {
    let mut gpr = make_exportable_gpr(width, height);
    let reflector = height / 2;
    gpr.data = ndarray::Array2::<f32>::from_shape_fn((height, width), |(row, col)| {
        // The direct wave drifts by up to a dozen samples across the
        // profile, as it does when the antenna leaves the ground.
        let direct = height / 10 + col % 13;
        if row == direct {
            100.
        } else if row == reflector {
            40.
        } else {
            0.
        }
    });
    gpr
}

/// A radargram whose per-trace quantities all say which input trace
/// they came from, so a test can see that they moved together.
pub(crate) fn make_labelled_gpr(width: usize, height: usize) -> super::GPR {
    let mut gpr = make_dummy_gpr(width, height, Some(1.));
    gpr.topo_data = Some(gpr.data.clone());
    gpr.crop_ns = (0..width).map(|i| i as f32).collect();
    gpr.time_zero_ns = (0..width).map(|i| 100. + i as f32).collect();
    gpr
}

/// Set the antenna separation `gpr` still needs correcting for, in metres,
/// which only processing changes otherwise.
pub(crate) fn set_separation_to_correct(gpr: &mut super::GPR, metres: f32) {
    gpr.antenna_separation_effective = metres;
}

/// `subset` on a copy, leaving `gpr` as it was.
pub(crate) fn subset_of(
    gpr: &super::GPR,
    min_trace: Option<u32>,
    max_trace: Option<u32>,
    min_sample: Option<u32>,
    max_sample: Option<u32>,
) -> Result<super::GPR, String> {
    let mut copy = gpr.window(0..gpr.width(), 0..gpr.height());
    crate::processing::steps::traces::run_subset(
        &mut copy, min_trace, max_trace, min_sample, max_sample,
    )?;
    Ok(copy)
}
