//! The seam between the shell and the engine.
//!
//! The shell can hold the engine in its own process or reach one over a socket; both answer the
//! same questions, so the shell has one code path either way. The engine owns the terminals; the
//! shell owns nothing but the view.

pub mod traits;

pub use traits::{PaneHandle, Runtime};
