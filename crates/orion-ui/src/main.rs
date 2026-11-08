//! Orion native UI entry point (GPUI).
//!
//! Main binary for the GPUI-based Orion shell. Renders the workspace sidebar
//! and a live terminal pane driven by `orion_core::Engine`.

use orion_ui::app::run;

fn main() {
    run();
}
