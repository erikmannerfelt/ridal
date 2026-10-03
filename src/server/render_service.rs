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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::render::colormap;
use crate::render::grid::{Chunk, OverviewSpec, CHUNK_SIZE};
use crate::render::profile::{AmplitudeLimits, DatasetView, RenderProfile};
use crate::render::renderer::Renderer;
use crate::render::stats::sampled_amplitude_limits;
use crate::render::topo::{self, ElevationRange, TopoGeometry, TopoSource, TopoUnavailable};
use crate::server::catalog::RevisionId;
use crate::server::overview_cache::{FileStamp, OverviewDiskCache};
use crate::source::SourceReader;

/// Bumped whenever a change to the resampling implementation would change
/// rendered pixels for existing content, so cached renders from a previous
/// version become unreachable rather than silently stale.
const RESAMPLER_VERSION: u32 = 1;
/// Bumped whenever a change to the renderer/encoder pipeline would change
/// rendered pixels or bytes for existing content.
///
/// 2: overviews average colours rather than amplitudes (#300).
const RENDERER_VERSION: u32 = 2;

/// Widest overview kept on disk (#180). The index and map thumbnails are
/// 512 px; the image-download route renders through the same call at any
/// width up to full resolution, and every such download persisted would be
/// a multi-megabyte file nobody is likely to ask for twice.
const MAX_PERSISTED_OVERVIEW_WIDTH: usize = 1024;

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
    /// `scope` is the [`RenderService`] the object belongs to (#288).
    ///
    /// The cache is shared by every radargram of every project on a
    /// server, and the revision in `variant` is only a fingerprint of a
    /// radargram id and a processing time that a file declares about
    /// itself. Two projects can hold files that declare the same pair and
    /// hold different data, so without the scope one project could be
    /// served -- or could fill the cache with -- the other's images.
    pub fn compute(
        scope: u64,
        variant: &RenderVariantId,
        descriptor: &RenderObjectDescriptor,
    ) -> Self {
        let desc = match descriptor {
            RenderObjectDescriptor::Chunk { x, y, size } => format!("chunk:{x}:{y}:{size}"),
            RenderObjectDescriptor::Overview { width, height } => {
                format!("overview:{width}:{height}")
            }
        };
        Self(blake3_hex32(&[
            &scope.to_le_bytes(),
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

/// The encoded-image cache of a whole server: one byte budget shared by
/// every [`RenderService`] built from the same [`RenderServiceConfig`]
/// (#288).
///
/// `--cache-memory-mb` used to size one cache per radargram, so the real
/// bound was that many megabytes times the number of radargrams on the
/// server. Sharing one cache makes the flag the bound it reads as, and
/// lets eviction work across radargrams: a busy one can use the space an
/// idle one no longer needs.
///
/// Cloning shares the cache. The lock is held only to look up or insert,
/// never while rendering, so radargrams contend on a hash lookup rather
/// than on each other's renders.
#[derive(Clone)]
pub struct RenderCache(Arc<Mutex<ByteBoundedCache>>);

impl RenderCache {
    pub fn new(max_bytes: usize) -> Self {
        Self(Arc::new(Mutex::new(ByteBoundedCache::new(max_bytes))))
    }

    pub fn from_mb(megabytes: usize) -> Self {
        Self::new(megabytes.saturating_mul(1024 * 1024))
    }

    /// The cache, even if a panic poisoned its lock. It holds only
    /// finished, immutable encodings, so a panic elsewhere cannot have left
    /// a half-written image in it, and losing the cache for the rest of the
    /// server's life would be worse than using it.
    fn lock(&self) -> std::sync::MutexGuard<'_, ByteBoundedCache> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn get(&self, key: &RenderObjectKey) -> Option<Vec<u8>> {
        self.lock().get(key)
    }

    pub fn insert(&self, key: RenderObjectKey, value: Vec<u8>) {
        self.lock().insert(key, value);
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn current_bytes(&self) -> usize {
        self.lock().current_bytes()
    }

    pub fn max_bytes(&self) -> usize {
        self.lock().max_bytes
    }
}

impl std::fmt::Debug for RenderCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cache = self.lock();
        f.debug_struct("RenderCache")
            .field("current_bytes", &cache.current_bytes)
            .field("max_bytes", &cache.max_bytes)
            .finish()
    }
}

/// The in-memory encoded-image budget when `--cache-memory-mb` is not given.
pub const DEFAULT_CACHE_MEMORY_MB: usize = 256;

/// Configuration the CLI (`ridal gui` / `ridal server start`) parses its
/// `--cache-memory-mb` / `--n-workers` flags into. Defined here, alongside
/// the service it configures, rather than in `cli.rs`, since the CLI only
/// needs to parse and pass these through.
///
/// Not `Copy`: it carries the server's [`RenderCache`], and every clone
/// shares it. That is what makes the budget server-wide -- a site hands the
/// same config to every project it opens.
#[derive(Debug, Clone)]
pub struct RenderServiceConfig {
    /// Every render service's encoded images, under one byte budget.
    pub cache: RenderCache,
    /// Reserved for `source.rs`'s deferred HDF5-chunk-aligned read cache;
    /// unused until that lands, and **not currently exposed as a CLI flag
    /// at all** -- always its `Default` value. Do not describe this as an
    /// inert `--source-cache-mb` flag: no such flag is parsed, so passing
    /// one is a hard CLI error, not a silent no-op.
    pub source_cache_mb: usize,
    pub n_workers: usize,
    /// Bounds how many overviews are *built* at once, server-wide (#301);
    /// see [`overview_build_permits`]. Shared by every clone, like `cache`.
    pub overview_builds: Arc<tokio::sync::Semaphore>,
    /// The radargrams with an open NetCDF handle, and how many may have one
    /// at a time (#306). Shared by every clone, like `cache`.
    pub open_readers: ReaderPool,
}

impl RenderServiceConfig {
    /// `n_workers`, and the two server-wide bounds sized from it. Use this
    /// rather than setting the field, which would leave them sized for the
    /// old value.
    pub fn with_n_workers(self, n_workers: usize) -> Self {
        Self {
            n_workers,
            overview_builds: Arc::new(tokio::sync::Semaphore::new(overview_build_permits(
                n_workers,
            ))),
            open_readers: ReaderPool::new(max_open_readers(n_workers)),
            ..self
        }
    }
}

/// Concurrent overview *builds* for `n_workers`: a quarter of them, at
/// least one (#301).
///
/// A build reads the whole radargram through a 64 MB band, plus whatever
/// HDF5 caches for the file, where a chunk reads one storage chunk. Under
/// the `--n-workers` permits alone, a card grid opened on a cold cache
/// could have every worker building an overview at once -- the memory ramp
/// in #301 -- and leave none for the viewer's chunks. Cache hits, which are
/// almost every overview request once the disk cache is warm, take no
/// build permit at all.
pub fn overview_build_permits(n_workers: usize) -> usize {
    (n_workers / 4).max(1)
}

/// How many radargrams may hold an open NetCDF handle at once (#306):
/// twice the render concurrency, at least 16, so the radargrams being
/// rendered and the ones just viewed stay open while a catalog of hundreds
/// does not hold hundreds of handles and their HDF5 caches.
pub fn max_open_readers(n_workers: usize) -> usize {
    (2 * n_workers).max(16)
}

impl Default for RenderServiceConfig {
    fn default() -> Self {
        let n_workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            cache: RenderCache::from_mb(DEFAULT_CACHE_MEMORY_MB),
            source_cache_mb: 256,
            n_workers,
            overview_builds: Arc::new(tokio::sync::Semaphore::new(1)),
            open_readers: ReaderPool::new(1),
        }
        .with_n_workers(n_workers)
    }
}

/// The server-wide set of render services holding an open NetCDF handle,
/// least recently used first (#306).
///
/// Every radargram used to keep its file open from startup for as long as
/// the server ran, so a catalog of hundreds held hundreds of HDF5 handles
/// and their per-file caches whether or not anyone looked at them. A
/// service now opens its file on first use, and [`Self::touch`] closes the
/// least recently used ones beyond the cap. Closing is only ever tried with
/// `try_lock`: a service that is busy is in use, so it is skipped rather
/// than waited for, which also means the pool can never deadlock against a
/// render.
#[derive(Debug, Clone)]
pub struct ReaderPool(Arc<Mutex<ReaderPoolInner>>);

#[derive(Debug)]
struct ReaderPoolInner {
    cap: usize,
    open: std::collections::VecDeque<std::sync::Weak<Mutex<RenderService>>>,
}

impl ReaderPool {
    pub fn new(cap: usize) -> Self {
        Self(Arc::new(Mutex::new(ReaderPoolInner {
            cap: cap.max(1),
            open: std::collections::VecDeque::new(),
        })))
    }

    /// Record that `service` was just used, then close readers of the
    /// least recently used others until at most `cap` remain open.
    ///
    /// Call it *after* releasing `service`'s own lock. A service whose
    /// reader is not open (a request answered from cache) is not recorded,
    /// so cache hits neither take a slot nor push an open one out.
    pub fn touch(&self, service: &Arc<Mutex<RenderService>>) {
        let open_now = service
            .try_lock()
            .map(|s| s.has_open_reader())
            .unwrap_or(true);
        let mut inner = self.0.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .open
            .retain(|w| w.strong_count() > 0 && !std::ptr::eq(w.as_ptr(), Arc::as_ptr(service)));
        if open_now {
            inner.open.push_back(Arc::downgrade(service));
        }
        // Each entry is examined at most once: a busy one goes to the back
        // as the most recently used, and the loop then stops rather than
        // spinning on a pool whose every member is mid-render.
        let mut budget = inner.open.len();
        while inner.open.len() > inner.cap && budget > 0 {
            budget -= 1;
            let Some(victim) = inner.open.pop_front() else {
                break;
            };
            let Some(strong) = victim.upgrade() else {
                continue;
            };
            if Arc::ptr_eq(&strong, service) {
                inner.open.push_back(victim);
                continue;
            }
            let closed = match strong.try_lock() {
                Ok(mut idle) => {
                    idle.close_reader();
                    true
                }
                Err(_) => false,
            };
            if !closed {
                inner.open.push_back(victim);
            }
        }
    }

    /// How many services are recorded as holding an open reader.
    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
    /// Opened on first use rather than at construction when the service
    /// knows its `source_path`, and closed again by [`ReaderPool`] when it
    /// has been idle longest (#306). See [`open_reader`].
    reader: Option<SourceReader>,
    /// Where to (re)open `reader` from. `None` for a service handed an
    /// already-open reader by [`Self::new`], which therefore never closes
    /// it.
    source_path: Option<PathBuf>,
    revision_id: RevisionId,
    /// Shared with every other service built from the same config.
    cache: RenderCache,
    /// This service's part of the shared cache; see
    /// [`RenderObjectKey::compute`]. A replaced service (a re-processed or
    /// re-added radargram) gets a new scope, so its predecessor's images
    /// become unreachable and age out under the budget instead of being
    /// served.
    scope: u64,
    /// Per profile: the profile to render with (an adaptive siglog
    /// strength pinned to this revision's noise floor, see
    /// [`crate::render::stats::pin_profile`]) and its amplitude limits.
    limits_cache: HashMap<RenderVariantId, (RenderProfile, (f32, f32))>,
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
    /// The project's on-disk overview store and the stamp of the file this
    /// service reads, or `None` when there is no project to keep one in
    /// (a bare directory) or the file could not be stat'ed. See
    /// [`Self::with_overview_disk_cache`].
    overview_disk: Option<(OverviewDiskCache, FileStamp)>,
}

/// Hand memory the allocator is holding but nobody is using back to the
/// operating system, after an overview build (#306).
///
/// A build allocates and frees tens of megabytes -- the source band, the
/// HDF5 chunk cache -- on whichever blocking thread ran it. glibc keeps
/// freed memory in that thread's arena rather than returning it, so a burst
/// of cold builds across many threads ratchets the process up and it never
/// comes back down: 120 radargrams requested at once left the server at
/// ~1.9 GB with no file open and nothing but small images cached.
/// `malloc_trim` returns those free pages. Other allocators return memory
/// on their own terms, so this is glibc-only and a no-op elsewhere.
fn release_freed_memory() {
    {
        let _guard = netcdf_sys::libnetcdf_lock.lock();
        // SAFETY: takes no arguments; under the lock every netcdf/HDF5
        // call in the process goes through.
        unsafe {
            hdf5_sys::h5::H5garbage_collect();
        }
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: no arguments that can be invalid; it only releases free
    // memory and is safe to call from any thread.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// `slot`'s reader, opening it from `path` first if it is closed.
///
/// A free function over the one field rather than a method, so that a
/// caller can hold the reader alongside borrows of the service's other
/// fields (its limits and geometry caches).
fn open_reader<'a>(
    slot: &'a mut Option<SourceReader>,
    path: Option<&Path>,
) -> Result<&'a SourceReader, String> {
    if slot.is_none() {
        let path = path.ok_or("this render service's reader was closed and cannot be reopened")?;
        *slot = Some(SourceReader::open(path)?);
    }
    slot.as_ref()
        .ok_or_else(|| "the reader was opened and is missing".to_string())
}

/// Hands each [`RenderService`] its own cache scope.
static NEXT_SCOPE: AtomicU64 = AtomicU64::new(0);

impl RenderService {
    pub fn new(
        reader: SourceReader,
        revision_id: RevisionId,
        config: &RenderServiceConfig,
    ) -> Self {
        Self {
            reader: Some(reader),
            source_path: None,
            revision_id,
            cache: config.cache.clone(),
            scope: NEXT_SCOPE.fetch_add(1, Ordering::Relaxed),
            limits_cache: HashMap::new(),
            topo_geometry: None,
            overview_disk: None,
        }
    }

    /// A service for the file at `path` that opens it only when a render
    /// needs it (#306), and its `(height, width)`.
    ///
    /// The file is opened once here and closed again, so that a broken
    /// file is still a clear warning at startup rather than a surprise on
    /// first view, and the catalog has the shape it lays the radargram out
    /// with.
    pub fn open_lazily(
        path: &Path,
        revision_id: RevisionId,
        config: &RenderServiceConfig,
    ) -> Result<(Self, (usize, usize)), String> {
        let shape = crate::source::AmplitudeSource::shape(&SourceReader::open(path)?);
        Ok((Self::new_closed(path, revision_id, config), shape))
    }

    fn new_closed(path: &Path, revision_id: RevisionId, config: &RenderServiceConfig) -> Self {
        Self {
            reader: None,
            source_path: Some(path.to_path_buf()),
            revision_id,
            cache: config.cache.clone(),
            scope: NEXT_SCOPE.fetch_add(1, Ordering::Relaxed),
            limits_cache: HashMap::new(),
            topo_geometry: None,
            overview_disk: None,
        }
    }

    /// Whether this service holds an open NetCDF handle right now.
    pub fn has_open_reader(&self) -> bool {
        self.reader.is_some()
    }

    /// Close the NetCDF handle, if this service can reopen it. Amplitude
    /// limits, topographic geometry and cached images are all kept, so a
    /// closed service costs one reopen on its next render, nothing more.
    pub fn close_reader(&mut self) {
        if self.source_path.is_some() {
            self.reader = None;
        }
    }

    /// Keep this service's overviews on disk as well as in memory (#180),
    /// so they survive a restart and a hit needs no read of the source.
    ///
    /// `source_path` is the file `reader` was opened from, stamped now: a
    /// file rewritten under a running server is picked up by rediscovery,
    /// which builds a new service and so a new stamp.
    pub fn with_overview_disk_cache(
        mut self,
        cache: OverviewDiskCache,
        source_path: &Path,
    ) -> Self {
        self.overview_disk = FileStamp::of(source_path).map(|stamp| (cache, stamp));
        self
    }

    /// Entries in the shared cache, every service's included.
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Bytes in the shared cache, every service's included.
    pub fn cache_bytes(&self) -> usize {
        self.cache.current_bytes()
    }

    /// Resolve a profile's amplitude limits, computing and caching them on
    /// first use. Never recomputed per chunk (#119) -- every chunk and the
    /// overview for one profile share the same call's result.
    ///
    /// Returns the profile to render with alongside: for an adaptive
    /// siglog profile, pinned to the strength the limits were estimated
    /// at, which is also resolved once here and never per chunk.
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
    fn resolve_limits(
        &mut self,
        profile: &RenderProfile,
    ) -> Result<(RenderProfile, (f32, f32)), String> {
        let key = RenderVariantId::compute(
            &self.revision_id,
            DatasetView::Standard,
            profile,
            ElevationRange::NONE,
        );
        if let Some(resolved) = self.limits_cache.get(&key) {
            return Ok(resolved.clone());
        }
        let reader = open_reader(&mut self.reader, self.source_path.as_deref())?;
        let pinned =
            crate::render::stats::pin_profile(reader, profile, crate::render::stats::SAMPLE_SEED)?;
        let sampled = match pinned.limits {
            AmplitudeLimits::Percentile { low, high } => Some(sampled_amplitude_limits(
                reader,
                pinned.source_transform,
                pinned.siglog_minval_log10,
                pinned.transform,
                crate::render::stats::SAMPLE_SEED,
                low,
                high,
                pinned.stats_skip_first_samples,
            )?),
            AmplitudeLimits::Explicit { .. } => None,
        };
        let limits = colormap::resolve_limits(&pinned, sampled)?;
        self.limits_cache.insert(key, (pinned.clone(), limits));
        Ok((pinned, limits))
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
        let reader =
            open_reader(&mut self.reader, self.source_path.as_deref()).map_err(|message| {
                TopoUnavailable {
                    cause: topo::TopoUnavailableCause::File,
                    message,
                }
            })?;
        let elevation = reader.read_axis_f64("elevation").ok();
        let depth = reader
            .read_axis_f64("depth")
            .ok()
            .map(|values| values.into_iter().map(|v| v as f32).collect::<Vec<f32>>());
        let (source_height, n_traces) = crate::source::AmplitudeSource::shape(reader);
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
            self.scope,
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
        let (pinned, limits) = self.resolve_limits(profile)?;
        let bytes = match view {
            DatasetView::Standard => {
                let reader = open_reader(&mut self.reader, self.source_path.as_deref())?;
                Renderer::new(reader).render_chunk(chunk, &pinned, limits)?
            }
            DatasetView::Topographic => {
                let geometry = self.resolve_topo_geometry(range).map_err(|e| e.message)?;
                let reader = open_reader(&mut self.reader, self.source_path.as_deref())?;
                let source = TopoSource::new(reader, &geometry);
                Renderer::new(&source).render_chunk(chunk, &pinned, limits)?
            }
        };
        self.cache.insert(key, bytes.clone());
        Ok(bytes)
    }

    /// The overview from memory or disk, without rendering; `None` when it
    /// would have to be built.
    ///
    /// What lets the overview route take a build permit (#301) only for a
    /// real build: a request this answers never waits behind one.
    pub fn cached_overview(
        &mut self,
        spec: &OverviewSpec,
        view: DatasetView,
        profile: &RenderProfile,
        range: ElevationRange,
    ) -> Option<Vec<u8>> {
        let (variant, key, disk) = self.overview_keys(spec, view, profile, range);
        if let Some(bytes) = self.cache.get(&key) {
            return Some(bytes);
        }
        let (disk, stamp) = disk?;
        let bytes = disk.get(
            &self.revision_id,
            &variant,
            stamp,
            spec.width,
            spec.height,
            profile.format,
        )?;
        self.cache.insert(key, bytes.clone());
        Some(bytes)
    }

    pub fn get_or_render_overview(
        &mut self,
        spec: &OverviewSpec,
        view: DatasetView,
        profile: &RenderProfile,
        range: ElevationRange,
    ) -> Result<Vec<u8>, String> {
        if let Some(bytes) = self.cached_overview(spec, view, profile, range) {
            return Ok(bytes);
        }
        let (variant, key, disk) = self.overview_keys(spec, view, profile, range);
        let (pinned, limits) = self.resolve_limits(profile)?;
        let bytes = match view {
            DatasetView::Standard => {
                let reader = open_reader(&mut self.reader, self.source_path.as_deref())?;
                Renderer::new(reader).render_overview(spec, &pinned, limits)?
            }
            DatasetView::Topographic => {
                let geometry = self.resolve_topo_geometry(range).map_err(|e| e.message)?;
                let reader = open_reader(&mut self.reader, self.source_path.as_deref())?;
                let source = TopoSource::new(reader, &geometry);
                Renderer::new(&source).render_overview(spec, &pinned, limits)?
            }
        };
        let bytes = match &disk {
            Some((disk, stamp)) => disk.put(
                &self.revision_id,
                &variant,
                *stamp,
                spec.width,
                spec.height,
                profile.format,
                bytes,
            ),
            None => bytes,
        };
        self.cache.insert(key, bytes.clone());
        // A build has just read the whole file through netcdf-c's chunk
        // cache, which stays allocated for as long as the file is open and
        // helps no later request: chunks read one storage chunk each, and
        // the next overview of this radargram comes from the image cache.
        // Closing frees it now instead of when the reader pool gets round to
        // it -- on 120 radargrams requested at once, the difference between
        // ending at ~1.3 GB and ~0.5 GB.
        self.close_reader();
        release_freed_memory();
        Ok(bytes)
    }

    /// An overview's variant, its memory-cache key, and the disk store to
    /// use for it -- none for one wider than
    /// [`MAX_PERSISTED_OVERVIEW_WIDTH`].
    fn overview_keys(
        &self,
        spec: &OverviewSpec,
        view: DatasetView,
        profile: &RenderProfile,
        range: ElevationRange,
    ) -> (
        RenderVariantId,
        RenderObjectKey,
        Option<(OverviewDiskCache, FileStamp)>,
    ) {
        let variant = RenderVariantId::compute(&self.revision_id, view, profile, range);
        let key = RenderObjectKey::compute(
            self.scope,
            &variant,
            &RenderObjectDescriptor::Overview {
                width: spec.width,
                height: spec.height,
            },
        );
        let disk = self
            .overview_disk
            .as_ref()
            .filter(|_| spec.width <= MAX_PERSISTED_OVERVIEW_WIDTH)
            .cloned();
        (variant, key, disk)
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
    pub fn read_trace(&mut self, trace: usize) -> Result<Option<Vec<f32>>, String> {
        use crate::source::AmplitudeSource;
        let reader = open_reader(&mut self.reader, self.source_path.as_deref())?;
        let (n_samples, n_traces) = reader.shape();
        if trace >= n_traces {
            return Ok(None);
        }
        let column = reader.read_window(0, n_samples, trace, trace + 1)?;
        Ok(Some(column.iter().copied().collect()))
    }
}

#[cfg(test)]
mod tests {
    #[test]
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
        // Few rows: the seed the equivalence is about is chosen from the
        // trace runs, so the loud columns below carry it, and every row
        // beyond a handful is resampling work this test does not read. The
        // width stays 4096 so the runs keep their 32-trace stride.
        write_trace_varying_nc(&path, 64, 4096);

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
            let spec = OverviewSpec::new(4096, 64, 300);
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

    #[test]
    #[serial_test::serial(netcdf)]
    fn the_corrected_view_is_the_same_picture_everywhere() {
        // #289: `ridal render --topo`, `ridal.render(topo=True)` (both
        // `render_topo_path_to_file`), `process --render-topo` (the array
        // in memory, with the axes it exports) and the browser's corrected
        // download must agree, as the standard view already does.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        let (height, width) = (60usize, 400usize);
        let data: Vec<f32> = (0..height * width)
            .map(|i| ((i * 7919) % 1000) as f32 - 500.0)
            .collect();
        let elevation: Vec<f64> = (0..width)
            .map(|i| 100.0 + (i as f64 * 0.05).sin() * 2.0)
            .collect();
        let depth: Vec<f32> = (0..height).map(|i| i as f32 * 0.1).collect();
        {
            let mut file = netcdf::create(&path).unwrap();
            file.add_dimension("y", height).unwrap();
            file.add_dimension("x", width).unwrap();
            let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
            var.put_values(&data, ..).unwrap();
            let mut var = file.add_variable::<f64>("elevation", &["x"]).unwrap();
            var.put_values(&elevation, ..).unwrap();
            let mut var = file.add_variable::<f32>("depth", &["y"]).unwrap();
            var.put_values(&depth, ..).unwrap();
        }
        let profile = RenderProfile {
            format: crate::render::profile::ImageFormat::Png,
            ..RenderProfile::default_profile()
        };
        let request = crate::render::oneshot::RenderRequest {
            profile: &profile,
            width: None,
            quality: None,
        };

        let from_file = dir.path().join("file.png");
        let (w, h) =
            crate::render::oneshot::render_topo_path_to_file(&path, &from_file, &request).unwrap();
        assert!(h > height, "the corrected raster is the taller one");

        let array = ndarray::Array2::from_shape_vec((height, width), data).unwrap();
        let from_memory = dir.path().join("memory.png");
        crate::render::oneshot::render_topo_to_file(
            &crate::source::ArraySource::new(array.view()),
            Some(&elevation),
            Some(&depth),
            &from_memory,
            &request,
        )
        .unwrap();

        let mut service = RenderService::new(
            SourceReader::open(&path).unwrap(),
            test_revision_id(),
            &RenderServiceConfig::default(),
        );
        let from_server = service
            .get_or_render_overview(
                &OverviewSpec {
                    width: w,
                    height: h,
                },
                DatasetView::Topographic,
                &profile,
                ElevationRange::NONE,
            )
            .unwrap();

        let from_file = std::fs::read(&from_file).unwrap();
        assert_eq!(
            from_file,
            std::fs::read(&from_memory).unwrap(),
            "file vs memory"
        );
        assert_eq!(from_file, from_server, "command line vs browser");
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
    #[serial_test::serial(netcdf)]
    fn read_trace_returns_the_stored_column_and_rejects_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace.nc");
        let (height, width) = (20usize, 300usize);
        write_test_nc(&path, height, width);
        let reader = SourceReader::open(&path).unwrap();
        let mut service = RenderService::new(
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
    #[serial_test::serial(netcdf)]
    fn seed_changes_the_limits_on_this_fixture() {
        // Guards the test below from going quietly vacuous. If the
        // fixture ever stops being seed-sensitive, an equivalence test
        // built on it proves nothing about seeds -- and the failure that
        // matters would pass unnoticed.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sensitive.nc");
        write_trace_varying_nc(&path, 64, 4096);
        let reader = SourceReader::open(&path).unwrap();

        let limits = |seed| {
            crate::render::stats::sampled_amplitude_limits(
                &reader,
                crate::render::profile::SourceTransform::None,
                crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
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
            0,
            &variant,
            &RenderObjectDescriptor::Chunk {
                x: 0,
                y: 0,
                size: 256,
            },
        );
        let other_chunk_key = RenderObjectKey::compute(
            0,
            &variant,
            &RenderObjectDescriptor::Chunk {
                x: 1,
                y: 0,
                size: 256,
            },
        );
        let overview_key = RenderObjectKey::compute(
            0,
            &variant,
            &RenderObjectDescriptor::Overview {
                width: 512,
                height: 400,
            },
        );
        let other_service_key = RenderObjectKey::compute(
            1,
            &variant,
            &RenderObjectDescriptor::Chunk {
                x: 0,
                y: 0,
                size: 256,
            },
        );
        assert_ne!(chunk_key, other_chunk_key);
        assert_ne!(chunk_key, overview_key);
        assert_ne!(
            chunk_key, other_service_key,
            "two services must not share an entry in the server-wide cache"
        );
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
    #[serial_test::serial(netcdf)]
    fn an_overview_on_disk_outlives_the_service_and_not_the_file() {
        // A fresh service with its own empty memory cache is what a
        // restart looks like. It must answer from disk -- proven by
        // planting different bytes there -- until the file it reads
        // changes, at which point the planted bytes must not be served.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_test_nc(&path, 120, 300);
        let cache_dir = dir.path().join("cache");
        let disk = OverviewDiskCache::new(&cache_dir);
        let spec = OverviewSpec::new(300, 120, 100);
        let profile = RenderProfile::default_profile();

        let fresh_service = || {
            let config = RenderServiceConfig::default();
            RenderService::new(
                SourceReader::open(&path).unwrap(),
                test_revision_id(),
                &config,
            )
            .with_overview_disk_cache(disk.clone(), &path)
        };
        let overview = |service: &mut RenderService| {
            service
                .get_or_render_overview(
                    &spec,
                    DatasetView::Standard,
                    &profile,
                    ElevationRange::NONE,
                )
                .unwrap()
        };

        let rendered = overview(&mut fresh_service());
        let stored: Vec<_> = walk_files(&cache_dir.join("overviews"));
        assert_eq!(stored.len(), 1, "{stored:?}");
        assert_eq!(std::fs::read(&stored[0]).unwrap(), rendered);

        std::fs::write(&stored[0], b"planted").unwrap();
        assert_eq!(overview(&mut fresh_service()), b"planted");

        // Rewritten in place: same revision id, different file.
        write_test_nc(&path, 120, 300);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(overview(&mut fresh_service()), rendered);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_download_sized_overview_is_not_kept_on_disk() {
        // The image-download route renders through the same call at any
        // width; only thumbnail-sized overviews are worth a file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        let width = MAX_PERSISTED_OVERVIEW_WIDTH + 100;
        write_test_nc(&path, 10, width);
        let cache_dir = dir.path().join("cache");
        let config = RenderServiceConfig::default();
        let mut service = RenderService::new(
            SourceReader::open(&path).unwrap(),
            test_revision_id(),
            &config,
        )
        .with_overview_disk_cache(OverviewDiskCache::new(&cache_dir), &path);
        service
            .get_or_render_overview(
                &OverviewSpec::new(width, 10, width),
                DatasetView::Standard,
                &RenderProfile::default_profile(),
                ElevationRange::NONE,
            )
            .unwrap();
        assert!(!cache_dir.join("overviews").exists());
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_lazy_service_opens_on_demand_and_reopens_after_closing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_test_nc(&path, 300, 300);
        let config = RenderServiceConfig::default();
        let (mut service, shape) =
            RenderService::open_lazily(&path, test_revision_id(), &config).unwrap();
        assert_eq!(shape, (300, 300));
        assert!(!service.has_open_reader());

        let grid = ViewerRaster::new(300, 300).grid();
        let render = |service: &mut RenderService, x| {
            service
                .get_or_render_chunk(
                    &grid.chunk(x, 0).unwrap(),
                    DatasetView::Standard,
                    &RenderProfile::default_profile(),
                    ElevationRange::NONE,
                )
                .unwrap()
        };
        render(&mut service, 0);
        assert!(service.has_open_reader());
        service.close_reader();
        assert!(!service.has_open_reader());
        render(&mut service, 1);
        assert!(service.has_open_reader());

        // A service handed an open reader has nowhere to reopen it from,
        // so it never closes.
        let mut fixed = RenderService::new(
            SourceReader::open(&path).unwrap(),
            test_revision_id(),
            &config,
        );
        fixed.close_reader();
        assert!(fixed.has_open_reader());
    }

    #[test]
    fn the_build_and_reader_bounds_follow_n_workers() {
        assert_eq!(overview_build_permits(1), 1);
        assert_eq!(overview_build_permits(7), 1);
        assert_eq!(overview_build_permits(16), 4);
        assert_eq!(max_open_readers(1), 16);
        assert_eq!(max_open_readers(32), 64);
        let config = RenderServiceConfig::default().with_n_workers(16);
        assert_eq!(config.overview_builds.available_permits(), 4);
    }

    fn walk_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(walk_files(&path));
            } else {
                files.push(path);
            }
        }
        files
    }

    #[test]
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
    #[serial_test::serial(netcdf)]
    fn services_from_one_config_share_one_budget() {
        // `--cache-memory-mb` bounds the server, not each radargram (#288).
        // Budget for about one chunk, then render a chunk in each of
        // several services: however many there are, the cache stays near
        // one chunk's worth instead of one per service.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_test_nc(&path, 300, 300);
        let profile = RenderProfile::default_profile();
        let chunk = ViewerRaster::new(300, 300).grid().chunk(0, 0).unwrap();

        let probe = RenderServiceConfig::default();
        let mut service = RenderService::new(
            SourceReader::open(&path).unwrap(),
            test_revision_id(),
            &probe,
        );
        service
            .get_or_render_chunk(
                &chunk,
                DatasetView::Standard,
                &profile,
                ElevationRange::NONE,
            )
            .unwrap();
        let one_chunk = probe.cache.current_bytes();
        assert!(one_chunk > 0);

        let config = RenderServiceConfig {
            cache: RenderCache::new(one_chunk + one_chunk / 2),
            ..RenderServiceConfig::default()
        };
        let mut services: Vec<RenderService> = (0..4)
            .map(|_| {
                RenderService::new(
                    SourceReader::open(&path).unwrap(),
                    test_revision_id(),
                    &config,
                )
            })
            .collect();
        for service in &mut services {
            service
                .get_or_render_chunk(
                    &chunk,
                    DatasetView::Standard,
                    &profile,
                    ElevationRange::NONE,
                )
                .unwrap();
        }
        assert!(
            config.cache.current_bytes() <= config.cache.max_bytes(),
            "{} bytes held against a {}-byte budget",
            config.cache.current_bytes(),
            config.cache.max_bytes()
        );
        assert_eq!(
            config.cache.len(),
            1,
            "the older services' chunks were evicted"
        );
        // Every service reports the one shared cache.
        assert!(services.iter().all(|s| s.cache_len() == 1));
    }

    #[test]
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
