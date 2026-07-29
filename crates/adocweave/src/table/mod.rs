//! Typed table phases and public semantic model.
//!
//! A table is scanned, configured, and laid out exactly once. Each phase
//! consumes the previous phase so callers cannot mutate a public table and
//! request a second layout.

mod configuration;
mod layout;
mod model;
mod scan;

pub use model::{
    HorizontalAlignment, Table, TableCell, TableCellContent, TableCellStyle, TableColumn,
    TableFormat, TableFrame, TableGrid, TablePresentation, TableProblem, TableProblemKind,
    TableRow, TableSection, TableStripes, VerticalAlignment,
};

pub(crate) use configuration::ResolvedTableConfiguration;
pub(crate) use model::ConfiguredCell;
pub(crate) use scan::{is_table_delimiter, scan_with_configuration};
