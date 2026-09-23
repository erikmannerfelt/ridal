//! Render identities, byte-bounded cache, and the service that ties
//! `SourceReader` + `Renderer` + cache together (#119).
//!
//! Required behavior, in order: construct the render-object key -> look
//! for an encoded result in the in-memory cache -> generate it on a miss
//! -> insert -> return. Amplitude limits are resolved once per
//! `RenderVariantId` and cached separately from the encoded images
//! themselves, which is what keeps adjacent chunks normalized identically
//! (the seam problem M4's tests already guard against).

// Cache introspection (RenderService::cache_len/cache_bytes,
// ByteBoundedCache::len/current_bytes, RenderObjectKey::as_str) exists for
// API completeness and for eventual cache metrics, and is currently only
// reached from tests. This allowance came with the file when it moved out
// of `render/mod.rs`, which carried it for the same reason.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use crate::render::colormap;
use crate::render::grid::{Chunk, OverviewSpec, CHUNK_SIZE};
use crate::render::profile::{AmplitudeLimits, DatasetView, RenderProfile};
use crate::render::renderer::Renderer;
use crate::render::stats::sampled_amplitude_limits;
use crate::render::topo::{self, ElevationRange, TopoGeometry, TopoSource, TopoUnavailable};
use crate::server::catalog::RevisionId;
use crate::source::SourceReader;

/// Bumped whenever a change to the resampling implementation would change
/// rendered pixels for existing content, so cached renders from a previous
/// version become unreachable rather than silently stale.
const RESAMPLER_VERSION: u32 = 1;
/// Bumped whenever a change to the renderer/encoder pipeline would change
/// rendered pixels or bytes for existing content.
const RENDERER_VERSION: u32 = 1;

fn blake3_hex32(parts: &[&[u8]]) -> String {
    let mut hasher = blake3::Hasher::new();
    for p in parts {
        hasher.update(p);
    }
    // First 16 bytes (32 hex chars): matches RevisionId's precedent in
    // catalog.rs -- collision-resistant among one user's radargrams and
    // profiles, not cryptographically unforgeable, and shorter than the
    // full 32-byte digest in already-long chunk/overview URLs.
    hasher.finalize().to_hex()[..32].to_string()
}

/// Identifies the selected dataset view and render profile for one
/// revision: everything that affects rendered pixels except which
/// specific chunk or overview is being requested.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderVariantId(String);

impl RenderVariantId {
    /// `elevation_range` is folded in as explicit presence flags plus
    /// `f64::to_bits()` values, not formatted decimals -- a decimal
    /// formatting change must not silently make an old cache entry
    /// unreachable, and `to_bits()` is exact where a decimal string could
    /// round two distinct bounds to the same text.
    ///
    /// Meaningful only for [`DatasetView::Topographic`], and **normalised
    /// away here** for [`DatasetView::Standard`] rather than left to
    /// callers to pass [`ElevationRange::NONE`]. Asking callers was the
    /// original design and it did not survive contact with the routes:
    /// they hand the catalog entry's configured range to every render,
    /// which is right for the corrected view and meaningless for the
    /// standard one -- so editing a floor or cap silently re-keyed every
    /// standard chunk and overview too, filling the bounded cache with
    /// duplicates of pixels that had not changed. Normalising at the one
    /// place the key is built makes that unrepresentable instead of
    /// merely discouraged.
    pub fn compute(
        revision_id: &RevisionId,
        view: DatasetView,
        profile: &RenderProfile,
        elevation_range: ElevationRange,
    ) -> Self {
        let elevation_range = match view {
            DatasetView::Standard => ElevationRange::NONE,
            DatasetView::Topographic => elevation_range,
        };
        let (has_min, min_bits) = match elevation_range.min {
            Some(v) => (1u8, v.to_bits()),
            None => (0u8, 0u64),
        };
        let (has_max, max_bits) = match elevation_range.max {
            Some(v) => (1u8, v.to_bits()),
            None => (0u8, 0u64),
        };
        Self(blake3_hex32(&[
            b"ridal-render-variant-v1",
            revision_id.as_str().as_bytes(),
            format!("{view:?}").as_bytes(),
            profile.cache_key_fragment().as_bytes(),
            &RESAMPLER_VERSION.to_le_bytes(),
            &RENDERER_VERSION.to_le_bytes(),
            &[has_min],
            &min_bits.to_le_bytes(),
            &[has_max],
            &max_bits.to_le_bytes(),
        ]))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identifies one concrete overview or image chunk within a render
/// variant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RenderObjectDescriptor {
    Chunk { x: usize, y: usize, size: usize },
    Overview { width: usize, height: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderObjectKey(String);

impl RenderObjectKey {
    pub fn compute(variant: &RenderVariantId, descriptor: &RenderObjectDescriptor) -> Self {
        let desc = match descriptor {
            RenderObjectDescriptor::Chunk { x, y, size } => format!("chunk:{x}:{y}:{size}"),
            RenderObjectDescriptor::Overview { width, height } => {
                format!("overview:{width}:{height}")
            }
        };
        Self(blake3_hex32(&[
            variant.as_str().as_bytes(),
            desc.as_bytes(),
        ]))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An in-memory LRU bounded by total encoded byte size, not item count
/// (#119: "bound the cache by encoded byte size rather than item count").
pub struct ByteBoundedCache {
    cache: lru::LruCache<RenderObjectKey, Vec<u8>>,
    current_bytes: usize,
    max_bytes: usize,
}

impl ByteBoundedCache {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            cache: lru::LruCache::unbounded(),
            current_bytes: 0,
            max_bytes,
        }
    }

    pub fn get(&mut self, key: &RenderObjectKey) -> Option<Vec<u8>> {
        self.cache.get(key).cloned()
    }

    /// Insert `value`, evicting least-recently-used entries until the
    /// cache is back under `max_bytes`. A single entry larger than the
    /// entire budget is still inserted (nothing else to evict) rather than
    /// silently refused -- correctness over strict enforcement, matching
    /// "cache write failures should not turn a successful render into an
    /// HTTP failure" in spirit (there's nothing to fail here yet, since
    /// this is memory-only; the same posture carries over once a fallible
    /// disk tier is added).
    pub fn insert(&mut self, key: RenderObjectKey, value: Vec<u8>) {
        let size = value.len();
        if let Some(old) = self.cache.put(key.clone(), value) {
            self.current_bytes -= old.len();
        }
        self.current_bytes += size;
        while self.current_bytes > self.max_bytes {
            match self.cache.peek_lru() {
                Some((lru_key, _)) if lru_key == &key && self.cache.len() == 1 => break,
                _ => {}
            }
            match self.cache.pop_lru() {
                Some((_, evicted)) => self.current_bytes -= evicted.len(),
                None => break,
            }
        }
    }

    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn current_bytes(&self) -> usize {
        self.current_bytes
    }
}

/// Configuration the CLI (`ridal gui` / `ridal server start`) parses its
/// `--cache-memory-mb` / `--n-workers` flags into. Defined here, alongside
/// the service it configures, rather than in `cli.rs`, since the CLI only
/// needs to parse and pass these through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderServiceConfig {
    pub cache_memory_mb: usize,
    /// Reserved for `source.rs`'s deferred HDF5-chunk-aligned read cache;
    /// unused until that lands, and **not currently exposed as a CLI flag
    /// at all** -- always its `Default` value. Do not describe this as an
    /// inert `--source-cache-mb` flag: no such flag is parsed, so passing
    /// one is a hard CLI error, not a silent no-op.
    pub source_cache_mb: usize,
    pub n_workers: usize,
}

impl Default for RenderServiceConfig {
    fn default() -> Self {
        Self {
            cache_memory_mb: 256,
            source_cache_mb: 256,
            n_workers: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
        }
    }
}

/// Coordinates render-object key construction, cache lookup, rendering on
/// a miss, and cache insertion for one revision's `data` variable.
///
/// Concurrency policy (the read/render split from the plan): this type's
/// own methods take `&mut self`, so callers serialize access naturally --
/// which is exactly right for the read side, since netcdf-c is not
/// thread-safe for concurrent access (confirmed the hard way in M2/M3).
/// Bounding *rendering* concurrency across multiple `RenderService`
/// instances (one per open radargram) via `--n-workers` and dispatching
/// CPU-heavy work off the async executor via `spawn_blocking` is the HTTP
/// layer's job (M6), where concurrent callers first exist; nothing here
/// should be read as already implementing that.
pub struct RenderService {
    reader: SourceReader,
    revision_id: RevisionId,
    cache: ByteBoundedCache,
    limits_cache: HashMap<RenderVariantId, (f32, f32)>,
    /// The topographic geometry resolved for the most recently requested
    /// elevation range, memoized so a burst of chunk/overview requests for
    /// the same range resolves it once. Constructed lazily -- never at
    /// startup, and never for a radargram nobody views topographically --
    /// on the first request for [`DatasetView::Topographic`], and
    /// recomputed (replacing this entry) whenever the requested range
    /// differs from the cached one, which is what makes an elevation-range
    /// override edit take effect without restarting the server: the
    /// override changes what range `routes.rs` asks for, and a changed
    /// range simply misses this cache.
    topo_geometry: Option<(ElevationRange, Arc<TopoGeometry>)>,
}

impl RenderService {
    pub fn new(
        reader: SourceReader,
        revision_id: RevisionId,
        config: &RenderServiceConfig,
    ) -> Self {
        Self {
            reader,
            revision_id,
            cache: ByteBoundedCache::new(config.cache_memory_mb * 1024 * 1024),
            limits_cache: HashMap::new(),
            topo_geometry: None,
        }
    }

    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    pub fn cache_bytes(&self) -> usize {
        self.cache.current_bytes()
    }

    /// Resolve a profile's amplitude limits, computing and caching them on
    /// first use. Never recomputed per chunk (#119) -- every chunk and the
    /// overview for one profile share the same call's result.
    ///
    /// Always sampled from the standard source, regardless of which view
    /// was actually requested (#168): the amplitude *distribution* a
    /// topographic shear relocates is unchanged by relocating it, so
    /// resampling through [`TopoSource`] here would read its NaN wedges
    /// into the percentile estimate -- shifting contrast every time the
    /// topo checkbox is ticked -- and would populate a second, redundant
    /// cache entry per profile for no reason. The cache key is therefore
    /// always computed as if the view were [`DatasetView::Standard`] and
    /// the elevation range [`ElevationRange::NONE`], so a toggle between
    /// views (or an elevation-range edit) never invalidates it.
    fn resolve_limits(&mut self, profile: &RenderProfile) -> Result<(f32, f32), String> {
        let key = RenderVariantId::compute(
            &self.revision_id,
            DatasetView::Standard,
            profile,
            ElevationRange::NONE,
        );
        if let Some(&limits) = self.limits_cache.get(&key) {
            return Ok(limits);
        }
        let sampled = match profile.limits {
            AmplitudeLimits::Percentile { low, high } => Some(sampled_amplitude_limits(
                &self.reader,
                profile.source_transform,
                profile.transform,
                crate::render::stats::SAMPLE_SEED,
                low,
                high,
                profile.stats_skip_first_samples,
            )?),
            AmplitudeLimits::Explicit { .. } => None,
        };
        let limits = colormap::resolve_limits(&profile.limits, sampled, profile.symmetric_limits)?;
        self.limits_cache.insert(key, limits);
        Ok(limits)
    }

    /// Resolve (and memoize) the topographic geometry for `range`. Reads
    /// the `elevation`/`depth` axes through the same open handle
    /// `self.reader` already holds -- no second NetCDF handle -- and
    /// otherwise does no I/O the memo can already answer.
    fn resolve_topo_geometry(
        &mut self,
        range: ElevationRange,
    ) -> Result<Arc<TopoGeometry>, TopoUnavailable> {
        if let Some((cached_range, geometry)) = &self.topo_geometry {
            if *cached_range == range {
                return Ok(Arc::clone(geometry));
            }
        }
        let elevation = self.reader.read_axis_f64("elevation").ok();
        let depth = self
            .reader
            .read_axis_f64("depth")
            .ok()
            .map(|values| values.into_iter().map(|v| v as f32).collect::<Vec<f32>>());
        let (source_height, n_traces) = crate::source::AmplitudeSource::shape(&self.reader);
        let geometry = Arc::new(topo::resolve_topo_geometry(
            elevation.as_deref(),
            depth.as_deref(),
            n_traces,
            source_height,
            range,
        )?);
        self.topo_geometry = Some((range, Arc::clone(&geometry)));
        Ok(geometry)
    }

    /// The topographically corrected raster's sample count for `range`,
    /// resolving the geometry if needed. What routing (`routes.rs`) builds
    /// its `ViewerRaster`/`ChunkGrid`/`OverviewSpec` from for the
    /// corrected view, since those have to cover the *sheared* extent, not
    /// the source one, or a corrected-view chunk below the source's own
    /// row count would 404 before ever reaching the render service.
    pub fn topo_raster_height(&mut self, range: ElevationRange) -> Result<usize, TopoUnavailable> {
        Ok(self.resolve_topo_geometry(range)?.raster_height)
    }

    /// The resolved topographic geometry for `range`, for the geometry
    /// HTTP endpoint (per-trace shifts, diagnostics) and for `routes.rs`'s
    /// availability check (a corrected-view checkbox disabled with the
    /// `Err` reason as its `title`).
    pub fn topo_geometry(
        &mut self,
        range: ElevationRange,
    ) -> Result<Arc<TopoGeometry>, TopoUnavailable> {
        self.resolve_topo_geometry(range)
    }

    pub fn get_or_render_chunk(
        &mut self,
        chunk: &Chunk,
        view: DatasetView,
        profile: &RenderProfile,
        range: ElevationRange,
    ) -> Result<Vec<u8>, String> {
        let variant = RenderVariantId::compute(&self.revision_id, view, profile, range);
        let key = RenderObjectKey::compute(
            &variant,
            &RenderObjectDescriptor::Chunk {
                x: chunk.x,
                y: chunk.y,
                size: CHUNK_SIZE,
            },
        );
        if let Some(bytes) = self.cache.get(&key) {
            return Ok(bytes);
        }
        let limits = self.resolve_limits(profile)?;
        let bytes = match view {
            DatasetView::Standard => {
                Renderer::new(&self.reader).render_chunk(chunk, profile, limits)?
            }
            DatasetView::Topographic => {
                let geometry = self.resolve_topo_geometry(range).map_err(|e| e.message)?;
                let source = TopoSource::new(&self.reader, &geometry);
                Renderer::new(&source).render_chunk(chunk, profile, limits)?
            }
        };
        self.cache.insert(key, bytes.clone());
        Ok(bytes)
    }

    pub fn get_or_render_overview(
        &mut self,
        spec: &OverviewSpec,
        view: DatasetView,
        profile: &RenderProfile,
        range: ElevationRange,
    ) -> Result<Vec<u8>, String> {
        let variant = RenderVariantId::compute(&self.revision_id, view, profile, range);
        let key = RenderObjectKey::compute(
            &variant,
            &RenderObjectDescriptor::Overview {
                width: spec.width,
                height: spec.height,
            },
        );
        if let Some(bytes) = self.cache.get(&key) {
            return Ok(bytes);
        }
        let limits = self.resolve_limits(profile)?;
        let bytes = match view {
            DatasetView::Standard => {
                Renderer::new(&self.reader).render_overview(spec, profile, limits)?
            }
            DatasetView::Topographic => {
                let geometry = self.resolve_topo_geometry(range).map_err(|e| e.message)?;
                let source = TopoSource::new(&self.reader, &geometry);
                Renderer::new(&source).render_overview(spec, profile, limits)?
            }
        };
        self.cache.insert(key, bytes.clone());
        Ok(bytes)
    }

    /// One source trace: the `data[:, trace]` column, as stored.
    ///
    /// The trace view plots this directly rather than a rendered image
    /// (#181). A single column is cheap to read and lets the browser scale
    /// and label it, which an encoded image could not. Display gain is
    /// already part of the processed `data` variable, so no render profile
    /// is applied -- deliberately, since a profile's `AbsLog`/`Positive`
    /// transform describes how amplitude maps to colour, not the waveform.
    ///
    /// `Ok(None)` for an index outside the radargram rather than a clamped
    /// column, so the route can answer 404 instead of silently serving an
    /// edge trace.
    pub fn read_trace(&self, trace: usize) -> Result<Option<Vec<f32>>, String> {
        use crate::source::AmplitudeSource;
        let (n_samples, n_traces) = self.reader.shape();
        if trace >= n_traces {
            return Ok(None);
        }
        let column = self.reader.read_window(0, n_samples, trace, trace + 1)?;
        Ok(Some(column.iter().copied().collect()))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn the_command_line_and_the_browser_draw_the_same_picture() {
        // The claim `ridal render` is built on, as an executable
        // assertion. Both paths resolve the same profile, sample the same
        // traces and call the same renderer, so one file at one width has
        // to come out byte for byte the same whether it was drawn on the
        // command line or downloaded from the browser.
        //
        // It did not, until recently: the server derived its sampling seed
        // from the render variant while the one-shot path used a constant,
        // so the two estimated amplitude limits from different traces and
        // disagreed by a shade.
        //
        // Checked for every built-in profile, including the ones whose
        // resampling reaches past a band's own rows (the Lanczos-based
        // `positive`, `abslog` and `siglog-positive`), since those are the
        // profiles whose banding had to be fixed for this to hold at all.
        use crate::render::oneshot::{render_path_to_file, RenderRequest};

        // Amplitudes that vary sharply *between* traces, which is what
        // makes this test able to fail: the seed picks the offset of the
        // sampled trace runs, so two seeds only disagree about the
        // percentiles when the traces they land on differ in amplitude.
        // A smooth fixture hides the bug completely -- the first version
        // of this test used one and passed against the very code it was
        // written to catch.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_trace_varying_nc(&path, 400, 4096);

        for name in [
            "default",
            "positive",
            "abslog",
            "siglog-default",
            "siglog-positive",
            "siglog-high-contrast",
            // The colormapped pair (#246): one grayscale-equivalent
            // pipeline on the outside, an RGB encoder (and a symmetric
            // limit pass) on the inside.
            "seismic",
            "siglog-seismic",
        ] {
            let profile = RenderProfile {
                format: crate::render::profile::ImageFormat::Png,
                ..RenderProfile::by_name(name).expect("built-in profile")
            };

            // The browser's path: through the caching render service.
            let reader = SourceReader::open(&path).unwrap();
            let mut service = RenderService::new(
                reader,
                RevisionId::fingerprint_v1(
                    &RadargramId::new("same-picture").unwrap(),
                    "2020-01-01T00:00:00Z",
                ),
                &RenderServiceConfig::default(),
            );
            let spec = OverviewSpec::new(4096, 400, 300);
            let from_server = service
                .get_or_render_overview(
                    &spec,
                    DatasetView::Standard,
                    &profile,
                    ElevationRange::NONE,
                )
                .unwrap();

            // The command line's path: straight to a file.
            let out = dir.path().join(format!("{name}.png"));
            render_path_to_file(
                &path,
                &out,
                &RenderRequest {
                    profile: &profile,
                    width: Some(300),
                    quality: None,
                },
            )
            .unwrap();
            let from_cli = std::fs::read(&out).unwrap();

            assert_eq!(
                from_cli, from_server,
                "'{name}' renders differently on the command line than in the browser"
            );
        }
    }

    use super::*;
    use crate::identity::RadargramId;
    use crate::render::grid::ViewerRaster;

    fn write_test_nc(path: &std::path::Path, height: usize, width: usize) {
        let mut file = netcdf::create(path).unwrap();
        file.add_dimension("y", height).unwrap();
        file.add_dimension("x", width).unwrap();
        let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
        let data: Vec<f32> = (0..(height * width)).map(|i| (i % 1000) as f32).collect();
        var.put_values(&data, ..).unwrap();
    }

    /// A radargram on which the sampling seed changes the answer.
    ///
    /// The shape is deliberate and looks strange on purpose. The sampler
    /// takes 128 runs of 16 consecutive traces at a fixed stride, and the
    /// seed only chooses where the first run starts. A gradient is
    /// therefore averaged over identically wherever the runs begin, and
    /// hides a seed difference completely -- a first version of the test
    /// below used one and passed against the very code it was written to
    /// catch.
    ///
    /// Loud traces clustered in the first 6 of every 32 instead, so a run
    /// of 16 either covers them or misses them entirely depending on the
    /// offset. `seed_changes_the_limits_on_this_fixture` pins that.
    fn write_trace_varying_nc(path: &std::path::Path, height: usize, width: usize) {
        let mut file = netcdf::create(path).unwrap();
        file.add_dimension("y", height).unwrap();
        file.add_dimension("x", width).unwrap();
        let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
        let data: Vec<f32> = (0..(height * width))
            .map(|i| {
                let (row, col) = (i / width, i % width);
                let quiet = ((row * 7 + col * 13) % 11) as f32 - 5.0;
                if col % 32 < 6 {
                    quiet + 5000.0
                } else {
                    quiet
                }
            })
            .collect();
        var.put_values(&data, ..).unwrap();
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn read_trace_returns_the_stored_column_and_rejects_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace.nc");
        let (height, width) = (20usize, 300usize);
        write_test_nc(&path, height, width);
        let reader = SourceReader::open(&path).unwrap();
        let service = RenderService::new(
            reader,
            RevisionId::fingerprint_v1(
                &RadargramId::new("trace-test").unwrap(),
                "2020-01-01T00:00:00Z",
            ),
            &RenderServiceConfig::default(),
        );

        let trace = 7usize;
        let column = service
            .read_trace(trace)
            .unwrap()
            .expect("trace is in range");
        assert_eq!(column.len(), height);
        let expected: Vec<f32> = (0..height)
            .map(|row| ((row * width + trace) % 1000) as f32)
            .collect();
        assert_eq!(column, expected);

        assert!(service.read_trace(width).unwrap().is_none());
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn seed_changes_the_limits_on_this_fixture() {
        // Guards the test below from going quietly vacuous. If the
        // fixture ever stops being seed-sensitive, an equivalence test
        // built on it proves nothing about seeds -- and the failure that
        // matters would pass unnoticed.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sensitive.nc");
        write_trace_varying_nc(&path, 400, 4096);
        let reader = SourceReader::open(&path).unwrap();

        let limits = |seed| {
            crate::render::stats::sampled_amplitude_limits(
                &reader,
                crate::render::profile::SourceTransform::None,
                crate::render::profile::AmplitudeTransform::Linear,
                seed,
                1.0,
                99.0,
                0,
            )
            .unwrap()
        };
        // Offset 1 lands on the loud traces; offset 7 misses them.
        assert_ne!(
            limits(1),
            limits(7),
            "the fixture is no longer seed-sensitive, so the equivalence \
             test below can no longer detect a seed difference"
        );
    }

    fn test_revision_id() -> RevisionId {
        RevisionId::fingerprint_v1(
            &RadargramId::new("test-service").unwrap(),
            "2020-01-01T00:00:00Z",
        )
    }

    #[test]
    fn variant_id_is_deterministic_and_changes_with_inputs() {
        let rev_a = test_revision_id();
        let rev_b =
            RevisionId::fingerprint_v1(&RadargramId::new("other").unwrap(), "2020-01-01T00:00:00Z");
        let profile = RenderProfile::default_profile();

        let a1 = RenderVariantId::compute(
            &rev_a,
            DatasetView::Standard,
            &profile,
            ElevationRange::NONE,
        );
        let a2 = RenderVariantId::compute(
            &rev_a,
            DatasetView::Standard,
            &profile,
            ElevationRange::NONE,
        );
        assert_eq!(a1, a2);

        let b = RenderVariantId::compute(
            &rev_b,
            DatasetView::Standard,
            &profile,
            ElevationRange::NONE,
        );
        assert_ne!(a1, b, "different revision must produce a different variant");

        let other_profile = RenderProfile::abslog_profile();
        let c = RenderVariantId::compute(
            &rev_a,
            DatasetView::Standard,
            &other_profile,
            ElevationRange::NONE,
        );
        assert_ne!(a1, c, "different profile must produce a different variant");
    }

    #[test]
    fn the_elevation_window_keys_the_corrected_view_and_only_it() {
        // #168 review: the routes hand the catalog entry's configured
        // window to *every* render, since one entry has one window. That
        // is right for the corrected view and meaningless for the standard
        // one, so normalising it away here is what stops an elevation edit
        // from re-keying -- and therefore duplicating, in a byte-bounded
        // cache -- every standard chunk and overview whose pixels did not
        // change.
        let rev = test_revision_id();
        let profile = RenderProfile::default_profile();
        let window = ElevationRange {
            min: Some(100.0),
            max: Some(600.0),
        };
        let other_window = ElevationRange {
            min: Some(150.0),
            max: Some(600.0),
        };

        let standard_none =
            RenderVariantId::compute(&rev, DatasetView::Standard, &profile, ElevationRange::NONE);
        let standard_windowed =
            RenderVariantId::compute(&rev, DatasetView::Standard, &profile, window);
        assert_eq!(
            standard_none, standard_windowed,
            "a standard render must key identically however the window is set"
        );

        let topo_none = RenderVariantId::compute(
            &rev,
            DatasetView::Topographic,
            &profile,
            ElevationRange::NONE,
        );
        let topo_windowed =
            RenderVariantId::compute(&rev, DatasetView::Topographic, &profile, window);
        let topo_other =
            RenderVariantId::compute(&rev, DatasetView::Topographic, &profile, other_window);
        assert_ne!(
            topo_none, topo_windowed,
            "the corrected view's pixels depend on the window, so its key must too"
        );
        assert_ne!(
            topo_windowed, topo_other,
            "two different windows must not share a corrected-view key"
        );
        assert_ne!(standard_none, topo_none, "the views must not share a key");
    }

    #[test]
    fn object_key_distinguishes_chunks_and_overviews() {
        let variant = RenderVariantId::compute(
            &test_revision_id(),
            DatasetView::Standard,
            &RenderProfile::default_profile(),
            ElevationRange::NONE,
        );
        let chunk_key = RenderObjectKey::compute(
            &variant,
            &RenderObjectDescriptor::Chunk {
                x: 0,
                y: 0,
                size: 256,
            },
        );
        let other_chunk_key = RenderObjectKey::compute(
            &variant,
            &RenderObjectDescriptor::Chunk {
                x: 1,
                y: 0,
                size: 256,
            },
        );
        let overview_key = RenderObjectKey::compute(
            &variant,
            &RenderObjectDescriptor::Overview {
                width: 512,
                height: 400,
            },
        );
        assert_ne!(chunk_key, other_chunk_key);
        assert_ne!(chunk_key, overview_key);
    }

    #[test]
    fn byte_bounded_cache_evicts_lru_when_over_budget() {
        let mut cache = ByteBoundedCache::new(10);
        let k = |s: &str| RenderObjectKey(s.to_string());
        cache.insert(k("a"), vec![0u8; 4]);
        cache.insert(k("b"), vec![0u8; 4]);
        assert_eq!(cache.current_bytes(), 8);
        // Touch "a" so "b" becomes the least-recently-used.
        assert!(cache.get(&k("a")).is_some());
        cache.insert(k("c"), vec![0u8; 4]); // now 12 bytes, over budget of 10
        assert!(cache.current_bytes() <= 10);
        assert!(
            cache.get(&k("b")).is_none(),
            "b should have been evicted, not a"
        );
        assert!(cache.get(&k("a")).is_some());
    }

    #[test]
    fn byte_bounded_cache_bounds_by_bytes_not_item_count() {
        let mut cache = ByteBoundedCache::new(1000);
        for i in 0..50 {
            cache.insert(RenderObjectKey(format!("k{i}")), vec![0u8; 5]);
        }
        // 50 * 5 = 250 bytes, well under the 1000-byte budget: nothing
        // evicted despite 50 items existing.
        assert_eq!(cache.len(), 50);
        assert_eq!(cache.current_bytes(), 250);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn service_caches_chunk_renders_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_test_nc(&path, 300, 300);

        let reader = SourceReader::open(&path).unwrap();
        let config = RenderServiceConfig::default();
        let mut service = RenderService::new(reader, test_revision_id(), &config);

        let raster = ViewerRaster::new(300, 300);
        let grid = raster.grid();
        let chunk = grid.chunk(0, 0).unwrap();
        let profile = RenderProfile::default_profile();

        assert_eq!(service.cache_len(), 0);
        let first = service
            .get_or_render_chunk(
                &chunk,
                DatasetView::Standard,
                &profile,
                ElevationRange::NONE,
            )
            .unwrap();
        assert_eq!(service.cache_len(), 1);

        let second = service
            .get_or_render_chunk(
                &chunk,
                DatasetView::Standard,
                &profile,
                ElevationRange::NONE,
            )
            .unwrap();
        assert_eq!(
            service.cache_len(),
            1,
            "second call must be a cache hit, not a new entry"
        );
        assert_eq!(first, second);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn service_reuses_sampled_limits_across_chunks_in_one_variant() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_test_nc(&path, 300, 600);

        let reader = SourceReader::open(&path).unwrap();
        let config = RenderServiceConfig::default();
        let mut service = RenderService::new(reader, test_revision_id(), &config);
        let raster = ViewerRaster::new(600, 300);
        let grid = raster.grid();
        let profile = RenderProfile::default_profile(); // percentile limits -> must be sampled

        service
            .get_or_render_chunk(
                &grid.chunk(0, 0).unwrap(),
                DatasetView::Standard,
                &profile,
                ElevationRange::NONE,
            )
            .unwrap();
        assert_eq!(service.limits_cache.len(), 1);

        service
            .get_or_render_chunk(
                &grid.chunk(1, 0).unwrap(),
                DatasetView::Standard,
                &profile,
                ElevationRange::NONE,
            )
            .unwrap();
        // A second chunk under the SAME variant must not add a second
        // limits entry -- it reuses the one computed for chunk (0,0).
        assert_eq!(service.limits_cache.len(), 1);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn service_gives_distinct_cache_entries_per_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_test_nc(&path, 300, 300);

        let reader = SourceReader::open(&path).unwrap();
        let config = RenderServiceConfig::default();
        let mut service = RenderService::new(reader, test_revision_id(), &config);
        let raster = ViewerRaster::new(300, 300);
        let grid = raster.grid();
        let chunk = grid.chunk(0, 0).unwrap();

        service
            .get_or_render_chunk(
                &chunk,
                DatasetView::Standard,
                &RenderProfile::default_profile(),
                ElevationRange::NONE,
            )
            .unwrap();
        service
            .get_or_render_chunk(
                &chunk,
                DatasetView::Standard,
                &RenderProfile::abslog_profile(),
                ElevationRange::NONE,
            )
            .unwrap();
        assert_eq!(service.cache_len(), 2);
    }
}
