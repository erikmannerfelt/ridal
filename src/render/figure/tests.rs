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
            interrupt: crate::render::interrupt::Interrupt::never(),
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

#[test]
fn the_topographic_view_can_leave_room_above_the_highest_surface() {
    // The surface tops out at 500 m; asking for 510 m leaves 10 m of empty
    // space above it, labelled, rather than being clipped to 500 m.
    let data = ramp(200, 100);
    let spec = FigureSpec {
        view: View::Topo,
        extent: Extent {
            traces: None,
            rows: None,
            elevation: Some([510.0, 490.0]),
        },
        y_axis: AxisSpec {
            spacing: Some(5.0),
            ..AxisSpec::default()
        },
        ..px_spec()
    };
    let svg = render(&data, &coordinates(200, 100), &spec, FigureFormat::Svg).unwrap();
    let svg = String::from_utf8(svg.bytes).unwrap();
    assert!(svg.contains(">510</text>") && svg.contains(">505</text>"));

    let raw = FigureSpec {
        axes: false,
        ..spec
    };
    let figure = render(&data, &coordinates(200, 100), &raw, FigureFormat::Png).unwrap();
    let image = image::load_from_memory(&figure.bytes).unwrap().to_rgba8();
    // The top half is above every surface: empty.
    assert_eq!(image.get_pixel(5, 2).0[3], 0);
    assert_eq!(image.get_pixel(5, image.height() - 2).0[3], 255);
}

#[test]
fn a_short_plot_leaves_room_for_its_y_label() {
    // 200 traces by 10 rows is a plot 30 px tall at 600 px wide, much less
    // than "Two-way travel time (ns)" set at 10 pt: the figure grows above
    // and below so the rotated label fits.
    let spec = px_spec();
    let label = "Two-way travel time (ns)";
    let layout = Layout::new(&spec, [0.0, 200.0], [0.0, 10.0], &[], &[], "x", label).unwrap();
    let label_px = font::text_width(label, layout.font_px);
    let centre = layout.plot_y as f64 + layout.plot_h as f64 / 2.0;
    assert!(centre - label_px / 2.0 >= 0.0, "runs off the top");
    assert!(
        centre + label_px / 2.0 <= layout.height as f64,
        "runs off the bottom"
    );
}

#[test]
fn distance_can_be_in_kilometres() {
    // 0 to 99.5 m is 0 to 0.0995 km; a 0.02 km spacing prints with the
    // decimals it needs.
    let data = ramp(200, 100);
    let spec = FigureSpec {
        x_axis: AxisSpec {
            unit: Some(XUnit::DistanceKm),
            spacing: Some(0.02),
            ..AxisSpec::default()
        },
        ..px_spec()
    };
    let figure = render(&data, &coordinates(200, 100), &spec, FigureFormat::Svg).unwrap();
    let svg = String::from_utf8(figure.bytes).unwrap();
    assert!(svg.contains(">Distance (km)</text>"));
    for label in ["0.00", "0.02", "0.04", "0.06", "0.08"] {
        assert!(svg.contains(&format!(">{label}</text>")), "no {label}");
    }
    assert_eq!(
        FigureSpec::from_json(r#"{"x_axis": {"unit": "distance_km"}}"#)
            .unwrap()
            .x_axis
            .unit,
        Some(XUnit::DistanceKm)
    );
}

/// The x position of the first `<text>` that says exactly `label`.
fn label_x(svg: &str, label: &str) -> f64 {
    let line = svg
        .lines()
        .find(|l| l.starts_with("<text") && l.ends_with(&format!(">{label}</text>")))
        .unwrap_or_else(|| panic!("no label {label}"));
    let x = line
        .split("x=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    x.parse().unwrap()
}

#[test]
fn a_reversed_figure_is_the_mirror_image_of_the_plain_one() {
    // #496. Amplitude rises with the trace, so a mirror is visible.
    let data = ndarray::Array2::from_shape_fn((100, 200), |(_, trace)| trace as f32 / 2.0);
    let coords = coordinates(200, 100);
    let plain = FigureSpec {
        axes: false,
        ..px_spec()
    };
    let reversed = FigureSpec {
        x_axis: AxisSpec {
            reverse: true,
            ..AxisSpec::default()
        },
        ..plain.clone()
    };
    let decode = |spec: &FigureSpec| {
        let figure = render(&data, &coords, spec, FigureFormat::Png).unwrap();
        image::load_from_memory(&figure.bytes).unwrap().to_rgba8()
    };
    let mut mirrored = decode(&plain);
    image::imageops::flip_horizontal_in_place(&mut mirrored);
    assert_eq!(decode(&reversed), mirrored);

    let sidecar = |spec: &FigureSpec| {
        render(&data, &coords, spec, FigureFormat::Sidecar)
            .unwrap()
            .sidecar
            .unwrap()
    };
    let (plain, reversed) = (sidecar(&plain), sidecar(&reversed));
    assert_eq!(reversed.traces, [200.0, 0.0]);
    assert_eq!(reversed.x.extent, [plain.x.extent[1], plain.x.extent[0]]);
    assert_eq!(reversed.x.centres.len(), plain.x.centres.len());
    for (r, p) in reversed.x.centres.iter().zip(plain.x.centres.iter().rev()) {
        assert!((r - p).abs() < 1e-9, "{r} {p}");
    }
    assert_eq!(reversed.y, plain.y);
}

#[test]
fn a_reversed_figure_has_its_ticks_from_right_to_left() {
    let data = ramp(200, 100);
    let coords = coordinates(200, 100);
    let spec = |reverse| FigureSpec {
        x_axis: AxisSpec {
            spacing: Some(20.0),
            reverse,
            ..AxisSpec::default()
        },
        ..px_spec()
    };
    let svg = |spec| {
        String::from_utf8(
            render(&data, &coords, &spec, FigureFormat::Svg)
                .unwrap()
                .bytes,
        )
        .unwrap()
    };
    let (plain, reversed) = (svg(spec(false)), svg(spec(true)));
    assert!(label_x(&plain, "0") < label_x(&plain, "80"));
    assert!(label_x(&reversed, "0") > label_x(&reversed, "80"));
    // Mirrored about the plot's centre, so a label's x in one plus its x
    // in the other is the same for every label.
    let sum = label_x(&plain, "40") + label_x(&reversed, "40");
    let edges = label_x(&plain, "0") + label_x(&reversed, "0");
    assert!((sum - edges).abs() < 0.02, "{sum} {edges}");
}

#[test]
fn a_descending_axis_counts_down_from_zero_at() {
    // #496: the profile is 0 to 99.5 m; labelled from its far end, 100 m
    // there is 0 and the start of the profile is 100.
    let data = ramp(200, 100);
    let coords = coordinates(200, 100);
    let spec = |reverse| FigureSpec {
        x_axis: AxisSpec {
            spacing: Some(20.0),
            zero_at: 100.0,
            descending: true,
            reverse,
            ..AxisSpec::default()
        },
        ..px_spec()
    };
    let svg = |spec| {
        String::from_utf8(
            render(&data, &coords, &spec, FigureFormat::Svg)
                .unwrap()
                .bytes,
        )
        .unwrap()
    };
    let plain = svg(spec(false));
    for label in ["100", "80", "60", "40", "20"] {
        label_x(&plain, label);
    }
    assert!(label_x(&plain, "100") < label_x(&plain, "20"));
    // Mirrored too, the labels increase from left to right again, now
    // from the far end of the profile.
    let reversed = svg(spec(true));
    assert!(label_x(&reversed, "20") < label_x(&reversed, "100"));

    let sidecar = render(
        &data,
        &coords,
        &FigureSpec {
            axes: false,
            ..spec(false)
        },
        FigureFormat::Sidecar,
    )
    .unwrap()
    .sidecar
    .unwrap();
    // The right edge is position 200, extrapolated to 100 m.
    assert_eq!(sidecar.x.extent, [100.0, 0.0]);
}

#[test]
fn a_halo_outlines_the_labels() {
    // #495: on a transparent figure, white around black text.
    let data = ramp(200, 100);
    let coords = coordinates(200, 100);
    let spec = |halo: Option<spec::Halo>| FigureSpec {
        style: Style {
            background: None,
            halo: halo,
            ..Style::default()
        },
        ..px_spec()
    };
    let halo = spec::Halo {
        color: "#fff".to_string(),
        width: 2.0,
    };
    let svg = String::from_utf8(
        render(&data, &coords, &spec(Some(halo.clone())), FigureFormat::Svg)
            .unwrap()
            .bytes,
    )
    .unwrap();
    // The stroked copy first, then the text itself.
    let stroked = svg
        .find(r##"stroke="#ffffff" stroke-width="2.67""##)
        .unwrap();
    let filled = svg.rfind(r##"fill="#000000">"##).unwrap();
    assert!(stroked < filled);
    assert_eq!(svg.matches(">Distance (m)</text>").count(), 2);
    assert!(!svg.contains("paint-order"));

    let white_below_plot = |spec: &FigureSpec| {
        let figure = render(&data, &coords, spec, FigureFormat::Png).unwrap();
        let image = image::load_from_memory(&figure.bytes).unwrap().to_rgba8();
        let bottom = image.height() - 1;
        (0..image.width())
            .flat_map(|x| (bottom - 25..bottom).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                let [r, g, b, a] = image.get_pixel(x, y).0;
                a > 200 && r > 200 && g > 200 && b > 200
            })
            .count()
    };
    assert_eq!(white_below_plot(&spec(None)), 0);
    assert!(white_below_plot(&spec(Some(halo))) > 50);
}

#[test]
fn the_new_axis_and_halo_options_are_checked() {
    for (json, needle) in [
        (r#"{"y_axis": {"reverse": true}}"#, "y_axis.reverse"),
        (
            r#"{"x_axis": {"unit": "time", "descending": true}}"#,
            "not for a time axis",
        ),
        (
            r#"{"x_axis": {"unit": "time", "zero_at": 5}}"#,
            "not for a time axis",
        ),
        (r#"{"style": {"halo": {"width": 0}}}"#, "halo.width"),
        (r#"{"style": {"halo": {"color": "white"}}}"#, "halo.color"),
    ] {
        let err = FigureSpec::from_json(json).unwrap_err();
        assert!(err.contains(needle), "{json}: {err}");
    }
    let spec = FigureSpec::from_json(r#"{"style": {"halo": {}}}"#).unwrap();
    assert_eq!(spec.style.halo, Some(spec::Halo::default()));
    FigureSpec::from_json(r#"{"y_axis": {"zero_at": 400, "descending": true}}"#).unwrap();
}

#[test]
fn a_halo_outlines_the_frame_and_ticks_but_not_the_radargram() {
    // #495: the frame and ticks need the outline too on a dark page, but
    // the plot must look exactly as it does without one.
    let data = ramp(200, 100);
    let coords = coordinates(200, 100);
    let spec = |halo: Option<spec::Halo>| FigureSpec {
        style: Style {
            background: None,
            halo: halo,
            ..Style::default()
        },
        ..px_spec()
    };
    // Two halos of one width share a layout, so their plots can be
    // compared pixel for pixel: wherever the halo's colour shows, they
    // differ.
    let halo = |color: &str| spec::Halo {
        color: color.to_string(),
        width: 3.0,
    };
    let decode = |spec: &FigureSpec| {
        let figure = render(&data, &coords, spec, FigureFormat::Png).unwrap();
        image::load_from_memory(&figure.bytes).unwrap().to_rgba8()
    };
    let (plain, outlined) = (
        decode(&spec(Some(halo("#000000")))),
        decode(&spec(Some(halo("#ffffff")))),
    );
    assert_eq!(plain.dimensions(), outlined.dimensions());
    // Where the plot is, from the raster's place in the SVG.
    let svg = String::from_utf8(
        render(
            &data,
            &coords,
            &spec(Some(halo("#ffffff"))),
            FigureFormat::Svg,
        )
        .unwrap()
        .bytes,
    )
    .unwrap();
    let image = svg.lines().find(|l| l.starts_with("<image")).unwrap();
    let attr = |name: &str| -> u32 {
        image
            .split(&format!(" {name}=\""))
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    let (x0, y0) = (attr("x"), attr("y"));
    let (x1, y1) = (x0 + attr("width"), y0 + attr("height"));
    for y in y0..y1 {
        for x in x0..x1 {
            assert_eq!(
                plain.get_pixel(x, y),
                outlined.get_pixel(x, y),
                "the halo shows at plot pixel ({x}, {y})"
            );
        }
    }
    // Just outside the frame, halfway along the top: the halo's colour.
    let (mx, above) = ((x0 + x1) / 2, y0 - 2);
    assert_eq!(plain.get_pixel(mx, above).0, [0, 0, 0, 255]);
    let [r, g, b, a] = outlined.get_pixel(mx, above).0;
    assert!(a > 200 && r > 200 && g > 200 && b > 200, "{r} {g} {b} {a}");
}
