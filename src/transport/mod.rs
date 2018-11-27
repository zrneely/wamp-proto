//! Contains first-party transport implementations.

/// A websocket-based transport.
#[cfg(feature = "ws_transport")]
pub mod websocket;
