//! Typed visualization configuration (#119, first cut).
//!
//! Only settings the v1 renderer actually supports -- no free-form
//! client-defined profiles (#120 explicitly warns against unbounded
//! client-driven render work). Every field here is part of the render
//! variant identity once that lands in M5: changing any of them must
//! produce a new render ID.

use serde::{Deserialize, Serialize};

use super::colormap::Colormap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResamplingMethod {
    /// Area-weighted mean over source-pixel footprints (#118).
    /// Nearest-neighbor is deliberately excluded per the issue's own
    /// guidance for this data.
    Mean,
    /// Largest source value in each footprint. For a profile that reads
    /// *signed* amplitude asymmetrically (see
    /// [`AmplitudeTransform::Positive`]), the mean is the wrong reducer:
    /// radar traces oscillate about zero, so averaging a large footprint
    /// cancels them toward zero and the asymmetric stretch then clips the
    /// result to black. Identical to `Mean` at a 1:1 footprint. See
    /// `resample::resample` for the full rationale.
    ///
    /// Superseded by [`LanczosRectified`](Self::LanczosRectified) for
    /// `positive` (biases every footprint upward into speckle, which the
    /// rectified windowed-sinc filter does not); not currently used by any
    /// built-in profile, kept as a documented option.
    Peak,
    /// Windowed-sinc (Lanczos-3) filtering, applied separably.
    ///
    /// The principled anti-aliasing choice: `Mean` is a box filter and
    /// blurs, `Peak` is an order statistic that biases every footprint
    /// upward and turns noise into speckle, while this preserves the
    /// shape of the signal around a feature rather than only its average
    /// or its maximum. Also reduces to an identity at an aligned 1:1
    /// footprint, where the kernel taps land on integers and `sinc` is
    /// zero at all of them but the centre.
    ///
    /// Plain (unrectified) Lanczos is still a *linear* filter, so on
    /// signed oscillating amplitude it cancels toward zero exactly the way
    /// `Mean` does -- measured, not assumed: `positive` overviews under
    /// this came out nearly black. Not currently used by any built-in
    /// profile as a result; kept as a documented option for a profile
    /// whose amplitude is not signed and asymmetric.
    Lanczos,
    /// Lanczos applied to `|amplitude|` rather than signed amplitude.
    ///
    /// Removes the cancellation plain `Lanczos` suffers on signed
    /// oscillating traces while keeping proper anti-aliasing, which `Peak`
    /// (an order statistic that biases every footprint upward into
    /// speckle) does not provide. Compared against both on real data (see
    /// `PHASE1_LOG.md`) and read best of the three; used by
    /// [`RenderProfile::positive_profile`] and
    /// [`RenderProfile::abslog_profile`].
    ///
    /// Safe for any profile whose display value is already sign-agnostic
    /// (`positive`'s asymmetric stretch, `abslog`'s `log10|amplitude|`).
    /// For a plain signed-linear profile it would change what the image
    /// means rather than just how it is anti-aliased.
    LanczosRectified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageFormat {
    /// Chosen over WebP for v1: `image` 0.25's WebP encoder
    /// (`image-webp` 0.2.4) is lossless-only in every released version:
    /// its own README states "only supports lossless encoding", and its
    /// `EncoderParams` has no `use_lossy` field. JPEG needs no new
    /// dependency -- `image` 0.24 already encodes it.
    Jpeg {
        quality: u8,
    },
    Png,
}

impl ImageFormat {
    pub fn content_type(&self) -> &'static str {
        match self {
            ImageFormat::Jpeg { .. } => "image/jpeg",
            ImageFormat::Png => "image/png",
        }
    }
}

/// How raw amplitude is mapped into the domain that limits and
/// normalization operate in, both for percentile estimation (`stats.rs`)
/// and for the pixel value that actually gets normalized (`colormap.rs`).
///
/// `AbsLog` uses the same domain (`log10|x|`) for both purposes. `Positive`
/// does not: PFA_website's `normalize()` estimates percentile bounds from
/// `|x|` but stretches the *signed* value against those bounds, which is
/// what pushes negative returns toward black while favoring positive ones.
/// That asymmetry is why `Positive` needs its own variant rather than
/// reusing a single "domain" transform for both stats and display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AmplitudeTransform {
    /// Plain linear amplitude, no transform.
    Linear,
    /// `log10(|amplitude|)`, matching svalbard_radar's "Absolute" display
    /// mode.
    AbsLog,
    /// Percentile bounds from `|amplitude|`, signed amplitude displayed --
    /// PFA_website's (mis-named there) "abslog" stretch.
    Positive,
}

/// A transform applied to each **source** sample *before* resampling, as
/// distinct from [`AmplitudeTransform`], which maps the already-resampled
/// value to the display domain afterwards. Order matters.
///
/// The motivating case is `siglog`: the processing step compresses each
/// sample before anything averages traces, so `mean(siglog(raw))` stays
/// meaningful at every scale. Folding the compression into the display
/// transform instead would leave it until *after* resampling, i.e.
/// `siglog(mean(raw))`, and the footprint mean of oscillating signed data
/// collapses toward zero, which the log then truncates to a flat overview.
/// Doing it here reproduces the processing order at render time and needs
/// no special resampler.
///
/// Pointwise and `NaN`-preserving, so it can be applied to a read window or
/// an overview band without affecting banding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SourceTransform {
    /// The sample unchanged.
    #[default]
    None,
    /// `(log10(|amplitude|) - offset).max(0) * sign(amplitude)`: the
    /// sign-corrected log transform `gpr.rs`'s `siglog` processing step
    /// applies, at its default offset
    /// ([`crate::filters::DEFAULT_SIGLOG_MINVAL_LOG10`]).
    SigLog,
}

/// How amplitude limits are determined for normalization.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AmplitudeLimits {
    /// User-specified; requires no radargram-wide estimate.
    Explicit { min: f32, max: f32 },
    /// Estimated once per revision+profile via sampled percentiles (#119),
    /// then reused for every chunk -- never estimated independently per
    /// chunk, which would make adjacent chunks normalize differently and
    /// produce visible seams.
    Percentile { low: f32, high: f32 },
}

/// Which vertical transform of the source `data` array a render draws
/// from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatasetView {
    Standard,
    /// A render-time-only vertical topographic correction (#168): traces
    /// are sheared so a given output row is one elevation everywhere,
    /// resolved from the `elevation`/`depth` axes into a
    /// [`crate::render::topo::TopoGeometry`] and applied by
    /// [`crate::render::topo::TopoSource`]. Never precomputed or stored,
    /// and never the coordinate space an interpretation is saved in --
    /// distinct from `gpr.rs::correct_topography`'s `data_topocorr`, which
    /// writes a NetCDF product. The catalog's own index overviews stay
    /// [`Standard`](Self::Standard); only the viewer offers this.
    Topographic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderProfile {
    pub name: String,
    pub view: DatasetView,
    /// Applied to each source sample before resampling. `#[serde(default)]`
    /// (`None`) so profile files written before this field existed keep
    /// loading.
    #[serde(default)]
    pub source_transform: SourceTransform,
    pub transform: AmplitudeTransform,
    pub limits: AmplitudeLimits,
    pub resampling: ResamplingMethod,
    pub format: ImageFormat,
    /// Post-normalization contrast multiplier: `1.0` (the default profile's
    /// value) leaves the plain min-max stretch untouched.
    pub contrast: f32,
    /// Post-normalization black-level offset, subtracted before the
    /// contrast multiplier: `0.0` (the default profile's value) leaves the
    /// plain min-max stretch untouched.
    pub black_level: f32,
    /// Optional named gradient (#246). `None` (the default, and every
    /// grayscale profile) renders through the original single-channel
    /// path, byte-identical to before this field existed; `Some` maps the
    /// normalized byte through the colormap's 256-entry RGB LUT.
    ///
    /// `#[serde(default)]` so profile files written before the field
    /// existed keep loading, and omitting it can only mean "grayscale".
    #[serde(default)]
    pub colormap: Option<Colormap>,
    /// Force the resolved limits to `(-vmax, vmax)` after estimation, so
    /// a diverging colormap's midpoint sits at zero amplitude. See
    /// [`super::colormap::resolve_limits`] for why this composes with
    /// both limit kinds. `false` for every grayscale profile, which is
    /// also what a file without the field means.
    #[serde(default)]
    pub symmetric_limits: bool,
    /// Sample rows dropped from the top of every trace before estimating
    /// percentile limits (`stats.rs`), excluding the direct-wave band from
    /// the estimate. `0` (the default profile's value) samples whole
    /// traces, unchanged from before this field existed.
    pub stats_skip_first_samples: usize,
}

impl RenderProfile {
    /// `default`: 1-99% quantile, linear amplitude.
    pub fn default_profile() -> Self {
        Self {
            name: "default".to_string(),
            view: DatasetView::Standard,
            source_transform: SourceTransform::None,
            transform: AmplitudeTransform::Linear,
            limits: AmplitudeLimits::Percentile {
                low: 0.01,
                high: 0.99,
            },
            resampling: ResamplingMethod::Mean,
            format: ImageFormat::Jpeg { quality: 85 },
            contrast: 1.0,
            black_level: 0.0,
            colormap: None,
            symmetric_limits: false,
            stats_skip_first_samples: 0,
        }
    }

    /// `abslog`: `log10|amplitude|`, same percentile window as default.
    pub fn abslog_profile() -> Self {
        Self {
            name: "abslog".to_string(),
            transform: AmplitudeTransform::AbsLog,
            // Unlike `default`, rectifying before filtering doesn't change
            // what this profile's image means: its display value is
            // already `log10|amplitude|`, sign-agnostic by construction.
            // Compared against Mean/Peak/Lanczos on real data and read
            // best of the four.
            resampling: ResamplingMethod::LanczosRectified,
            ..Self::default_profile()
        }
    }

    /// `positive`: PFA_website's asymmetric stretch (mis-named "abslog"
    /// there, and not a log transform at all -- see `AmplitudeTransform`).
    /// Biases the display heavily toward positive returns, clipping
    /// negative ones toward black; deliberately not the default, useful in
    /// specific cases rather than as a general-purpose view.
    pub fn positive_profile() -> Self {
        Self {
            name: "positive".to_string(),
            transform: AmplitudeTransform::Positive,
            contrast: 0.9,
            black_level: 0.1,
            // Excludes the direct-wave band (the source wavelet's own very
            // high amplitude near the top of the radargram) from the
            // percentile estimate, matching PFA_website's
            // `normalize()`, which skips the first 50 sample rows.
            stats_skip_first_samples: 50,
            // Settled after comparing Peak, Lanczos and LanczosRectified
            // against real data (see PHASE1_LOG.md): plain `Mean` or
            // `Lanczos` cancel signed, oscillating amplitude toward zero
            // over a downsampled footprint, which this profile's black
            // level then clips to black -- the reason `positive` overviews
            // once rendered almost entirely dark. `Peak` fixed that but
            // biases every footprint upward into speckle. `LanczosRectified`
            // (filtering `|amplitude|`) removes the cancellation while
            // keeping proper anti-aliasing, and read best of the three on
            // real data.
            resampling: ResamplingMethod::LanczosRectified,
            ..Self::default_profile()
        }
    }

    /// `high-contrast`: 5-95% quantile, linear amplitude.
    pub fn high_contrast_profile() -> Self {
        Self {
            name: "high-contrast".to_string(),
            limits: AmplitudeLimits::Percentile {
                low: 0.05,
                high: 0.95,
            },
            ..Self::default_profile()
        }
    }

    /// `siglog-default`: the `default` profile with the `siglog` source
    /// preprocessing step in front of it. The siglog version of a plain
    /// linear radargram.
    ///
    /// The compression is a [`SourceTransform`], applied to every sample
    /// *before* resampling, exactly as the processing step is. That is the
    /// whole point: `Mean` then computes `mean(siglog(raw))`, which stays
    /// meaningful at overview scale, whereas leaving the compression to a
    /// post-resample display transform would compute `siglog(mean(raw))`
    /// and flatten the overview (a downsampled footprint of oscillating
    /// signed data averages toward zero, which the log then truncates).
    ///
    /// Resampling is inherited from `default` (`Mean`) rather than set
    /// here: every `siglog-*` is exactly its base profile plus the source
    /// step, so it uses whatever reducer the base profile already settled
    /// on.
    pub fn siglog_default_profile() -> Self {
        Self {
            name: "siglog-default".to_string(),
            source_transform: SourceTransform::SigLog,
            ..Self::default_profile()
        }
    }

    /// `siglog-positive`: the `positive` profile with the `siglog` source
    /// preprocessing step in front of it.
    ///
    /// The display transform stays [`AmplitudeTransform::Positive`], so
    /// limits are estimated from `|siglog(raw)|` and the *signed* siglog
    /// value is stretched against them -- the faithful "positive view of
    /// siglog". Resampling is inherited from `positive`
    /// ([`ResamplingMethod::LanczosRectified`]), and that is load-bearing:
    /// `positive` displays the *rectified* envelope, so filtering
    /// `|siglog(raw)|` is the right reducer and exactly reproduces "run the
    /// `siglog` step, then render with `positive`". Using `Mean` here would
    /// average the signed siglog values back toward zero, and `Positive`'s
    /// black level would clip that to an almost entirely black overview.
    pub fn siglog_positive_profile() -> Self {
        Self {
            name: "siglog-positive".to_string(),
            source_transform: SourceTransform::SigLog,
            ..Self::positive_profile()
        }
    }

    /// `siglog-high-contrast`: the `high-contrast` profile's 5-95%
    /// quantile with the `siglog` source preprocessing step in front of it.
    pub fn siglog_high_contrast_profile() -> Self {
        Self {
            name: "siglog-high-contrast".to_string(),
            source_transform: SourceTransform::SigLog,
            ..Self::high_contrast_profile()
        }
    }

    /// `seismic`: the diverging blue--white--red ramp, for signed
    /// amplitude. The white midpoint is only meaningful at zero
    /// amplitude, so `symmetric_limits` forces it there.
    ///
    /// Three couplings exist so the *sign* reaching the colormap is the
    /// sign of the data, and none is inherited by accident:
    ///
    /// - Resampling stays [`ResamplingMethod::Mean`] (the `default`
    ///   profile's). `LanczosRectified`, which `positive` uses, filters
    ///   `|amplitude|` and destroys the sign the ramp is meant to show.
    ///   `Mean` averages amplitude before colormapping, so `+` and `-`
    ///   average toward zero -- correctly white -- rather than blending
    ///   into purple.
    /// - `transform` is [`AmplitudeTransform::Linear`], not `AbsLog` or
    ///   `Positive`, both of which have already discarded or folded the
    ///   sign.
    /// - `contrast` and `black_level` stay neutral (1.0 / 0.0);
    ///   `black_level` shifts the normalized value and would walk the
    ///   white point off centre.
    pub fn seismic_profile() -> Self {
        Self {
            name: "seismic".to_string(),
            colormap: Some(Colormap::seismic()),
            symmetric_limits: true,
            ..Self::default_profile()
        }
    }

    /// `siglog-seismic`: the `seismic` ramp on `siglog`-compressed
    /// source, exactly as the other `siglog-*` relate to their bases.
    ///
    /// The pairing is not a box-ticking variant: `siglog` compresses
    /// magnitude while *preserving* sign, which is precisely what the
    /// diverging ramp needs, so weak and strong returns on both sides of
    /// zero stay on their own side of the white midpoint. Resampling is
    /// inherited from `seismic` (`Mean`), as `siglog-default` inherits
    /// from `default`.
    pub fn siglog_seismic_profile() -> Self {
        Self {
            name: "siglog-seismic".to_string(),
            source_transform: SourceTransform::SigLog,
            ..Self::seismic_profile()
        }
    }

    /// The server-defined profiles (#121's dropdown; #246 added the
    /// colormapped pair).
    ///
    /// Each `siglog-*` sits next to the profile it is the log view of.
    /// There is deliberately no `siglog-abslog` (#182): `abslog` is
    /// already a log transform, so the siglog view of it would be a log
    /// of a log, not a distinct useful picture.
    pub fn built_in_profiles() -> Vec<Self> {
        vec![
            Self::default_profile(),
            Self::siglog_default_profile(),
            Self::positive_profile(),
            Self::siglog_positive_profile(),
            Self::high_contrast_profile(),
            Self::siglog_high_contrast_profile(),
            Self::abslog_profile(),
            Self::seismic_profile(),
            Self::siglog_seismic_profile(),
        ]
    }

    pub fn by_name(name: &str) -> Option<Self> {
        Self::built_in_profiles()
            .into_iter()
            .find(|p| p.name == name)
    }

    /// Resolve what a user typed after `--render-profile`: either the name
    /// of a built-in profile, or a path to a TOML file describing one.
    ///
    /// The two are told apart by *looking at the filesystem*, not by
    /// pattern-matching the string. A name that happens to contain a dot
    /// or a slash is not automatically a path, and a file called `default`
    /// in the working directory does not shadow the built-in of that name
    /// -- built-ins win, so a project cannot silently change what
    /// `--render-profile default` means by adding a file.
    ///
    /// Anything that is neither is an error naming both possibilities,
    /// since "no such profile" and "no such file" are the same mistake
    /// from the user's side and they will not know which one they made.
    pub fn resolve(spec: &str) -> Result<Self, String> {
        if let Some(profile) = Self::by_name(spec) {
            return Ok(profile);
        }
        let path = std::path::Path::new(spec);
        if path.is_file() {
            return Self::from_toml_file(path);
        }
        let names: Vec<String> = Self::built_in_profiles()
            .into_iter()
            .map(|p| p.name)
            .collect();
        Err(format!(
            "'{spec}' is neither a built-in render profile ({}) nor a readable file.",
            names.join(", ")
        ))
    }

    /// Read a profile from a TOML file.
    ///
    /// Every field is required except `source_transform`, `colormap` and
    /// `symmetric_limits`, which `#[serde(default)]` so profile files
    /// written before each field existed keep loading unchanged; omitting
    /// them can only mean "no source preprocessing", "grayscale" and
    /// "asymmetric", which is what those files did. A `colormap` that is
    /// present is checked by [`Colormap::validate`], since that is the
    /// only field with structural invariants the type cannot express.
    /// Profiles that inherit from a built-in and override a field or two
    /// are the obvious next step and deliberately not guessed at here -- whether
    /// that is a `base = "default"` key, a separate
    /// `--render-profile-override`, or a broader set of serde defaults
    /// changes what a file means, and getting it wrong later would
    /// silently re-interpret files people had already written.
    pub fn from_toml_file(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("Could not read render profile {}: {e}", path.display()))?;
        let profile: Self = toml::from_str(&text)
            .map_err(|e| format!("Could not parse render profile {}: {e}", path.display()))?;
        // The colormap is the one part of a profile with structural
        // invariants the type cannot express, so a malformed file is
        // rejected here rather than panicking in the render path.
        if let Some(colormap) = &profile.colormap {
            colormap
                .validate()
                .map_err(|e| format!("Could not use render profile {}: {e}", path.display()))?;
        }
        Ok(profile)
    }

    /// A stable string identifying everything about this profile that
    /// affects rendered pixels, for folding into the render variant ID
    /// (M5). Deliberately excludes nothing display-affecting and includes
    /// nothing identity-affecting (e.g. no display name).
    ///
    /// The colormap and symmetric-limit fields are appended only when
    /// set, so every grayscale profile's fragment stays byte-identical to
    /// what it was before #246 and no cached render is mass-invalidated.
    pub fn cache_key_fragment(&self) -> String {
        let limits = match self.limits {
            AmplitudeLimits::Explicit { min, max } => format!("explicit:{min}:{max}"),
            AmplitudeLimits::Percentile { low, high } => format!("pct:{low}:{high}"),
        };
        let format = match self.format {
            ImageFormat::Jpeg { quality } => format!("jpeg:{quality}"),
            ImageFormat::Png => "png".to_string(),
        };
        let mut fragment = format!(
            "{:?}|{:?}|{:?}|{}|{:?}|{}|{}|{}|{}",
            self.view,
            self.source_transform,
            self.transform,
            limits,
            self.resampling,
            format,
            self.contrast,
            self.black_level,
            self.stats_skip_first_samples
        );
        if let Some(colormap) = &self.colormap {
            fragment.push('|');
            fragment.push_str(&colormap.cache_key_fragment());
        }
        if self.symmetric_limits {
            fragment.push_str("|sym");
        }
        fragment
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_built_in_profile_round_trips_through_toml() {
        // Also the answer to "what does a profile file look like": every
        // built-in serialises to a complete, re-readable one.
        for original in RenderProfile::built_in_profiles() {
            let text = toml::to_string(&original).unwrap();
            let parsed: RenderProfile = toml::from_str(&text).unwrap();
            assert_eq!(parsed, original, "{}:\n{text}", original.name);
        }
    }

    #[test]
    // Changes the process-wide working directory, so it must not overlap
    // any test that reads a relative path. Same hazard as the tests that
    // unset PATH.
    #[serial_test::serial(current_dir)]
    fn resolve_prefers_a_built_in_name_over_a_file_of_that_name() {
        let dir = tempfile::tempdir().unwrap();
        let decoy = dir.path().join("default");
        std::fs::write(&decoy, "name = 'not this one'").unwrap();
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let resolved = RenderProfile::resolve("default");
        std::env::set_current_dir(previous).unwrap();
        // A file cannot silently redefine what a built-in name means.
        assert_eq!(resolved.unwrap().name, "default");
    }

    #[test]
    fn resolve_reads_a_profile_from_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mine.toml");
        let mut source = RenderProfile::default_profile();
        source.name = "mine".to_string();
        source.contrast = 1.75;
        std::fs::write(&path, toml::to_string(&source).unwrap()).unwrap();

        let resolved = RenderProfile::resolve(path.to_str().unwrap()).unwrap();
        assert_eq!(resolved, source);
    }

    #[test]
    fn a_profile_file_with_a_malformed_colormap_is_rejected() {
        // A hand-written file is the only way an invalid gradient can
        // enter, and it must fail here rather than panic in `sample` when
        // the render reaches the LUT.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.toml");
        let text = r#"
name = "broken"
view = "Standard"
transform = "Linear"
limits = { Percentile = { low = 0.01, high = 0.99 } }
resampling = "Mean"
format = "Png"
contrast = 1.0
black_level = 0.0
stats_skip_first_samples = 0
colormap = { name = "none", stops = [] }
"#;
        std::fs::write(&path, text).unwrap();
        let error = RenderProfile::resolve(path.to_str().unwrap()).unwrap_err();
        assert!(error.contains("no stops"), "{error}");
        assert!(
            error.contains("broken.toml"),
            "should name the file: {error}"
        );
    }

    #[test]
    fn resolve_says_both_things_it_looked_for() {
        // "No such profile" and "no such file" are the same mistake from
        // the user's side, and they will not know which one they made.
        let error = RenderProfile::resolve("noideawhatthisis").unwrap_err();
        assert!(error.contains("built-in render profile"), "{error}");
        assert!(error.contains("readable file"), "{error}");
        assert!(
            error.contains("abslog"),
            "should list the real ones: {error}"
        );
    }

    #[test]
    fn built_in_profiles_have_distinct_names_and_cache_keys() {
        let profiles = RenderProfile::built_in_profiles();
        let names: std::collections::HashSet<&str> =
            profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names.len(), profiles.len(), "profile names must be unique");

        let keys: std::collections::HashSet<String> =
            profiles.iter().map(|p| p.cache_key_fragment()).collect();
        assert_eq!(
            keys.len(),
            profiles.len(),
            "distinct profiles must produce distinct cache keys"
        );
    }

    #[test]
    fn by_name_finds_built_ins_and_rejects_unknown() {
        assert!(RenderProfile::by_name("default").is_some());
        assert!(RenderProfile::by_name("positive").is_some());
        assert!(RenderProfile::by_name("abslog").is_some());
        assert!(RenderProfile::by_name("high-contrast").is_some());
        assert!(RenderProfile::by_name("siglog-default").is_some());
        assert!(RenderProfile::by_name("siglog-positive").is_some());
        assert!(RenderProfile::by_name("siglog-high-contrast").is_some());
        assert!(RenderProfile::by_name("seismic").is_some());
        assert!(RenderProfile::by_name("siglog-seismic").is_some());
        // A siglog view of abslog would be a log of a log (#182).
        assert!(RenderProfile::by_name("siglog-abslog").is_none());
        assert!(RenderProfile::by_name("nonexistent").is_none());
        // The comparison profiles this settled from no longer exist.
        assert!(RenderProfile::by_name("positive-lanczos").is_none());
        assert!(RenderProfile::by_name("positive-lanczos-rect").is_none());
        assert!(RenderProfile::by_name("default-lanczos").is_none());
        assert!(RenderProfile::by_name("default-lanczos-rect").is_none());
    }

    #[test]
    fn siglog_profiles_are_their_base_profile_plus_the_source_step() {
        // Each `siglog-*` is exactly the base profile with the `siglog`
        // *source* preprocessing step in front of it (#182): same display
        // transform (so `siglog-positive` keeps the asymmetric positive
        // stretch on signed siglog values) and same resampler, which is
        // load-bearing -- `positive` displays the rectified envelope, so
        // it must keep `LanczosRectified`, not fall back to `Mean`.
        let cases: &[(&str, RenderProfile)] = &[
            ("siglog-default", RenderProfile::default_profile()),
            ("siglog-positive", RenderProfile::positive_profile()),
            (
                "siglog-high-contrast",
                RenderProfile::high_contrast_profile(),
            ),
            ("siglog-seismic", RenderProfile::seismic_profile()),
        ];
        for (name, base) in cases {
            let siglog = RenderProfile::by_name(name).unwrap();
            assert_eq!(
                siglog.source_transform,
                SourceTransform::SigLog,
                "{name} source transform"
            );
            assert_eq!(siglog.transform, base.transform, "{name} transform");
            assert_eq!(siglog.resampling, base.resampling, "{name} resampling");
            assert_eq!(siglog.limits, base.limits, "{name} limits");
            assert_eq!(siglog.contrast, base.contrast, "{name} contrast");
            assert_eq!(siglog.black_level, base.black_level, "{name} black level");
            assert_eq!(siglog.colormap, base.colormap, "{name} colormap");
            assert_eq!(
                siglog.symmetric_limits, base.symmetric_limits,
                "{name} symmetric limits"
            );
            assert_eq!(
                siglog.stats_skip_first_samples, base.stats_skip_first_samples,
                "{name} stats skip"
            );
        }
        // `siglog-positive` specifically must keep the rectified reducer:
        // `Mean` averages the signed siglog values back toward zero and
        // `Positive`'s black level clips that to a black overview.
        assert_eq!(
            RenderProfile::siglog_positive_profile().resampling,
            ResamplingMethod::LanczosRectified
        );
        // And the base profiles keep no source preprocessing of their own.
        for name in ["default", "positive", "abslog", "high-contrast"] {
            assert_eq!(
                RenderProfile::by_name(name).unwrap().source_transform,
                SourceTransform::None,
                "{name} should not preprocess the source"
            );
        }
    }

    #[test]
    fn seismic_profiles_keep_the_sign_and_neutral_tuning() {
        // The couplings from #246, pinned so a later edit cannot quietly
        // give `seismic` the `positive` tuning: sign must survive
        // resampling (`Mean`, not `LanczosRectified`), the display
        // transform must not have discarded the sign (`Linear`), and
        // contrast/black level must stay neutral or `black_level` walks
        // the white point off centre. `symmetric_limits` is what puts the
        // midpoint at zero at all.
        for name in ["seismic", "siglog-seismic"] {
            let profile = RenderProfile::by_name(name).unwrap();
            assert_eq!(profile.colormap, Some(Colormap::seismic()), "{name}");
            assert!(profile.symmetric_limits, "{name}");
            assert_eq!(profile.transform, AmplitudeTransform::Linear, "{name}");
            assert_eq!(profile.resampling, ResamplingMethod::Mean, "{name}");
            assert_eq!(profile.contrast, 1.0, "{name}");
            assert_eq!(profile.black_level, 0.0, "{name}");
        }
        assert_eq!(
            RenderProfile::seismic_profile().source_transform,
            SourceTransform::None
        );
        assert_eq!(
            RenderProfile::siglog_seismic_profile().source_transform,
            SourceTransform::SigLog
        );
        // The base profile itself stays grayscale and asymmetric.
        assert_eq!(RenderProfile::default_profile().colormap, None);
        assert!(!RenderProfile::default_profile().symmetric_limits);
    }

    #[test]
    fn a_profile_file_without_source_transform_still_loads_as_none() {
        // Files written before the field existed must keep working, and
        // omitting it can only mean "no preprocessing".
        let text = r#"
name = "legacy"
view = "Standard"
transform = "Linear"
limits = { Percentile = { low = 0.01, high = 0.99 } }
resampling = "Mean"
format = "Png"
contrast = 1.0
black_level = 0.0
stats_skip_first_samples = 0
"#;
        let profile: RenderProfile = toml::from_str(text).unwrap();
        assert_eq!(profile.source_transform, SourceTransform::None);
        // Same for the #246 fields: absent means grayscale and asymmetric.
        assert_eq!(profile.colormap, None);
        assert!(!profile.symmetric_limits);
        // A file that does set them round-trips.
        let text = text.replace(
            "view = \"Standard\"",
            "view = \"Standard\"\nsource_transform = \"SigLog\"\nsymmetric_limits = true\n\
             colormap = { name = \"seismic\", stops = [\
             { position = 0.0, color = [0, 0, 76] },\
             { position = 1.0, color = [128, 0, 0] }] }",
        );
        let profile: RenderProfile = toml::from_str(&text).unwrap();
        assert_eq!(profile.source_transform, SourceTransform::SigLog);
        assert!(profile.symmetric_limits);
        assert_eq!(profile.colormap.unwrap().stops.len(), 2);
    }

    #[test]
    fn changing_only_the_source_transform_changes_the_cache_key() {
        // Two views that draw different pixels must not share a cached
        // render: `default` on raw data and `default` on siglog'd data.
        let mut a = RenderProfile::default_profile();
        let b = a.clone();
        a.source_transform = SourceTransform::SigLog;
        assert_ne!(a.cache_key_fragment(), b.cache_key_fragment());
    }

    #[test]
    fn built_in_resampling_methods_are_the_settled_choice() {
        // Pins the outcome of the Peak/Lanczos/LanczosRectified comparison
        // (PHASE1_LOG.md) so a change here is a deliberate edit, not a
        // silent side effect of something else.
        let expected: &[(&str, ResamplingMethod)] = &[
            ("default", ResamplingMethod::Mean),
            ("positive", ResamplingMethod::LanczosRectified),
            ("abslog", ResamplingMethod::LanczosRectified),
            ("high-contrast", ResamplingMethod::Mean),
            // The `siglog-*` profiles preprocess the source, so they use
            // their base profile's reducer: `Mean` for the linear views,
            // the rectified envelope filter for `positive` (whose display
            // is already rectified).
            ("siglog-default", ResamplingMethod::Mean),
            ("siglog-positive", ResamplingMethod::LanczosRectified),
            ("siglog-high-contrast", ResamplingMethod::Mean),
            // `seismic` displays *signed* amplitude (that is what the
            // diverging ramp shows) and `siglog-seismic` its
            // sign-preserving compression, so both must keep the plain
            // area-weighted mean -- rectifying would destroy the sign.
            ("seismic", ResamplingMethod::Mean),
            ("siglog-seismic", ResamplingMethod::Mean),
        ];
        for (name, method) in expected {
            let profile = RenderProfile::by_name(name).unwrap();
            assert_eq!(
                profile.resampling, *method,
                "profile '{name}' resampling method changed"
            );
        }
    }

    #[test]
    fn positive_profile_is_not_the_default() {
        assert_ne!(
            RenderProfile::positive_profile(),
            RenderProfile::default_profile()
        );
        assert_eq!(RenderProfile::default_profile().contrast, 1.0);
        assert_eq!(RenderProfile::default_profile().black_level, 0.0);
        assert_eq!(RenderProfile::default_profile().stats_skip_first_samples, 0);
    }

    #[test]
    fn identical_profiles_produce_identical_cache_keys() {
        let a = RenderProfile::default_profile();
        let b = RenderProfile::default_profile();
        assert_eq!(a.cache_key_fragment(), b.cache_key_fragment());
    }

    #[test]
    fn changing_quality_changes_the_cache_key() {
        let mut a = RenderProfile::default_profile();
        let b = a.clone();
        a.format = ImageFormat::Jpeg { quality: 50 };
        assert_ne!(a.cache_key_fragment(), b.cache_key_fragment());
    }

    #[test]
    fn grayscale_profiles_keep_their_pre_colormap_cache_key_fragment() {
        // #246 appends the colormap and symmetric-limit fields only when
        // set, so every grayscale profile's fragment is byte-identical to
        // what it was before the change and no cached render is
        // invalidated. Pinned literally: the format machinery is shared,
        // so this pins it for all seven.
        assert_eq!(
            RenderProfile::default_profile().cache_key_fragment(),
            "Standard|None|Linear|pct:0.01:0.99|Mean|jpeg:85|1|0|0"
        );
        for name in [
            "default",
            "positive",
            "abslog",
            "high-contrast",
            "siglog-default",
            "siglog-positive",
            "siglog-high-contrast",
        ] {
            let fragment = RenderProfile::by_name(name).unwrap().cache_key_fragment();
            assert!(
                !fragment.contains("|cm:") && !fragment.contains("|sym"),
                "{name}'s fragment gained a colormap field: {fragment}"
            );
        }
    }

    #[test]
    fn adding_a_colormap_or_symmetry_changes_the_cache_key() {
        let plain = RenderProfile::default_profile();

        let mut colormapped = plain.clone();
        colormapped.colormap = Some(Colormap::seismic());
        assert_ne!(plain.cache_key_fragment(), colormapped.cache_key_fragment());

        let mut symmetric = plain.clone();
        symmetric.symmetric_limits = true;
        assert_ne!(plain.cache_key_fragment(), symmetric.cache_key_fragment());

        // The two new fields are independent, so a symmetric grayscale
        // profile cannot collide with the colormapped one.
        let mut symmetric_colormapped = colormapped.clone();
        symmetric_colormapped.symmetric_limits = true;
        assert_ne!(
            colormapped.cache_key_fragment(),
            symmetric_colormapped.cache_key_fragment()
        );
    }
}
