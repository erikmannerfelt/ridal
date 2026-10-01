//! Ties the reusable pieces together: source window -> encoded image
//! (#118).
//!
//! Render order, per #118: source amplitudes -> dataset view (standard
//! only in v1) -> float-domain resampling -> normalization -> colormap ->
//! image encoding. Overviews swap the last steps round -- normalization
//! and colormap at full resolution, then a colour-domain downsample (see
//! [`Renderer::render_overview`]). Amplitude limits are resolved once per call and passed
//! in rather than recomputed per chunk -- the caller (the render service,
//! M5) is responsible for computing them once per revision+profile and
//! reusing them, which is what keeps adjacent chunks' normalization
//! consistent and seamless.

use ndarray::Array2;

use super::colormap;
use super::grid::{Chunk, OverviewSpec, SourceWindow};
use super::profile::{RenderProfile, SourceTransform};
use super::resample::resample;
use crate::source::AmplitudeSource;

/// Fill color for pixels with no valid source data: padding beyond the
/// raster extent, or an empty resampling footprint. Mid-gray reads as
/// "no data" without the visual harshness of pure black or white against
/// real radargram content.
const PAD_VALUE: u8 = 96;

/// [`PAD_VALUE`] as the three channels the RGB path needs.
///
/// Deliberately the literal pad grey, never `lut[PAD_VALUE]`: pushing the
/// pad through the colormap would paint no-data as mid-amplitude white
/// and make an empty footprint indistinguishable from a real zero
/// crossing.
const PAD_COLOR: [u8; 3] = [PAD_VALUE, PAD_VALUE, PAD_VALUE];

/// Ceiling on how much source an overview render reads at once. An
/// overview is small (~512 px wide) but its *input* is the whole
/// radargram, so without a cap one thumbnail request allocates the
/// entire array as `f32` -- ~180 MB for the largest file in the test
/// corpus, multiplied by every concurrent request. 64 MB keeps reads
/// large enough to stay HDF5-chunk-efficient while bounding the peak.
const OVERVIEW_READ_BUDGET_BYTES: usize = 64 * 1024 * 1024;

pub struct Renderer<'a, S: AmplitudeSource> {
    reader: &'a S,
}

impl<'a, S: AmplitudeSource> Renderer<'a, S> {
    pub fn new(reader: &'a S) -> Self {
        Self { reader }
    }

    /// Render one chunk to encoded image bytes.
    ///
    /// `limits` are the already-resolved `(min, max)` display-domain
    /// bounds for this revision+profile (see module docs on why these are
    /// not recomputed here).
    pub fn render_chunk(
        &self,
        chunk: &Chunk,
        profile: &RenderProfile,
        limits: (f32, f32),
    ) -> Result<Vec<u8>, String> {
        let mut source =
            self.read_source_for_window(&chunk.source_window, super::grid::CHUNK_SIZE)?;
        apply_source_transform(&mut source, profile);
        // Resample into the chunk's *valid* extent, which for a
        // rightmost/bottommost chunk is smaller than CHUNK_SIZE. Rendering
        // straight into a full CHUNK_SIZE output stretched that chunk's
        // source window across the whole box (by CHUNK_SIZE/valid_width),
        // shifting every trace in it off its true position and making the
        // last chunk row/column visibly discontinuous.
        //
        // The image is returned at its true size rather than padded out:
        // the viewer places each chunk using the same valid extent (see
        // `chunkBounds` in viewer.html.jinja), so padding would only add a
        // border of dead pixels beyond the radargram's real extent.
        // `PAD_VALUE` still fills footprints with no valid source data
        // *inside* the chunk, which is a different thing entirely.
        let resampled = resample(
            source.view(),
            &self.local_window(&chunk.source_window),
            chunk.valid_width,
            chunk.valid_height,
            profile.resampling,
        );
        render_and_encode(&resampled, profile, limits)
    }

    /// Render a full-radargram overview to encoded image bytes.
    ///
    /// Every source sample is taken to its display colour first, at full
    /// resolution, and the *colours* are then area-averaged down to the
    /// overview size (#300). This is what a browser does when it shrinks a
    /// full-resolution render, and it is the reverse of how chunks work:
    /// a chunk resamples amplitude and colours the result.
    ///
    /// The order matters for an overview because its footprints are large
    /// (often 10-25 source samples per pixel in each direction) and the
    /// display mapping is not linear. Averaging signed, oscillating
    /// amplitude cancels it towards zero before the colormap sees it, so
    /// `seismic` came out nearly white and `siglog-positive` nearly twice
    /// as bright as the radargram looks at full resolution, while
    /// `abslog` and `positive` lost their contrast. Averaging colours keeps
    /// what each sample looks like. Measured against a downscale of the
    /// full-resolution render, the error fell 3-8x for those profiles and
    /// did not grow for the linear grey ones.
    ///
    /// `profile.resampling` therefore has no effect here; it applies to
    /// chunks only.
    ///
    /// Reads the source in horizontal bands, as an overview's input is
    /// the whole radargram: the 3678x12187 file in the test corpus is
    /// ~180 MB of `f32`, per call, per profile. Each source row adds its
    /// weighted colours to the output rows it overlaps, so the result does
    /// not depend on where band boundaries fall -- bit for bit -- and no
    /// band needs a halo.
    pub fn render_overview(
        &self,
        spec: &OverviewSpec,
        profile: &RenderProfile,
        limits: (f32, f32),
    ) -> Result<Vec<u8>, String> {
        let band = self.overview_rows_per_band();
        self.render_overview_banded(spec, profile, limits, band)
    }

    /// Source rows per read, derived from [`OVERVIEW_READ_BUDGET_BYTES`].
    /// At least one, so a radargram whose single source row already
    /// exceeds the budget still renders (one row at a time) rather than
    /// dividing to zero and looping forever.
    ///
    /// `vertical_read_overhead` (zero for every source but
    /// [`crate::render::topo::TopoSource`]) is reserved out of the budget
    /// first: a topographically sheared source additionally spans the
    /// shift range across a band's columns, so a band sized only from the
    /// budget would overshoot it by roughly that span -- a 2-3x overshoot
    /// measured on a long profile with a few hundred metres of relief.
    fn overview_rows_per_band(&self) -> usize {
        let (_, src_w) = self.reader.shape();
        let overhead = self.reader.vertical_read_overhead(0, src_w);
        let bytes_per_source_row = src_w.max(1) * std::mem::size_of::<f32>();
        (OVERVIEW_READ_BUDGET_BYTES / bytes_per_source_row)
            .saturating_sub(overhead)
            .max(1)
    }

    /// The banded implementation behind [`Renderer::render_overview`],
    /// with the band height (in source rows) injected so tests can force
    /// many small bands and compare against a single whole-array band.
    fn render_overview_banded(
        &self,
        spec: &OverviewSpec,
        profile: &RenderProfile,
        limits: (f32, f32),
        source_rows_per_band: usize,
    ) -> Result<Vec<u8>, String> {
        let (src_h, src_w) = self.reader.shape();
        let (out_h, out_w) = (spec.height.max(1), spec.width.max(1));
        let lut = profile.colormap.as_ref().map(|cmap| cmap.lut());
        let channels = if lut.is_some() { 3 } else { 1 };

        let col_taps = footprint_taps(src_w, out_w);
        let row_taps = footprint_taps(src_h, out_h);
        // Weighted colour sums and the weight of the *valid* samples that
        // went into them. A NaN sample (no data, or a topographic wedge)
        // adds nothing, so a partly empty footprint is the mean of what it
        // does have, and only an entirely empty one is padded.
        let mut sums = vec![0.0_f32; out_h * out_w * channels];
        let mut weights = vec![0.0_f32; out_h * out_w];

        let mut colours: Vec<Option<[u8; 3]>> = Vec::with_capacity(src_w);

        let band = source_rows_per_band.max(1);
        let mut row0 = 0usize;
        while row0 < src_h {
            let row1 = (row0 + band).min(src_h);
            let mut source = self.reader.read_window(row0, row1, 0, src_w)?;
            apply_source_transform(&mut source, profile);
            for (local_row, row) in source.outer_iter().enumerate() {
                // Coloured once per source row, however many output rows
                // it is split between.
                colours.clear();
                colours.extend(row.iter().map(|&raw| {
                    colormap::normalized_pixel(raw, profile, limits).map(|byte| match &lut {
                        Some(lut) => lut[byte as usize],
                        None => [byte; 3],
                    })
                }));
                for &(oy, wy) in &row_taps[row0 + local_row] {
                    for (col, colour) in colours.iter().enumerate() {
                        let Some(colour) = colour else {
                            continue;
                        };
                        for &(ox, wx) in &col_taps[col] {
                            let w = wy * wx;
                            let pixel = oy * out_w + ox;
                            weights[pixel] += w;
                            for (c, &value) in colour[..channels].iter().enumerate() {
                                sums[pixel * channels + c] += w * value as f32;
                            }
                        }
                    }
                }
            }
            row0 = row1;
        }

        let channel = |pixel: usize, c: usize, pad: u8| -> u8 {
            if weights[pixel] > 0.0 {
                (sums[pixel * channels + c] / weights[pixel])
                    .round()
                    .clamp(0.0, 255.0) as u8
            } else {
                pad
            }
        };
        if lut.is_some() {
            let image = image::RgbImage::from_fn(out_w as u32, out_h as u32, |x, y| {
                let pixel = y as usize * out_w + x as usize;
                image::Rgb([0, 1, 2].map(|c| channel(pixel, c, PAD_COLOR[c])))
            });
            colormap::encode_rgb(&image, profile.format)
        } else {
            let image = image::GrayImage::from_fn(out_w as u32, out_h as u32, |x, y| {
                let pixel = y as usize * out_w + x as usize;
                image::Luma([channel(pixel, 0, PAD_VALUE)])
            });
            colormap::encode(&image, profile.format)
        }
    }

    /// Read exactly the (integer-rounded) source region a window touches,
    /// padded by one row/col of slack so the resampler's ceil-rounded
    /// footprints never read past what was fetched.
    fn read_source_for_window(
        &self,
        window: &SourceWindow,
        _chunk_size: usize,
    ) -> Result<Array2<f32>, String> {
        let row0 = window.row0.floor().max(0.0) as usize;
        let col0 = window.col0.floor().max(0.0) as usize;
        let row1 = window.row1.ceil() as usize;
        let col1 = window.col1.ceil() as usize;
        self.reader.read_window(row0, row1, col0, col1)
    }

    /// Re-express `window` relative to the sub-array `read_source_for_window`
    /// actually fetched (which starts at `window`'s floored origin, not at
    /// the full array's origin).
    fn local_window(&self, window: &SourceWindow) -> SourceWindow {
        let row0_floor = window.row0.floor().max(0.0);
        let col0_floor = window.col0.floor().max(0.0);
        SourceWindow {
            row0: window.row0 - row0_floor,
            row1: window.row1 - row0_floor,
            col0: window.col0 - col0_floor,
            col1: window.col1 - col0_floor,
        }
    }
}

/// Colormap (if any) and encode a resampled chunk or band.
///
/// `None` takes the original grayscale path unchanged -- the same
/// functions, producing the same bytes, so every grayscale profile's
/// cached renders stay valid. `Some` builds the 256-entry LUT once for
/// this render and takes the RGB path, which is the point of keeping the
/// two apart: a greyscale JPEG is a genuinely one-component JPEG and a
/// greyscale PNG is markedly smaller than an RGB one, so re-encoding
/// every render with a replicated grey channel would triple the lossless
/// downloads for no visual difference.
fn render_and_encode(
    resampled: &Array2<f32>,
    profile: &RenderProfile,
    limits: (f32, f32),
) -> Result<Vec<u8>, String> {
    match &profile.colormap {
        None => {
            let image = colormap::render_grayscale(resampled, profile, limits, PAD_VALUE);
            colormap::encode(&image, profile.format)
        }
        Some(cmap) => {
            let lut = cmap.lut();
            let image = colormap::render_colormapped(resampled, profile, limits, &lut, PAD_COLOR);
            colormap::encode_rgb(&image, profile.format)
        }
    }
}

/// For each of `source_len` source samples along one axis, the output
/// samples (of `out_len`) its unit interval overlaps and by how much, in
/// source units.
///
/// Output sample `o` covers `[o * step, (o + 1) * step)` of the source,
/// with `step = source_len / out_len`, so a source sample straddling a
/// footprint boundary is split between the two in proportion. Weights are
/// summed per output pixel and divided out at the end, so they need not be
/// normalised here.
fn footprint_taps(source_len: usize, out_len: usize) -> Vec<Vec<(usize, f32)>> {
    let step = source_len as f64 / out_len.max(1) as f64;
    (0..source_len)
        .map(|s| {
            let (a, b) = (s as f64, s as f64 + 1.0);
            let first = ((a / step).floor() as usize).min(out_len - 1);
            let last = ((b / step).ceil() as usize).min(out_len);
            (first..last)
                .filter_map(|o| {
                    let overlap = b.min((o + 1) as f64 * step) - a.max(o as f64 * step);
                    (overlap > 1e-9).then_some((o, overlap as f32))
                })
                .collect()
        })
        .collect()
}

/// Apply a profile's [`SourceTransform`] to a freshly read source window or
/// band, before it is resampled.
///
/// Pointwise and `NaN`-preserving, so banding is unaffected: an output row
/// sees exactly the transformed samples it would in a whole-array render.
/// The `None` early-return keeps every profile without source preprocessing
/// on its previous code path.
fn apply_source_transform(data: &mut Array2<f32>, profile: &RenderProfile) {
    if profile.source_transform == SourceTransform::None {
        return;
    }
    data.mapv_inplace(|v| {
        colormap::to_source_domain(v, profile.source_transform, profile.siglog_minval_log10)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::grid::ViewerRaster;
    use crate::render::profile::AmplitudeLimits;
    use crate::source::SourceReader;

    fn write_asymmetric_nc(path: &std::path::Path, height: usize, width: usize) {
        let mut file = netcdf::create(path).unwrap();
        file.add_dimension("y", height).unwrap();
        file.add_dimension("x", width).unwrap();
        let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
        // Distinct per-cell values so orientation bugs (transpose/mirror)
        // are detectable from rendered pixel values.
        let data: Vec<f32> = (0..(height * width)).map(|i| i as f32).collect();
        var.put_values(&data, ..).unwrap();
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn render_chunk_produces_a_correctly_sized_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_asymmetric_nc(&path, 300, 700);

        let reader = SourceReader::open(&path).unwrap();
        let renderer = Renderer::new(&reader);
        let raster = ViewerRaster::new(700, 300);
        let grid = raster.grid();
        let chunk = grid.chunk(0, 0).unwrap();

        let profile = RenderProfile {
            limits: AmplitudeLimits::Explicit {
                min: 0.0,
                max: (300 * 700) as f32,
            },
            ..RenderProfile::default_profile()
        };
        let bytes = renderer
            .render_chunk(&chunk, &profile, (0.0, (300 * 700) as f32))
            .unwrap();

        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width(), super::super::grid::CHUNK_SIZE as u32);
        assert_eq!(decoded.height(), super::super::grid::CHUNK_SIZE as u32);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn edge_chunk_renders_at_its_valid_extent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        // 300x300 with 256px chunks -> edge chunk (1,1) is only 44x44 valid.
        write_asymmetric_nc(&path, 300, 300);

        let reader = SourceReader::open(&path).unwrap();
        let renderer = Renderer::new(&reader);
        let raster = ViewerRaster::new(300, 300);
        let grid = raster.grid();
        let chunk = grid.chunk(1, 1).unwrap();
        assert!(chunk.valid_width < super::super::grid::CHUNK_SIZE);

        let profile = RenderProfile::default_profile();
        let bytes = renderer
            .render_chunk(&chunk, &profile, (0.0, (300 * 300) as f32))
            .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        // Exactly the valid extent, not padded out to CHUNK_SIZE: the
        // viewer places edge chunks using the same extent, so padding
        // would just add dead pixels past the radargram's real edge.
        assert_eq!(decoded.width(), chunk.valid_width as u32);
        assert_eq!(decoded.height(), chunk.valid_height as u32);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn edge_chunk_is_not_stretched_across_a_full_chunk_box() {
        // Regression: render_chunk used to resample an edge chunk's source
        // window into the *full* CHUNK_SIZE output, stretching it by
        // CHUNK_SIZE/valid_width. That put the chunk's traces at the wrong
        // x positions and made the last chunk row/column visibly
        // discontinuous against their neighbours -- glaring under the
        // high-contrast `positive` profile, subtle but still wrong under
        // `default`.
        //
        // Pinned by comparing the edge chunk against the same source
        // region resampled at its true scale: a stretched render would
        // disagree everywhere except the leftmost column.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_asymmetric_nc(&path, 300, 300);

        let reader = SourceReader::open(&path).unwrap();
        let renderer = Renderer::new(&reader);
        let raster = ViewerRaster::new(300, 300);
        let grid = raster.grid();
        let chunk = grid.chunk(1, 1).unwrap();
        assert!(chunk.valid_width < super::super::grid::CHUNK_SIZE);
        assert!(chunk.valid_height < super::super::grid::CHUNK_SIZE);

        // PNG so the assertion reads exact pixel values rather than JPEG's
        // approximations of them.
        let profile = RenderProfile {
            format: super::super::profile::ImageFormat::Png,
            ..RenderProfile::default_profile()
        };
        let limits = (0.0, (300 * 300) as f32);
        let bytes = renderer.render_chunk(&chunk, &profile, limits).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_luma8();
        assert_eq!(decoded.width(), chunk.valid_width as u32);
        assert_eq!(decoded.height(), chunk.valid_height as u32);

        // Independently resample the same source window at the chunk's
        // true output size and render it the same way; the chunk route
        // must agree pixel for pixel.
        let source = renderer
            .read_source_for_window(&chunk.source_window, super::super::grid::CHUNK_SIZE)
            .unwrap();
        let expected = colormap::render_grayscale(
            &resample(
                source.view(),
                &renderer.local_window(&chunk.source_window),
                chunk.valid_width,
                chunk.valid_height,
                profile.resampling,
            ),
            &profile,
            limits,
            PAD_VALUE,
        );
        for y in 0..chunk.valid_height as u32 {
            for x in 0..chunk.valid_width as u32 {
                assert_eq!(
                    decoded.get_pixel(x, y).0[0],
                    expected.get_pixel(x, y).0[0],
                    "edge chunk disagrees at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn overview_preserves_aspect_and_orientation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_asymmetric_nc(&path, 200, 1000);

        let reader = SourceReader::open(&path).unwrap();
        let renderer = Renderer::new(&reader);
        let spec = OverviewSpec::new(1000, 200, 100);
        let profile = RenderProfile::default_profile();
        let bytes = renderer
            .render_overview(&spec, &profile, (0.0, (200 * 1000) as f32))
            .unwrap();

        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width(), spec.width as u32);
        assert_eq!(decoded.height(), spec.height as u32);
        assert!(decoded.width() > decoded.height()); // wide source stays wide
    }

    fn write_nc_values(path: &std::path::Path, height: usize, width: usize, data: &[f32]) {
        let mut file = netcdf::create(path).unwrap();
        file.add_dimension("y", height).unwrap();
        file.add_dimension("x", width).unwrap();
        let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
        var.put_values(data, ..).unwrap();
    }

    fn png(profile: RenderProfile) -> RenderProfile {
        RenderProfile {
            format: super::super::profile::ImageFormat::Png,
            ..profile
        }
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn banding_never_changes_the_overview() {
        // Banding must change how much source is held in memory at once
        // and nothing else. Every source row adds the same weighted
        // colours to the same output pixels in the same order whichever
        // band it arrives in, so this holds bit for bit, at fractional
        // scales too, for grey and colormapped profiles alike.
        let dir = tempfile::tempdir().unwrap();
        for (height, width, max_width) in [(200, 800, 200), (197, 613, 100)] {
            let path = dir.path().join(format!("t{height}.nc"));
            write_asymmetric_nc(&path, height, width);
            let reader = SourceReader::open(&path).unwrap();
            let renderer = Renderer::new(&reader);
            let spec = OverviewSpec::new(width, height, max_width);
            let limits = (0.0, (height * width) as f32);

            for name in ["default", "seismic"] {
                let profile = png(RenderProfile::by_name(name).unwrap());
                let whole = renderer
                    .render_overview_banded(&spec, &profile, limits, usize::MAX)
                    .unwrap();
                for band in [1usize, 2, 7, 49] {
                    let banded = renderer
                        .render_overview_banded(&spec, &profile, limits, band)
                        .unwrap();
                    assert_eq!(
                        banded, whole,
                        "{name} {height}x{width}: band height {band} changed the output"
                    );
                }
            }
        }
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn the_overview_ignores_the_resampling_method() {
        // Overviews average colours (#300); the amplitude resampler is a
        // chunk setting. Pinned so that a profile choosing a method for
        // its chunks cannot quietly change its overview.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_asymmetric_nc(&path, 200, 800);
        let reader = SourceReader::open(&path).unwrap();
        let renderer = Renderer::new(&reader);
        let spec = OverviewSpec::new(800, 200, 200);
        let limits = (0.0, (200 * 800) as f32);

        let render = |method| {
            let profile = RenderProfile {
                resampling: method,
                ..png(RenderProfile::default_profile())
            };
            renderer.render_overview(&spec, &profile, limits).unwrap()
        };
        let mean = render(super::super::profile::ResamplingMethod::Mean);
        for method in [
            super::super::profile::ResamplingMethod::Peak,
            super::super::profile::ResamplingMethod::Lanczos,
            super::super::profile::ResamplingMethod::LanczosRectified,
        ] {
            assert_eq!(render(method), mean, "{method:?} changed the overview");
        }
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn the_overview_averages_colours_not_amplitudes() {
        // The #300 failure in miniature: traces alternating +1/-1 under
        // `seismic`. Averaging amplitude gives 0, which `seismic` paints
        // white, the colour of no reflection at all. A full-resolution
        // render is solid red and blue stripes, and shrunk, those read as
        // their mix -- which is what the overview must show.
        let (height, width) = (4, 8);
        let data: Vec<f32> = (0..height * width)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_nc_values(&path, height, width, &data);
        let reader = SourceReader::open(&path).unwrap();

        let profile = png(RenderProfile::by_name("seismic").unwrap());
        let lut = profile.colormap.as_ref().unwrap().lut();
        let spec = OverviewSpec::new(width, height, width / 2);
        let bytes = Renderer::new(&reader)
            .render_overview(&spec, &profile, (-1.0, 1.0))
            .unwrap();
        let image = image::load_from_memory(&bytes).unwrap().to_rgb8();

        let mix = [0, 1, 2].map(|c| ((lut[255][c] as f32 + lut[0][c] as f32) / 2.0).round() as u8);
        assert_ne!(mix, lut[128], "the fixture must tell the two apart");
        for pixel in image.pixels() {
            assert_eq!(pixel.0, mix);
        }
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn an_overview_pixel_averages_only_its_valid_samples() {
        // A footprint half NaN (a topographic wedge, a gap) is the mean of
        // the samples it has; one with none is the no-data grey. Averaging
        // the pad grey in instead would draw a grey fringe along every
        // wedge edge.
        let (height, width) = (2, 4);
        #[rustfmt::skip]
        let data = [
            0.0, f32::NAN, f32::NAN, f32::NAN,
            0.0, f32::NAN, f32::NAN, f32::NAN,
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_nc_values(&path, height, width, &data);
        let reader = SourceReader::open(&path).unwrap();

        let profile = png(RenderProfile::default_profile());
        let spec = OverviewSpec::new(width, height, 2);
        assert_eq!((spec.width, spec.height), (2, 1));
        let bytes = Renderer::new(&reader)
            .render_overview(&spec, &profile, (0.0, 1.0))
            .unwrap();
        let image = image::load_from_memory(&bytes).unwrap().to_luma8();
        assert_eq!(image.get_pixel(0, 0).0[0], 0, "half-valid footprint");
        assert_eq!(image.get_pixel(1, 0).0[0], PAD_VALUE, "empty footprint");
    }

    #[test]
    fn footprint_taps_split_straddling_samples_by_overlap() {
        // 5 source samples onto 2 outputs: a step of 2.5, so sample 2
        // straddles the boundary and is shared equally.
        let taps = footprint_taps(5, 2);
        assert_eq!(taps[0], vec![(0, 1.0)]);
        assert_eq!(taps[2], vec![(0, 0.5), (1, 0.5)]);
        assert_eq!(taps[4], vec![(1, 1.0)]);
        // Every source sample is fully accounted for.
        for t in &taps {
            let total: f32 = t.iter().map(|(_, w)| w).sum();
            assert!((total - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn overview_band_height_is_at_least_one_row() {
        // A source row wider than the whole byte budget must still
        // render, one output row at a time, rather than dividing to a
        // zero-height band and looping forever.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_asymmetric_nc(&path, 40, 500);
        let reader = SourceReader::open(&path).unwrap();
        let renderer = Renderer::new(&reader);
        assert!(renderer.overview_rows_per_band() >= 1);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn a_high_relief_topo_source_shrinks_the_band_to_stay_in_budget() {
        // A pathological shift vector (traces alternating between no shift
        // and a huge one) blows up the per-band read if band sizing does
        // not account for it -- the overshoot #168 measured on a real
        // survey. `vertical_read_overhead` must shrink the band enough
        // that the band's *actual* read (rows_per_band + overhead) stays
        // within budget.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        let height = 200;
        let width = 300;
        write_asymmetric_nc(&path, height, width);
        let reader = SourceReader::open(&path).unwrap();

        let elevation: Vec<f64> = (0..width)
            .map(|i| if i % 2 == 0 { 0.0 } else { 5000.0 })
            .collect();
        let depth: Vec<f32> = (0..height).map(|i| i as f32).collect();
        let geometry = super::super::topo::resolve_topo_geometry(
            Some(&elevation),
            Some(&depth),
            width,
            height,
            super::super::topo::ElevationRange::NONE,
        )
        .unwrap();
        let source = super::super::topo::TopoSource::new(&reader, &geometry);
        let renderer = Renderer::new(&source);

        let band = renderer.overview_rows_per_band();
        assert!(band >= 1);

        let overhead = source.vertical_read_overhead(0, width);
        let estimated_read_rows = band + overhead;
        let bytes_per_source_row = width * std::mem::size_of::<f32>();
        assert!(
            estimated_read_rows * bytes_per_source_row <= OVERVIEW_READ_BUDGET_BYTES,
            "band size {band} with overhead {overhead} exceeds the read budget"
        );
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn adjacent_chunks_at_scale_one_use_consistent_limits_no_visible_seam() {
        // Regression guard for #119's seam warning: if two adjacent chunks
        // were normalized independently (e.g. per-chunk min/max), a flat
        // ramp across the boundary would show a visible step. With shared
        // limits, the same source value always maps to the same byte
        // regardless of which chunk it was rendered in.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_asymmetric_nc(&path, 100, 600);

        let reader = SourceReader::open(&path).unwrap();
        let renderer = Renderer::new(&reader);
        let raster = ViewerRaster::new(600, 100);
        let grid = raster.grid();
        let profile = RenderProfile::default_profile();
        let limits = (0.0, (100 * 600) as f32);

        let c0 = grid.chunk(0, 0).unwrap();
        let c1 = grid.chunk(1, 0).unwrap();
        let img0 = image::load_from_memory(&renderer.render_chunk(&c0, &profile, limits).unwrap())
            .unwrap();
        let img1 = image::load_from_memory(&renderer.render_chunk(&c1, &profile, limits).unwrap())
            .unwrap();

        // The rightmost column of chunk 0 and leftmost column of chunk 1
        // represent adjacent source columns; under shared limits their
        // brightness must be nearly continuous (allow JPEG lossy slack).
        let right_of_0 = img0.to_luma8().get_pixel(255, 0).0[0] as i32;
        let left_of_1 = img1.to_luma8().get_pixel(0, 0).0[0] as i32;
        assert!(
            (right_of_0 - left_of_1).abs() < 10,
            "seam detected: {right_of_0} vs {left_of_1}"
        );
    }

    #[test]
    fn siglog_profile_is_the_siglog_step_then_the_base_profile() {
        // The claim this design rests on: every `siglog-*` is exactly "run
        // the adaptive_siglog processing step at that profile's own offset,
        // then render with the base profile" -- compress *before*
        // resampling, and keep the base profile's own reducer.
        // Byte-identical and with no NetCDF: one render applies the source
        // transform on the way out, the other is handed an array that was
        // already transformed. The render side is pinned to the whole
        // array's noise floor, which is what the step estimates on an
        // array this small; the sampled estimate the render entry points
        // use is tested against it in `stats.rs`.
        //
        // Parameterised over every pair rather than pinned to
        // `siglog-default`. The `positive` pair is the one that caught a
        // real bug: overriding its reducer to `Mean` left the signed mean
        // of oscillating siglog values near zero, which `Positive`'s black
        // level then clipped to an almost entirely black overview, while
        // "siglog step then `positive`" (which rectifies) looked right.
        // `siglog-seismic` is in the list precisely because it overrides
        // the shared offset: the equivalence must hold at its own offset,
        // not at the default.
        let raw = ndarray::Array2::from_shape_fn((40, 60), |(r, c)| {
            let v = (r as f32 * 0.9).sin() * 30.0 + (c as f32 * 0.2).cos() * 10.0;
            if (r + c) % 7 == 0 {
                -v
            } else {
                v
            }
        });

        let limits = (-2.5f32, 3.0f32);
        let with_explicit_limits = |base: RenderProfile| RenderProfile {
            format: super::super::profile::ImageFormat::Png,
            limits: AmplitudeLimits::Explicit {
                min: limits.0,
                max: limits.1,
            },
            ..base
        };

        let raw_source = crate::source::ArraySource::new(raw.view());
        let spec = OverviewSpec::new(60, 40, 30);
        let grid = ViewerRaster::new(60, 40).grid();
        let chunk = grid.chunk(0, 0).unwrap();

        let pairs: &[(&str, RenderProfile, RenderProfile)] = &[
            (
                "siglog-default",
                RenderProfile::siglog_default_profile(),
                RenderProfile::default_profile(),
            ),
            (
                "siglog-positive",
                RenderProfile::siglog_positive_profile(),
                RenderProfile::positive_profile(),
            ),
            (
                "siglog-high-contrast",
                RenderProfile::siglog_high_contrast_profile(),
                RenderProfile::high_contrast_profile(),
            ),
            (
                "siglog-seismic",
                RenderProfile::siglog_seismic_profile(),
                RenderProfile::seismic_profile(),
            ),
        ];
        let noise = crate::filters::siglog::noise_floor_log10(raw.iter().copied()).unwrap();
        for (name, siglog_base, plain_base) in pairs {
            let siglog_profile = with_explicit_limits(siglog_base.pin_siglog_strength(noise));
            let plain_profile = with_explicit_limits(plain_base.clone());

            // The "already ran the step" array, at this profile's offset.
            let mut pre = raw.clone();
            let strength =
                crate::filters::siglog::adaptive_siglog(&mut pre, siglog_base.siglog_noise_offset)
                    .unwrap();
            assert_eq!(
                strength, siglog_profile.siglog_minval_log10,
                "'{name}' resolved a different strength than the step"
            );
            let pre_source = crate::source::ArraySource::new(pre.view());

            // Overview path (banded/haloed reads).
            let overview_from_raw = Renderer::new(&raw_source)
                .render_overview(&spec, &siglog_profile, limits)
                .unwrap();
            let overview_from_pre = Renderer::new(&pre_source)
                .render_overview(&spec, &plain_profile, limits)
                .unwrap();
            assert_eq!(
                overview_from_raw, overview_from_pre,
                "'{name}' must equal the siglog step plus its base profile"
            );

            // Chunk path, which reads its window through a different call.
            let chunk_from_raw = Renderer::new(&raw_source)
                .render_chunk(&chunk, &siglog_profile, limits)
                .unwrap();
            let chunk_from_pre = Renderer::new(&pre_source)
                .render_chunk(&chunk, &plain_profile, limits)
                .unwrap();
            assert_eq!(
                chunk_from_raw, chunk_from_pre,
                "the '{name}' chunk path must preprocess the same way the overview does"
            );
        }
    }

    #[test]
    fn a_colormapped_render_encodes_as_rgb_and_grayscale_as_l8() {
        // The branch `render_and_encode` exists for: a profile with a
        // colormap produces a real RGB image (so PNG does not have to
        // store a replicated grey channel), and one without still
        // produces the single-channel image it always did.
        let raw = ndarray::Array2::from_shape_fn((40, 60), |(r, c)| {
            ((r as f32 * 0.7).sin() * 30.0) + ((c as f32 * 0.3).cos() * 5.0)
        });
        let source = crate::source::ArraySource::new(raw.view());
        let renderer = Renderer::new(&source);
        let spec = OverviewSpec::new(60, 40, 30);

        let colormapped = RenderProfile {
            format: super::super::profile::ImageFormat::Png,
            ..RenderProfile::seismic_profile()
        };
        let decoded = image::load_from_memory(
            &renderer
                .render_overview(&spec, &colormapped, (-35.0, 35.0))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded.color(), image::ColorType::Rgb8);
        assert_eq!((decoded.width(), decoded.height()), (30, 20));

        let grayscale = RenderProfile {
            format: super::super::profile::ImageFormat::Png,
            ..RenderProfile::default_profile()
        };
        let decoded = image::load_from_memory(
            &renderer
                .render_overview(&spec, &grayscale, (-35.0, 35.0))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded.color(), image::ColorType::L8);
    }

    /// Opt-in integration check against a real processed asset, writing
    /// its outputs to disk for visual inspection. Not run by default
    /// (`cargo test -- --ignored` to run it) since it depends on
    /// processing a real MALA file first.
    #[test]
    #[ignore]
    #[serial_test::serial(netcdf)]
    fn manual_visual_check_against_real_asset() {
        let dir = tempfile::tempdir().unwrap();
        let nc_path = dir.path().join("real.nc");
        let params = crate::gpr::RunParams {
            filepaths: vec![std::path::PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/mala/dronbreen-20220329-DAT_0237_A1.rad"
            ))],
            output_path: Some(nc_path.clone()),
            dem_path: None,
            cor_path: None,
            medium_velocity: 0.168,
            crs: None,
            quiet: true,
            track_path: None,
            steps: crate::gpr::default_processing_profile(),
            no_export: false,
            render_path: None,
            render_profile: None,
            render_width: None,
            render_topo: false,
            override_antenna_mhz: None,
            override_antenna_separation: None,
            user_metadata: Default::default(),
            radargram_id: Some("manual-check".to_string()),
            display_name: None,
            group: None,
            group_id: None,
        };
        crate::gpr::run(params).unwrap();

        let reader = SourceReader::open(&nc_path).unwrap();
        let renderer = Renderer::new(&reader);
        let profile = RenderProfile::default_profile();
        let seed = 0;
        let (low, high) = super::super::stats::sampled_amplitude_limits(
            &reader,
            profile.source_transform,
            profile.siglog_minval_log10,
            profile.transform,
            seed,
            0.01,
            0.99,
            profile.stats_skip_first_samples,
        )
        .unwrap();
        println!("estimated limits: {low} .. {high}");

        let (src_h, src_w) = reader.shape();
        let spec = OverviewSpec::new(src_w, src_h, 512);
        let overview_bytes = renderer
            .render_overview(&spec, &profile, (low, high))
            .unwrap();
        std::fs::write("/tmp/m4_overview.jpg", &overview_bytes).unwrap();

        let raster = ViewerRaster::new(src_w, src_h);
        let grid = raster.grid();
        for (x, y) in [(0, 0), (grid.n_cols / 2, grid.n_rows / 2)] {
            let chunk = grid.chunk(x, y).unwrap();
            let bytes = renderer
                .render_chunk(&chunk, &profile, (low, high))
                .unwrap();
            std::fs::write(format!("/tmp/m4_chunk_{x}_{y}.jpg"), &bytes).unwrap();
        }
        println!("wrote /tmp/m4_overview.jpg and /tmp/m4_chunk_*.jpg");
    }
}
