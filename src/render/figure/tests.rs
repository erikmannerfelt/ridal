use super::*;
use crate::render::profile::{AmplitudeLimits, ImageFormat};
use crate::source::ArraySource;
use spec::{AxisSpec, Extent, Size, Style};

/// A radargram whose amplitude increases with the row, so a pixel's grey
/// says which row it came from.
fn ramp(width: usize, height: usize) -> ndarray::Array2<f32> {
    ndarray::Array2::from_shape_fn((height, width), |(row, _)| row as f32)
}

fn profile(max: f32) -> RenderProfile {
    RenderProfile {
        limits: AmplitudeLimits::Explicit { min: 0.0, max },
        format: ImageFormat::Png,
        ..RenderProfile::default_profile()
    }
}

/// 200 traces at 0.5 m, 100 samples at 2 ns.
fn coordinates(width: usize, height: usize) -> Coordinates {
    Coordinates {
        distance: Some((0..width).map(|i| i as f64 * 0.5).collect()),
        time: Some((0..width).map(|i| 1_714_932_000.0 + i as f64).collect()),
        elevation: Some((0..width).map(|i| 500.0 - i as f64 * 0.1).collect()),
        twtt: Some((0..height).map(|i| i as f64 * 2.0).collect()),
        depth: Some((0..height).map(|i| i as f64 * 0.17).collect()),
    }
}

fn px_spec() -> FigureSpec {
    FigureSpec {
        size: Size {
            width: 600.0,
            unit: SizeUnit::Px,
            dpi: 96.0,
        },
        ..FigureSpec::default()
    }
}

fn render(
    data: &ndarray::Array2<f32>,
    coords: &Coordinates,
    spec: &FigureSpec,
    format: FigureFormat,
) -> Result<RenderedFigure, FigureError> {
    let source = ArraySource::new(data.view());
    render_figure(
        &FigureSource {
            source: &source,
            coordinates: coords,
            revision_id: Some("rev-a".into()),
            elevation_range: topo::ElevationRange::NONE,
            overlays: None,
            contrast: None,
        },
        spec,
        &profile(data.nrows() as f32),
        format,
    )
}

fn layout_of(spec: &FigureSpec, traces: [f64; 2], rows: [f64; 2]) -> Layout {
    Layout::new(spec, traces, rows, &[], &[], "", "").unwrap()
}

#[test]
fn the_plot_keeps_the_viewers_aspect_and_horizontal_scale() {
    // 200 traces by 100 rows at x_scale 1 is twice as wide as tall; at
    // x_scale 0.5 it is square.
    let spec = FigureSpec {
        axes: false,
        ..px_spec()
    };
    let layout = layout_of(&spec, [0.0, 200.0], [0.0, 100.0]);
    assert_eq!((layout.plot_w, layout.plot_h), (600, 300));
    let squeezed = FigureSpec {
        x_scale: 0.5,
        ..spec.clone()
    };
    let layout = layout_of(&squeezed, [0.0, 200.0], [0.0, 100.0]);
    assert_eq!((layout.plot_w, layout.plot_h), (600, 600));
}

#[test]
fn the_width_is_the_whole_figures_and_converts_from_centimetres() {
    let spec = FigureSpec {
        size: Size {
            width: 2.54 * 4.0,
            unit: SizeUnit::Cm,
            dpi: 100.0,
        },
        ..FigureSpec::default()
    };
    let layout = layout_of(&spec, [0.0, 200.0], [0.0, 100.0]);
    assert_eq!(layout.width, 400);
    assert!(layout.plot_x > 0 && layout.plot_x + layout.plot_w < 400);
}

#[test]
fn a_figure_too_large_is_refused_with_what_to_change() {
    // A long thin extent squeezed horizontally is enormously tall.
    let spec = FigureSpec {
        x_scale: 0.0001,
        ..px_spec()
    };
    let data = ramp(200, 100);
    let err = render(&data, &coordinates(200, 100), &spec, FigureFormat::Png).unwrap_err();
    assert!(err.to_string().contains("x_scale"), "{err}");
}

#[test]
fn an_svg_figure_has_its_raster_axes_and_labels() {
    let data = ramp(200, 100);
    let spec = FigureSpec {
        size: Size {
            width: 16.0,
            unit: SizeUnit::Cm,
            dpi: 100.0,
        },
        x_axis: AxisSpec {
            spacing: Some(20.0),
            ..AxisSpec::default()
        },
        ..FigureSpec::default()
    };
    let figure = render(&data, &coordinates(200, 100), &spec, FigureFormat::Svg).unwrap();
    let svg = String::from_utf8(figure.bytes).unwrap();
    assert!(svg.starts_with("<svg"));
    assert!(svg.contains(r#"width="16.002cm""#) || svg.contains(r#"width="16.000cm""#));
    assert!(svg.contains("data:image/png;base64,"));
    // Distance and travel time by default, as the file has both.
    assert!(svg.contains(">Distance (m)</text>"));
    assert!(svg.contains(">Two-way travel time (ns)</text>"));
    // 0 to 99.5 m at a 20 m spacing.
    for label in ["0", "20", "40", "60", "80"] {
        assert!(svg.contains(&format!(">{label}</text>")), "no {label}");
    }
    // No 100 m tick (the last trace is at 99.5 m), and the y axis's own
    // ticks are every 50 ns.
    assert!(!svg.contains(">120</text>"));
    assert!(svg.contains(">150</text>"));
    // resvg can read what was written.
    resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).unwrap();
}

#[test]
fn a_png_figure_has_a_background_raster_and_drawn_labels() {
    let data = ramp(200, 100);
    let figure = render(&data, &coordinates(200, 100), &px_spec(), FigureFormat::Png).unwrap();
    let image = image::load_from_memory(&figure.bytes).unwrap().to_rgba8();
    assert_eq!(
        (image.width() as usize, image.height() as usize),
        (figure.width, figure.height)
    );
    assert_eq!(image.width(), 600);
    // White in the corner, and something dark in the bottom margin: the
    // tick labels and axis title were drawn, with the bundled font.
    assert_eq!(image.get_pixel(0, 0).0, [255, 255, 255, 255]);
    let bottom = image.height() - 1;
    let dark = (0..image.width())
        .flat_map(|x| (bottom - 25..bottom).map(move |y| (x, y)))
        .filter(|&(x, y)| image.get_pixel(x, y).0[0] < 100)
        .count();
    assert!(dark > 50, "only {dark} dark pixels below the plot");
}

#[test]
fn a_transparent_background_is_transparent() {
    let data = ramp(200, 100);
    let spec = FigureSpec {
        style: Style {
            background: None,
            ..Style::default()
        },
        ..px_spec()
    };
    let figure = render(&data, &coordinates(200, 100), &spec, FigureFormat::Png).unwrap();
    let image = image::load_from_memory(&figure.bytes).unwrap().to_rgba8();
    assert_eq!(image.get_pixel(0, 0).0[3], 0);
    let svg = render(&data, &coordinates(200, 100), &spec, FigureFormat::Svg).unwrap();
    let svg = String::from_utf8(svg.bytes).unwrap();
    assert!(!svg.contains("<rect x=\"0\" y=\"0\""));
}

#[test]
fn an_excerpt_shows_only_its_own_rows() {
    // Rows 50 to 60 of a ramp: every raster pixel is from that band, so
    // much brighter than the top of the radargram and darker than its
    // bottom.
    let data = ramp(200, 100);
    let spec = FigureSpec {
        axes: false,
        extent: Extent {
            traces: Some([20.0, 120.0]),
            rows: Some([50.0, 60.0]),
            elevation: None,
        },
        ..px_spec()
    };
    let figure = render(&data, &coordinates(200, 100), &spec, FigureFormat::Png).unwrap();
    let image = image::load_from_memory(&figure.bytes).unwrap().to_luma8();
    // 100 traces by 10 rows at 600 px wide.
    assert_eq!((image.width(), image.height()), (600, 60));
    let (lo, hi) = image
        .pixels()
        .fold((255u8, 0u8), |(lo, hi), p| (lo.min(p.0[0]), hi.max(p.0[0])));
    // Row r maps to byte 255 * r / 100.
    assert!(lo >= 125 && hi <= 155, "{lo}..{hi}");
}

#[test]
fn a_figure_without_axes_carries_its_coordinates_in_a_sidecar() {
    let data = ramp(200, 100);
    let spec = FigureSpec {
        axes: false,
        extent: Extent {
            traces: Some([0.0, 100.0]),
            rows: None,
            elevation: None,
        },
        ..px_spec()
    };
    let figure = render(&data, &coordinates(200, 100), &spec, FigureFormat::Png).unwrap();
    let sidecar = figure.sidecar.unwrap();
    assert_eq!((sidecar.width, sidecar.height), (600, 600));
    assert_eq!(sidecar.x.unit, "distance");
    assert_eq!(sidecar.x.extent, [0.0, 50.0]);
    assert_eq!(sidecar.y.unit, "twtt");
    assert_eq!(sidecar.y.extent, [0.0, 200.0]);
    assert_eq!(sidecar.x.centres.len(), 600);
    assert!((sidecar.x.centres[0] - 50.0 / 1200.0).abs() < 1e-9);
    // And it is refused as SVG, which would only wrap the PNG.
    assert!(render(&data, &coordinates(200, 100), &spec, FigureFormat::Svg).is_err());
}

#[test]
fn a_spec_for_another_revision_is_refused() {
    let data = ramp(200, 100);
    let spec = FigureSpec {
        revision_id: Some("rev-b".into()),
        ..px_spec()
    };
    let err = render(&data, &coordinates(200, 100), &spec, FigureFormat::Png).unwrap_err();
    assert!(matches!(err, FigureError::RevisionMismatch { .. }), "{err}");
    let same = FigureSpec {
        revision_id: Some("rev-a".into()),
        ..px_spec()
    };
    assert!(render(&data, &coordinates(200, 100), &same, FigureFormat::Png).is_ok());
}

#[test]
fn a_unit_the_file_lacks_is_an_error_but_the_default_falls_back() {
    let data = ramp(200, 100);
    let bare = Coordinates::default();
    let spec = FigureSpec {
        x_axis: AxisSpec {
            unit: Some(XUnit::Distance),
            ..AxisSpec::default()
        },
        ..px_spec()
    };
    let err = render(&data, &bare, &spec, FigureFormat::Svg).unwrap_err();
    assert!(err.to_string().contains("'distance'"), "{err}");
    // With no unit asked for, trace and sample numbers.
    let figure = render(&data, &bare, &px_spec(), FigureFormat::Svg).unwrap();
    let svg = String::from_utf8(figure.bytes).unwrap();
    assert!(svg.contains(">Trace</text>") && svg.contains(">Sample</text>"));
}

#[test]
fn time_ticks_are_whole_minutes_of_utc() {
    let data = ramp(200, 100);
    let spec = FigureSpec {
        x_axis: AxisSpec {
            unit: Some(XUnit::Time),
            spacing: Some(60.0),
            ..AxisSpec::default()
        },
        ..px_spec()
    };
    let figure = render(&data, &coordinates(200, 100), &spec, FigureFormat::Svg).unwrap();
    let svg = String::from_utf8(figure.bytes).unwrap();
    // 1714932000 is 18:00:00 UTC; 200 traces at one a second.
    for label in ["18:00", "18:01", "18:02", "18:03"] {
        assert!(svg.contains(&format!(">{label}</text>")), "no {label}");
    }
}

#[test]
fn the_topographic_view_is_labelled_in_elevation_and_takes_an_elevation_extent() {
    let data = ramp(200, 100);
    let coords = coordinates(200, 100);
    let spec = FigureSpec {
        view: View::Topo,
        y_axis: AxisSpec {
            spacing: Some(2.0),
            ..AxisSpec::default()
        },
        extent: Extent {
            traces: None,
            rows: None,
            // dz is 0.17 m and the top 500 m: rows 0 to ~58.8.
            elevation: Some([500.0, 490.0]),
        },
        ..px_spec()
    };
    let figure = render(&data, &coords, &spec, FigureFormat::Svg).unwrap();
    let svg = String::from_utf8(figure.bytes).unwrap();
    assert!(svg.contains(">Elevation (m)</text>"));
    for label in ["500", "498", "496", "494", "492", "490"] {
        assert!(svg.contains(&format!(">{label}</text>")), "no {label}");
    }
    assert!(!svg.contains(">488</text>"));
}

#[test]
fn the_topographic_wedge_is_transparent() {
    // The surface drops 20 m over the profile, so the top right of the
    // corrected raster has no data.
    let data = ramp(200, 100);
    let spec = FigureSpec {
        view: View::Topo,
        axes: false,
        ..px_spec()
    };
    let figure = render(&data, &coordinates(200, 100), &spec, FigureFormat::Png).unwrap();
    let image = image::load_from_memory(&figure.bytes).unwrap().to_rgba8();
    let (w, h) = (image.width(), image.height());
    assert_eq!(image.get_pixel(w - 1, 0).0[3], 0, "above the surface");
    assert_eq!(image.get_pixel(0, 0).0[3], 255, "the surface on the left");
    assert_eq!(image.get_pixel(w / 2, h / 2).0[3], 255);
}

#[test]
fn text_is_escaped() {
    let data = ramp(200, 100);
    let spec = FigureSpec {
        x_axis: AxisSpec {
            label: Some("Distance <along> & across".into()),
            ..AxisSpec::default()
        },
        ..px_spec()
    };
    let figure = render(&data, &coordinates(200, 100), &spec, FigureFormat::Svg).unwrap();
    let svg = String::from_utf8(figure.bytes).unwrap();
    assert!(svg.contains("Distance &lt;along&gt; &amp; across"));
}

#[test]
fn an_empty_label_draws_no_title() {
    let data = ramp(200, 100);
    let spec = FigureSpec {
        x_axis: AxisSpec {
            label: Some(String::new()),
            ..AxisSpec::default()
        },
        ..px_spec()
    };
    let figure = render(&data, &coordinates(200, 100), &spec, FigureFormat::Svg).unwrap();
    let svg = String::from_utf8(figure.bytes).unwrap();
    assert!(!svg.contains("Distance (m)"));
}

#[test]
fn output_formats_come_from_the_extension() {
    assert_eq!(
        FigureFormat::for_path(Path::new("a/b.SVG")).unwrap(),
        FigureFormat::Svg
    );
    assert_eq!(
        FigureFormat::for_path(Path::new("b.png")).unwrap(),
        FigureFormat::Png
    );
    assert!(FigureFormat::for_path(Path::new("b.jpg")).is_err());
}
