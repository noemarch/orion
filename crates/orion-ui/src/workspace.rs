//! Workspace sidebar — real engine snapshot data.
//!
//! The old static entries ("w1"/"t1"/"p1") are gone; the sidebar now polls
//! `GET /api/session` through [`WorkspaceSidebar`]. This module re-exports
//! the implementation from [`crate::sidebar`] so existing imports keep
//! working.
pub use crate::sidebar::WorkspaceSidebar;
