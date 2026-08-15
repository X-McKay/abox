//! Hash-chained audit logging for the proxy daemon.
//!
//! The writer, entry format, canonicalization, keyed hashing, and verification
//! all live in [`abox_core::audit`] so the daemon, the per-VM proxy bridge
//! (`abox_core::command_broker::FileAuditSink`), `abox audit verify`, and
//! `abox doctor` share one implementation and one chain that cannot drift.
//!
//! `AuditLog` is an alias for [`abox_core::audit::AuditChainWriter`]; multiple
//! writers (e.g. this daemon and an `abox run` orchestrator) may target the
//! same log file safely — each append takes a blocking exclusive lock and
//! re-derives the chain head under it, so entries are never dropped or forked.
//! See that module for the tamper-evidence guarantees and concurrency model.

pub use abox_core::audit::AuditChainWriter as AuditLog;
