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
    /// Area-weighted mean over source-pixel footprints (#118). The only
    /// method implemented in v1; nearest-neighbor is deliberately excluded
    /// per the issue's own guidance for this data.
    Mean,
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
    /// `log10(|amplitude|)` applied before normalization, matching
    /// svalbard_radar's "Absolute" display mode. `false` for a plain
    /// linear-amplitude view.
    pub abslog: bool,
    pub limits: AmplitudeLimits,
    pub resampling: ResamplingMethod,
    pub format: ImageFormat,
}

impl RenderProfile {
    /// `default`: 1-99% quantile, linear amplitude.
    pub fn default_profile() -> Self {
        Self {
            name: "default".to_string(),
            view: DatasetView::Standard,
            abslog: false,
            limits: AmplitudeLimits::Percentile {
                low: 0.01,
                high: 0.99,
            },
            resampling: ResamplingMethod::Mean,
            format: ImageFormat::Jpeg { quality: 85 },
        }
    }

    /// `abslog`: `log10|amplitude|`, same percentile window as default.
    pub fn abslog_profile() -> Self {
        Self {
            name: "abslog".to_string(),
            abslog: true,
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

    /// The three server-defined profiles offered in v1 (#121's dropdown).
    pub fn built_in_profiles() -> Vec<Self> {
        vec![
            Self::default_profile(),
            Self::abslog_profile(),
            Self::high_contrast_profile(),
        ]
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
            "{:?}|{}|{}|{:?}|{}",
            self.view, self.abslog, limits, self.resampling, format
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
        assert!(RenderProfile::by_name("abslog").is_some());
        assert!(RenderProfile::by_name("high-contrast").is_some());
        assert!(RenderProfile::by_name("nonexistent").is_none());
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
