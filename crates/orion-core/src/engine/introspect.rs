//! Reading an agent's own record of a conversation, in place of its terminal grid.
//!
//! Each agent keeps a different store in a different format. One module per agent translates
//! that store into the wire's `ConversationResponse`; the engine keeps the readers alive per pane
//! and asks the right one by agent kind.

pub mod claude;
pub mod codex;
pub mod cursor;
pub mod grok;
pub mod hermes;
pub mod omo;
pub mod opencode;
pub mod pi;
