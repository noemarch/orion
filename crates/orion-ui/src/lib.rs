//! Orion native UI (GPUI) — application crate root.

pub mod agents;
pub mod app;
pub mod chat;
pub mod chat_entities;
pub mod chat_model;
pub mod composer;
pub mod diff_view;
pub mod file_reader;
pub mod files_entities;
pub mod files_view;
pub mod markdown;
pub mod new_workspace;
pub mod pane_seam;
#[cfg(debug_assertions)]
pub mod perf;
pub mod prompt_card;
pub mod review_pane;
pub mod session_status;
pub mod settings;
pub mod shell_model;
pub mod shell_picker;
pub mod sidebar;
pub mod sprites;
pub mod terminal;
pub mod terminal_font;
pub mod terminal_grid;
pub mod terminal_tree;
pub mod test_support;
pub mod theme;
pub mod tool_card;
pub mod workspace;
