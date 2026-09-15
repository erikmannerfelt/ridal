//! Topographic correction as a render-time decorator over `AmplitudeSource`
//! (#168).
//!
//! This is deliberately **not** `gpr.rs::correct_topography`'s
//! `data_topocorr`: nothing here is precomputed or stored, and no
//! interpretation is ever saved in the corrected coordinate space. The
//! correction is a vertical shear applied to the existing `data` array on
//! the way out (`TopoSource::read_window`), and inverted on the way back in
//! by the viewer's pick transform (`picker.js`).
//!
//! `dz` is the median positive diff of the exported `depth(y)` axis, not
//! `height / max_depth`. `GPR::depths` clamps early samples to depth 0
//! through the antenna-separation correction, so the top of the axis is
//! non-linear; the median of the *positive* diffs is robust against that
//! clamped region. `gpr.rs::correct_topography` uses the `height /
//! max_depth` approximation instead, so this view and the saved
//! `data_topocorr` product differ very slightly -- intended, not
//! reconciled.
//!
//! # Geometry
//!
//! ```text
//! elev_eff[i] = elev[i], interpolated from nearest finite neighbours when
//!               non-finite (constant extrapolation at the ends), then
//!               clamped down to `range.max` when one is configured
//! dz          = median positive diff of the depth(y) axis
//! E_top       = max_i elev_eff[i]
//! shift[i]    = (E_top - elev_eff[i]) / dz          [float, in samples, >= 0]
//! S           = max_i shift[i]
//! H_natural   = ceil(S) + H
//! H_topo      = min(H_natural, ceil((E_top - range.min) / dz))  [the floor]
//!
//! out row R on trace i  <-  source row  R - shift[i]   (NaN outside [0, H-1])
//! elevation of row R    =   E_top - R*dz
//! ```
//!
//! The configured window's two bounds are **not** two ends of one range,
//! and the asymmetry is deliberate -- see [`ElevationRange`]. `max` bounds
//! a trace's *surface* (clamping it, so a spike flattens instead of
//! stretching the view); `min` bounds the *raster* (cropping it, so
//! nothing below that elevation is drawn and no trace moves).
//!
//! Resolved once into a [`TopoGeometry`] and reused everywhere the numbers
//! are needed -- the decorator, chunk/overview routing, the geometry HTTP
//! endpoint, the CLI, and the render cache key -- rather than recomputed at
//! each site, which is a class of bug that produces subtly misplaced picks
//! rather than a crash.

use std::sync::Arc;

use ndarray::Array2;

use crate::source::AmplitudeSource;

/// A project-configured elevation window for the corrected view. The two
/// bounds do **different** things, because the two failure modes they
/// guard against are different:
///
/// - `max` is a bound on a trace's **surface elevation**. A surface above
///   it is clamped down to it, so an upward GPS spike becomes a flat
///   plateau at the cap instead of dragging the whole raster's top
///   hundreds of metres above the real terrain.
/// - `min` is the **floor of the rendered raster**: nothing is drawn below
///   that elevation, on any trace. It crops the view rather than touching
///   any trace's position, which is what stops a downward spike (or
///   simply a very deep record) from making the view enormously tall.
///
/// Between them they are what keeps erroneous elevations from silently
/// inflating the vertical bounds of the view, from either direction.
/// `None` on either end means no bound on that side; [`Self::NONE`] means
/// no configured window at all, the default for a catalog with no project.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ElevationRange {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl ElevationRange {
    pub const NONE: Self = Self {
        min: None,
        max: None,
    };
}

/// Why the topographically corrected view cannot be offered.
///
/// Split by *what could fix it*, which is the only distinction the
/// frontend needs: a file that never carried the axes is a quiet,
/// permanent limitation of that radargram, while a configured window that
/// excludes its own data is somebody's edit and is fixed by editing it
/// again. Collapsing the two meant a bad window looked exactly like an
/// unsupported file -- a checkbox that silently refused to enable, with
/// the reason only in a tooltip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopoUnavailableCause {
    /// The radargram itself cannot support the view: no `elevation`, no
    /// `depth`, a length mismatch, or no finite elevation at all. Nothing
    /// in the project's configuration will change that.
    File,
    /// The project's configured elevation window is what makes this fail.
    /// Editing it in the catalog's properties dialog will fix it.
    Window,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopoUnavailable {
    pub cause: TopoUnavailableCause,
    pub message: String,
}

impl TopoUnavailable {
    fn file(message: impl Into<String>) -> Self {
        Self {
            cause: TopoUnavailableCause::File,
            message: message.into(),
        }
    }

    fn window(message: impl Into<String>) -> Self {
        Self {
            cause: TopoUnavailableCause::Window,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for TopoUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Default for ElevationRange {
    fn default() -> Self {
        Self::NONE
    }
}

/// Ratio of the full elevation span to its 1-99% span above which a
/// radargram is flagged as likely carrying GPS spikes rather than real
/// relief.
///
/// A handful of spikes inflate the full span while barely moving the
/// robust one, so the *ratio* catches them whether the survey is on a flat
/// ice shelf (robust span near zero, so a modest spike already produces a
/// large ratio) or a steep valley glacier (robust span already large, so
/// only a genuine outlier moves the ratio this far) -- unlike a fixed
/// absolute threshold, which would miss a spike on flat ground and
/// permanently flag a genuine mountain traverse. `3.0` is a judgement
/// call, not a measured constant: it is comfortably above the ratio a
/// smoothly undulating survey produces (order 1-1.5x) and comfortably
/// below what even a single-trace spike produces on a short profile.
const SUSPECT_SPAN_RATIO: f64 = 3.0;

/// Below this many finite elevation values, a 1st/99th percentile is not a
/// meaningful estimate and is not attempted.
const MIN_FINITE_FOR_PERCENTILE: usize = 5;

/// What the elevation spread says about whether this radargram's GPS track
/// is trustworthy, computed once as part of resolving the geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TopoDiagnostics {
    /// Whether the spread looks like it contains spikes rather than real
    /// topography.
    pub suspect: bool,
    pub full_span: f64,
    pub robust_span: f64,
    /// `full_span / robust_span`. `None` when `robust_span` is zero (a
    /// constant profile, in which case `suspect` alone says whether
    /// `full_span` disagrees) or when there are too few finite values for
    /// the percentile to mean anything.
    pub ratio: Option<f64>,
    /// Elevation values that were finite, before any configured window was
    /// applied.
    pub finite_count: usize,
    /// Traces whose elevation was non-finite and therefore interpolated
    /// from their nearest finite neighbours. A trace with no elevation at
    /// all has no vertical position, so "leave it alone" is not an
    /// available behaviour -- but it is never silent.
    pub interpolated_count: usize,
    /// Traces whose surface elevation was above the configured maximum and
    /// was clamped down to it.
    pub clamped_count: usize,
    /// Raster rows dropped from the bottom by the configured minimum, i.e.
    /// how much of the view the floor is cropping away.
    pub cropped_rows: usize,
}

/// Everything downstream of the raw `elevation`/`depth` axes needs to
/// place a radargram in the topographically corrected view: the resolved
/// geometry, an [`Arc`] internally so cloning it (the render service's
/// cache key, the geometry HTTP response) is cheap.
#[derive(Debug, Clone)]
pub struct TopoGeometry {
    pub dz: f64,
    pub elevation_top: f64,
    /// Per-trace effective elevation: the file's own value, or interpolated
    /// from its nearest valid neighbours.
    pub elev_eff: Arc<[f64]>,
    /// Per-trace downward shift, in samples, `>= 0`.
    pub shift: Arc<[f32]>,
    /// `H`: the source array's own sample count.
    pub source_height: usize,
    /// `H_topo = ceil(S) + H`: the corrected raster's sample count.
    pub raster_height: usize,
    pub range: ElevationRange,
    pub diagnostics: TopoDiagnostics,
}

impl TopoGeometry {
    /// A stable identifier of this resolved geometry, for the frontend to
    /// carry as a chunk-URL query parameter so a range change (which keeps
    /// the same view/x/y/profile) busts the browser's own image cache
    /// rather than being served a stale chunk. The server does not read
    /// this back -- the current override is always the source of truth for
    /// what a chunk renders.
    pub fn fingerprint(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ridal-topo-geometry-v1");
        hasher.update(&self.dz.to_le_bytes());
        hasher.update(&self.elevation_top.to_le_bytes());
        hasher.update(&self.source_height.to_le_bytes());
        // `raster_height` is not derivable from the rest: editing only the
        // floor crops the raster while leaving `dz`, `elevation_top` and
        // every shift untouched. Without it in the hash the bottom chunk
        // row keeps its URL across that edit, and the browser serves the
        // taller cached image for a chunk whose valid extent just shrank
        // -- which `chunkBounds` then stretches into the new, shorter box.
        hasher.update(&self.raster_height.to_le_bytes());
        for s in self.shift.iter() {
            hasher.update(&s.to_le_bytes());
        }
        hasher.finalize().to_hex()[..32].to_string()
    }
}

/// Resolve a [`TopoGeometry`] from the raw `elevation`/`depth` axes.
///
/// Reads no NetCDF itself -- axis reading lives in
/// `SourceReader::read_axis_f64` -- which is what keeps this function pure
/// and its synthetic-array tests meaningful.
///
/// `elevation` is `None` when the file has no such variable at all,
/// distinct from present-but-wrong-length. `n_traces` is the source
/// array's own trace count, i.e. what `elevation.len()` must equal.
pub fn resolve_topo_geometry(
    elevation: Option<&[f64]>,
    depth: Option<&[f32]>,
    n_traces: usize,
    source_height: usize,
    range: ElevationRange,
) -> Result<TopoGeometry, TopoUnavailable> {
    let elevation = elevation.ok_or_else(|| {
        TopoUnavailable::file(
            "the topographically corrected view needs an 'elevation' variable, which this \
             radargram does not have",
        )
    })?;
    if elevation.len() != n_traces {
        return Err(TopoUnavailable::file(format!(
            "'elevation' has {} values but this radargram has {n_traces} traces",
            elevation.len()
        )));
    }

    let finite_count = elevation.iter().filter(|v| v.is_finite()).count();
    if finite_count == 0 {
        return Err(TopoUnavailable::file(
            "the topographically corrected view needs at least one finite elevation value, \
             and this radargram has none",
        ));
    }

    let depth = depth.ok_or_else(|| {
        TopoUnavailable::file(
            "the topographically corrected view needs a 'depth' axis, which this radargram \
             does not have",
        )
    })?;
    // Checked for the same reason `elevation`'s length is, and it was an
    // oversight that only one of them was: `median_positive_diff` accepts
    // any vector with one positive step, so a malformed two-value `depth`
    // would yield a plausible `dz` and silently apply the wrong vertical
    // scale to every sample row -- a wrong picture rather than a refusal.
    if depth.len() != source_height {
        return Err(TopoUnavailable::file(format!(
            "'depth' has {} values but this radargram has {source_height} samples per trace",
            depth.len()
        )));
    }
    let dz = match median_positive_diff(depth) {
        Some(dz) if dz.is_finite() && dz > 0.0 => dz,
        _ => {
            return Err(TopoUnavailable::file(
                "the 'depth' axis has no usable sample spacing (its positive diffs are \
                 non-finite or its median is not positive), so a vertical scale cannot be \
                 derived",
            ))
        }
    };

    // Only *missing* elevations are interpolated. A trace with no
    // elevation at all has no vertical position, so there is nothing to
    // leave alone; a trace outside the configured window has a perfectly
    // good position and is handled by the window's own rules below.
    let finite: Vec<bool> = elevation.iter().map(|v| v.is_finite()).collect();
    let mut elev_eff = interpolate_gaps(elevation, &finite);
    let interpolated_count = finite.iter().filter(|&&v| !v).count();

    // `max` bounds the *surface*: a spike is flattened to the cap rather
    // than lifting the whole raster's top to meet it.
    let mut clamped_count = 0usize;
    if let Some(max) = range.max {
        for e in elev_eff.iter_mut() {
            if *e > max {
                *e = max;
                clamped_count += 1;
            }
        }
    }

    let elevation_top = elev_eff.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let shift: Vec<f32> = elev_eff
        .iter()
        .map(|&e| ((elevation_top - e) / dz) as f32)
        .collect();
    let s_max = shift.iter().cloned().fold(0.0f32, f32::max);
    let natural_height = source_height + (s_max.ceil().max(0.0) as usize);

    // `min` bounds the *raster*, not any trace: it is the elevation the
    // view stops at, so it simply truncates the raster's height. Nothing
    // below it is drawn, which is what keeps a downward spike (or just a
    // very deep record) from making the view enormously tall, without
    // moving a single trace off its true position.
    let mut raster_height = natural_height;
    if let Some(min) = range.min {
        if min >= elevation_top {
            return Err(TopoUnavailable::window(format!(
                "the floor ({min} m) is at or above this radargram's highest surface \
                 ({elevation_top:.1} m), so the corrected view would have no rows left to \
                 draw. Lower the floor, or raise the surface cap, in this radargram's \
                 properties."
            )));
        }
        let floor_row = ((elevation_top - min) / dz).ceil();
        // `>= 1.0` is guaranteed by the check above, so this cannot round
        // down to a zero-height raster.
        raster_height = natural_height.min(floor_row as usize);
    }
    let cropped_rows = natural_height - raster_height;

    let mut diagnostics = compute_diagnostics(elevation, finite_count);
    diagnostics.interpolated_count = interpolated_count;
    diagnostics.clamped_count = clamped_count;
    diagnostics.cropped_rows = cropped_rows;

    Ok(TopoGeometry {
        dz,
        elevation_top,
        elev_eff: elev_eff.into(),
        shift: shift.into(),
        source_height,
        raster_height,
        range,
        diagnostics,
    })
}

/// The median of the strictly positive diffs of `depth`, robust against
/// `GPR::depths`' antenna-separation clamp of the axis's early (near-zero)
/// samples, which would otherwise pull a plain mean toward zero.
fn median_positive_diff(depth: &[f32]) -> Option<f64> {
    let mut diffs: Vec<f64> = depth
        .windows(2)
        .map(|w| (w[1] - w[0]) as f64)
        .filter(|d| *d > 0.0)
        .collect();
    if diffs.is_empty() {
        return None;
    }
    diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mid = diffs.len() / 2;
    Some(if diffs.len().is_multiple_of(2) {
        (diffs[mid - 1] + diffs[mid]) / 2.0
    } else {
        diffs[mid]
    })
}

/// Fill entries where `valid[i]` is false by linear interpolation between
/// the nearest valid neighbours either side, holding the nearest valid
/// value constant beyond the first/last valid entry. `valid` must contain
/// at least one `true` (the caller's precondition, already checked).
fn interpolate_gaps(values: &[f64], valid: &[bool]) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![0.0; n];
    let valid_indices: Vec<usize> = (0..n).filter(|&i| valid[i]).collect();
    let first = valid_indices[0];
    let last = *valid_indices.last().expect("checked non-empty by caller");

    for slot in out.iter_mut().take(first) {
        *slot = values[first];
    }
    for slot in out.iter_mut().skip(last) {
        *slot = values[last];
    }
    for w in valid_indices.windows(2) {
        let (lo, hi) = (w[0], w[1]);
        out[lo] = values[lo];
        out[hi] = values[hi];
        let span = (hi - lo) as f64;
        for (offset, slot) in out[(lo + 1)..hi].iter_mut().enumerate() {
            let t = (offset + 1) as f64 / span;
            *slot = values[lo] + t * (values[hi] - values[lo]);
        }
    }
    out
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let p = p.clamp(0.0, 1.0);
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Detect likely GPS spikes: compare the full elevation span against its
/// 1st-99th percentile span. `interpolated_count` is left at `0` -- the
/// caller fills it in, since it depends on the configured range, which this
/// function (deliberately, so the same spikes are flagged regardless of
/// where the range happens to be set) does not take.
fn compute_diagnostics(elevation: &[f64], finite_count: usize) -> TopoDiagnostics {
    let mut finite: Vec<f64> = elevation
        .iter()
        .cloned()
        .filter(|v| v.is_finite())
        .collect();
    finite.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let full_span = finite.last().unwrap() - finite.first().unwrap();

    if finite_count < MIN_FINITE_FOR_PERCENTILE {
        return TopoDiagnostics {
            suspect: false,
            full_span,
            robust_span: full_span,
            ratio: None,
            finite_count,
            interpolated_count: 0,
            clamped_count: 0,
            cropped_rows: 0,
        };
    }

    let robust_span = percentile(&finite, 0.99) - percentile(&finite, 0.01);
    let (suspect, ratio) = if robust_span == 0.0 {
        // A flat profile with an extreme spike is the worst real case: no
        // ratio is computable, but a non-zero full span next to a zero
        // robust one is unambiguous.
        (full_span != 0.0, None)
    } else {
        let ratio = full_span / robust_span;
        (ratio > SUSPECT_SPAN_RATIO, Some(ratio))
    };

    TopoDiagnostics {
        suspect,
        full_span,
        robust_span,
        ratio,
        finite_count,
        interpolated_count: 0,
        clamped_count: 0,
        cropped_rows: 0,
    }
}

/// Half-width, in source samples, of the windowed-sinc kernel that
/// resolves a trace's sub-sample vertical shift.
///
/// **Not a quality knob -- the reason the shear is not visibly banded.**
/// The obvious implementation of a fractional shift is a two-tap linear
/// blend, `(1 - f) * src[i] + f * src[i + 1]`, and on this data that is
/// wrong in a way that is easy to miss and impossible to unsee. Linear
/// interpolation is a low-pass filter whose strength depends on the
/// fraction `f`: against the high-frequency, near-uncorrelated content of
/// a deep radargram it scales amplitude by `sqrt((1 - f)^2 + f^2)`, i.e.
/// by 1.00 at `f = 0` but only 0.71 at `f = 0.5`. Since `f` is
/// `frac((E_top - elev[i]) / dz)` it sweeps the whole `[0, 1)` range as
/// the surface rises and falls, so that 29% swing lands *across traces*
/// as vertical banding -- measured on a real profile at a 30% peak-to-peak
/// contrast swing, glaring under a high-gain profile like `positive`.
///
/// A windowed-sinc kernel is the standard fix: an ideal fractional-delay
/// filter has unit magnitude at every frequency and changes only phase, so
/// amplitude stops depending on `f` at all. Truncating it to a finite
/// window costs a little of that flatness, and the measured residual swing
/// on the same profile falls off with the half-width: 14.8% at 2, 5.7% at
/// 3, 3.0% at 4, 1.6% at 6. `4` is where the curve flattens against the
/// cost -- eight taps per output sample, and eight extra source rows per
/// read rather than two.
///
/// Deliberately its own constant rather than `resample::LANCZOS_A` (which
/// is 3): that one sizes an *anti-aliasing* kernel for downsampling, where
/// the ringing of extra lobes is a real cost, while this one sizes a
/// *fractional-delay* kernel at a 1:1 rate, where a wider window is
/// straightforwardly flatter. They answer different questions and should
/// be free to move independently.
const SHIFT_KERNEL_HALF_WIDTH: i64 = 4;

/// `sinc(x) = sin(pi x) / (pi x)`, with the removable singularity at zero
/// filled in.
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let pix = std::f64::consts::PI * x;
        pix.sin() / pix
    }
}

/// The Lanczos window of [`SHIFT_KERNEL_HALF_WIDTH`], evaluated at `x`.
fn shift_kernel_weight(x: f64) -> f64 {
    let a = SHIFT_KERNEL_HALF_WIDTH as f64;
    if x.abs() >= a {
        0.0
    } else {
        sinc(x) * sinc(x / a)
    }
}

/// A decorator over `AmplitudeSource` that reports the taller, sheared
/// topographically corrected raster and assembles each output row by
/// reading the appropriately shifted source row(s) -- see the module docs
/// for the geometry.
///
/// Borrows the inner reader rather than owning one, so no second NetCDF
/// handle is opened per view. Takes an already-resolved [`TopoGeometry`]:
/// this type reads no NetCDF itself.
pub struct TopoSource<'a, S: AmplitudeSource> {
    inner: &'a S,
    geometry: &'a TopoGeometry,
}

impl<'a, S: AmplitudeSource> TopoSource<'a, S> {
    pub fn new(inner: &'a S, geometry: &'a TopoGeometry) -> Self {
        Self { inner, geometry }
    }
}

impl<S: AmplitudeSource> AmplitudeSource for TopoSource<'_, S> {
    fn shape(&self) -> (usize, usize) {
        let (_, width) = self.inner.shape();
        (self.geometry.raster_height, width)
    }

    /// For output rows `[row0, row1)` and columns `[col0, col1)`, with
    /// `K = SHIFT_KERNEL_HALF_WIDTH`:
    ///
    /// ```text
    /// lo   = row0 as f64 - max(shift[col0..col1])
    /// hi   = row1 as f64 - min(shift[col0..col1])
    /// slab = [ (floor(lo) - K + 1).clamp(0, H), (ceil(hi) + K).clamp(0, H) ]
    /// ```
    ///
    /// The `K` margins are the interpolation kernel's reach either side of
    /// the samples this window actually lands on. Without them every chunk
    /// edge and every overview band boundary would lose the contributions
    /// its kernel needs from just outside, and seam.
    ///
    /// Per output cell at row `R`, column `c`, with `p = R - shift[c]`,
    /// `i = floor(p)`, `f = p - i`:
    ///
    /// - `f == 0` takes `src[i]` exactly, with no kernel at all. Not a
    ///   special case for its own sake: the kernel's taps land on integers
    ///   there, where `sinc` is zero everywhere but the centre, so this is
    ///   the value the general branch would compute anyway -- taken
    ///   directly so the "shift is a whole number of samples" path is
    ///   exact rather than exact-to-rounding, which is what keeps the
    ///   final source row renderable rather than NaN for want of a row `H`
    ///   that does not exist.
    /// - `f != 0` evaluates the windowed-sinc kernel over
    ///   `[i - K + 1, i + K]`. Taps outside `[0, H - 1]` and non-finite
    ///   taps are dropped and the surviving weights renormalised, the same
    ///   contract `resample.rs`'s Lanczos path follows: NaN means "no data
    ///   here", so it must not drag the result toward zero.
    ///
    /// The *data band* is unchanged by the wider kernel: an output row is
    /// NaN unless `i` and `i + 1` are both inside `[0, H - 1]`, exactly as
    /// the two-tap version required. That keeps the wedge boundary, and
    /// therefore the frontend's `chunkDataBand` culling, identical -- the
    /// kernel changes what a renderable sample *is*, never which samples
    /// are renderable.
    fn read_window(
        &self,
        row0: usize,
        row1: usize,
        col0: usize,
        col1: usize,
    ) -> Result<Array2<f32>, String> {
        let (raster_h, width) = self.shape();
        let row1 = row1.min(raster_h);
        let col1 = col1.min(width);
        if row0 >= row1 || col0 >= col1 {
            return Ok(Array2::from_elem((0, 0), 0.0));
        }
        let h = self.geometry.source_height;
        let shifts = &self.geometry.shift[col0..col1];
        let max_shift = shifts.iter().cloned().fold(f32::NEG_INFINITY, f32::max) as f64;
        let min_shift = shifts.iter().cloned().fold(f32::INFINITY, f32::min) as f64;

        let k = SHIFT_KERNEL_HALF_WIDTH;
        let lo = row0 as f64 - max_shift;
        let hi = row1 as f64 - min_shift;
        let slab_row0 = ((lo.floor() as i64 - k + 1).max(0) as usize).min(h);
        let slab_row1 = ((hi.ceil() as i64 + k).max(0) as usize).min(h);

        let slab = if slab_row0 < slab_row1 {
            self.inner.read_window(slab_row0, slab_row1, col0, col1)?
        } else {
            Array2::from_elem((0, col1 - col0), f32::NAN)
        };

        let mut out = Array2::from_elem((row1 - row0, col1 - col0), f32::NAN);
        for (local_c, &s) in shifts.iter().enumerate() {
            let shift = s as f64;
            let read_tap = |idx: i64| -> f32 {
                if idx < 0 {
                    return f32::NAN;
                }
                let idx = idx as usize;
                if idx >= h || idx < slab_row0 || idx >= slab_row1 {
                    return f32::NAN;
                }
                slab[[idx - slab_row0, local_c]]
            };

            for r in row0..row1 {
                let p = r as f64 - shift;
                let ip = p.floor();
                let f = p - ip;
                let i0 = ip as i64;

                let value = if f == 0.0 {
                    read_tap(i0)
                } else if !read_tap(i0).is_finite() || !read_tap(i0 + 1).is_finite() {
                    // Outside the data band (or straddling a genuine
                    // no-data sample): NaN, on exactly the same condition
                    // the two-tap version used, so the wedge does not move.
                    f32::NAN
                } else {
                    let mut acc = 0.0_f64;
                    let mut wsum = 0.0_f64;
                    for idx in (i0 - k + 1)..=(i0 + k) {
                        let v = read_tap(idx);
                        if !v.is_finite() {
                            continue;
                        }
                        let w = shift_kernel_weight(p - idx as f64);
                        acc += v as f64 * w;
                        wsum += w;
                    }
                    // Lanczos weights are signed, so a footprint whose
                    // surviving taps nearly cancel would divide by ~0.
                    // Fall back to the two central taps there rather than
                    // producing a wild value -- same guard, and same
                    // threshold, as `resample::resample_lanczos`.
                    if wsum.abs() > 1e-6 {
                        (acc / wsum) as f32
                    } else {
                        let v0 = read_tap(i0) as f64;
                        let v1 = read_tap(i0 + 1) as f64;
                        ((1.0 - f) * v0 + f * v1) as f32
                    }
                };
                out[[r - row0, local_c]] = value;
            }
        }
        Ok(out)
    }

    /// Source rows a read over `[col0, col1)` fetches beyond the naive
    /// `row1 - row0` its caller asked for, from the shift spanning that
    /// column range. Column-ranged because the two callers need different
    /// answers: a 256-column chunk needs only its local shift span, while a
    /// full-width overview band needs the global one.
    ///
    /// Used by `Renderer::overview_rows_per_band` to size bands so their
    /// *actual* reads (which `read_window` above already computes
    /// correctly regardless) stay within the overview read budget, rather
    /// than overshooting it by the shift span on every band.
    fn vertical_read_overhead(&self, col0: usize, col1: usize) -> usize {
        let width = self.geometry.shift.len();
        let col1 = col1.min(width);
        let col0 = col0.min(col1);
        if col0 >= col1 {
            return 0;
        }
        let shifts = &self.geometry.shift[col0..col1];
        let max_shift = shifts.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let min_shift = shifts.iter().cloned().fold(f32::INFINITY, f32::min);
        // The shift span across these columns, plus the interpolation
        // kernel's own reach either side of the rows the window lands on.
        (max_shift - min_shift).ceil().max(0.0) as usize + 2 * SHIFT_KERNEL_HALF_WIDTH as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ArraySource;

    fn flat_depth(height: usize, dz: f32) -> Vec<f32> {
        (0..height).map(|i| i as f32 * dz).collect()
    }

    // --- Geometry resolution: the usability table -------------------------

    #[test]
    fn missing_elevation_variable_is_unavailable() {
        let err = resolve_topo_geometry(
            None,
            Some(&flat_depth(10, 0.1)),
            5,
            10,
            ElevationRange::NONE,
        )
        .unwrap_err();
        assert!(err.message.contains("elevation"), "{err}");
    }

    #[test]
    fn elevation_length_mismatch_is_unavailable() {
        let elevation = vec![100.0; 3];
        let err = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(10, 0.1)),
            5,
            10,
            ElevationRange::NONE,
        )
        .unwrap_err();
        assert!(err.message.contains("traces"), "{err}");
    }

    #[test]
    fn no_finite_elevation_is_unavailable() {
        let elevation = vec![f64::NAN; 5];
        let err = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(10, 0.1)),
            5,
            10,
            ElevationRange::NONE,
        )
        .unwrap_err();
        assert!(err.message.contains("finite"), "{err}");
    }

    #[test]
    fn missing_depth_axis_is_unavailable() {
        let elevation = vec![100.0; 5];
        let err =
            resolve_topo_geometry(Some(&elevation), None, 5, 10, ElevationRange::NONE).unwrap_err();
        assert!(err.message.contains("depth"), "{err}");
    }

    #[test]
    fn non_positive_dz_is_unavailable() {
        let elevation = vec![100.0; 5];
        let flat = vec![0.0f32; 10]; // no positive diffs at all
        let err = resolve_topo_geometry(Some(&elevation), Some(&flat), 5, 10, ElevationRange::NONE)
            .unwrap_err();
        assert!(err.message.contains("sample spacing"), "{err}");
    }

    #[test]
    fn a_floor_at_or_above_the_surface_is_unavailable_and_blames_the_window() {
        // The one way a configured window can still refuse outright:
        // cropping at or above the highest surface leaves no rows at all.
        // Reported as a *window* problem, not a file one, because editing
        // the window is what fixes it -- the distinction the frontend uses
        // to decide between a quiet disabled checkbox and a visible
        // warning.
        let elevation = vec![100.0; 5];
        let range = ElevationRange {
            min: Some(200.0),
            max: None,
        };
        let err = resolve_topo_geometry(Some(&elevation), Some(&flat_depth(10, 0.1)), 5, 10, range)
            .unwrap_err();
        assert_eq!(err.cause, TopoUnavailableCause::Window);
        assert!(err.message.contains("floor"), "{err}");

        // A file that simply lacks the axes is the other cause, and must
        // stay distinguishable from it.
        let missing =
            resolve_topo_geometry(None, Some(&flat_depth(10, 0.1)), 5, 10, range).unwrap_err();
        assert_eq!(missing.cause, TopoUnavailableCause::File);
    }

    #[test]
    fn a_floor_crops_the_raster_without_moving_any_trace() {
        // `min` is the floor of the rendered raster, not a test on any
        // trace's own elevation: it truncates the view and leaves every
        // shift exactly where it was.
        let dz = 0.1;
        let elevation = vec![100.0, 99.5, 100.0];
        let uncropped = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(200, dz as f32)),
            3,
            200,
            ElevationRange::NONE,
        )
        .unwrap();

        // Floor 5 m below the top: (100 - 95) / 0.1 = 50 rows.
        let range = ElevationRange {
            min: Some(95.0),
            max: None,
        };
        let cropped = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(200, dz as f32)),
            3,
            200,
            range,
        )
        .unwrap();

        assert_eq!(cropped.raster_height, 50);
        assert!(cropped.raster_height < uncropped.raster_height);
        assert_eq!(
            cropped.diagnostics.cropped_rows,
            uncropped.raster_height - 50
        );
        // Not one trace moved: same shifts, same top.
        assert_eq!(cropped.shift, uncropped.shift);
        assert_eq!(cropped.elevation_top, uncropped.elevation_top);
        assert_eq!(cropped.diagnostics.clamped_count, 0);
    }

    #[test]
    fn the_fingerprint_changes_when_only_the_floor_does() {
        // #168 review: the floor crops the raster while leaving `dz`,
        // `elevation_top` and every shift untouched, so a fingerprint over
        // those alone is identical across a floor-only edit -- and the
        // browser then reuses the bottom chunk it cached for the taller
        // raster, which `chunkBounds` stretches into the shorter box.
        let elevation = vec![100.0, 99.5, 100.0];
        let geom = |range| {
            resolve_topo_geometry(Some(&elevation), Some(&flat_depth(200, 0.1)), 3, 200, range)
                .unwrap()
        };
        let uncropped = geom(ElevationRange::NONE);
        let cropped = geom(ElevationRange {
            min: Some(95.0),
            max: None,
        });
        let cropped_more = geom(ElevationRange {
            min: Some(96.0),
            max: None,
        });

        assert_eq!(cropped.shift, uncropped.shift, "the shear is untouched");
        assert_eq!(cropped.dz, uncropped.dz);
        assert_eq!(cropped.elevation_top, uncropped.elevation_top);
        // ...so only `raster_height` distinguishes them, and the
        // fingerprint has to notice it.
        assert_ne!(cropped.fingerprint(), uncropped.fingerprint());
        assert_ne!(cropped.fingerprint(), cropped_more.fingerprint());
        // Same inputs still give the same answer.
        assert_eq!(cropped.fingerprint(), geom(cropped.range).fingerprint());
    }

    #[test]
    fn a_floor_below_the_data_crops_nothing() {
        let elevation = vec![100.0, 99.5, 100.0];
        let range = ElevationRange {
            min: Some(-1000.0),
            max: None,
        };
        let geometry =
            resolve_topo_geometry(Some(&elevation), Some(&flat_depth(200, 0.1)), 3, 200, range)
                .unwrap();
        assert_eq!(geometry.diagnostics.cropped_rows, 0);
        assert_eq!(geometry.raster_height, 200 + 5);
    }

    #[test]
    fn a_flat_elevation_profile_reduces_to_the_identity() {
        let elevation = vec![100.0; 5];
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(10, 0.1)),
            5,
            10,
            ElevationRange::NONE,
        )
        .unwrap();
        assert_eq!(geometry.raster_height, 10);
        assert!(geometry.shift.iter().all(|&s| s == 0.0));
        assert!(!geometry.diagnostics.suspect);
        assert_eq!(geometry.diagnostics.interpolated_count, 0);
    }

    #[test]
    fn individual_non_finite_elevations_are_interpolated_and_counted() {
        // A spike (NaN) at the first and last trace: the edges the
        // constant-extrapolation rule exists for.
        let elevation = vec![f64::NAN, 100.0, 110.0, 120.0, f64::NAN];
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(10, 0.1)),
            5,
            10,
            ElevationRange::NONE,
        )
        .unwrap();
        assert_eq!(geometry.diagnostics.interpolated_count, 2);
        assert_eq!(
            geometry.elev_eff[0], 100.0,
            "held constant at the first valid value"
        );
        assert_eq!(
            geometry.elev_eff[4], 120.0,
            "held constant at the last valid value"
        );
    }

    #[test]
    fn a_gap_interpolates_linearly_between_its_valid_neighbours() {
        let elevation = vec![100.0, f64::NAN, f64::NAN, 130.0];
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(10, 0.1)),
            4,
            10,
            ElevationRange::NONE,
        )
        .unwrap();
        assert!((geometry.elev_eff[1] - 110.0).abs() < 1e-9);
        assert!((geometry.elev_eff[2] - 120.0).abs() < 1e-9);
    }

    #[test]
    fn a_surface_above_the_maximum_is_clamped_to_it() {
        // `max` bounds the surface: an upward GPS spike is flattened to
        // the cap rather than lifting the whole raster's top to meet it.
        // Without the clamp this spike alone would put `elevation_top` at
        // 9999 and make the raster ~99 000 rows tall.
        let elevation = vec![100.0, 9999.0, 100.0];
        let range = ElevationRange {
            min: None,
            max: Some(200.0),
        };
        let geometry =
            resolve_topo_geometry(Some(&elevation), Some(&flat_depth(10, 0.1)), 3, 10, range)
                .unwrap();
        assert_eq!(geometry.diagnostics.clamped_count, 1);
        assert_eq!(geometry.elev_eff[1], 200.0, "flattened to the cap");
        assert_eq!(geometry.elevation_top, 200.0);
        // Untouched traces keep their real elevation.
        assert_eq!(geometry.elev_eff[0], 100.0);
        assert_eq!(geometry.elev_eff[2], 100.0);
        // And nothing was interpolated -- the clamp is not a validity test.
        assert_eq!(geometry.diagnostics.interpolated_count, 0);
    }

    #[test]
    fn a_surface_below_the_floor_keeps_its_real_elevation() {
        // The floor crops the view; it must never pull a trace up to
        // itself the way the maximum pulls one down. A trace sitting below
        // the floor keeps its true position -- its data simply falls
        // outside the rows the raster covers.
        let elevation = vec![100.0, 90.0, 100.0];
        let range = ElevationRange {
            min: Some(95.0),
            max: None,
        };
        let geometry =
            resolve_topo_geometry(Some(&elevation), Some(&flat_depth(200, 0.1)), 3, 200, range)
                .unwrap();
        assert_eq!(geometry.elev_eff[1], 90.0, "not raised to the floor");
        assert_eq!(geometry.diagnostics.clamped_count, 0);
        assert_eq!(geometry.diagnostics.interpolated_count, 0);
    }

    // --- Diagnostics --------------------------------------------------------

    #[test]
    fn a_healthy_spread_is_not_flagged() {
        let elevation: Vec<f64> = (0..20).map(|i| 100.0 + i as f64 * 0.5).collect();
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(10, 0.1)),
            20,
            10,
            ElevationRange::NONE,
        )
        .unwrap();
        assert!(!geometry.diagnostics.suspect);
        assert!(geometry.diagnostics.ratio.unwrap() < SUSPECT_SPAN_RATIO);
    }

    #[test]
    fn a_single_spike_is_flagged() {
        // 200 traces so the 1st/99th percentile has enough points to trim
        // a single outlier rather than landing on it: with too few points,
        // 1% of the count rounds to the extreme itself and the spike
        // pollutes the "robust" span too.
        let mut elevation: Vec<f64> = (0..200).map(|i| 100.0 + (i as f64 * 0.01)).collect();
        elevation[100] = 5000.0;
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(10, 0.1)),
            200,
            10,
            ElevationRange::NONE,
        )
        .unwrap();
        assert!(geometry.diagnostics.suspect);
        assert!(geometry.diagnostics.ratio.unwrap() > SUSPECT_SPAN_RATIO);
    }

    #[test]
    fn a_constant_profile_is_healthy_with_zero_spans() {
        let elevation = vec![100.0; 10];
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(10, 0.1)),
            10,
            10,
            ElevationRange::NONE,
        )
        .unwrap();
        assert!(!geometry.diagnostics.suspect);
        assert_eq!(geometry.diagnostics.full_span, 0.0);
    }

    #[test]
    fn a_flat_profile_with_one_spike_is_flagged_without_a_ratio() {
        // 200 traces, all but one identical, so the 1st/99th percentile
        // excludes the single spike entirely and the robust span is
        // exactly zero -- the degenerate case that skips computing a
        // ratio (dividing by zero) rather than reporting one.
        let mut elevation = vec![100.0; 200];
        elevation[199] = 500.0;
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(10, 0.1)),
            200,
            10,
            ElevationRange::NONE,
        )
        .unwrap();
        assert!(geometry.diagnostics.suspect);
        assert!(geometry.diagnostics.ratio.is_none());
    }

    #[test]
    fn too_few_finite_values_is_indeterminate_not_flagged() {
        let elevation = vec![100.0, f64::NAN, f64::NAN, 300.0];
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(10, 0.1)),
            4,
            10,
            ElevationRange::NONE,
        )
        .unwrap();
        assert!(geometry.diagnostics.ratio.is_none());
        assert!(!geometry.diagnostics.suspect);
    }

    // --- TopoSource -----------------------------------------------------------

    /// Two traces at different elevation, a third row height so the shear
    /// is visible: trace 0 sits at `E_top` (shift 0, the reference surface
    /// this whole raster is levelled to), trace 1 is one `dz` lower and so
    /// shifts *down* by exactly one whole sample -- the lower trace is the
    /// one that moves, since its column has to be pushed down to align its
    /// surface with the higher one's.
    fn sheared_geometry() -> (Array2<f32>, TopoGeometry) {
        let dz = 1.0;
        let source = Array2::from_shape_fn((4, 2), |(r, c)| (r * 2 + c) as f32);
        let elevation = vec![101.0, 100.0]; // trace 1 is 1 unit (1 sample) lower
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(4, dz as f32)),
            2,
            4,
            ElevationRange::NONE,
        )
        .unwrap();
        (source, geometry)
    }

    #[test]
    fn shape_reports_the_taller_sheared_raster() {
        let (source, geometry) = sheared_geometry();
        let inner = ArraySource::new(source.view());
        let topo = TopoSource::new(&inner, &geometry);
        assert_eq!(geometry.raster_height, 5); // ceil(1) + 4
        assert_eq!(topo.shape(), (5, 2));
    }

    #[test]
    fn shear_shifts_the_lower_trace_down_by_a_whole_sample() {
        let (source, geometry) = sheared_geometry();
        let inner = ArraySource::new(source.view());
        let topo = TopoSource::new(&inner, &geometry);
        let full = topo.read_window(0, 5, 0, 2).unwrap();

        // Trace 0 (elevation 101, at E_top, shift 0) is untouched.
        for r in 0..4 {
            assert_eq!(full[[r, 0]], source[[r, 0]]);
        }
        assert!(full[[4, 0]].is_nan());

        // Trace 1 (elevation 100, one dz lower, shift 1) moves down by
        // exactly one row: out row R holds source row R-1, and out row 0
        // (nothing above the source) is NaN.
        assert!(full[[0, 1]].is_nan());
        for r in 1..5 {
            assert_eq!(full[[r, 1]], source[[r - 1, 1]]);
        }
    }

    #[test]
    fn the_final_source_row_is_renderable_at_an_integral_shift() {
        // Regression guard for the f==0 special case: without it, the last
        // source row (which has no row H to pair with) would be NaN even
        // though its shift is a whole number.
        let (_, geometry) = sheared_geometry();
        let source = Array2::from_elem((4, 2), 1.0f32);
        let inner = ArraySource::new(source.view());
        let topo = TopoSource::new(&inner, &geometry);
        let full = topo.read_window(0, 5, 0, 2).unwrap();
        assert_eq!(full[[4, 1]], 1.0, "row 4 = source row 3, an exact tap");
    }

    #[test]
    fn a_fractional_shift_interpolates_a_ramp_to_its_true_intermediate_value() {
        // A linear ramp is where any correct interpolator is essentially
        // exact: half a sample down a ramp rising by 10 per sample reads
        // 5 below its neighbour. Asserted away from the array's ends,
        // where the kernel is truncated and renormalised over an
        // asymmetric set of surviving taps and is approximate by
        // construction (the same caveat `resample.rs`'s ramp test carries).
        let dz = 1.0;
        let height = 24;
        let source = Array2::from_shape_fn((height, 2), |(r, _)| r as f32 * 10.0);
        let elevation = vec![100.5, 100.0]; // trace 1 is half a sample lower
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(height, dz as f32)),
            2,
            height,
            ElevationRange::NONE,
        )
        .unwrap();
        assert_eq!(geometry.elevation_top, 100.5);
        assert!((geometry.shift[0] - 0.0).abs() < 1e-6);
        assert!((geometry.shift[1] - 0.5).abs() < 1e-6);

        let inner = ArraySource::new(source.view());
        let topo = TopoSource::new(&inner, &geometry);
        let full = topo.read_window(0, topo.shape().0, 0, 2).unwrap();
        // Trace 1 row R samples the ramp at source position R - 0.5, i.e.
        // `(R - 0.5) * 10`. Checked well inside the kernel's reach.
        for r in 12..18 {
            let expected = (r as f32 - 0.5) * 10.0;
            assert!(
                (full[[r, 1]] - expected).abs() < 0.05,
                "row {r}: got {}, expected about {expected}",
                full[[r, 1]]
            );
        }
        // Trace 0 has no shift at all and must be untouched.
        for r in 0..height {
            assert_eq!(full[[r, 0]], source[[r, 0]]);
        }
    }

    #[test]
    fn a_fractional_shift_preserves_amplitude_regardless_of_the_fraction() {
        // The regression guard for the banding this kernel exists to fix
        // (#168). A two-tap linear blend scales high-frequency amplitude
        // by `sqrt((1-f)^2 + f^2)` -- 1.00 at f=0 but 0.71 at f=0.5 -- and
        // because `f` sweeps `[0, 1)` as the surface rises and falls, that
        // 29% swing lands across traces as vertical banding. Measured at a
        // 30% peak-to-peak contrast swing on a real profile.
        //
        // Built from a deterministic *band-limited* high-frequency signal,
        // sheared by a different fraction on every trace, then compared on
        // standard deviation: with a flat fractional-delay response every
        // trace keeps essentially the same amplitude no matter what its
        // fraction is.
        //
        // Band-limited, not a pure alternating sign, and the distinction
        // is not a convenience. Content at *exactly* Nyquist cannot
        // survive a fractional delay at all -- delaying `(-1)^n` by `f`
        // scales it by `cos(pi f)`, which is zero at `f = 0.5` -- so a
        // fixture built from one would be asserting against information
        // theory rather than against this kernel. Properly sampled radar
        // data lives below Nyquist, which is the case that matters and
        // the case a windowed-sinc kernel actually fixes. The components
        // here top out at 0.7 of Nyquist.
        let dz = 1.0;
        let height = 256;
        let n_traces = 16;
        let source = Array2::from_shape_fn((height, n_traces), |(r, c)| {
            let t = r as f32;
            let phase = c as f32 * 0.37;
            (0.70 * std::f32::consts::PI * t + phase).sin() * 10.0
                + (0.55 * std::f32::consts::PI * t + phase * 2.0).sin() * 6.0
                + (0.31 * std::f32::consts::PI * t + phase * 3.0).sin() * 4.0
        });
        // One trace per sixteenth of a sample, so the whole `[0, 1)` range
        // of fractions is represented.
        let elevation: Vec<f64> = (0..n_traces).map(|i| 100.0 - i as f64 / 16.0).collect();
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(height, dz as f32)),
            n_traces,
            height,
            ElevationRange::NONE,
        )
        .unwrap();

        let inner = ArraySource::new(source.view());
        let topo = TopoSource::new(&inner, &geometry);
        let full = topo.read_window(0, topo.shape().0, 0, n_traces).unwrap();

        // Measured well inside the data band, so no trace's window is
        // truncated by the wedge above or the array's end below.
        let std_dev = |c: usize| -> f64 {
            let vals: Vec<f64> = (32..(height - 32))
                .map(|r| full[[r, c]] as f64)
                .filter(|v| v.is_finite())
                .collect();
            let mean = vals.iter().sum::<f64>() / vals.len() as f64;
            (vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64).sqrt()
        };

        let reference = std_dev(0); // fraction 0: an exact, unfiltered tap
        for c in 1..n_traces {
            let ratio = std_dev(c) / reference;
            assert!(
                ratio > 0.9,
                "trace {c} (shift fraction {:.3}) lost {:.1}% of its amplitude; a \
                 fractional-delay kernel must not attenuate by fraction",
                geometry.shift[c] - geometry.shift[c].floor(),
                (1.0 - ratio) * 100.0
            );
        }
    }

    #[test]
    fn window_read_matches_a_crop_of_the_full_render() {
        // The highest-value property: any sub-window must equal the
        // corresponding crop of the full-array read, over several small
        // windows spanning fractional-shift boundaries.
        let dz = 0.3;
        let source = Array2::from_shape_fn((6, 5), |(r, c)| (r * 5 + c) as f32);
        let elevation = vec![100.0, 100.2, 100.35, 99.9, 100.6];
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(6, dz as f32)),
            5,
            6,
            ElevationRange::NONE,
        )
        .unwrap();
        let inner = ArraySource::new(source.view());
        let topo = TopoSource::new(&inner, &geometry);
        let (raster_h, raster_w) = topo.shape();
        let full = topo.read_window(0, raster_h, 0, raster_w).unwrap();

        for (r0, r1, c0, c1) in [
            (0, 2, 0, 3),
            (1, 3, 2, 5),
            (2, raster_h, 1, 4),
            (0, raster_h, 0, raster_w),
        ] {
            let window = topo.read_window(r0, r1, c0, c1).unwrap();
            for r in r0..r1 {
                for c in c0..c1 {
                    let a = window[[r - r0, c - c0]];
                    let b = full[[r, c]];
                    assert!(
                        a.is_nan() && b.is_nan() || (a - b).abs() < 1e-5,
                        "window ({r0},{r1},{c0},{c1}) disagrees with full render at ({r},{c}): {a} vs {b}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_nan_source_sample_propagates_through_the_shear() {
        let dz = 1.0;
        let mut source = Array2::from_elem((4, 1), 5.0f32);
        source[[2, 0]] = f32::NAN;
        let elevation = vec![100.0];
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(4, dz as f32)),
            1,
            4,
            ElevationRange::NONE,
        )
        .unwrap();
        let inner = ArraySource::new(source.view());
        let topo = TopoSource::new(&inner, &geometry);
        // No shift here, so this is a direct pass-through: still worth
        // pinning, since the interpolation branch must not accidentally
        // convert a NaN neighbour into a finite blended value.
        let full = topo.read_window(0, 4, 0, 1).unwrap();
        assert!(full[[2, 0]].is_nan());
    }

    #[test]
    fn worst_case_read_is_bounded_by_the_source_height() {
        // An inner read can never exceed the source's own height, even
        // with a pathological shift vector.
        let dz = 1.0;
        let height = 50;
        let source = Array2::from_shape_fn((height, 3), |(r, c)| (r * 3 + c) as f32);
        let elevation = vec![100.0, 100.0 + height as f64 * dz, 100.0];
        let geometry = resolve_topo_geometry(
            Some(&elevation),
            Some(&flat_depth(height, dz as f32)),
            3,
            height,
            ElevationRange::NONE,
        )
        .unwrap();
        let inner = ArraySource::new(source.view());
        let topo = TopoSource::new(&inner, &geometry);
        let (raster_h, _) = topo.shape();
        assert!(topo.read_window(0, raster_h, 0, 3).is_ok());
        // The shift span across these columns, plus the interpolation
        // kernel's reach either side.
        let span = geometry
            .shift
            .iter()
            .cloned()
            .fold(f32::NEG_INFINITY, f32::max)
            - geometry.shift.iter().cloned().fold(f32::INFINITY, f32::min);
        assert_eq!(
            topo.vertical_read_overhead(0, 3),
            span.ceil() as usize + 2 * SHIFT_KERNEL_HALF_WIDTH as usize
        );
    }

    #[test]
    fn a_window_entirely_outside_the_raster_is_empty_not_an_error() {
        let (source, geometry) = sheared_geometry();
        let inner = ArraySource::new(source.view());
        let topo = TopoSource::new(&inner, &geometry);
        assert_eq!(topo.read_window(50, 60, 0, 2).unwrap().shape(), &[0, 0]);
    }

    #[test]
    fn a_wedge_above_the_shifted_data_is_nan() {
        let (source, geometry) = sheared_geometry();
        let inner = ArraySource::new(source.view());
        let topo = TopoSource::new(&inner, &geometry);
        // Trace 1 (shift 1) has no data at output row 0 -- the wedge above
        // the sheared surface.
        let top = topo.read_window(0, 1, 1, 2).unwrap();
        assert!(top[[0, 0]].is_nan());
    }
}
