//! The seam between the shell and the engine.
//!
//! The shell can hold the engine in its own process or reach one over a socket; both answer the
//! same questions, so the shell has one code path either way. The engine owns the terminals; the
//! shell owns nothing but the view.

pub mod client;
pub mod framing;
pub mod limits;
pub mod lock;
pub mod registry;
pub mod terminal;
pub mod traits;
pub mod transport;

pub use client::{FrameConnection, FrameSink, RemotePane, RuntimeClient};
pub use framing::{read_frame, write_frame, MAX_FRAME_BYTES, PROTOCOL_VERSION};
pub use limits::RuntimeLimits;
pub use lock::RuntimeLock;
pub use registry::{Registry, RegistryEvent, TerminalInfo};
pub use terminal::{OutputSink, Terminal};
pub use traits::{PaneHandle, Runtime};
pub use transport::{current_session, socket_path, RuntimeListener, RuntimeStream};
