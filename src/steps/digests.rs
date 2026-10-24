//! The golden test that keeps step and core versions honest (#382).
//!
//! A revision id is derived from a recipe, and a recipe names each step by
//! its version and everything else by [`CORE_PROCESSING_VERSION`]. That
//! only means something if a change to what a step computes really does
//! come with a new version. This test is what makes it so: it processes a
//! real recording and compares a digest of **every** variable of the result
//! -- amplitudes, travel times, depths, positions, times, and the numeric
//! attributes -- against `digests.json`.
//!
//! - `core` is the recording read with no steps, guarded by
//!   [`CORE_PROCESSING_VERSION`]. #263 changed the travel-time-to-depth
//!   geometry outside any step; it would have tripped this.
//! - `base` is the window every step runs on, cut with `subset`, so it is
//!   guarded by both.
//! - Each entry of [`RUNS`] is one step run on that window, guarded by the
//!   step's [`Step::version`]. Only variables that differ from `base` are
//!   stored for a run; a variable the step removes is stored as `null`.
//!
//! When output changes and the version that guards it does not, the test
//! fails and says which. Bump the version, then regenerate:
//!
//! ```text
//! UPDATE_STEP_DIGESTS=1 cargo test --no-default-features -F cli --bin ridal step_digests
//! ```
//!
//! Regenerating refuses to record a change no version was bumped for.
//! `UPDATE_STEP_DIGESTS=force` records it anyway, for a change that is
//! numerically insignificant (such as a reordered sum), and nothing else.
//!
//! Digests are compared with a tolerance, so the three CI platforms, which
//! differ in thread counts and PROJ builds, agree.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::export::{ExportArray, ExportAttr};
use crate::gpr::GPR;
use crate::recipe::CORE_PROCESSING_VERSION;

use super::Step;

const TABLE: &str = "src/steps/digests.json";
const RECORDING: &str = "assets/mala/dronbreen-20220329-DAT_0237_A1.rad";
/// The window every step runs on: traces and samples, end exclusive. Big
/// enough for the direct wave, the bed of this 100 MHz line, and the
/// running windows the steps default to; small enough for a debug build.
const BASE_TRACES: (u32, u32) = (0, 400);
const BASE_SAMPLES: (u32, u32) = (0, 600);

/// What runs on the window. Every registered step appears at least once
/// (`every_step_has_a_digest`), and a step whose arguments choose between
/// algorithms appears once for each.
const RUNS: &[&str] = &[
    "subset(10 300 5 500)",
    "remove_traces(0 5-9)",
    "remove_empty_traces",
    "remove_standstills",
    "remove_standstills(gps_uncertainty=1)",
    "average_traces(3)",
    "zero_corr",
    "zero_corr(coppens, trace)",
    "zero_corr(coppens, smooth, window=101)",
    "zero_corr(max_peak, trace, peak)",
    "zero_corr(first_break, sigma=4)",
    "zero_corr(coppens, margin=5)",
    "zero_corr(legacy, factor=0.9)",
    "bandpass",
    "bandpass_mhz(50 200)",
    "equidistant_traces",
    "equidistant_traces(2.)",
    "shift_coordinates(3 -2 1)",
    "dewow",
    "dewow(method=mean)",
    "background_removal",
    "background_removal(101)",
    "background_removal(all, mean)",
    "remove_tones(500)",
    "balance_traces(500)",
    "auto_gain",
    "gain(0.002)",
    "kirchhoff_migration2d",
    "abslog",
    "siglog",
    "adaptive_siglog",
    "correct_topography",
    "correct_antenna_separation",
    "correct_antenna_separation(legacy)",
    "multiply(5)",
];

/// Relative tolerance, against each variable's own scale.
const TOLERANCE: f64 = 1e-5;
const ABSOLUTE: f64 = 1e-9;

/// A summary of one variable that moves when its values do.
///
/// Values are taken relative to the first finite one, so a coordinate
/// variable's spread is compared at the resolution of its spread rather
/// than of its distance from the origin; `first` itself is compared
/// separately. The two projections onto fixed pseudo-random weights are
/// what notice a shift: moving every value one sample along leaves sums
/// and extremes all but unchanged, but not these.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Digest {
    n: usize,
    non_finite: usize,
    first: f64,
    sum: f64,
    sum_abs: f64,
    sum_sq: f64,
    min: f64,
    max: f64,
    p1: f64,
    p2: f64,
}

/// splitmix64, scaled to [-0.5, 0.5): a fixed weight per position.
fn weight(index: u64, seed: u64) -> f64 {
    let mut z = index
        .wrapping_add(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64 - 0.5
}

fn digest(values: impl Iterator<Item = f64>) -> Digest {
    let values: Vec<f64> = values.collect();
    let first = values.iter().copied().find(|v| v.is_finite()).unwrap_or(0.);
    let mut d = Digest {
        n: values.len(),
        non_finite: 0,
        first,
        sum: 0.,
        sum_abs: 0.,
        sum_sq: 0.,
        min: f64::INFINITY,
        max: f64::NEG_INFINITY,
        p1: 0.,
        p2: 0.,
    };
    for (i, v) in values.iter().enumerate() {
        if !v.is_finite() {
            d.non_finite += 1;
            continue;
        }
        let c = v - first;
        d.sum += c;
        d.sum_abs += c.abs();
        d.sum_sq += c * c;
        d.min = d.min.min(c);
        d.max = d.max.max(c);
        d.p1 += c * weight(i as u64, 1);
        d.p2 += c * weight(i as u64, 2);
    }
    if d.min > d.max {
        // No finite values: keep the file free of infinities.
        d.min = 0.;
        d.max = 0.;
    }
    d
}

/// Whether two digests describe the same values, up to [`TOLERANCE`].
fn same(a: &Digest, b: &Digest) -> bool {
    let close = |x: f64, y: f64, scale: f64| (x - y).abs() <= TOLERANCE * scale + ABSOLUTE;
    let range = (a.max - a.min).max(b.max - b.min);
    let mass = a.sum_abs.max(b.sum_abs);
    a.n == b.n
        && a.non_finite == b.non_finite
        && (a.first - b.first).abs() <= 1e-12 * a.first.abs() + TOLERANCE * range + ABSOLUTE
        && close(a.sum, b.sum, mass)
        && close(a.sum_abs, b.sum_abs, mass)
        && close(a.sum_sq, b.sum_sq, a.sum_sq.max(b.sum_sq))
        && close(a.min, b.min, range)
        && close(a.max, b.max, range)
        && close(a.p1, b.p1, mass)
        && close(a.p2, b.p2, mass)
}

type Digests = BTreeMap<String, Digest>;

/// Every variable of what `gpr` would export, and its numeric attributes.
fn digests_of(gpr: &GPR) -> Digests {
    let dataset = gpr.export_dataset().expect("the golden radargram exports");
    let mut out = Digests::new();
    for (name, variable) in dataset.coords.iter().chain(dataset.data_vars.iter()) {
        let d = match &variable.data {
            ExportArray::F32Borrowed2D(a) => digest(a.iter().map(|v| *v as f64)),
            ExportArray::F32Owned1D(v) => digest(v.iter().map(|v| *v as f64)),
            ExportArray::F64Owned1D(v) => digest(v.iter().copied()),
            ExportArray::U32Owned1D(v) => digest(v.iter().map(|v| *v as f64)),
            ExportArray::U8Scalar(v) => digest(std::iter::once(*v as f64)),
            ExportArray::F64Scalar(v) => digest(std::iter::once(*v)),
        };
        out.insert(name.clone(), d);
    }
    for (name, attr) in &dataset.attrs {
        let value = match attr {
            ExportAttr::F64(v) => *v,
            ExportAttr::F32(v) => *v as f64,
            ExportAttr::I64(v) => *v as f64,
            ExportAttr::U8(v) => *v as f64,
            ExportAttr::String(_) | ExportAttr::Strings(_) => continue,
        };
        out.insert(format!("attr:{name}"), digest(std::iter::once(value)));
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StepEntry {
    version: u32,
    /// Per run, the variables that differ from `base`; `None` for one the
    /// step removed.
    runs: BTreeMap<String, BTreeMap<String, Option<Digest>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Table {
    core_version: u32,
    subset_version: u32,
    core: Digests,
    base: Digests,
    steps: BTreeMap<String, StepEntry>,
}

fn step_of(call: &str) -> (String, Step) {
    let parsed = super::parse_one(call).unwrap_or_else(|e| panic!("{call}: {e}"));
    let name = parsed
        .canonical
        .split('(')
        .next()
        .unwrap_or_default()
        .to_string();
    (name, parsed.step)
}

fn subset_version() -> u32 {
    step_of("subset(0 1)").1.version()
}

fn load() -> GPR {
    let params = crate::gpr::RunParams {
        filepaths: vec![RECORDING.into()],
        output_path: Some(std::env::temp_dir().join("ridal-golden.nc")),
        track_path: None,
        render_path: None,
        radargram_id: Some("golden".to_string()),
        display_name: None,
        options: crate::gpr::ProcessingOptions {
            quiet: true,
            no_export: true,
            ..Default::default()
        },
    };
    crate::gpr::build_processed_gpr(params)
        .expect("the golden recording loads")
        .0
}

/// The table as this build produces it.
fn compute() -> Table {
    let raw = load();
    let core = digests_of(&raw);
    // Cut afresh for every run rather than cloned: a GPR is deliberately
    // not `Clone`, and the cut is cheap.
    let window = || {
        raw.subset(
            Some(BASE_TRACES.0),
            Some(BASE_TRACES.1),
            Some(BASE_SAMPLES.0),
            Some(BASE_SAMPLES.1),
        )
        .expect("the window fits the recording")
    };
    let base = digests_of(&window());

    let mut steps = BTreeMap::<String, StepEntry>::new();
    for call in RUNS {
        let (name, step) = step_of(call);
        let mut gpr = window();
        gpr.process(call)
            .unwrap_or_else(|e| panic!("`{call}` failed on the golden window: {e}"));
        let after = digests_of(&gpr);
        let mut changed = BTreeMap::new();
        for (variable, d) in &after {
            if base.get(variable) != Some(d) {
                changed.insert(variable.clone(), Some(*d));
            }
        }
        for variable in base.keys() {
            if !after.contains_key(variable) {
                changed.insert(variable.clone(), None);
            }
        }
        steps
            .entry(name)
            .or_insert_with(|| StepEntry {
                version: step.version(),
                runs: BTreeMap::new(),
            })
            .runs
            .insert(call.to_string(), changed);
    }
    Table {
        core_version: CORE_PROCESSING_VERSION,
        subset_version: subset_version(),
        core,
        base,
        steps,
    }
}

/// The full digests of a run: `base` with the run's differences applied.
fn expand(base: &Digests, changes: &BTreeMap<String, Option<Digest>>) -> Digests {
    let mut out = base.clone();
    for (variable, d) in changes {
        match d {
            Some(d) => {
                out.insert(variable.clone(), *d);
            }
            None => {
                out.remove(variable);
            }
        }
    }
    out
}

/// The variables on which two sets of digests disagree.
fn differences(stored: &Digests, current: &Digests) -> Vec<String> {
    let mut out = Vec::new();
    for (variable, d) in current {
        match stored.get(variable) {
            Some(s) if same(s, d) => {}
            Some(_) => out.push(variable.clone()),
            None => out.push(format!("{variable} (new)")),
        }
    }
    for variable in stored.keys() {
        if !current.contains_key(variable) {
            out.push(format!("{variable} (gone)"));
        }
    }
    out
}

#[derive(Debug, Default)]
struct Verdict {
    /// Output changed and the version guarding it did not.
    unbumped: Vec<String>,
    /// The table is behind: a version moved, a run was added or removed.
    stale: Vec<String>,
}

fn compare(stored: &Table, current: &Table) -> Verdict {
    let mut verdict = Verdict::default();

    let core_bumped = stored.core_version != current.core_version;
    let core_changes = differences(&stored.core, &current.core);
    if core_bumped {
        verdict.stale.push(format!(
            "the core processing version is {} but the table was made with {}",
            current.core_version, stored.core_version
        ));
    } else if !core_changes.is_empty() {
        verdict.unbumped.push(format!(
            "reading the recording with no steps changed ({}), and \
             CORE_PROCESSING_VERSION is still {}",
            core_changes.join(", "),
            current.core_version
        ));
    }

    let base_bumped = core_bumped || stored.subset_version != current.subset_version;
    let base_changes = differences(&stored.base, &current.base);
    if stored.subset_version != current.subset_version {
        verdict.stale.push(format!(
            "`subset` is version {} but the table was made with {}",
            current.subset_version, stored.subset_version
        ));
    } else if !base_bumped && !base_changes.is_empty() && core_changes.is_empty() {
        verdict.unbumped.push(format!(
            "the window cut with `subset` changed ({}), and `subset` is still version {}",
            base_changes.join(", "),
            current.subset_version
        ));
    }
    // When what every step starts from has moved, every run moves with it,
    // and naming each step would only bury the cause.
    let start_moved = !core_changes.is_empty() || !base_changes.is_empty();

    for (name, entry) in &current.steps {
        let Some(old) = stored.steps.get(name) else {
            verdict
                .stale
                .push(format!("`{name}` has no digests in the table"));
            continue;
        };
        let step_bumped = old.version != entry.version;
        if step_bumped {
            verdict.stale.push(format!(
                "`{name}` is version {} but the table was made with {}",
                entry.version, old.version
            ));
        }
        for (call, changes) in &entry.runs {
            let Some(old_changes) = old.runs.get(call) else {
                verdict
                    .stale
                    .push(format!("`{call}` has no digests in the table"));
                continue;
            };
            if step_bumped || start_moved {
                continue;
            }
            let diff = differences(
                &expand(&stored.base, old_changes),
                &expand(&current.base, changes),
            );
            if !diff.is_empty() {
                verdict.unbumped.push(format!(
                    "`{call}` changed ({}), and `{name}` is still version {}",
                    diff.join(", "),
                    entry.version
                ));
            }
        }
        for call in old.runs.keys() {
            if !entry.runs.contains_key(call) {
                verdict
                    .stale
                    .push(format!("`{call}` is in the table but no longer run"));
            }
        }
    }
    for name in stored.steps.keys() {
        if !current.steps.contains_key(name) {
            verdict
                .stale
                .push(format!("`{name}` is in the table but no longer run"));
        }
    }
    verdict
}

/// The table as JSON, one digest per line so that a diff shows which
/// variable of which run moved.
fn render(table: &Table) -> String {
    let line = |d: &Option<Digest>| serde_json::to_string(d).expect("digests serialize");
    let block = |digests: &Digests, indent: &str| {
        digests
            .iter()
            .map(|(k, d)| format!("{indent}\"{k}\": {}", line(&Some(*d))))
            .collect::<Vec<_>>()
            .join(",\n")
    };
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"core_version\": {},\n", table.core_version));
    out.push_str(&format!(
        "  \"subset_version\": {},\n",
        table.subset_version
    ));
    out.push_str(&format!(
        "  \"core\": {{\n{}\n  }},\n",
        block(&table.core, "    ")
    ));
    out.push_str(&format!(
        "  \"base\": {{\n{}\n  }},\n",
        block(&table.base, "    ")
    ));
    out.push_str("  \"steps\": {\n");
    let steps: Vec<String> = table
        .steps
        .iter()
        .map(|(name, entry)| {
            let runs: Vec<String> = entry
                .runs
                .iter()
                .map(|(call, changes)| {
                    let lines: Vec<String> = changes
                        .iter()
                        .map(|(k, d)| format!("          \"{k}\": {}", line(d)))
                        .collect();
                    if lines.is_empty() {
                        format!("        \"{call}\": {{}}")
                    } else {
                        format!("        \"{call}\": {{\n{}\n        }}", lines.join(",\n"))
                    }
                })
                .collect();
            format!(
                "    \"{name}\": {{\n      \"version\": {},\n      \"runs\": {{\n{}\n      }}\n    }}",
                entry.version,
                runs.join(",\n")
            )
        })
        .collect();
    out.push_str(&steps.join(",\n"));
    out.push_str("\n  }\n}\n");
    out
}

#[test]
fn every_step_has_a_digest() {
    let run: std::collections::BTreeSet<String> = RUNS.iter().map(|call| step_of(call).0).collect();
    for (name, _) in super::descriptions() {
        assert!(
            run.contains(&name),
            "`{name}` is registered but not in steps::digests::RUNS. Add a call \
             that exercises it, then regenerate the table (see src/steps/digests.rs)."
        );
    }
}

/// `PRE_PROFILE_STEPS` is what the step help marks as allowed before a
/// profile (#423); `Step::selects_only` is what the server enforces. `RUNS`
/// has a call for every step, so this compares the two for all of them.
#[test]
fn pre_profile_steps_are_the_ones_that_only_select() {
    for call in RUNS {
        let (name, step) = step_of(call);
        assert_eq!(
            super::PRE_PROFILE_STEPS.contains(&name.as_str()),
            step.selects_only(),
            "`{call}`: steps::PRE_PROFILE_STEPS and Step::selects_only disagree"
        );
    }
}

#[test]
fn a_shift_moves_the_digest() {
    // The property the projections are for: the same values one place
    // along have the same sums, and must still not compare equal.
    let values: Vec<f64> = (0..10_000).map(|i| ((i as f64) * 0.37).sin()).collect();
    let shifted: Vec<f64> = values[1..]
        .iter()
        .copied()
        .chain(std::iter::once(0.))
        .collect();
    let a = digest(values.iter().copied());
    let b = digest(shifted.iter().copied());
    assert!(!same(&a, &b));
    // And noise far below the tolerance is not a change.
    let noisy = digest(values.iter().map(|v| v * (1. + 1e-9)));
    assert!(same(&a, &noisy));
}

#[test]
fn step_digests() {
    let current = compute();
    let path = Path::new(TABLE);
    let update = std::env::var("UPDATE_STEP_DIGESTS").ok();
    let stored: Option<Table> = std::fs::read_to_string(path)
        .ok()
        .map(|text| serde_json::from_str(&text).expect("digests.json parses"));

    let verdict = match &stored {
        Some(stored) => compare(stored, &current),
        None => Verdict {
            stale: vec![format!("{TABLE} does not exist")],
            ..Verdict::default()
        },
    };

    if let Some(mode) = update {
        assert!(
            std::env::var("CI").is_err(),
            "UPDATE_STEP_DIGESTS is for regenerating the table locally, not in CI"
        );
        assert!(
            verdict.unbumped.is_empty() || mode == "force",
            "Refusing to record output that changed without a version bump:\n- {}\n\
             Bump the version that guards it (Step::version or \
             recipe::CORE_PROCESSING_VERSION) and run this again. \
             UPDATE_STEP_DIGESTS=force records it anyway; use it only for a change \
             that is numerically insignificant.",
            verdict.unbumped.join("\n- ")
        );
        std::fs::write(path, render(&current)).unwrap();
        return;
    }

    let mut problems = Vec::new();
    if !verdict.unbumped.is_empty() {
        problems.push(format!(
            "Output changed without a version bump:\n- {}\n\
             If the change is intended, bump the version that guards it \
             (Step::version, or recipe::CORE_PROCESSING_VERSION for anything \
             outside the steps), so that files processed before and after it get \
             different revision ids.",
            verdict.unbumped.join("\n- ")
        ));
    }
    if !verdict.stale.is_empty() {
        problems.push(format!(
            "The table is out of date:\n- {}",
            verdict.stale.join("\n- ")
        ));
    }
    assert!(
        problems.is_empty(),
        "{}\n\nThen regenerate {TABLE} with\n  UPDATE_STEP_DIGESTS=1 cargo test \
         --no-default-features -F cli --bin ridal step_digests",
        problems.join("\n\n")
    );
}

/// A small table by hand: one core variable, one base variable, and one
/// step with one run that changes `data`.
fn toy_table(data_sum: f64) -> Table {
    let d = |sum: f64| digest([1., 2., 3. + sum].into_iter());
    let core: Digests = [("data".to_string(), d(0.))].into();
    let base = core.clone();
    let runs = [(
        "gain(0.002)".to_string(),
        [("data".to_string(), Some(d(data_sum)))].into(),
    )]
    .into();
    Table {
        core_version: 1,
        subset_version: 1,
        core,
        base,
        steps: [("gain".to_string(), StepEntry { version: 1, runs })].into(),
    }
}

#[test]
fn the_comparison_tells_an_unbumped_change_from_a_stale_table() {
    let stored = toy_table(1.);
    assert!(compare(&stored, &toy_table(1.)).unbumped.is_empty());
    assert!(compare(&stored, &toy_table(1.)).stale.is_empty());

    // Output moved, version did not: the one failure that matters.
    let moved = compare(&stored, &toy_table(2.));
    assert_eq!(moved.unbumped.len(), 1, "{moved:?}");
    assert!(moved.unbumped[0].contains("`gain(0.002)` changed (data)"));

    // Bumped: the table is behind, which regenerating fixes.
    let mut bumped = toy_table(2.);
    bumped.steps.get_mut("gain").unwrap().version = 2;
    let verdict = compare(&stored, &bumped);
    assert!(verdict.unbumped.is_empty(), "{verdict:?}");
    assert!(verdict.stale[0].contains("`gain` is version 2"));

    // The core moving is reported once, as the core, not once per step.
    let mut core_moved = toy_table(2.);
    core_moved.core = [("data".to_string(), digest([5., 6.].into_iter()))].into();
    let verdict = compare(&stored, &core_moved);
    assert_eq!(verdict.unbumped.len(), 1, "{verdict:?}");
    assert!(verdict.unbumped[0].contains("CORE_PROCESSING_VERSION"));

    // Runs and steps that come and go make the table stale.
    let mut extra = toy_table(1.);
    extra
        .steps
        .get_mut("gain")
        .unwrap()
        .runs
        .insert("gain(1)".to_string(), BTreeMap::new());
    extra.steps.insert(
        "abslog".to_string(),
        StepEntry {
            version: 1,
            runs: BTreeMap::new(),
        },
    );
    let verdict = compare(&stored, &extra);
    assert_eq!(verdict.stale.len(), 2, "{verdict:?}");
    let verdict = compare(&extra, &stored);
    assert_eq!(verdict.stale.len(), 2, "{verdict:?}");
}

#[test]
fn the_rendered_table_reads_back_as_itself() {
    let mut table = toy_table(1.);
    // A removed variable is stored as null.
    table
        .steps
        .get_mut("gain")
        .unwrap()
        .runs
        .get_mut("gain(0.002)")
        .unwrap()
        .insert("twtt".to_string(), None);
    let read: Table = serde_json::from_str(&render(&table)).unwrap();
    let verdict = compare(&read, &table);
    assert!(
        verdict.unbumped.is_empty() && verdict.stale.is_empty(),
        "{verdict:?}"
    );
    assert_eq!(read.steps["gain"].runs["gain(0.002)"]["twtt"], None);
}
