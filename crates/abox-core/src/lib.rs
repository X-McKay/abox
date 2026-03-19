//! abox-core: Domain logic for agentbox.
//!
//! This crate contains the core domain types, port traits (hexagonal architecture),
//! and adapter implementations for workspace management, VM lifecycle, credential
//! policy evaluation, and snapshot/template management.

pub mod adapters;
pub mod config;
pub mod policy;
pub mod sandbox;
pub mod snapshot;
pub mod vm;
pub mod workspace;
