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
//! - **Ports** define trait interfaces: [`workspace::WorkspacePort`],
//!   [`runtime::SandboxRuntimePort`]
//! - **Adapters** implement those traits: [`adapters::git2_workspace::Git2Workspace`],
//!   [`adapters::cloud_hypervisor_runtime::CloudHypervisorRuntime`] (legacy,
//!   transitional per ADR-008)
//! - **Domain types** live in [`config`], [`policy`], [`protocol`], [`error`]
//! - **Orchestration** happens in [`sandbox::SandboxOrchestrator`]

pub mod adapters;
pub mod audit;
pub mod binary_resolve;
pub mod boot_meta;
pub mod ca;
pub mod config;
pub mod console;
pub mod egress;
pub mod error;
pub mod mcp_oauth;
pub mod policy;
pub mod project;
pub mod protocol;
pub mod proxy_bridge;
pub mod runtime;
pub mod sandbox;
pub mod services;
pub mod snapshot;
pub mod util;
pub mod vm;
pub mod workspace;
