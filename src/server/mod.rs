//! Web server and browser GUI foundations (#115).
//!
//! Everything under this module requires the `server` feature. Rendering
//! and dataset logic here must not depend on Axum, MiniJinja, or other HTTP
//! or template types -- those live in `app.rs`/`routes.rs`/`templates.rs`
//! once they exist, and compose the modules below rather than the reverse.

pub mod catalog;
pub mod track;
