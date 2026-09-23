//! Ties the reusable pieces together: source window -> encoded image
//! (#118).
//!
//! Render order, per #118: source amplitudes -> dataset view (standard
//! only in v1) -> float-domain resampling -> normalization -> colormap ->
//! image encoding. Amplitude limits are resolved once per call and passed
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
    /// Reads the source in horizontal bands rather than all at once. A
    /// single whole-array read is proportional to the *entire* radargram
    /// regardless of how small the overview is: the 3678x12187 file in
    /// the test corpus is ~180 MB of `f32` per call, per profile, and
    /// several concurrent index thumbnails multiply that. Banding caps
    /// the read at [`OVERVIEW_READ_BUDGET_BYTES`] while producing the
    /// same picture.
    ///
    /// For the box-footprint methods that is because each output row
    /// draws only on its own contiguous source-row footprint, so no row
    /// straddles a band boundary. Lanczos does not have that property --
    /// its kernel reaches well past the footprint -- so a band reads a
    /// halo of extra source rows on each side and resamples only the
    /// middle. Without one, every internal boundary is a false edge the
    /// kernel truncates against, and the seams are visible.
    pub fn render_overview(
        &self,
        spec: &OverviewSpec,
        profile: &RenderProfile,
        limits: (f32, f32),
    ) -> Result<Vec<u8>, String> {
        let band = self.overview_rows_per_band(spec, profile);
        self.render_overview_banded(spec, profile, limits, band)
    }

    /// Output rows per read, derived from [`OVERVIEW_READ_BUDGET_BYTES`]:
    /// how many output rows' worth of source fits in the budget. At least
    /// one, so a radargram whose single source row already exceeds the
    /// budget still renders (one row at a time) rather than dividing to
    /// zero and looping forever.
    ///
    /// `vertical_read_overhead` (zero for every source but
    /// [`crate::render::topo::TopoSource`]) is reserved out of the budget
    /// before dividing it into bands: a topographically sheared source
    /// additionally spans the shift range across a band's columns, so a
    /// band sized only from `OVERVIEW_READ_BUDGET_BYTES` would overshoot
    /// the budget by roughly that span -- a 2-3x overshoot measured on a
    /// long profile with a few hundred metres of relief. Reserving it here
    /// shrinks the band instead, so the read `read_window` actually
    /// performs (which already accounts for the shear correctly,
    /// independent of this) stays within budget.
    fn overview_rows_per_band(&self, spec: &OverviewSpec, profile: &RenderProfile) -> usize {
        let (src_h, src_w) = self.reader.shape();
        let source_rows_per_output_row = src_h as f64 / spec.height.max(1) as f64;
        // Both of the things `render_overview_banded` adds to a band's own
        // rows before reading. `vertical_read_overhead` is the shear span
        // (zero for every source but `TopoSource`); `halo` is what a
        // Lanczos kernel reaches beyond the band on each side, which is
        // `3 * scale` and therefore the larger of the two on a typical
        // ~24x overview downsample. Reserving only the first left the
        // Lanczos profiles reading past the budget this function exists to
        // enforce.
        let overhead = self.reader.vertical_read_overhead(0, src_w)
            + 2 * super::resample::halo(profile.resampling, source_rows_per_output_row);
        let bytes_per_source_row = src_w.max(1) * std::mem::size_of::<f32>();
        let max_source_rows = (OVERVIEW_READ_BUDGET_BYTES / bytes_per_source_row.max(1))
            .saturating_sub(overhead)
            .max(1);
        if source_rows_per_output_row <= 1.0 {
            // No vertical downsampling, so one output row is at most one
            // source row and the band is bounded by the budget directly
            // rather than by the ratio. Still capped: a full-resolution
            // render of a tall radargram through a shear would otherwise
            // read the whole array in one go regardless of the budget.
            return spec.height.max(1).min(max_source_rows);
        }
        ((max_source_rows as f64 / source_rows_per_output_row).floor() as usize).max(1)
    }

    /// The banded implementation behind [`Renderer::render_overview`],
    /// with the band height injected so tests can force many small bands
    /// and compare against the single-band (whole-array) path.
    fn render_overview_banded(
        &self,
        spec: &OverviewSpec,
        profile: &RenderProfile,
        limits: (f32, f32),
        out_rows_per_band: usize,
    ) -> Result<Vec<u8>, String> {
        let (src_h, src_w) = self.reader.shape();
        let out_height = spec.height.max(1);
        let source_rows_per_output_row = src_h as f64 / out_height as f64;

        let mut resampled = Array2::from_elem((out_height, spec.width), f32::NAN);
        let halo = super::resample::halo(profile.resampling, source_rows_per_output_row);
        let band = out_rows_per_band.max(1);
        let mut oy0 = 0usize;
        while oy0 < out_height {
            let oy1 = (oy0 + band).min(out_height);
            // Absolute source-row span of this band's output rows. The
            // same arithmetic the resampler would do internally for these
            // rows given the whole array, so the picture does not depend
            // on where the band boundaries fall.
            let window = SourceWindow {
                row0: oy0 as f64 * source_rows_per_output_row,
                row1: oy1 as f64 * source_rows_per_output_row,
                col0: 0.0,
                col1: src_w as f64,
            };
            // Read wider than the band, resample only the band. The
            // halo is what the kernel needs either side to see the same
            // neighbourhood it would in a whole-array render; at the
            // true top and bottom of the radargram it is clamped away,
            // which is correct -- those edges are real.
            let read_row0 = (window.row0.floor() as usize).saturating_sub(halo);
            let read_row1 = ((window.row1.ceil() as usize) + halo).min(src_h);
            let mut source = self.reader.read_window(read_row0, read_row1, 0, src_w)?;
            apply_source_transform(&mut source, profile);
            let local = SourceWindow {
                row0: window.row0 - read_row0 as f64,
                row1: window.row1 - read_row0 as f64,
                col0: 0.0,
                col1: src_w as f64,
            };
            let band_out = resample(
                source.view(),
                &local,
                spec.width,
                oy1 - oy0,
                profile.resampling,
            );
            resampled
                .slice_mut(ndarray::s![oy0..oy1, ..])
                .assign(&band_out);
            oy0 = oy1;
        }

        render_and_encode(&resampled, profile, limits)
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
    data.mapv_inplace(|v| colormap::to_source_domain(v, profile.source_transform));
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

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn banded_overview_is_identical_to_whole_array_at_an_integer_scale() {
        // Banding must change how much source is held in memory at once
        // and nothing else. At an integer downsample ratio every band
        // boundary lands on an exact source row, so the two paths agree
        // bit for bit and the comparison can be byte-exact.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_asymmetric_nc(&path, 200, 800); // 200 rows -> 50 out rows = 4x
        let reader = SourceReader::open(&path).unwrap();
        let renderer = Renderer::new(&reader);

        let spec = OverviewSpec::new(800, 200, 200);
        assert_eq!(spec.height, 50, "expected an exact 4x row ratio");
        let profile = RenderProfile {
            format: super::super::profile::ImageFormat::Png,
            ..RenderProfile::default_profile()
        };
        let limits = (0.0, (200 * 800) as f32);

        let whole = renderer
            .render_overview_banded(&spec, &profile, limits, usize::MAX)
            .unwrap();
        for band in [1usize, 2, 7, 49] {
            let banded = renderer
                .render_overview_banded(&spec, &profile, limits, band)
                .unwrap();
            assert_eq!(banded, whole, "band height {band} changed the output");
        }
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn banding_is_invisible_for_every_resampling_method() {
        // The test above covers `Mean`, which is safe to band because each
        // output row reads only its own source rows. Lanczos is not: its
        // kernel reaches `3 * scale` rows either side, and the resampler
        // truncates taps at the edge of the array it is handed. Bands were
        // read to their own extent, so every internal boundary looked like
        // the top of the radargram and left a seam -- in the server's
        // `positive` and `abslog` overviews as well as in `ridal render`.
        //
        // Parameterised over the methods rather than pinned to the two
        // profiles that use Lanczos today, so a profile switching method
        // later cannot quietly reintroduce it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_asymmetric_nc(&path, 200, 800);
        let reader = SourceReader::open(&path).unwrap();
        let renderer = Renderer::new(&reader);

        let spec = OverviewSpec::new(800, 200, 200);
        assert_eq!(spec.height, 50, "expected an exact 4x row ratio");
        let limits = (0.0, (200 * 800) as f32);

        for method in [
            super::super::profile::ResamplingMethod::Mean,
            super::super::profile::ResamplingMethod::Peak,
            super::super::profile::ResamplingMethod::Lanczos,
            super::super::profile::ResamplingMethod::LanczosRectified,
        ] {
            let profile = RenderProfile {
                format: super::super::profile::ImageFormat::Png,
                resampling: method,
                ..RenderProfile::default_profile()
            };
            let whole = renderer
                .render_overview_banded(&spec, &profile, limits, usize::MAX)
                .unwrap();
            for band in [1usize, 2, 7, 49] {
                let banded = renderer
                    .render_overview_banded(&spec, &profile, limits, band)
                    .unwrap();
                assert_eq!(
                    banded, whole,
                    "{method:?} at band height {band} differs from the \
                     whole-array render"
                );
            }
        }
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn banded_overview_matches_whole_array_at_a_fractional_scale() {
        // A non-integer ratio puts band boundaries between source rows.
        // Re-basing each band's window on its own floored origin costs a
        // little floating-point precision versus computing the same
        // footprint from the whole array, so this asserts near-identity
        // (within one grey level) rather than bit-exactness -- enough to
        // catch a real off-by-one or misplaced band, which would shift
        // content by whole rows, not by one level.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_asymmetric_nc(&path, 197, 613);
        let reader = SourceReader::open(&path).unwrap();
        let renderer = Renderer::new(&reader);

        let spec = OverviewSpec::new(613, 197, 100);
        let profile = RenderProfile {
            format: super::super::profile::ImageFormat::Png,
            ..RenderProfile::default_profile()
        };
        let limits = (0.0, (197 * 613) as f32);

        let whole = image::load_from_memory(
            &renderer
                .render_overview_banded(&spec, &profile, limits, usize::MAX)
                .unwrap(),
        )
        .unwrap()
        .to_luma8();
        let banded = image::load_from_memory(
            &renderer
                .render_overview_banded(&spec, &profile, limits, 3)
                .unwrap(),
        )
        .unwrap()
        .to_luma8();

        assert_eq!(whole.dimensions(), banded.dimensions());
        for (a, b) in whole.pixels().zip(banded.pixels()) {
            let diff = a.0[0].abs_diff(b.0[0]);
            assert!(
                diff <= 1,
                "pixel differs by {diff}, not a rounding artifact"
            );
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

        let spec = OverviewSpec::new(500, 40, 100);
        assert!(renderer.overview_rows_per_band(&spec, &RenderProfile::default_profile()) >= 1);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn a_high_relief_topo_source_shrinks_the_band_to_stay_in_budget() {
        // A pathological shift vector (traces alternating between no shift
        // and a huge one) blows up the per-band read if band sizing does
        // not account for it -- the overshoot #168 measured on a real
        // survey. `vertical_read_overhead` must shrink the band enough
        // that the band's *actual* inner read (rows_per_band *
        // source_rows_per_output_row + overhead) stays within budget.
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

        let spec = OverviewSpec::new(width, geometry.raster_height, 100);
        let profile = RenderProfile::default_profile();
        let band = renderer.overview_rows_per_band(&spec, &profile);
        assert!(band >= 1);

        let overhead = source.vertical_read_overhead(0, width);
        let source_rows_per_output_row = geometry.raster_height as f64 / spec.height.max(1) as f64;
        let estimated_read_rows = (band as f64 * source_rows_per_output_row) as usize + overhead;
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
        // the siglog processing step, then render with the base profile" --
        // compress *before* resampling, and keep the base profile's own
        // reducer. Byte-identical and with no NetCDF: one render applies the
        // source transform on the way out, the other is handed an array
        // that was already transformed.
        //
        // Parameterised over all three pairs rather than pinned to
        // `siglog-default`. The `positive` pair is the one that caught a
        // real bug: overriding its reducer to `Mean` left the signed mean
        // of oscillating siglog values near zero, which `Positive`'s black
        // level then clipped to an almost entirely black overview, while
        // "siglog step then `positive`" (which rectifies) looked right.
        let raw = ndarray::Array2::from_shape_fn((40, 60), |(r, c)| {
            let v = (r as f32 * 0.9).sin() * 30.0 + (c as f32 * 0.2).cos() * 10.0;
            if (r + c) % 7 == 0 {
                -v
            } else {
                v
            }
        });
        let mut pre = raw.clone();
        crate::filters::siglog(&mut pre, crate::filters::DEFAULT_SIGLOG_MINVAL_LOG10);

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
        let pre_source = crate::source::ArraySource::new(pre.view());
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
        ];
        for (name, siglog_base, plain_base) in pairs {
            let siglog_profile = with_explicit_limits(siglog_base.clone());
            let plain_profile = with_explicit_limits(plain_base.clone());

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
