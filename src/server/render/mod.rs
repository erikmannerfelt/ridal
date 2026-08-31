//! Turns raw source amplitude into an encoded image, with no HTTP or
//! template types anywhere in the path (#118). This module is what
//! [`crate::server::routes`] calls into; everything below is plain,
//! independently testable Rust that never materializes a full-resolution
//! image in memory.
//!
//! # Pipeline
//!
//! Source amplitude -> dataset view -> resample -> normalize -> colormap
//! -> encode, in that fixed order:
//!
//! 1. [`grid`] is pure geometry, computed with no I/O: [`grid::ViewerRaster`]
//!    maps a source array's shape to a display raster (downscaled if
//!    larger than the viewer's cap), [`grid::ChunkGrid`] divides that raster
//!    into addressable 256x256 [`grid::Chunk`]s, and [`grid::OverviewSpec`]
//!    describes a whole-radargram thumbnail. Both resolve to a
//!    [`grid::SourceWindow`] -- floating-point source-array bounds, not
//!    necessarily integer-aligned.
//! 2. [`renderer::Renderer`] reads exactly that window (via
//!    [`crate::server::source::SourceReader`], never the whole array), then
//!    calls [`resample::resample`] to reduce it to the output size.
//!    Overviews are read in bounded-size horizontal bands rather than all
//!    at once, since an overview's *input* is the entire radargram
//!    regardless of how small its output is.
//! 3. [`profile::RenderProfile`] is the one server-defined, non-free-form
//!    configuration type threading through all of this: it picks the
//!    [`profile::AmplitudeTransform`] (linear / log / asymmetric-positive),
//!    the [`profile::ResamplingMethod`], and the normalization and contrast
//!    settings. [`colormap`] applies the transform and the percentile
//!    stretch and produces the final grayscale bytes; [`stats`] is what
//!    estimates the percentile bounds in the first place, once per
//!    revision+profile from a fixed-seed sample, never per chunk -- doing
//!    it per chunk would make adjacent chunks normalize differently and
//!    produce a visible seam at every boundary.
//! 4. [`service::RenderService`] is the entry point everything above is
//!    reached through: given a [`profile::RenderProfile`] and a chunk or
//!    overview request, it resolves amplitude limits (cached separately
//!    from images, since they're reused across every chunk), checks its
//!    in-memory cache, and renders on a miss. Cache keys fold in
//!    [`crate::server::catalog::RevisionId`] plus every profile field that
//!    affects pixels, so a reprocessed file or a changed profile can never
//!    return a stale image.
//!
//! # Why resampling method is more than one knob
//!
//! Radar traces oscillate around zero, so naively averaging a downsampled
//! footprint (`ResamplingMethod::Mean`) cancels signed amplitude toward
//! zero -- fine for a plain linear profile, but it silently breaks any
//! profile whose display value is supposed to stay non-negative or
//! asymmetric (`positive`, `abslog`). `Peak` and `LanczosRectified` exist
//! to fix that without regressing the *un*-downsampled case: both must
//! degrade gracefully to the exact raw sample at a true 1:1 footprint, the
//! same way a naive box filter would -- see `resample.rs`'s module doc and
//! tests for the specific bug this graceful-degradation property guards
//! against when it's missing.
//!
//! # Testing
//!
//! Every submodule is unit-tested against synthetic arrays with no NetCDF
//! or HTTP involved. Where a real-data regression matters (edge-chunk
//! stretching, a black `positive` overview, a resampling method that
//! silently strips sign at native resolution), the module doc and test
//! name say so directly -- these were all found by looking at real
//! radargrams, not by reasoning about the code in the abstract.

// A handful of small public methods (ChunkGrid::iter, cache introspection
// on RenderService/ByteBoundedCache, etc.) exist for API completeness and
// future callers (an eventual grid-manifest endpoint, cache metrics) but
// have no caller yet. Kept rather than deleted, since removing and later
// re-adding an identical method is pure churn.
#![allow(dead_code)]

pub mod colormap;
pub mod grid;
pub mod profile;
pub mod renderer;
pub mod resample;
pub mod service;
pub mod stats;
