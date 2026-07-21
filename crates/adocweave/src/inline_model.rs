//! Output-independent inline semantic model.

use crate::source::TextRange;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineText {
    pub range: TextRange,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Link {
    pub range: TextRange,
    pub target_range: TextRange,
    pub target_source: String,
    pub target: String,
    pub target_attributes: Vec<AttributeUse>,
    pub target_expansion_error: Option<crate::substitution::AttributeExpansionError>,
    pub label_range: Option<TextRange>,
    pub label: Vec<Inline>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttributeUse {
    pub name: String,
    pub name_range: TextRange,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StandardMacroKind {
    Email,
    Footnote,
    Anchor,
    BibliographyAnchor,
    IndexTerm,
    Keyboard,
    Button,
    Menu,
    Image,
    Icon,
    Audio,
    Video,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MacroForm {
    Inline,
    Block,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MacroAttribute {
    pub range: TextRange,
    pub name: Option<String>,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StandardMacro {
    pub kind: StandardMacroKind,
    pub form: MacroForm,
    pub range: TextRange,
    pub target_range: TextRange,
    pub target_source: String,
    pub target: String,
    pub target_attributes: Vec<AttributeUse>,
    pub target_expansion_error: Option<crate::substitution::AttributeExpansionError>,
    pub attributes_range: TextRange,
    pub attributes: Vec<MacroAttribute>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MathLanguage {
    Latex,
    Typst,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineFormula {
    pub range: TextRange,
    pub content_range: TextRange,
    pub language: MathLanguage,
    pub value: String,
    pub closed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reference {
    pub range: TextRange,
    pub target_range: TextRange,
    pub target_source: String,
    pub destination: ReferenceDestination,
    pub label_range: Option<TextRange>,
    pub label: Vec<Inline>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReferenceDestination {
    Local {
        anchor: String,
        anchor_range: TextRange,
    },
    Document {
        document: String,
        document_range: TextRange,
        anchor: Option<String>,
        anchor_range: Option<TextRange>,
    },
    Scheme {
        scheme: String,
        scheme_range: TextRange,
        locator: String,
        locator_range: TextRange,
        anchor: Option<String>,
        anchor_range: Option<TextRange>,
    },
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Inline {
    Text(InlineText),
    Literal {
        kind: InlineLiteralKind,
        range: TextRange,
        content_range: TextRange,
        value: String,
    },
    Styled {
        style: InlineStyle,
        range: TextRange,
        content_range: TextRange,
        children: Vec<Inline>,
    },
    AttributeReference {
        range: TextRange,
        name_range: TextRange,
        name: String,
        value: Option<String>,
        expansion_error: Option<crate::substitution::AttributeExpansionError>,
    },
    Link(Link),
    Reference(Reference),
    Formula(InlineFormula),
    Macro(StandardMacro),
    Passthrough {
        kind: PassthroughKind,
        range: TextRange,
        content_range: TextRange,
        value: String,
    },
    HardBreak {
        range: TextRange,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineLiteralKind {
    Monospace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PassthroughKind {
    SinglePlus,
    DoublePlus,
    TriplePlus,
    Macro,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineStyle {
    Strong,
    Emphasis,
    Highlight,
    Subscript,
    Superscript,
    CurvedDoubleQuote,
    CurvedSingleQuote,
}

impl Inline {
    pub const fn range(&self) -> TextRange {
        match self {
            Self::Text(text) => text.range,
            Self::Literal { range, .. }
            | Self::Styled { range, .. }
            | Self::AttributeReference { range, .. } => *range,
            Self::Link(link) => link.range,
            Self::Reference(reference) => reference.range,
            Self::Formula(formula) => formula.range,
            Self::Macro(node) => node.range,
            Self::Passthrough { range, .. } => *range,
            Self::HardBreak { range } => *range,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineProblemKind {
    UnclosedMonospace,
    UnclosedStrong,
    UnclosedEmphasis,
    UnclosedHighlight,
    UnclosedSubscript,
    UnclosedSuperscript,
    NestingLimitExceeded,
    UnclosedAttributeReference,
    IncompleteLink,
    UnclosedPassthrough,
    IncompleteCrossReference,
    InvalidCrossReference,
    UnclosedStem,
    EmptyStem,
    StemSizeLimitExceeded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InlineProblem {
    pub kind: InlineProblemKind,
    pub range: TextRange,
}
