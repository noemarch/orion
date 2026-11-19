//! Orion native UI entry point (GPUI).
//!
//! Main binary for the GPUI-based Orion shell. Renders the workspace sidebar
//! and a live terminal pane driven by `orion_core::Engine`.

// A GUI app never has a console window: without this, launching from Explorer creates one, and
// the daemon started from that console-less app hands every console child its own window.
// Gated off under `cfg(test)` so the bin's test harness still links its dependencies.
#![cfg_attr(not(test), windows_subsystem = "windows")]

use orion_ui::app::run;

fn main() {
    run();
}
