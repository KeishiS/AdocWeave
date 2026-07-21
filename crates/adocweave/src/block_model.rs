//! Backend-independent block semantic model.

use crate::attributes::DocumentAttribute;
use crate::inline::{Inline, InlineProblem, MathLanguage};
use crate::source::{TextRange, TextSize};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BlockMetadata {
    pub range: Option<TextRange>,
    pub title: Option<MetadataValue>,
    pub id: Option<MetadataValue>,
    pub roles: Vec<MetadataValue>,
    pub options: Vec<MetadataValue>,
    pub attributes: Vec<ElementAttribute>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataValue {
    pub value: String,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElementAttribute {
    pub name: Option<String>,
    pub value: String,
    pub range: TextRange,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DocumentType {
    #[default]
    Article,
    Book,
    Manpage,
    Inline,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Author {
    pub range: TextRange,
    pub name_range: TextRange,
    pub email_range: Option<TextRange>,
    pub name: String,
    pub email: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Revision {
    pub range: TextRange,
    pub number: Option<MetadataValue>,
    pub date: Option<MetadataValue>,
    pub remark: Option<MetadataValue>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentHeader {
    pub range: Option<TextRange>,
    pub authors: Vec<Author>,
    pub revision: Option<Revision>,
    pub doctype: DocumentType,
    pub end: TextSize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paragraph {
    pub metadata: BlockMetadata,
    pub range: TextRange,
    pub content_range: TextRange,
    pub value: String,
    pub inlines: Vec<Inline>,
    pub(crate) inline_problems: Vec<InlineProblem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiteralParagraph {
    pub metadata: BlockMetadata,
    pub range: TextRange,
    pub content_range: TextRange,
    pub value: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BreakKind {
    Thematic,
    Page,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BreakBlock {
    pub metadata: BlockMetadata,
    pub range: TextRange,
    pub kind: BreakKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Unsupported {
    pub metadata: BlockMetadata,
    pub range: TextRange,
    pub raw: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplicitAnchor {
    pub range: TextRange,
    pub id_range: TextRange,
    pub label_range: Option<TextRange>,
    pub id: String,
    pub label: Option<String>,
    pub target_range: Option<TextRange>,
    pub valid: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockProblemKind {
    UnclosedBlock,
    MissingSourceLanguage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockProblem {
    pub kind: BlockProblemKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiteralBlock {
    pub metadata: BlockMetadata,
    pub range: TextRange,
    pub delimiter_range: TextRange,
    pub content_range: TextRange,
    pub value: String,
    pub(crate) problems: Vec<BlockProblem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceBlock {
    pub metadata: BlockMetadata,
    pub range: TextRange,
    pub attribute_range: TextRange,
    pub language_range: Option<TextRange>,
    pub language: Option<String>,
    pub delimiter_range: TextRange,
    pub content_range: TextRange,
    pub value: String,
    pub callouts: Vec<CalloutMarker>,
    pub(crate) problems: Vec<BlockProblem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DelimitedBlockKind {
    Comment,
    Example,
    Listing,
    Literal,
    Open,
    Sidebar,
    Pass,
    Quote,
    Table,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DelimitedContent {
    Compound(Vec<AstBlock>),
    Verbatim(String),
    Passthrough(String),
    Table(crate::table::Table),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelimitedBlock {
    pub metadata: BlockMetadata,
    pub kind: DelimitedBlockKind,
    pub range: TextRange,
    pub opening_delimiter_range: TextRange,
    pub closing_delimiter_range: Option<TextRange>,
    pub content_range: TextRange,
    pub delimiter: String,
    pub content: DelimitedContent,
    pub problems: Vec<BlockProblem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MathProblemKind {
    Unclosed,
    Empty,
    SizeLimitExceeded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MathProblem {
    pub kind: MathProblemKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MathBlock {
    pub metadata: BlockMetadata,
    pub range: TextRange,
    pub attribute_range: TextRange,
    pub delimiter_range: TextRange,
    pub content_range: TextRange,
    pub language: MathLanguage,
    pub value: String,
    pub(crate) problems: Vec<MathProblem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListKind {
    Unordered,
    Ordered,
    Description,
    Callout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChecklistState {
    Unchecked,
    Checked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DescriptionTerm {
    pub range: TextRange,
    pub text: String,
    pub inlines: Vec<Inline>,
    pub(crate) inline_problems: Vec<InlineProblem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CalloutMarker {
    pub id: u32,
    pub range: TextRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListProblemKind {
    EmptyItem,
    InconsistentMarker,
    InvalidNesting,
    DepthLimitExceeded,
    NonCanonicalSeparator,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListProblem {
    pub kind: ListProblemKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListBlock {
    pub metadata: BlockMetadata,
    pub kind: ListKind,
    pub range: TextRange,
    pub items: Vec<ListItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListItem {
    pub range: TextRange,
    pub marker_range: TextRange,
    pub separator_range: TextRange,
    pub text_range: TextRange,
    pub text: String,
    pub inlines: Vec<Inline>,
    pub terms: Vec<DescriptionTerm>,
    pub checklist: Option<ChecklistState>,
    pub callout_id: Option<u32>,
    pub(crate) inline_problems: Vec<InlineProblem>,
    pub children: Vec<ListBlock>,
    pub continuations: Vec<AstBlock>,
    pub continuation_ranges: Vec<TextRange>,
    pub(crate) problems: Vec<ListProblem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeadingKind {
    DocumentTitle,
    Part,
    Section { level: u8 },
    Discrete { level: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeadingProblem {
    MissingSpace,
    EmptyText,
    LevelTooDeep,
    MisplacedDocumentTitle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Heading {
    pub metadata: BlockMetadata,
    pub range: TextRange,
    pub marker_range: TextRange,
    pub separator_range: TextRange,
    pub text_range: TextRange,
    pub kind: HeadingKind,
    pub well_formed: bool,
    pub hierarchy_valid: bool,
    pub text: String,
    pub inlines: Vec<Inline>,
    pub(crate) inline_problems: Vec<InlineProblem>,
    pub(crate) problems: Vec<HeadingProblem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AstBlock {
    Heading(Heading),
    Paragraph(Paragraph),
    LiteralParagraph(LiteralParagraph),
    Break(BreakBlock),
    Literal(LiteralBlock),
    Source(SourceBlock),
    List(ListBlock),
    Math(MathBlock),
    Delimited(DelimitedBlock),
    Unsupported(Unsupported),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AstDocument {
    pub(crate) blocks: Vec<AstBlock>,
    pub(crate) attributes: Vec<DocumentAttribute>,
    pub(crate) anchors: Vec<ExplicitAnchor>,
    pub(crate) header: DocumentHeader,
    pub(crate) catalogs: crate::catalog::DocumentCatalogs,
    pub(crate) structure: crate::structure::DocumentStructure,
}
