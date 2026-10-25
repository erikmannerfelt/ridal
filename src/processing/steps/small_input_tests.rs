//! No step panics on a radargram too small or too flat for it (#7).
//!
//! A step that cannot work on what an earlier step left should say so
//! with an error, or log a warning and leave the data as it was. Every
//! run of [`super::digests::RUNS`], and a few arguments at the edge of
//! what they accept, runs on windows of a real recording down to one
//! trace or one sample, and on silence. Each result is also exported,
//! since a radargram a step accepted must also be one that can be
//! written.

use crate::gpr::test_fixtures::subset_of;
use crate::gpr::GPR;

/// Arguments at the edge of what the steps accept, beside `RUNS`.
const EDGES: &[&str] = &[
    "remove_traces(0)",
    "remove_empty_traces(0)",
    "remove_standstills(0)",
    "remove_standstills(min_duration=1)",
    "average_traces(1)",
    "average_traces(1000000)",
    "zero_corr(aic)",
    "zero_corr(aic, trace)",
    "zero_corr(first_break, trace)",
    "zero_corr(coppens, smooth, window=1)",
    "zero_corr(coppens, margin=1000)",
    "bandpass(0 1)",
    "bandpass(0.5 0.5)",
    "bandpass_mhz(1 1000000)",
    "equidistant_traces(1000000)",
    "shift_coordinates(1000000)",
    "dewow(1)",
    "dewow(1000000)",
    "background_removal(1)",
    "background_removal(1000000)",
    "remove_tones(0)",
    "remove_tones(10 traces=1)",
    "balance_traces(0)",
    "balance_traces(100000)",
    "balance_traces(50 traces=1 reference=1)",
    "auto_gain(1)",
    "gain(0)",
    "siglog(-100)",
    "correct_antenna_separation(direct_velocity=0.0001)",
];

/// Recordings, and windows of each as (traces, samples) from its start.
const WINDOWS: &[(&str, &[(u32, u32)])] = &[
    (
        "assets/mala/dronbreen-20220329-DAT_0237_A1.rad",
        &[(1, 600), (2, 600), (3, 600), (400, 1), (400, 2), (3, 3)],
    ),
    (
        "assets/mala/dronbreen-20250327-DAT_0066_A1.rad",
        &[(2, 736), (3, 3), (400, 1)],
    ),
];

fn calls() -> impl Iterator<Item = &'static str> {
    super::digests::RUNS.iter().chain(EDGES).copied()
}

/// What went wrong running `call` on `gpr`, if it panicked. An error is
/// an answer, not a failure.
fn panic_of(mut gpr: GPR, call: &str) -> Option<String> {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if gpr.process(call).is_ok() {
            let _ = gpr.export_dataset();
        }
    }));
    outcome.err().map(|payload| {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default()
    })
}

#[test]
fn no_step_panics_on_a_small_or_silent_radargram() {
    let mut panics = Vec::new();
    for &(recording, windows) in WINDOWS {
        let raw = super::digests::load_recording(recording);
        for &(traces, samples) in windows {
            for call in calls() {
                let window = subset_of(&raw, Some(0), Some(traces), Some(0), Some(samples))
                    .expect("the window fits the recording");
                if let Some(message) = panic_of(window, call) {
                    panics.push(format!(
                        "{recording} {traces} x {samples}, `{call}`: {message}"
                    ));
                }
            }
        }
    }
    let raw = super::digests::load_recording(WINDOWS[0].0);
    for call in calls() {
        let mut silent = subset_of(&raw, Some(0), Some(400), Some(0), Some(600)).unwrap();
        silent.data.fill(0.);
        if let Some(message) = panic_of(silent, call) {
            panics.push(format!("silence, `{call}`: {message}"));
        }
    }
    assert!(panics.is_empty(), "{}", panics.join("\n"));
}

#[test]
fn a_step_that_leaves_no_traces_or_samples_fails() {
    let raw = super::digests::load_recording(WINDOWS[0].0);
    for call in [
        "subset(min_trace=50)",
        "subset(0 -1 100)",
        "remove_traces(0-49)",
        "remove_empty_traces(1e30)",
    ] {
        let mut gpr = subset_of(&raw, Some(0), Some(50), Some(0), Some(100)).unwrap();
        let error = gpr
            .process(call)
            .expect_err(&format!("`{call}` left nothing, so it should fail"))
            .to_string();
        assert!(
            error.contains("no traces")
                || error.contains("no samples")
                || error.contains("no data"),
            "{call}: {error}"
        );
    }
}

#[test]
fn an_empty_subset_written_out_is_refused_when_parsed() {
    for call in ["subset(10 10)", "subset(20 10)", "subset(0 -1 5 5)"] {
        let error = super::parse_one(call).expect_err(call);
        assert!(error.contains("would be kept"), "{call}: {error}");
    }
}

#[test]
fn equidistant_traces_makes_at_most_four_times_as_many_traces() {
    let raw = super::digests::load_recording(WINDOWS[0].0);
    let window = || subset_of(&raw, Some(0), Some(400), Some(0), Some(50)).unwrap();
    let length = window().location.distances().last().copied().unwrap() as f32;
    let limit = super::traces::MAX_TRACE_FACTOR * 400;

    // Just inside the limit, then just outside it.
    let mut gpr = window();
    gpr.process(&format!(
        "equidistant_traces({})",
        length / (limit - 2) as f32
    ))
    .unwrap();
    assert!(gpr.width() <= limit, "{}", gpr.width());

    let mut gpr = window();
    let error = gpr
        .process(&format!(
            "equidistant_traces({})",
            length / (limit + 2) as f32
        ))
        .unwrap_err()
        .to_string();
    assert!(error.contains("more than 4 times as many"), "{error}");
    assert_eq!(gpr.width(), 400, "a refused step changes nothing");

    for call in ["equidistant_traces(0)", "equidistant_traces(-1)"] {
        assert!(super::parse_one(call).is_err(), "{call}");
    }
}

#[test]
fn a_recording_with_no_traces_is_refused_when_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let header = dir.path().join("empty.rad");
    std::fs::copy("assets/mala/dronbreen-20250327-DAT_0066_A1.rad", &header).unwrap();
    std::fs::write(dir.path().join("empty.rd3"), b"").unwrap();
    std::fs::copy(
        "assets/mala/dronbreen-20250327-DAT_0066_A1.cor",
        dir.path().join("empty.cor"),
    )
    .unwrap();
    let params = crate::processing::RunParams {
        filepaths: vec![header],
        output_path: Some(dir.path().join("empty.nc")),
        track_path: None,
        render_path: None,
        radargram_id: Some("empty".to_string()),
        display_name: None,
        options: crate::processing::ProcessingOptions {
            quiet: true,
            no_export: true,
            ..Default::default()
        },
    };
    let error = crate::processing::build_processed_gpr(params)
        .map(|_| ())
        .expect_err("an empty recording loads")
        .to_string();
    assert!(error.contains("holds no traces"), "{error}");
}
