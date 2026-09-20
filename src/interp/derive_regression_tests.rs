//! The #210 regression test: reproduce a published scientific result through
//! the derived-layer evaluator.
//!
//! `dronbreen-20250327-DAT_0066_A1_1` is the one radargram whose raw Malå file
//! is small enough to commit, and whose published consensus can therefore be
//! recomputed from committed data with no network and no `old/`. The expected
//! values come from `misc/legacy_consensus_reference.py`, the verified
//! reproduction of the study's algorithm, applied to the published Zenodo
//! picks.
//!
//! The assertions are sharp -- medians of ~4e-6 m, i.e. float32 precision --
//! because the whole chain was measured to be bit-exact. A median near
//! 0.066 m means the whole-sample quantisation is missing; ~0.26 m means one
//! contributor is missing or the order statistic is off by one; tens of metres
//! means the pick `y` flip is wrong. None of those are reasons to loosen a
//! bound.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gprinterp::Document;

use crate::interp::derive::{self, GridPosition, Kind, Unit};
use crate::interp::level2::RadargramGeometry;
use crate::project::derived::{Audience, DerivedItem, DerivedSet, Scope};
use crate::project::layers::{ExclusivityGroup, Layer, LayerSet, Reducer};

/// The steps that regenerate the published grid bit-for-bit (PLAN §2.4). They
/// must be comma-separated: issue #130 makes space-separated steps silently
/// run only one.
const STEPS: &str = "remove_empty_traces,zero_corr,correct_antenna_separation,bandpass,dewow(15),gain(0.00234),siglog(0)";

const RADARGRAM_ID: &str = "dronbreen-20250327-dat_0066_a1_1";
const RADAR_KEY: &str = "dronbreen-20250327-DAT_0066_A1_1";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Process the committed Malå file into a tempdir and read the geometry.
fn processed_geometry(directory: &std::path::Path) -> RadargramGeometry {
    let output = directory.join("r.nc");
    let params = crate::gpr::RunParams {
        filepaths: vec![manifest_dir().join("assets/mala/dronbreen-20250327-DAT_0066_A1.rd3")],
        output_path: Some(output.clone()),
        dem_path: None,
        cor_path: None,
        medium_velocity: 0.168,
        crs: Some("EPSG:32633".to_string()),
        quiet: true,
        track_path: None,
        steps: crate::tools::parse_step_list(STEPS).expect("valid steps"),
        no_export: false,
        render_path: None,
        render_profile: None,
        render_width: None,
        override_antenna_mhz: None,
        override_antenna_separation: None,
        user_metadata: Default::default(),
        radargram_id: Some(RADARGRAM_ID.to_string()),
        display_name: None,
        group: None,
        group_id: None,
    };
    crate::gpr::run(params).expect("processing the committed Malå file");
    crate::interp::source::read_geometry(&output).expect("reading the processed geometry")
}

fn fixture_directory() -> PathBuf {
    manifest_dir().join(format!("assets/interp/{RADAR_KEY}"))
}

/// Load the committed `.gprinterp.json` fixtures, one per contributor.
fn load_fixtures() -> Vec<(String, Document)> {
    let mut fixtures = Vec::new();
    for entry in std::fs::read_dir(fixture_directory()).expect("fixture directory") {
        let path = entry.expect("fixture entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(user) = name.strip_suffix(".gprinterp.json") else {
            continue;
        };
        let text = std::fs::read_to_string(&path).expect("fixture readable");
        let document = Document::from_json(&text).expect("fixture parses as gprinterp");
        fixtures.push((user.to_string(), document));
    }
    fixtures.sort_by(|a, b| a.0.cmp(&b.0));
    fixtures
}

fn layer(id: &str) -> Layer {
    Layer {
        id: id.to_string(),
        name: id.to_string(),
        color: None,
        description: None,
        allow_overhangs: false,
        reducer: Some(Reducer::Shallowest),
        warn_on_duplicates: true,
        groups: Vec::new(),
        extra: Default::default(),
    }
}

/// The study's layer vocabulary and the #208 exclusivity groups.
fn layer_set() -> LayerSet {
    LayerSet {
        layers: vec![
            layer("bed"),
            layer("bed_no_temperate"),
            layer("temperate_ice"),
            layer("bed_not_visible"),
        ],
        // Only the bed-vs-cold-bed group. #208's *illustrative* second group
        // (bed_no_temperate and temperate_ice) must NOT be declared here:
        // `bed_cold` answers "where is the bed", while `temperate_ice` answers
        // "where is the CTS", so they are not contradictory answers to one
        // question, and the published algorithm counts a contributor's cold
        // bed even when they also drew a temperate line. Declaring them
        // exclusive NaNs EchoTeammate's cold bed at trace 1643 and shifts
        // `thickness` by two samples, which the 0.30 m bound catches. See
        // PROGRESS_LOG P7.
        groups: vec![ExclusivityGroup {
            id: "bed_and_cold_bed".to_string(),
            name: "Bed with and without temperate ice above".to_string(),
            members: vec!["bed".to_string(), "bed_no_temperate".to_string()],
            extra: Default::default(),
        }],
        ..LayerSet::default()
    }
}

fn item(id: &str, expression: &str) -> DerivedItem {
    DerivedItem {
        id: id.to_string(),
        name: id.to_string(),
        expression: expression.to_string(),
        unit: Unit::Meters,
        color: None,
        show: false,
        listed: true,
        fill_to: None,
        scope: Scope::Project,
        audience: Audience::OwnPicks,
        extra: Default::default(),
    }
}

/// The derived items of PLAN §P7 step 4. `>=` (not `>`) is the legacy
/// `bed_missing` tie rule: a tie keeps the position.
fn derived_set() -> DerivedSet {
    DerivedSet {
        items: vec![
            item(
                "thickness",
                "if count(bed) + count(bed_no_temperate) >= count(bed_not_visible) { \
                 percentile_lower(concatenate(bed, bed_no_temperate), 49.0) } else { NaN }",
            ),
            item(
                "cts_depth",
                "percentile_lower(concatenate(bed_no_temperate, temperate_ice), 49.0)",
            ),
            item(
                "thickness_user_count",
                "count(concatenate(bed, bed_no_temperate))",
            ),
            // The CTS depth with the study's gap rule: where nobody picked a
            // CTS but the bed is known, the CTS *is* the bed, i.e. no
            // temperate ice rather than no information.
            item(
                "cts_filled",
                "if is_nan(cts_depth) { thickness } else { cts_depth }",
            ),
            // Temperate-ice thickness above the bed, before the study's two
            // post-rules. Clipped because a CTS picked below the bed would
            // otherwise give a negative thickness.
            item(
                "temperate_raw",
                "clamp(thickness - cts_filled, 0.0, thickness)",
            ),
            // The published `temperate`, post-rules included.
            //
            // `no_temp` first, because it wins: if hardly anyone drew an
            // actual temperate-ice line, ambiguity in the "bed with no
            // temperate ice" class would otherwise read as real temperate
            // ice everywhere. `to_clamp` then says that a thin column which
            // is mostly temperate is temperate all through -- below about
            // one wavelength the CTS and the bed are not separable.
            item(
                "temperate",
                "if count(temperate_ice) == 0 \
                 || count(temperate_ice) \
                    / max(count(concatenate(bed_no_temperate, temperate_ice)), 1.0) < 0.25 { \
                     0.0 \
                 } else if temperate_raw / thickness > 0.5 && temperate_raw <= 17.0 { \
                     thickness \
                 } else { \
                     temperate_raw \
                 }",
            ),
            item(
                "thickness_user_lower",
                "percentile_lower(concatenate(bed, bed_no_temperate), 25.0)",
            ),
            item(
                "thickness_user_upper",
                "percentile_lower(concatenate(bed, bed_no_temperate), 75.0)",
            ),
            item(
                "thickness_user_std",
                "std(concatenate(bed, bed_no_temperate))",
            ),
            item(
                "thickness_user_nmad",
                "nmad(concatenate(bed, bed_no_temperate))",
            ),
        ],
        ..DerivedSet::default()
    }
}

#[derive(Debug, Default)]
struct ExpectedRow {
    thickness: f64,
    thickness_user_lower: f64,
    thickness_user_upper: f64,
    thickness_user_std: f64,
    thickness_user_nmad: f64,
    thickness_user_count: f64,
    temperate: f64,
}

/// Parse the committed expected CSV, indexed by trace.
fn expected_rows() -> BTreeMap<i64, ExpectedRow> {
    let text = std::fs::read_to_string(fixture_directory().join("expected_consensus.csv"))
        .expect("expected_consensus.csv");
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().expect("header").split(',').collect();
    let index = |name: &str| {
        header
            .iter()
            .position(|h| *h == name)
            .unwrap_or_else(|| panic!("missing column {name}"))
    };
    let parse = |value: &str| -> f64 {
        if value.is_empty() {
            f64::NAN
        } else {
            value.parse().expect("numeric csv value")
        }
    };

    let mut rows = BTreeMap::new();
    for line in lines {
        let fields: Vec<&str> = line.split(',').collect();
        let trace: f64 = parse(fields[0]);
        rows.insert(
            trace.round() as i64,
            ExpectedRow {
                thickness: parse(fields[index("thickness")]),
                thickness_user_lower: parse(fields[index("thickness_user_lower")]),
                thickness_user_upper: parse(fields[index("thickness_user_upper")]),
                thickness_user_std: parse(fields[index("thickness_user_std")]),
                thickness_user_nmad: parse(fields[index("thickness_user_nmad")]),
                thickness_user_count: parse(fields[index("thickness_user_count")]),
                temperate: parse(fields[index("temperate")]),
            },
        );
    }
    rows
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return f64::NAN;
    }
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

/// Assert median and max absolute error, printing the whole row.
fn assert_errors(label: &str, errors: &[f64], median_bound: f64, max_bound: f64) {
    let mut sorted = errors.to_vec();
    let med = median(&mut sorted);
    let max = errors.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    println!(
        "{label:<22} median|e|={med:.6}  max|e|={max:.6}  n={}",
        errors.len()
    );
    assert!(
        med <= median_bound,
        "{label}: median abs error {med:.6} exceeds {median_bound:.6}"
    );
    assert!(
        max <= max_bound,
        "{label}: max abs error {max:.6} exceeds {max_bound:.6}"
    );
}

#[test]
#[serial_test::serial(netcdf)]
fn the_derived_layers_reproduce_the_published_consensus() {
    let directory = tempfile::tempdir().expect("tempdir");
    let geometry = processed_geometry(directory.path());

    // Step 1: the grid must be the published one, or nothing after it means
    // anything.
    assert_eq!(geometry.n_traces(), 3548, "n_traces");
    assert_eq!(geometry.n_samples(), 700, "n_samples");
    let mut spacings: Vec<f64> = geometry
        .depth
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|d| *d > 0.0)
        .collect();
    let spacing = median(&mut spacings);
    assert!(
        (spacing - 0.255867).abs() < 1e-5,
        "depth spacing {spacing} is not the published 0.255867 m"
    );

    // Step 2: the committed fixtures.
    let fixtures = load_fixtures();
    assert_eq!(fixtures.len(), 10, "expected ten committed contributors");
    assert!(
        fixtures.iter().any(|(user, _)| user == "avalancheamigo"),
        "the recovered tenth contributor must be present"
    );

    // Step 3-4: layers and items, evaluated per trace.
    let layers = layer_set();
    let grid: Vec<GridPosition> = (0..geometry.n_traces())
        .map(|trace| GridPosition {
            trace: trace as f64,
            distance_m: derive::sample_to_unit(trace as f64, Unit::Meters, &geometry),
        })
        .collect();

    // Step 5: the legacy storage quantised pick y to whole samples; do the
    // same in the comparison, not in the fixtures.
    let reduced = derive::reduce_picks(&fixtures, &layers, &geometry, &grid, true);
    let set = derived_set();
    let results = set
        .evaluate(&reduced, &geometry)
        .expect("evaluating the derived items");

    assert_eq!(results["thickness"].kind, Kind::Position);

    // Step 6: does any contributor hold both `bed` and `bed_not_visible` at
    // one trace? Printed either way -- the answer defines the priority
    // reducer deferred in #208.
    let mut both_present = 0usize;
    for position in 0..reduced.n_positions() {
        let bed = reduced.values("bed", position).expect("bed");
        let missing = reduced
            .values("bed_not_visible", position)
            .expect("bed_not_visible");
        for (a, b) in bed.iter().zip(missing.iter()) {
            if a.is_finite() && b.is_finite() {
                both_present += 1;
            }
        }
    }
    println!("contributors holding both bed and bed_not_visible at one trace: {both_present}");
    assert_eq!(
        both_present, 0,
        "a contributor voted both bed and bed-not-visible"
    );

    // Step 5: join on trace.
    let expected = expected_rows();
    let mut joined = 0usize;
    let mut thickness = Vec::new();
    let mut lower = Vec::new();
    let mut upper = Vec::new();
    let mut std_dev = Vec::new();
    let mut nmad = Vec::new();
    let mut count_exact = 0usize;
    let mut temperate = Vec::new();

    for (trace, row) in &expected {
        let position = *trace as usize;
        let value = results["thickness"].values[position];
        if value.is_nan() != row.thickness.is_nan() {
            panic!(
                "trace {trace}: NaN mismatch (ours {value}, expected {})",
                row.thickness
            );
        }
        if value.is_nan() {
            continue;
        }
        joined += 1;
        thickness.push((value - row.thickness).abs());
        lower.push(
            (results["thickness_user_lower"].values[position] - row.thickness_user_lower).abs(),
        );
        upper.push(
            (results["thickness_user_upper"].values[position] - row.thickness_user_upper).abs(),
        );
        std_dev
            .push((results["thickness_user_std"].values[position] - row.thickness_user_std).abs());
        nmad.push(
            (results["thickness_user_nmad"].values[position] - row.thickness_user_nmad).abs(),
        );
        if results["thickness_user_count"].values[position].round() == row.thickness_user_count {
            count_exact += 1;
        }
        temperate.push((results["temperate"].values[position] - row.temperate).abs());
    }

    println!("rows joined: {joined}");
    assert_eq!(joined, 507, "all 507 published rows must join");

    assert_errors("thickness", &thickness, 1e-5, 0.30);
    assert_errors("thickness_user_lower", &lower, 1e-5, 0.30);
    assert_errors("thickness_user_upper", &upper, 1e-5, 0.60);
    assert_errors("thickness_user_std", &std_dev, 1e-5, 0.50);
    assert_errors("thickness_user_nmad", &nmad, 1e-5, 0.40);

    let exact_share = count_exact as f64 / joined as f64;
    println!(
        "thickness_user_count exact on {:.1}% of rows",
        exact_share * 100.0
    );
    assert!(
        exact_share >= 0.99,
        "count matched on only {:.1}% of rows",
        exact_share * 100.0
    );

    // `temperate` is now asserted rather than informational: the study's two
    // post-rules are expressed as derived items, so the comparison is
    // like-for-like. It reaches the published column through six chained
    // expressions -- cts_depth, cts_filled, temperate_raw and the rules --
    // which is the strongest end-to-end check the evaluator has.
    assert_errors("temperate", &temperate, 1e-5, 0.30);
}
