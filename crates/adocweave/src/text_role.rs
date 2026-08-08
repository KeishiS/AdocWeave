//! Single decision table for whether a construct's text is prose or code.
//!
//! The browser search index and the textlint plan both need to know which
//! syntax carries body text and which carries verbatim code. Answering that
//! question in each consumer lets the two drift apart when a construct is
//! added, so both derive their classification from this table.

use crate::block_model::{AstBlock, DelimitedBlockKind};
use crate::table::TableCellContent;

/// How one block's own text participates in prose-oriented outputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockTextRole {
    /// Body text: linted as prose and indexed as searchable prose.
    Prose,
    /// Verbatim text: presented as code and excluded from prose linting.
    Code,
    /// No own text; children are classified individually.
    Container,
    /// Text that participates in neither prose nor code output.
    Excluded,
}

/// Classifies one block's own text.
///
/// List items and table cells carry their text on child nodes, so the block
/// itself is a container; see [`table_cell_text_role`] for cells.
pub const fn block_text_role(block: &AstBlock) -> BlockTextRole {
    match block {
        AstBlock::Heading(_) | AstBlock::Paragraph(_) => BlockTextRole::Prose,
        AstBlock::LiteralParagraph(_) | AstBlock::Source(_) | AstBlock::Verbatim(_) => {
            BlockTextRole::Code
        }
        AstBlock::List(_) => BlockTextRole::Container,
        AstBlock::Break(_) | AstBlock::Math(_) | AstBlock::Unsupported(_) => {
            BlockTextRole::Excluded
        }
        AstBlock::Delimited(block) => delimited_text_role(block.kind),
    }
}

/// Classifies the verbatim or nested text a delimited block carries.
pub const fn delimited_text_role(kind: DelimitedBlockKind) -> BlockTextRole {
    match kind {
        DelimitedBlockKind::Listing | DelimitedBlockKind::Literal => BlockTextRole::Code,
        // Passthrough carries raw output (often HTML): it is findable as code
        // but never prose. Comments carry no output at all.
        DelimitedBlockKind::Pass => BlockTextRole::Code,
        DelimitedBlockKind::Comment => BlockTextRole::Excluded,
        DelimitedBlockKind::Example
        | DelimitedBlockKind::Open
        | DelimitedBlockKind::Sidebar
        | DelimitedBlockKind::Quote
        | DelimitedBlockKind::Table => BlockTextRole::Container,
    }
}

/// Classifies the text one table cell carries.
pub const fn table_cell_text_role(content: &TableCellContent) -> BlockTextRole {
    match content {
        TableCellContent::Inlines(_) => BlockTextRole::Prose,
        TableCellContent::Verbatim(_) => BlockTextRole::Code,
        TableCellContent::AsciiDoc(_) => BlockTextRole::Container,
    }
}
