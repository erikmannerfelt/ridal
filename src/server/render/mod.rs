//! Reusable rendering: chunk/overview geometry, resampling, amplitude
//! statistics, colormapping and encoding (#118).
//!
//! Must not depend on Axum, MiniJinja, or other HTTP/template types.

// Consumed by renderer.rs/service.rs as this submodule fills in (M4/M5);
// until then the only callers are each submodule's own tests.
#![allow(dead_code)]

pub mod colormap;
pub mod grid;
pub mod profile;
pub mod renderer;
pub mod resample;
pub mod stats;
