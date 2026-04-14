//! `abox-core` — Domain logic for abox.
//!
//! This crate contains the core domain types, port traits (hexagonal architecture),
//! and adapter implementations for workspace management, VM lifecycle, credential
//! policy evaluation, and snapshot/template management.
//!
//! # Architecture
//!
//! The crate follows hexagonal (ports & adapters) architecture:
//!
//! - **Ports** define trait interfaces: [`workspace::WorkspacePort`], [`vm::VmPort`]
//! - **Adapters** implement those traits: [`adapters::git2_workspace::Git2Workspace`],
//!   [`adapters::cloud_hypervisor::CloudHypervisorAdapter`]
//! - **Domain types** live in [`config`], [`policy`], [`protocol`], [`error`]
//! - **Orchestration** happens in [`sandbox::SandboxOrchestrator`]

pub mod adapters;
pub mod boot_meta;
pub mod ca;
pub mod config;
pub mod console;
pub mod error;
pub mod policy;
pub mod protocol;
pub mod proxy_bridge;
pub mod sandbox;
pub mod snapshot;
pub mod util;
pub mod vm;
pub mod workspace;
