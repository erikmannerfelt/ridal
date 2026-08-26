//! Viewer raster and chunk grid geometry (#118).
//!
//! The viewer raster is a logical coordinate space, not a materialized
//! image: chunks are computed independently from their own source windows,
//! so nothing here ever needs a complete rendered array in memory.
//!
//! Source array convention (NumPy/Ridal): `data(row, col)` where `(0, 0)`
//! is the upper-left element, rows are sample indices, columns are trace
//! indices. Chunk indices are `(x, y)`, where `x` selects source columns
//! (traces) and `y` selects source rows (samples) -- physical axis labels
//! (distance, TWTT) never enter this module.

/// The viewer never renders larger than this in either dimension; larger
/// source arrays are downscaled to fit (`scale < 1`), matching or smaller
/// arrays render at `scale == 1` (an identity resampling pass).
pub const MAX_VIEWER_WIDTH: usize = 8192;
pub const MAX_VIEWER_HEIGHT: usize = 4096;

/// Fixed chunk size in pixels, matching the HDF5 storage chunking chosen in
/// M1 so that at `scale == 1` one render chunk is exactly one storage
/// chunk.
pub const CHUNK_SIZE: usize = 256;

/// The logical (never-materialized) raster that chunks are drawn from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerRaster {
    pub source_width: usize,
    pub source_height: usize,
    pub width: usize,
    pub height: usize,
    pub scale: f64,
}

impl ViewerRaster {
    /// Compute the viewer raster for a `source_width x source_height`
    /// array. `scale` is at most 1: this never upsamples.
    pub fn new(source_width: usize, source_height: usize) -> Self {
        let scale = 1.0_f64
            .min(MAX_VIEWER_WIDTH as f64 / source_width.max(1) as f64)
            .min(MAX_VIEWER_HEIGHT as f64 / source_height.max(1) as f64);
        let width = ((source_width as f64 * scale).round() as usize).max(1);
        let height = ((source_height as f64 * scale).round() as usize).max(1);
        Self {
            source_width,
            source_height,
            width,
            height,
            scale,
        }
    }

    pub fn grid(&self) -> ChunkGrid {
        ChunkGrid::new(*self)
    }
}

/// The chunk grid over a [`ViewerRaster`]: `n_cols x n_rows` chunks of
/// [`CHUNK_SIZE`], with the rightmost/bottommost row padded as needed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkGrid {
    pub raster: ViewerRaster,
    pub n_cols: usize,
    pub n_rows: usize,
}

/// A half-open source-array window `[row0, row1) x [col0, col1)`, as float
/// bounds -- the resampler's area-weighted footprints are not generally
/// integer-aligned with the source grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceWindow {
    pub row0: f64,
    pub row1: f64,
    pub col0: f64,
    pub col1: f64,
}

/// Explicit Leaflet `L.CRS.Simple` display bounds for one chunk:
/// `[[lat0, lng0], [lat1, lng1]]`, upper-left origin, y grows downward in
/// viewer pixels but *upward* in Leaflet lat (hence the negation) -- see
/// the M0 finding note: verified against a real headless-Chromium
/// screenshot that this places chunk (0,0) upper-left with no
/// transposition or mirroring.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeafletBounds {
    pub lat0: f64,
    pub lng0: f64,
    pub lat1: f64,
    pub lng1: f64,
}

/// One addressable chunk: its grid position, valid (unpadded) pixel
/// extent, source window to resample from, and display bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chunk {
    pub x: usize,
    pub y: usize,
    /// Valid width/height in pixels before padding to [`CHUNK_SIZE`]:
    /// equal to `CHUNK_SIZE` except on the rightmost column / bottommost
    /// row of the grid, where the raster may not divide evenly.
    pub valid_width: usize,
    pub valid_height: usize,
    pub source_window: SourceWindow,
    pub bounds: LeafletBounds,
}

impl ChunkGrid {
    pub fn new(raster: ViewerRaster) -> Self {
        let n_cols = raster.width.div_ceil(CHUNK_SIZE);
        let n_rows = raster.height.div_ceil(CHUNK_SIZE);
        Self {
            raster,
            n_cols,
            n_rows,
        }
    }

    /// The chunk at grid position `(x, y)`, or `None` if it falls entirely
    /// outside the grid (a structurally valid but out-of-range request,
    /// which callers should turn into a 404 rather than a 400 -- see
    /// #118's distinction between "empty but valid" and "structurally
    /// invalid").
    pub fn chunk(&self, x: usize, y: usize) -> Option<Chunk> {
        if x >= self.n_cols || y >= self.n_rows {
            return None;
        }

        let px0 = x * CHUNK_SIZE;
        let py0 = y * CHUNK_SIZE;
        let valid_width = CHUNK_SIZE.min(self.raster.width - px0);
        let valid_height = CHUNK_SIZE.min(self.raster.height - py0);

        // Source window: divide viewer pixel bounds by scale to get back
        // into source array coordinates. Half-open, float -- the resampler
        // owns rounding/footprint behavior at the edges.
        let source_window = SourceWindow {
            col0: px0 as f64 / self.raster.scale,
            col1: (px0 + valid_width) as f64 / self.raster.scale,
            row0: py0 as f64 / self.raster.scale,
            row1: (py0 + valid_height) as f64 / self.raster.scale,
        };

        // Leaflet CRS.Simple: (0,0) upper-left, lat decreases downward.
        let bounds = LeafletBounds {
            lat0: -((py0 + CHUNK_SIZE) as f64),
            lng0: px0 as f64,
            lat1: -(py0 as f64),
            lng1: (px0 + CHUNK_SIZE) as f64,
        };

        Some(Chunk {
            x,
            y,
            valid_width,
            valid_height,
            source_window,
            bounds,
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = Chunk> + '_ {
        (0..self.n_rows).flat_map(move |y| (0..self.n_cols).filter_map(move |x| self.chunk(x, y)))
    }
}

/// An overview request: downscale the complete source array to at most
/// `max_width` pixels wide, preserving the display aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverviewSpec {
    pub width: usize,
    pub height: usize,
}

impl OverviewSpec {
    pub fn new(source_width: usize, source_height: usize, max_width: usize) -> Self {
        let scale = 1.0_f64.min(max_width as f64 / source_width.max(1) as f64);
        let width = ((source_width as f64 * scale).round() as usize).max(1);
        let height = ((source_height as f64 * scale).round() as usize).max(1);
        Self { width, height }
    }

    pub fn source_window(&self, source_width: usize, source_height: usize) -> SourceWindow {
        SourceWindow {
            row0: 0.0,
            row1: source_height as f64,
            col0: 0.0,
            col1: source_width as f64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_scale_for_small_arrays() {
        let raster = ViewerRaster::new(2494, 1988);
        assert_eq!(raster.scale, 1.0);
        assert_eq!(raster.width, 2494);
        assert_eq!(raster.height, 1988);
    }

    #[test]
    fn downscales_large_arrays_preserving_aspect_within_rounding() {
        let raster = ViewerRaster::new(20000, 4000);
        assert!(raster.scale < 1.0);
        assert!(raster.width <= MAX_VIEWER_WIDTH);
        assert!(raster.height <= MAX_VIEWER_HEIGHT);
        // width-bound case: 8192/20000
        assert_eq!(raster.scale, MAX_VIEWER_WIDTH as f64 / 20000.0);
    }

    #[test]
    fn height_bound_case_is_also_handled() {
        let raster = ViewerRaster::new(4000, 20000);
        assert_eq!(raster.scale, MAX_VIEWER_HEIGHT as f64 / 20000.0);
        assert!(raster.height <= MAX_VIEWER_HEIGHT);
    }

    #[test]
    fn chunk_00_is_upper_left_with_correct_bounds() {
        let raster = ViewerRaster::new(600, 600);
        let grid = raster.grid();
        let c = grid.chunk(0, 0).unwrap();
        assert_eq!((c.x, c.y), (0, 0));
        assert_eq!(c.bounds.lat1, 0.0); // top edge at lat=0
        assert_eq!(c.bounds.lng0, 0.0); // left edge at lng=0
        assert_eq!(c.bounds.lat0, -(CHUNK_SIZE as f64));
        assert_eq!(c.bounds.lng1, CHUNK_SIZE as f64);
        // upper-left source window starts at (0,0)
        assert_eq!(c.source_window.row0, 0.0);
        assert_eq!(c.source_window.col0, 0.0);
    }

    #[test]
    fn adjacent_chunks_share_exact_boundary_no_gap_no_overlap() {
        let raster = ViewerRaster::new(600, 600);
        let grid = raster.grid();
        let c00 = grid.chunk(0, 0).unwrap();
        let c10 = grid.chunk(1, 0).unwrap();
        assert_eq!(c00.bounds.lng1, c10.bounds.lng0);
        assert_eq!(c00.source_window.col1, c10.source_window.col0);

        let c01 = grid.chunk(0, 1).unwrap();
        assert_eq!(c00.bounds.lat0, c01.bounds.lat1);
        assert_eq!(c00.source_window.row1, c01.source_window.row0);
    }

    #[test]
    fn edge_chunks_are_padded_deterministically() {
        // 600 px raster, 256 px chunks -> 3 cols/rows, last one is 600-512=88 px valid.
        let raster = ViewerRaster::new(600, 600);
        let grid = raster.grid();
        assert_eq!(grid.n_cols, 3);
        assert_eq!(grid.n_rows, 3);

        let last = grid.chunk(2, 2).unwrap();
        assert_eq!(last.valid_width, 600 - 2 * CHUNK_SIZE);
        assert_eq!(last.valid_height, 600 - 2 * CHUNK_SIZE);
        assert!(last.valid_width < CHUNK_SIZE);
        assert!(last.valid_height < CHUNK_SIZE);

        let interior = grid.chunk(0, 0).unwrap();
        assert_eq!(interior.valid_width, CHUNK_SIZE);
        assert_eq!(interior.valid_height, CHUNK_SIZE);
    }

    #[test]
    fn out_of_grid_chunk_is_none_not_a_panic() {
        let raster = ViewerRaster::new(300, 300);
        let grid = raster.grid();
        assert!(grid.chunk(999, 999).is_none());
        assert!(grid.chunk(grid.n_cols, 0).is_none());
        assert!(grid.chunk(0, grid.n_rows).is_none());
    }

    #[test]
    fn asymmetric_fixture_orientation_is_not_transposed() {
        // A distinctly non-square raster: many more columns than rows.
        // Regression guard for row/col (y/x) swap bugs.
        let raster = ViewerRaster::new(2000, 300);
        let grid = raster.grid();
        assert_eq!(grid.n_cols, 2000usize.div_ceil(CHUNK_SIZE));
        assert_eq!(grid.n_rows, 300usize.div_ceil(CHUNK_SIZE));
        assert!(grid.n_cols > grid.n_rows);

        // Chunk (1, 0) must move along columns (x), not rows (y): its
        // source window's column range advances, row range does not.
        let c00 = grid.chunk(0, 0).unwrap();
        let c10 = grid.chunk(1, 0).unwrap();
        assert_eq!(c00.source_window.row0, c10.source_window.row0);
        assert_ne!(c00.source_window.col0, c10.source_window.col0);
    }

    #[test]
    fn iter_covers_every_chunk_exactly_once() {
        let raster = ViewerRaster::new(600, 400);
        let grid = raster.grid();
        let chunks: Vec<_> = grid.iter().collect();
        assert_eq!(chunks.len(), grid.n_cols * grid.n_rows);
        let mut seen = std::collections::HashSet::new();
        for c in &chunks {
            assert!(seen.insert((c.x, c.y)), "duplicate chunk {:?}", (c.x, c.y));
        }
    }

    #[test]
    fn overview_preserves_aspect_ratio() {
        let spec = OverviewSpec::new(2494, 1988, 512);
        assert_eq!(spec.width, 512);
        let expected_height = (1988.0_f64 * (512.0 / 2494.0)).round() as usize;
        assert_eq!(spec.height, expected_height);
    }

    #[test]
    fn overview_does_not_upscale_small_arrays() {
        let spec = OverviewSpec::new(100, 50, 512);
        assert_eq!(spec.width, 100);
        assert_eq!(spec.height, 50);
    }
}
