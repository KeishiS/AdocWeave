//! Shared native-host infrastructure for AdocWeave.
//!
//! This crate owns dependency tracking and the bounded local-filesystem boundary.
//! It deliberately does not depend on the parser core.

mod dependency_graph;
mod local_resource;
mod local_target;

pub use dependency_graph::DependencyGraph;
pub use local_resource::{
    LoadedLocalResource, LocalResourcePolicy, ResourceBudget, ResourceError, ResourceLimits,
    ValidatedFilesystemTarget, normalize_relative,
};
pub use local_target::{
    FilesystemRaceResistance, LoadedLocalTarget, LocalTargetError, LocalTargetPolicy,
    LocalTargetSession,
};
