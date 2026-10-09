//! Carrying picks across `correct_antenna_separation` (#370), measured on
//! the committed Drønbreen Malå file.

use std::path::{Path, PathBuf};

use crate::interp::anchors;
use crate::interp::carry;

const BASE: &str = "remove_empty_traces,zero_corr";

fn process(directory: &Path, name: &str, steps: &str) -> PathBuf {
    let output = directory.join(format!("{name}.nc"));
    let params = crate::gpr::RunParams {
        filepaths: vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/mala/dronbreen-20250327-DAT_0066_A1.rd3")],
        output_path: Some(output.clone()),
        dem_path: None,
        cor_path: None,
        medium_velocity: 0.168,
        crs: Some("EPSG:32633".to_string()),
        quiet: true,
        track_path: None,
        steps: crate::tools::parse_step_list(steps).expect("valid steps"),
        no_export: false,
        render_path: None,
        render_profile: None,
        render_width: None,
        render_topo: false,
        override_antenna_mhz: None,
        override_antenna_separation: None,
        user_metadata: Default::default(),
        radargram_id: Some("dronbreen".to_string()),
        display_name: None,
        group: None,
        group_id: None,
    };
    crate::gpr::run(params).expect("processing the committed Malå file");
    output
}

fn processing_log(path: &Path) -> String {
    let file = netcdf::open(path).unwrap();
    match file.attribute("processing_log").unwrap().value().unwrap() {
        netcdf::AttributeValue::Str(value) => value,
        other => panic!("{other:?}"),
    }
}

fn document(revision: &str, axes: &anchors::Axes, points: &[(f64, f64)]) -> gprinterp::Document {
    serde_json::from_value(serde_json::json!({
        "key": "dronbreen",
        "source": {"radargram_id": "dronbreen", "revision_id": revision},
        "coordinates": {
            "space": "index",
            "axes": {
                "x": serde_json::to_value(axes.x.as_ref().unwrap()).unwrap(),
                "y": serde_json::to_value(axes.y.as_ref().unwrap()).unwrap(),
            }
        },
        "features": [{
            "type": "Feature",
            "geometry": {
                "type": "LineString",
                "coordinates": points.iter().map(|(x, y)| vec![*x, *y]).collect::<Vec<_>>(),
            },
            "properties": {"id": "f-0001", "label": "bed"}
        }]
    }))
    .unwrap()
}

/// Where a sample of the uncorrected revision belongs on the corrected one,
/// from the correction's own depth function and the grid it logged.
fn true_sample(method: &str, twtt: f64, first_twtt: f64, time_zero: f64, resolution: f64) -> f64 {
    use crate::interp::separation::{legacy_depth, legacy_separation, slant_depth};
    let depth = |t: f64| match method {
        "slant" => slant_depth(
            t,
            0.168,
            6.2,
            crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS as f64,
        ),
        _ => legacy_depth(t, 0.168, legacy_separation(6.2, time_zero, 0.168)),
    };
    (depth(twtt) - depth(first_twtt)) / resolution
}

#[test]
#[serial_test::serial(netcdf)]
fn picks_carry_onto_a_revision_corrected_for_antenna_separation() {
    // #370: the corrected revision used to offer the same regular
    // recording clock as the uncorrected one, so the carry reported that
    // nothing had moved while slant-corrected picks landed five to seven
    // samples too shallow.
    let directory = tempfile::tempdir().unwrap();
    let plain = process(directory.path(), "plain", BASE);
    let plain_geometry = crate::interp::source::read_geometry(&plain).unwrap();
    let plain_declared = crate::interp::source::read_axis_declarations(&plain).unwrap();
    let plain_axes = anchors::axes_from_declarations(&plain_declared);
    let time_zero = plain_declared.twtt_time_zero[0];
    let samples = [10.0, 40.0, 60.0, 100.0, 200.0, 400.0, 650.0];
    let points: Vec<(f64, f64)> = samples.iter().map(|s| (1000.0, *s)).collect();

    for method in ["slant", "legacy"] {
        let corrected = process(
            directory.path(),
            &format!("corrected-{method}"),
            &format!("{BASE},correct_antenna_separation({method})"),
        );
        let corrected_geometry = crate::interp::source::read_geometry(&corrected).unwrap();
        let corrected_axes = anchors::axes_from_declarations(
            &crate::interp::source::read_axis_declarations(&corrected).unwrap(),
        );
        let resolution =
            crate::interp::separation::logged_resolution(&processing_log(&corrected)).unwrap();

        for (to, from_revision, to_axes, forward) in [
            (
                &corrected_geometry,
                &plain_geometry.revision_id,
                &corrected_axes,
                true,
            ),
            (
                &plain_geometry,
                &corrected_geometry.revision_id,
                &plain_axes,
                false,
            ),
        ] {
            let from_axes = if forward {
                &plain_axes
            } else {
                &corrected_axes
            };
            let drawn: Vec<(f64, f64)> = if forward {
                points.clone()
            } else {
                // Drawn on the corrected revision where the same reflector is.
                samples
                    .iter()
                    .map(|s| {
                        let twtt = plain_geometry.twtt[*s as usize];
                        (
                            1000.0,
                            true_sample(
                                method,
                                twtt,
                                plain_geometry.twtt[0],
                                time_zero,
                                resolution,
                            ),
                        )
                    })
                    .collect()
            };
            let carried = carry::carry(
                &document(from_revision, from_axes, &drawn),
                to_axes,
                &to.revision_id,
            );
            assert_eq!(
                carried.report.y_anchor.as_deref(),
                Some("recording_time"),
                "{method}"
            );
            let document = carried.document.expect("carried");
            let gprinterp::Geometry::LineString(coordinates) = &document.features[0].geometry
            else {
                panic!("{method}: not a line")
            };
            for (sample, position) in samples.iter().zip(coordinates) {
                let truth = if forward {
                    let twtt = plain_geometry.twtt[*sample as usize];
                    true_sample(method, twtt, plain_geometry.twtt[0], time_zero, resolution)
                } else {
                    *sample
                };
                assert!(
                    (position.0[1] - truth).abs() < 0.05,
                    "{method}, {}: sample {sample} carried to {}, belongs at {truth}",
                    if forward {
                        "onto the corrected"
                    } else {
                        "back to the plain"
                    },
                    position.0[1],
                );
            }
        }
    }
}

#[test]
#[serial_test::serial(netcdf)]
fn picks_saved_with_the_old_clock_are_set_aside_and_rescued_by_the_snapshot() {
    // A document saved on a corrected revision before #370 carries that
    // revision's recording clock as a regular axis. Carried through it,
    // the picks went several samples off with nothing said; set aside,
    // there is no shared anchor left, so the carry refuses -- until the
    // snapshot kept when the revision was superseded supplies the clock.
    let directory = tempfile::tempdir().unwrap();
    let plain = process(directory.path(), "plain", BASE);
    let corrected = process(
        directory.path(),
        "corrected",
        &format!("{BASE},correct_antenna_separation"),
    );
    let plain_geometry = crate::interp::source::read_geometry(&plain).unwrap();
    let plain_declared = crate::interp::source::read_axis_declarations(&plain).unwrap();
    let plain_axes = anchors::axes_from_declarations(&plain_declared);
    let corrected_geometry = crate::interp::source::read_geometry(&corrected).unwrap();
    let corrected_declared = crate::interp::source::read_axis_declarations(&corrected).unwrap();
    let time_zero = plain_declared.twtt_time_zero[0];
    let resolution =
        crate::interp::separation::logged_resolution(&processing_log(&corrected)).unwrap();

    // The axes as the picker saved them before: both regular.
    let current = anchors::axes_from_declarations(&corrected_declared);
    let travel_time = current.y.as_ref().unwrap().anchor[0].clone();
    assert_eq!(travel_time.name, "twtt_normal_incidence");
    let mut old_clock = travel_time.clone();
    old_clock.name = "recording_time".to_string();
    old_clock.t0 = Some(corrected_declared.twtt_crop[0]);
    let old_axes = anchors::Axes {
        x: current.x.clone(),
        y: Some(anchors::Axis {
            anchor: vec![travel_time, old_clock],
        }),
    };

    let samples = [40.0, 200.0, 650.0];
    let drawn: Vec<(f64, f64)> = samples
        .iter()
        .map(|s| {
            let twtt = plain_geometry.twtt[*s as usize];
            (
                1000.0,
                true_sample("slant", twtt, plain_geometry.twtt[0], time_zero, resolution),
            )
        })
        .collect();
    let document = document(&corrected_geometry.revision_id, &old_axes, &drawn);

    let alone = carry::carry(&document, &plain_axes, &plain_geometry.revision_id);
    assert_eq!(
        alone.report.severity,
        carry::Severity::Refused,
        "{:?}",
        alone.report
    );
    assert!(
        alone.report.headline.contains("antenna separation"),
        "{}",
        alone.report.headline
    );

    let values = anchors::snapshot_values(&corrected_declared).unwrap();
    assert!(values.y_alternate.as_ref().unwrap().tiepoints.is_some());
    let snapshot = crate::project::revisions::AxisSnapshot {
        radargram_id: "dronbreen".into(),
        revision_id: corrected_geometry.revision_id.clone(),
        y_anchor: values.y_anchor,
        y_values: values.y_values,
        x_values: values.x_values,
        y_alternate: values.y_alternate,
    };
    let rescued = carry::carry(
        &carry::with_snapshot_axes(&document, &snapshot),
        &plain_axes,
        &plain_geometry.revision_id,
    );
    assert_eq!(rescued.report.y_anchor.as_deref(), Some("recording_time"));
    let carried = rescued.document.unwrap();
    let gprinterp::Geometry::LineString(coordinates) = &carried.features[0].geometry else {
        panic!("not a line")
    };
    for (sample, position) in samples.iter().zip(coordinates) {
        assert!(
            (position.0[1] - sample).abs() < 0.05,
            "sample {sample} carried back to {}",
            position.0[1]
        );
    }
}
