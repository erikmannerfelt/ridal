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

use super::colormap::{self, encode};
use super::grid::{Chunk, OverviewSpec, SourceWindow};
use super::profile::RenderProfile;
use super::resample::resample;
use crate::server::source::SourceReader;

/// Fill color for pixels with no valid source data: padding beyond the
/// raster extent, or an empty resampling footprint. Mid-gray reads as
/// "no data" without the visual harshness of pure black or white against
/// real radargram content.
const PAD_VALUE: u8 = 96;

pub struct Renderer<'a> {
    reader: &'a SourceReader,
}

impl<'a> Renderer<'a> {
    pub fn new(reader: &'a SourceReader) -> Self {
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
        let source = self.read_source_for_window(&chunk.source_window, super::grid::CHUNK_SIZE)?;
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
        let image = colormap::render_grayscale(&resampled, profile, limits, PAD_VALUE);
        encode(&image, profile.format)
    }

    /// Render a full-radargram overview to encoded image bytes.
    pub fn render_overview(
        &self,
        spec: &OverviewSpec,
        profile: &RenderProfile,
        limits: (f32, f32),
    ) -> Result<Vec<u8>, String> {
        let (src_h, src_w) = self.reader.shape();
        let window = SourceWindow {
            row0: 0.0,
            row1: src_h as f64,
            col0: 0.0,
            col1: src_w as f64,
        };
        let source = self.reader.read_window(0, src_h, 0, src_w)?;
        let resampled = resample(
            source.view(),
            &window,
            spec.width,
            spec.height,
            profile.resampling,
        );
        let image = colormap::render_grayscale(&resampled, profile, limits, PAD_VALUE);
        encode(&image, profile.format)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::render::grid::ViewerRaster;
    use crate::server::render::profile::AmplitudeLimits;

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
