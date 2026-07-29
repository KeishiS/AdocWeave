//! Shared native-host infrastructure for AdocWeave.
//!
//! This crate owns the bounded local-filesystem boundary. It deliberately does
//! not depend on the parser core or workspace state.

mod local_resource;
mod local_target;

pub use local_resource::{
    FilesystemReadLimits, FilesystemReadRollback, FilesystemReadRollbackResult,
    LoadedFilesystemSource, LocalFilesystemPolicy, LocalFilesystemSession, LogicalSourceId,
    ResourceBudget, ResourceError,
};
pub use local_target::{
    FilesystemRaceResistance, LoadedLocalTarget, LocalTargetError, LocalTargetPolicy,
    LocalTargetSession,
};
