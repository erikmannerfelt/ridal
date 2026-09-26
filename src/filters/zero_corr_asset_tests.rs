//! `zero_corr` on a real radargram whose time zero will not hold still.
//!
//! `scott_turnerbreen-20240207-DAT_0454_A1_2500-4000` is traces 2500-4000 of
//! a recording made with a damaged fibre-optic cable under tension, salvaged
//! rather than re-surveyed (Zenodo record 20734239; see
//! `assets/mala/README.md`). The direct wave jumps by up to ~8 samples from
//! one trace to the next, its first samples zig-zag as if one of the
//! interleaved sampling series ran early, and from about trace 450 of the cut
//! it drifts later by several samples.
//!
//! The first 500 traces are hard for every method: the direct wave has two
//! lobes of similar size, so `max_peak` flips between them, and the onset
//! methods are thrown by the zig-zag. The last 1000 are where the per-trace
//! correction plainly works. The bounds sit a little below what was measured
//! when this file was added (noted beside each), so they catch a regression
//! without pinning noise.

use std::path::PathBuf;

use crate::gpr::GPR;

const STEM: &str = "assets/mala/scott_turnerbreen-20240207-DAT_0454_A1_2500-4000";

fn load() -> GPR {
    let rad = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("{STEM}.rad"));
    let meta = crate::io::load_rad(&rad, 0.168, None, None).unwrap();
    let location = meta.find_cor(None).unwrap();
    GPR::from_meta_and_loc(location, meta).unwrap()
}

fn processed(step: &str) -> GPR {
    let mut gpr = load();
    gpr.process(step).unwrap();
    gpr
}

/// How well the worst-aligned traces in `traces` match their neighbours:
/// the 10th percentile, over those traces, of each trace's correlation with
/// the mean of its block of 250 traces, over the first `rows` samples. Each
/// trace's median is removed first, which is its DC level.
fn worst_alignment(gpr: &GPR, rows: usize, traces: std::ops::Range<usize>) -> f32 {
    let rows = rows.min(gpr.height());
    let centred: Vec<Vec<f32>> = gpr
        .data
        .columns()
        .into_iter()
        .map(|column| {
            let mut sorted: Vec<f32> = column.to_vec();
            sorted.sort_by(f32::total_cmp);
            let dc = sorted[sorted.len() / 2];
            column.iter().take(rows).map(|v| v - dc).collect()
        })
        .collect();
    let correlation = |a: &[f32], b: &[f32]| {
        let n = a.len() as f32;
        let (ma, mb) = (a.iter().sum::<f32>() / n, b.iter().sum::<f32>() / n);
        let cov: f32 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
        let va: f32 = a.iter().map(|x| (x - ma).powi(2)).sum();
        let vb: f32 = b.iter().map(|y| (y - mb).powi(2)).sum();
        cov / (va * vb).sqrt()
    };
    let mut scores: Vec<f32> = Vec::new();
    for block in (0..centred.len()).step_by(250) {
        let members = &centred[block..(block + 250).min(centred.len())];
        let stack: Vec<f32> = (0..rows)
            .map(|i| members.iter().map(|t| t[i]).sum::<f32>() / members.len() as f32)
            .collect();
        for (offset, trace) in members.iter().enumerate() {
            if traces.contains(&(block + offset)) {
                scores.push(correlation(trace, &stack));
            }
        }
    }
    scores.sort_by(f32::total_cmp);
    scores[scores.len() / 10]
}

#[test]
fn the_cut_is_the_jittery_recording_it_claims_to_be() {
    let raw = load();
    assert_eq!((raw.height(), raw.width()), (2024, 1500));
    // Uncorrected, a tenth of the traces anti-correlate with their block
    // (measured -0.27 in the first 500, 0.04 overall).
    assert!(worst_alignment(&raw, 160, 0..500) < 0.);
}

#[test]
fn a_per_trace_correction_aligns_what_a_global_one_cannot() {
    for (step, bound) in [
        // Measured 0.51, 0.49 and 0.46.
        ("zero_corr(coppens, trace)", 0.4),
        ("zero_corr(aic, trace)", 0.4),
        ("zero_corr(first_break, trace)", 0.35),
    ] {
        let gpr = processed(step);
        let score = worst_alignment(&gpr, 120, 0..1500);
        assert!(score > bound, "{step}: {score}");
    }
    // One shift for every trace cannot remove trace-to-trace jumps.
    let global = processed("zero_corr(coppens, global)");
    let score = worst_alignment(&global, 120, 0..1500);
    assert!(score < 0.2, "zero_corr(coppens, global): {score}");
}

#[test]
fn per_trace_picks_follow_the_jumps_rather_than_being_smoothed_away() {
    // The outlier test used to compare each pick with its neighbours alone,
    // and replaced a fifth of them here: the jumps being corrected. Now a
    // pick has to disagree with its own trace's direct wave too (measured:
    // 36 of 1500 replaced).
    let gpr = processed("zero_corr(coppens, trace)");
    let log = gpr.log.last().unwrap();
    let replaced: usize = log
        .split(" of 1500 picks")
        .next()
        .and_then(|s| s.rsplit(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no replacement count in {log}"));
    assert!(replaced < 75, "{log}");

    let step = gpr.vertical_resolution_ns();
    let zeros: std::collections::BTreeSet<i64> = gpr
        .twtt_time_zero_ns()
        .iter()
        .map(|t| (t / step).round() as i64)
        .collect();
    assert!(
        zeros.len() >= 8,
        "time zero should vary per trace: {zeros:?}"
    );
}

#[test]
fn max_peak_with_an_auto_margin_aligns_the_drift_and_keeps_the_wavelet() {
    let gpr = processed("zero_corr(max_peak, trace)");
    // Measured 0.85 over the last 1000 traces, against 0.60 for coppens.
    let score = worst_alignment(&gpr, 120, 500..1500);
    assert!(score > 0.75, "{score}");

    // The onset-to-peak distance was kept above time zero (measured 29
    // samples, 14.4 ns), so the record starts before time zero.
    let first = gpr.twtt_first_sample_ns();
    assert!((-20.0..-8.0).contains(&first), "{first}");
    assert!(gpr.depths()[0] < 0.);
    for (crop, time_zero) in gpr.twtt_crop_ns().iter().zip(gpr.twtt_time_zero_ns()) {
        assert!((time_zero - crop + first).abs() < 1e-3);
    }
}
