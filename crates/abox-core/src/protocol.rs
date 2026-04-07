//! Shared protocol types for communication between the guest shim and host proxy.
//!
//! These types are defined in the tiny `abox-protocol` crate so the guest
//! shim (a static musl binary) can depend on them without pulling in tokio,
//! git2, or any other heavy dependency. They are re-exported here so existing
//! call sites under `abox_core::protocol::*` keep working.

pub use abox_protocol::{ProxyRequest, ProxyResponse};
