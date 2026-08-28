//! Typed visualization configuration (#119, first cut).
//!
//! Only settings the v1 renderer actually supports -- no free-form
//! client-defined profiles (#120 explicitly warns against unbounded
//! client-driven render work). Every field here is part of the render
//! variant identity once that lands in M5: changing any of them must
//! produce a new render ID.

use serde::{Deserialize, Serialize};

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
    /// **Not the default for any profile.** Whether it suits `positive`
    /// better than `Peak` is a visual judgement against real data, not
    /// something to assert here.
    Lanczos,
    /// Lanczos applied to `|amplitude|` rather than signed amplitude.
    ///
    /// Plain `Lanczos` is a *linear* filter, so it cancels signed
    /// oscillating traces exactly the way the box mean does -- measured,
    /// not assumed: `positive` overviews under `Lanczos` come out nearly
    /// black, reproducing the failure `Peak` was introduced to fix.
    /// Rectifying first removes the cancellation while keeping proper
    /// anti-aliasing, which `Peak` (an order statistic that biases every
    /// footprint upward and turns noise into speckle) does not provide.
    ///
    /// Only meaningful for a profile that already reads amplitude
    /// asymmetrically; for a linear profile it would change what the
    /// image means. Also an experiment pending judgement.
    LanczosRectified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageFormat {
    /// Chosen over WebP for v1: `image` 0.25's WebP encoder
    /// (`image-webp` 0.2.4) is lossless-only in every released version:
    /// its own README states "only supports lossless encoding", and its
    /// `EncoderParams` has no `use_lossy` field. JPEG needs no new
    /// dependency -- `image` 0.24 already encodes it, and `io.rs`'s
    /// existing `render_jpg` already uses it.
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

/// Dataset view: the standard (trace, sample) view is the only one
/// implemented in v1. Topographic correction is an explicitly deferred
/// future view (#118).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatasetView {
    Standard,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderProfile {
    pub name: String,
    pub view: DatasetView,
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
            transform: AmplitudeTransform::Linear,
            limits: AmplitudeLimits::Percentile {
                low: 0.01,
                high: 0.99,
            },
            resampling: ResamplingMethod::Mean,
            format: ImageFormat::Jpeg { quality: 85 },
            contrast: 1.0,
            black_level: 0.0,
            stats_skip_first_samples: 0,
        }
    }

    /// `abslog`: `log10|amplitude|`, same percentile window as default.
    pub fn abslog_profile() -> Self {
        Self {
            name: "abslog".to_string(),
            transform: AmplitudeTransform::AbsLog,
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
            // Not `Mean`: averaging signed, oscillating amplitude over a
            // downsampled footprint cancels it toward zero, which this
            // profile's black level then clips to black -- the reason
            // `positive` overviews rendered almost entirely dark.
            resampling: ResamplingMethod::Peak,
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

    /// The server-defined profiles offered in v1 (#121's dropdown).
    pub fn built_in_profiles() -> Vec<Self> {
        vec![
            Self::default_profile(),
            Self::default_lanczos_profile(),
            Self::positive_profile(),
            Self::positive_lanczos_profile(),
            Self::positive_lanczos_rect_profile(),
            Self::abslog_profile(),
            Self::high_contrast_profile(),
        ]
    }

    /// `positive-lanczos`: `positive` with [`ResamplingMethod::Lanczos`]
    /// in place of `Peak`.
    ///
    /// Exists to make the two directly comparable in the viewer against
    /// real data, since which one reads better at a given downsample
    /// ratio is a visual judgement. Expected to be short-lived: once
    /// that judgement is made, either `positive` adopts Lanczos and this
    /// goes away, or it stays on `Peak` and this goes away.
    pub fn positive_lanczos_profile() -> Self {
        Self {
            name: "positive-lanczos".to_string(),
            resampling: ResamplingMethod::Lanczos,
            ..Self::positive_profile()
        }
    }

    /// `positive-lanczos-rect`: `positive` with
    /// [`ResamplingMethod::LanczosRectified`].
    ///
    /// The candidate that should, in principle, beat both: `Peak`'s
    /// freedom from cancellation plus `Lanczos`'s proper anti-aliasing,
    /// without `Peak`'s upward bias. Also short-lived.
    pub fn positive_lanczos_rect_profile() -> Self {
        Self {
            name: "positive-lanczos-rect".to_string(),
            resampling: ResamplingMethod::LanczosRectified,
            ..Self::positive_profile()
        }
    }

    /// `default-lanczos`: `default` with [`ResamplingMethod::Lanczos`] in
    /// place of `Mean`.
    ///
    /// The comparison that matters more than `positive-lanczos`, on
    /// evidence: `default` is a plain linear stretch with no asymmetric
    /// clipping, so a box mean's blurring is the only thing hurting it
    /// and a windowed-sinc filter is exactly the standard remedy. Also
    /// short-lived -- it exists to be judged.
    pub fn default_lanczos_profile() -> Self {
        Self {
            name: "default-lanczos".to_string(),
            resampling: ResamplingMethod::Lanczos,
            ..Self::default_profile()
        }
    }

    pub fn by_name(name: &str) -> Option<Self> {
        Self::built_in_profiles()
            .into_iter()
            .find(|p| p.name == name)
    }

    /// A stable string identifying everything about this profile that
    /// affects rendered pixels, for folding into the render variant ID
    /// (M5). Deliberately excludes nothing display-affecting and includes
    /// nothing identity-affecting (e.g. no display name).
    pub fn cache_key_fragment(&self) -> String {
        let limits = match self.limits {
            AmplitudeLimits::Explicit { min, max } => format!("explicit:{min}:{max}"),
            AmplitudeLimits::Percentile { low, high } => format!("pct:{low}:{high}"),
        };
        let format = match self.format {
            ImageFormat::Jpeg { quality } => format!("jpeg:{quality}"),
            ImageFormat::Png => "png".to_string(),
        };
        format!(
            "{:?}|{:?}|{}|{:?}|{}|{}|{}|{}",
            self.view,
            self.transform,
            limits,
            self.resampling,
            format,
            self.contrast,
            self.black_level,
            self.stats_skip_first_samples
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(RenderProfile::by_name("positive-lanczos").is_some());
        assert!(RenderProfile::by_name("abslog").is_some());
        assert!(RenderProfile::by_name("high-contrast").is_some());
        assert!(RenderProfile::by_name("nonexistent").is_none());
    }

    #[test]
    fn no_built_in_profile_defaults_to_lanczos_except_the_comparison_one() {
        // Lanczos is added as an option to be judged, not adopted. If a
        // profile other than the explicit comparison one starts using it,
        // that was a decision and should be a deliberate edit here.
        for profile in RenderProfile::built_in_profiles() {
            let is_experiment = profile.name.contains("-lanczos");
            let uses_lanczos = matches!(
                profile.resampling,
                ResamplingMethod::Lanczos | ResamplingMethod::LanczosRectified
            );
            assert_eq!(
                is_experiment, uses_lanczos,
                "profile '{}' and its resampling method disagree about being an experiment",
                profile.name
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
}
