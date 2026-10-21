//! Picks and derived layers in figures (#142): drawn where they belong, and
//! read from a project as the viewer reads them.

use super::overlays::{DerivedLine, FillBand, LayerAccess, Overlays, PickLine, Readable};
use super::spec::{DerivedOverlay, FigureSpec, PickOverlay, PickSelector, Size, SizeUnit, View};
use super::*;
use crate::render::profile::{AmplitudeLimits, ImageFormat};
use crate::source::ArraySource;

fn spec() -> FigureSpec {
    FigureSpec {
        size: Size {
            width: 400.0,
            unit: SizeUnit::Px,
            dpi: 72.0,
        },
        axes: false,
        ..FigureSpec::default()
    }
}

fn wants_picks(spec: FigureSpec) -> FigureSpec {
    FigureSpec {
        picks: PickOverlay {
            items: vec![PickSelector::default()],
            width: 4.0,
            opacity: 1.0,
        },
        ..spec
    }
}

/// A flat line across a 100 x 50 radargram at sample 25.
fn one_pick() -> Overlays {
    Overlays {
        picks: vec![PickLine {
            user: "a".into(),
            layer: Some("bed".into()),
            colour: "#ff0000".into(),
            vertices: vec![[0.0, 25.0], [100.0, 25.0]],
        }],
        ..Overlays::default()
    }
}

fn render(
    spec: &FigureSpec,
    overlays: Option<&Overlays>,
    coordinates: &Coordinates,
    format: FigureFormat,
) -> Result<RenderedFigure, FigureError> {
    let data = ndarray::Array2::from_elem((50, 100), 0.0_f32);
    let source = ArraySource::new(data.view());
    render_figure(
        &FigureSource {
            source: &source,
            coordinates,
            revision_id: None,
            elevation_range: topo::ElevationRange::NONE,
            overlays,
            contrast: None,
            interrupt: crate::render::interrupt::Interrupt::never(),
        },
        spec,
        &RenderProfile {
            limits: AmplitudeLimits::Explicit {
                min: -1.0,
                max: 1.0,
            },
            format: ImageFormat::Png,
            ..RenderProfile::default_profile()
        },
        format,
    )
}

fn rgba(figure: &RenderedFigure) -> image::RgbaImage {
    image::load_from_memory(&figure.bytes).unwrap().to_rgba8()
}

fn is_red(p: &image::Rgba<u8>) -> bool {
    p.0[0] > 200 && p.0[1] < 60 && p.0[2] < 60
}

#[test]
fn a_pick_is_drawn_at_its_sample() {
    // 100 traces at 400 px: 4 px a trace and a sample, so sample 25 is at
    // y = 100.
    let figure = render(
        &wants_picks(spec()),
        Some(&one_pick()),
        &Coordinates::default(),
        FigureFormat::Png,
    )
    .unwrap();
    let image = rgba(&figure);
    assert_eq!((image.width(), image.height()), (400, 200));
    assert!(is_red(image.get_pixel(200, 100)));
    assert!(!is_red(image.get_pixel(200, 90)));
    assert!(!is_red(image.get_pixel(200, 110)));
}

#[test]
fn an_svg_draws_picks_and_derived_lines_in_their_colours() {
    let mut overlays = one_pick();
    overlays.derived.push(DerivedLine {
        id: "mid".into(),
        colour: "#00ff00".into(),
        samples: vec![10.0; 100],
    });
    let spec = FigureSpec {
        axes: true,
        // No background rectangle, so the first `<rect>` is the frame.
        style: spec::Style {
            background: None,
            ..spec::Style::default()
        },
        derived: DerivedOverlay {
            items: vec!["mid".into()],
            ..DerivedOverlay::default()
        },
        ..wants_picks(spec())
    };
    let figure = render(
        &spec,
        Some(&overlays),
        &Coordinates::default(),
        FigureFormat::Svg,
    )
    .unwrap();
    let svg = String::from_utf8(figure.bytes).unwrap();
    assert!(svg.contains(r##"stroke="#ff0000""##));
    assert!(svg.contains(r##"stroke="#00ff00""##));
    // Over the radargram, picks over derived lines (a consensus of one
    // contributor would otherwise hide their pick), and under the frame.
    let image = svg.find("<image").unwrap();
    let derived = svg.find(r##"stroke="#00ff00""##).unwrap();
    let pick = svg.find(r##"stroke="#ff0000""##).unwrap();
    let frame = svg.find("<rect x=").unwrap();
    assert!(image < derived && derived < pick && pick < frame);
}

#[test]
fn a_figure_without_the_radargram_draws_only_its_overlays() {
    let spec = FigureSpec {
        radargram: false,
        ..wants_picks(spec())
    };
    let figure = render(
        &spec,
        Some(&one_pick()),
        &Coordinates::default(),
        FigureFormat::Png,
    )
    .unwrap();
    let image = rgba(&figure);
    assert!(is_red(image.get_pixel(200, 100)));
    // No radargram and, without axes, no background: transparent.
    assert_eq!(image.get_pixel(200, 20).0[3], 0);

    let with_axes = FigureSpec { axes: true, ..spec };
    let svg = render(
        &with_axes,
        Some(&one_pick()),
        &Coordinates::default(),
        FigureFormat::Svg,
    )
    .unwrap();
    let svg = String::from_utf8(svg.bytes).unwrap();
    assert!(!svg.contains("<image"));
    assert!(svg.contains(r##"stroke="#ff0000""##));
}

#[test]
fn overlays_are_clipped_to_the_extent() {
    // Traces 50 to 100 only: the pick's left half is outside and the line
    // starts at the plot's edge, not past it.
    let spec = FigureSpec {
        extent: spec::Extent {
            traces: Some([50.0, 100.0]),
            ..spec::Extent::default()
        },
        axes: true,
        style: spec::Style {
            background: None,
            ..spec::Style::default()
        },
        ..wants_picks(spec())
    };
    let figure = render(
        &spec,
        Some(&one_pick()),
        &Coordinates::default(),
        FigureFormat::Svg,
    )
    .unwrap();
    let svg = String::from_utf8(figure.bytes).unwrap();
    let path = svg
        .lines()
        .find(|l| l.contains(r##"stroke="#ff0000""##))
        .unwrap();
    let first_x: f64 = path
        .split("d=\"M")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let plot_x = svg
        .split("<rect x=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .parse::<f64>()
        .unwrap();
    assert!((first_x - plot_x).abs() < 1e-6, "{first_x} vs {plot_x}");
}

#[test]
fn the_topographic_view_moves_overlays_with_the_shear() {
    // The surface drops 5 m over the profile at 0.5 m a sample, so the last
    // trace is shifted down 10 samples and a flat pick slopes down by 10
    // rows across the figure.
    let coordinates = Coordinates {
        elevation: Some((0..100).map(|i| 100.0 - i as f64 * 5.0 / 99.0).collect()),
        depth: Some((0..50).map(|i| i as f64 * 0.5).collect()),
        ..Coordinates::default()
    };
    let spec = FigureSpec {
        view: View::Topo,
        ..wants_picks(spec())
    };
    let figure = render(&spec, Some(&one_pick()), &coordinates, FigureFormat::Png).unwrap();
    let image = rgba(&figure);
    // 60 rows at 4 px a row; sample 25 at row 25 on the left and row 35 at
    // the right.
    assert_eq!(image.height(), 240);
    assert!(is_red(image.get_pixel(2, 100)));
    assert!(is_red(image.get_pixel(397, 140)));
    assert!(!is_red(image.get_pixel(397, 100)));
}

#[test]
fn fills_are_drawn_under_the_lines() {
    let overlays = Overlays {
        fills: vec![FillBand {
            colour: "#0000ff".into(),
            opacity: 1.0,
            a: vec![10.0; 100],
            b: vec![20.0; 100],
        }],
        ..one_pick()
    };
    let figure = render(
        &wants_picks(spec()),
        Some(&overlays),
        &Coordinates::default(),
        FigureFormat::Png,
    )
    .unwrap();
    let image = rgba(&figure);
    // Between samples 10 and 20, so y 40 to 80.
    let p = image.get_pixel(200, 60).0;
    assert!(p[2] > 200 && p[0] < 60, "{p:?}");
    assert!(is_red(image.get_pixel(200, 100)));
}

#[test]
fn picks_without_a_project_are_refused() {
    let err = render(
        &wants_picks(spec()),
        None,
        &Coordinates::default(),
        FigureFormat::Png,
    )
    .unwrap_err();
    assert!(err.to_string().contains("project"), "{err}");
}

#[test]
fn an_empty_figure_is_refused() {
    let spec = FigureSpec {
        radargram: false,
        ..spec()
    };
    assert!(spec.validate().is_err());
}

// --- Read from a project -------------------------------------------------

const RADARGRAM: &str = "line-01";

/// A processed radargram, 40 traces by 8 samples, with depth, travel time
/// and distance, as the server's derived-route tests write one.
fn write_nc(path: &Path) {
    let mut file = netcdf::create(path).unwrap();
    file.add_dimension("y", 8).unwrap();
    file.add_dimension("x", 40).unwrap();
    let mut data = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
    let values: Vec<f32> = (0..(8 * 40)).map(|i| (i % 97) as f32).collect();
    data.put_values(&values, ..).unwrap();
    let mut twtt = file.add_variable::<f32>("twtt", &["y"]).unwrap();
    twtt.put_values(&(0..8).map(|i| i as f32 * 0.4).collect::<Vec<_>>(), ..)
        .unwrap();
    twtt.put_attribute("anchor_name", "twtt_normal_incidence")
        .unwrap();
    let mut depth = file.add_variable::<f32>("depth", &["y"]).unwrap();
    depth
        .put_values(&(0..8).map(|i| i as f32 * 0.02).collect::<Vec<_>>(), ..)
        .unwrap();
    for (name, values) in [
        ("distance", (0..40).map(|i| i as f64).collect::<Vec<_>>()),
        ("easting", (0..40).map(|i| 400_000.0 + i as f64).collect()),
        ("northing", (0..40).map(|_| 8_700_000.0).collect()),
        (
            "longitude",
            (0..40).map(|i| 15.0 + i as f64 * 1e-5).collect(),
        ),
        ("latitude", (0..40).map(|_| 78.0).collect()),
    ] {
        let mut var = file.add_variable::<f64>(name, &["x"]).unwrap();
        var.put_values(&values, ..).unwrap();
    }
    file.add_attribute("ridal_radargram_id", RADARGRAM).unwrap();
    file.add_attribute("ridal_version", "ridal version 0.0.0 by test")
        .unwrap();
    file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
        .unwrap();
    file.add_attribute("crs", "EPSG:32633").unwrap();
}

/// A project with the radargram, a red `bed` layer, a flat `bed` pick per
/// user at the given sample, and two derived layers: `mid`, the median, and
/// `top`, the shallowest, filled to `mid`. Returns the project's directory
/// and the radargram's path.
fn project_with_picks(picks: &[(&str, f64)]) -> (tempfile::TempDir, std::path::PathBuf) {
    project_with(picks, true)
}

/// [`project_with_picks`], with the picks naming the revision they were drawn
/// on or not.
fn project_with(picks: &[(&str, f64)], anchored: bool) -> (tempfile::TempDir, std::path::PathBuf) {
    use crate::project::store::Expectation;
    let dir = tempfile::tempdir().unwrap();
    let project = crate::project::Project::init(dir.path(), None).unwrap();
    let nc = project
        .data_dir()
        .join(crate::project::DEFAULT_RADARGRAM_DIR)
        .join("line-01.nc");
    std::fs::create_dir_all(nc.parent().unwrap()).unwrap();
    write_nc(&nc);
    let revision = match crate::io::inspect_ridal_netcdf(&nc).unwrap() {
        crate::io::RidalNetcdfKind::Supported(m) => m.revision_id().to_string(),
        other => panic!("{other:?}"),
    };

    let layers: crate::project::layers::LayerSet = serde_json::from_value(serde_json::json!({
        "layers": [{"id": "bed", "name": "Bed", "color": "#ff0000"}]
    }))
    .unwrap();
    crate::project::layers::write(project.documents(), &layers, &Expectation::Any).unwrap();

    let derived: crate::project::derived::DerivedSet = serde_json::from_value(serde_json::json!({
        "items": [
            {"id": "mid", "name": "Mid", "expression": "median(bed)", "unit": "samples",
             "color": "#00ff00"},
            {"id": "top", "name": "Top", "expression": "min(bed)", "unit": "samples",
             "color": "#0000ff", "fill_to": {"target": "mid", "opacity": 0.5}},
            {"id": "n", "name": "Count", "expression": "count(bed)", "unit": "dimensionless"}
        ]
    }))
    .unwrap();
    crate::project::derived::write(project.documents(), &derived, &Expectation::Any).unwrap();

    let radargram = crate::identity::RadargramId::new(RADARGRAM).unwrap();
    for (user, sample) in picks {
        let document: gprinterp::Document = serde_json::from_value(serde_json::json!({
            "key": RADARGRAM,
            "source": if anchored {
                serde_json::json!({"id": RADARGRAM, "revision_id": revision})
            } else {
                serde_json::json!({"id": RADARGRAM})
            },
            "features": [{
                "type": "Feature",
                "geometry": {"type": "LineString", "coordinates": [[0.0, sample], [39.0, sample]]},
                "properties": {"id": "f-0", "label": "bed"}
            }]
        }))
        .unwrap();
        crate::project::interpretations::write(
            project.documents(),
            &radargram,
            &crate::identity::UserId::new(*user).unwrap(),
            &document,
            &Expectation::Absent,
        )
        .unwrap();
    }
    (dir, nc)
}

fn gather(
    dir: &Path,
    nc: &Path,
    spec: &FigureSpec,
    access: &LayerAccess,
) -> Result<Overlays, FigureError> {
    let project = crate::project::Project::open(dir).unwrap();
    let revision = match crate::io::inspect_ridal_netcdf(nc).unwrap() {
        crate::io::RidalNetcdfKind::Supported(m) => m.revision_id(),
        other => panic!("{other:?}"),
    };
    overlays::gather(
        &project,
        &crate::identity::RadargramId::new(RADARGRAM).unwrap(),
        nc,
        &revision,
        spec,
        access,
    )
}

fn layers_spec(items: &[&str]) -> FigureSpec {
    FigureSpec {
        picks: PickOverlay {
            items: vec![PickSelector {
                user: None,
                layer: Some("bed".into()),
            }],
            ..PickOverlay::default()
        },
        derived: DerivedOverlay {
            items: items.iter().map(|s| s.to_string()).collect(),
            ..DerivedOverlay::default()
        },
        ..FigureSpec::default()
    }
}

#[test]
#[serial_test::serial(netcdf)]
fn picks_and_derived_layers_are_read_as_the_viewer_reads_them() {
    let (dir, nc) = project_with_picks(&[("ann", 2.0), ("bob", 4.0), ("cat", 6.0)]);
    let overlays = gather(
        dir.path(),
        &nc,
        &layers_spec(&["mid", "top"]),
        &LayerAccess::everything(),
    )
    .unwrap();
    assert!(overlays.warnings.is_empty(), "{:?}", overlays.warnings);

    let mut users: Vec<&str> = overlays.picks.iter().map(|p| p.user.as_str()).collect();
    users.sort();
    assert_eq!(users, ["ann", "bob", "cat"]);
    assert!(overlays.picks.iter().all(|p| p.colour == "#ff0000"));

    let mid = overlays.derived.iter().find(|d| d.id == "mid").unwrap();
    assert_eq!(mid.colour, "#00ff00");
    assert_eq!(mid.samples.len(), 40);
    assert!(mid.samples.iter().all(|&s| (s - 4.0).abs() < 1e-9));
    let top = overlays.derived.iter().find(|d| d.id == "top").unwrap();
    assert!(top.samples.iter().all(|&s| (s - 2.0).abs() < 1e-9));

    // `top` fills to `mid`, both drawn.
    assert_eq!(overlays.fills.len(), 1);
    assert_eq!(overlays.fills[0].colour, "#0000ff");
    assert_eq!(overlays.fills[0].opacity, 0.5);

    // Without `mid`, there is nothing to fill to.
    let alone = gather(
        dir.path(),
        &nc,
        &layers_spec(&["top"]),
        &LayerAccess::everything(),
    )
    .unwrap();
    assert!(alone.fills.is_empty());
}

#[test]
#[serial_test::serial(netcdf)]
fn access_limits_whose_picks_are_drawn_and_evaluated() {
    let (dir, nc) = project_with_picks(&[("ann", 2.0), ("bob", 4.0), ("cat", 6.0)]);
    let bob = || Readable::Only(Some(crate::identity::UserId::new("bob").unwrap()));
    let access = LayerAccess {
        picks: bob(),
        own_picks_items: bob(),
        released_items: Readable::All,
        viewer: Some("bob".into()),
    };
    let overlays = gather(dir.path(), &nc, &layers_spec(&["top"]), &access).unwrap();
    assert_eq!(overlays.picks.len(), 1);
    assert_eq!(overlays.picks[0].user, "bob");
    // `top` is evaluated over bob's picks alone: the shallowest is his.
    assert!(overlays.derived[0]
        .samples
        .iter()
        .all(|&s| (s - 4.0).abs() < 1e-9));
}

#[test]
#[serial_test::serial(netcdf)]
fn a_derived_layer_that_cannot_be_drawn_is_an_error() {
    let (dir, nc) = project_with_picks(&[("ann", 2.0)]);
    let everything = LayerAccess::everything();
    let err = gather(dir.path(), &nc, &layers_spec(&["nope"]), &everything).unwrap_err();
    assert!(err.to_string().contains("'nope'"), "{err}");
    let err = gather(dir.path(), &nc, &layers_spec(&["n"]), &everything).unwrap_err();
    assert!(err.to_string().contains("attribute"), "{err}");
}

#[test]
#[serial_test::serial(netcdf)]
fn a_figure_with_picks_renders_from_a_project_directory() {
    let (dir, nc) = project_with_picks(&[("ann", 2.0), ("bob", 4.0)]);
    let out = dir.path().join("figure.svg");
    let spec = FigureSpec {
        size: Size {
            width: 400.0,
            unit: SizeUnit::Px,
            dpi: 72.0,
        },
        ..layers_spec(&["mid"])
    };
    let written = render_figure_path(&nc, &out, &spec, Some(dir.path())).unwrap();
    assert!(written.warnings.is_empty());
    assert!(written.sidecar.is_none());
    let svg = std::fs::read_to_string(&out).unwrap();
    assert!(svg.contains(r##"stroke="#ff0000""##));
    assert!(svg.contains(r##"stroke="#00ff00""##));

    // Without the project, the same spec is refused rather than drawn bare.
    let err = render_figure_path(&nc, &out, &spec, None).unwrap_err();
    assert!(err.contains("project"), "{err}");

    // And a figure without axes writes its sidecar.
    let raw = FigureSpec {
        axes: false,
        ..spec
    };
    let png = dir.path().join("raw.png");
    let written = render_figure_path(&nc, &png, &raw, Some(dir.path())).unwrap();
    assert_eq!(written.sidecar, Some(dir.path().join("raw.json")));
    assert!(dir.path().join("raw.json").is_file());
}

#[test]
#[serial_test::serial(netcdf)]
fn picks_that_name_no_revision_are_drawn_as_stored_with_a_warning() {
    // Imported picks (the legacy study's, for one) say nothing about the
    // revision they were drawn on, so there is nothing to carry them from.
    let (dir, nc) = project_with(&[("ann", 2.0)], false);
    let overlays = gather(
        dir.path(),
        &nc,
        &layers_spec(&[]),
        &LayerAccess::everything(),
    )
    .unwrap();
    assert_eq!(overlays.picks.len(), 1);
    assert_eq!(overlays.picks[0].vertices, vec![[0.0, 2.0], [39.0, 2.0]]);
    assert_eq!(overlays.warnings.len(), 1);
    assert!(
        overlays.warnings[0].contains("drawn as stored"),
        "{:?}",
        overlays.warnings
    );
}

#[test]
#[serial_test::serial(netcdf)]
fn picks_from_a_revision_they_cannot_be_carried_from_are_drawn_as_stored_with_a_warning() {
    let (dir, nc) = project_with_picks(&[("ann", 2.0)]);
    // Point the stored picks at a revision the project knows nothing about.
    let radargram = crate::identity::RadargramId::new(RADARGRAM).unwrap();
    let user = crate::identity::UserId::new("ann").unwrap();
    let project = crate::project::Project::open(dir.path()).unwrap();
    let mut document =
        crate::project::interpretations::read(project.documents(), &radargram, &user)
            .unwrap()
            .unwrap()
            .document;
    document.source.as_mut().unwrap().revision_id = Some("someone-elses-revision".into());
    crate::project::interpretations::write(
        project.documents(),
        &radargram,
        &user,
        &document,
        &crate::project::store::Expectation::Any,
    )
    .unwrap();

    let overlays = gather(
        dir.path(),
        &nc,
        &layers_spec(&[]),
        &LayerAccess::everything(),
    )
    .unwrap();
    // As the viewer's contributor overlay shows them, which is where the
    // legacy study's picks are seen.
    assert_eq!(overlays.picks.len(), 1);
    assert_eq!(overlays.picks[0].vertices, vec![[0.0, 2.0], [39.0, 2.0]]);
    assert_eq!(overlays.warnings.len(), 1);
    assert!(
        overlays.warnings[0].contains("drawn as stored"),
        "{:?}",
        overlays.warnings
    );
}
