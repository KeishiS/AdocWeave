//! Shared native-host infrastructure for AdocWeave.
//!
//! This crate owns the bounded local-filesystem boundary. It deliberately does
//! not depend on the parser core or workspace state.

mod exit_status;
mod filesystem_job;
mod filesystem_limits;
mod local_resource;
mod local_target;

pub use exit_status::ExitStatus;
pub use filesystem_job::{
    FilesystemJobCoordinator, FilesystemJobError, FilesystemJobId, FilesystemJobLimit,
    FilesystemJobLimits, FilesystemJobUsage,
};
pub use filesystem_limits::FilesystemReadLimits;
pub use local_resource::{
    DerivedFilesystemRoots, FilesystemDraftError, FilesystemReadOutcome, FilesystemReleaseOutcome,
    FilesystemResourceBinding, LoadedFilesystemSource, LocalFilesystemDraft, LocalFilesystemPolicy,
    LocalFilesystemSession, LocalFilesystemSessionId, LogicalSourceId, PreparedFilesystemCommit,
    ResourceBudget, ResourceError,
};
pub use local_target::{
    FilesystemRaceResistance, LoadedLocalBytes, LoadedLocalTarget, LocalTargetError,
    LocalTargetPolicy, LocalTargetSession,
};
