//! Orion native UI (GPUI) — application crate root.

pub mod agents;
pub mod app;
pub mod chat;
pub mod chat_entities;
pub mod chat_model;
pub mod components;
pub mod composer;
pub mod diff_view;
pub mod elements;
pub mod file_reader;
pub mod files_entities;
pub mod files_view;
pub mod markdown;
pub mod new_workspace;
pub mod pane_seam;
#[cfg(debug_assertions)]
pub mod perf;
pub mod prompt_card;
pub mod settings;
pub mod shell_model;
pub mod sidebar;
pub mod terminal;
pub mod terminal_font;
pub mod terminal_tree;
pub mod theme;
pub mod tool_card;
pub mod workspace;
