//! JSON request values at the public WASM boundary.
//!
//! `protocol/public-api.json` is the source of truth for these shapes and
//! defaults. Generated enums and preprocess inputs are composed here; core
//! semantic types are introduced only by `request_conversion`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::preprocess_wire::WasmAnalysisPreprocessInput;
use crate::protocol_generated::WasmProductSet;
use crate::render_inputs::WasmRenderInputs;
use crate::request_enum_generated::{
    WasmDocumentMode, WasmSyntaxMode, WasmUnknownSourceLanguage,
    WasmUnresolvedReferencePresentation,
};
use crate::shared_wire_generated::{WasmMathLanguage, WasmSeverity};

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmRequest {
    pub package_version: String,
    pub source_id: Option<String>,
    pub version: u32,
    pub generation: u32,
    pub source: String,
    #[serde(default)]
    pub preprocess: Option<WasmAnalysisPreprocessInput>,
    #[serde(default)]
    pub products: WasmProductSet,
    #[serde(default)]
    pub render_inputs: WasmRenderInputs,
    #[serde(default)]
    pub analysis_options: WasmAnalysisOptions,
    #[serde(default)]
    pub render_policy: WasmRenderPolicy,
    #[serde(default)]
    pub output_limits: WasmOutputLimits,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmAnalysisOptions {
    pub syntax: WasmSyntaxOptions,
    pub diagnostics: WasmDiagnosticProfile,
    pub attributes: BTreeMap<String, Option<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmSyntaxOptions {
    pub syntax_mode: WasmSyntaxMode,
    pub limits: WasmLimits,
}

impl Default for WasmSyntaxOptions {
    fn default() -> Self {
        Self {
            syntax_mode: WasmSyntaxMode::Permissive,
            limits: WasmLimits::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmDiagnosticProfile {
    pub protected_attributes: BTreeMap<String, Option<String>>,
    pub authored_urls: WasmAuthoredUrlPolicy,
    pub max_diagnostics: u32,
    pub rules: BTreeMap<String, WasmRuleSettings>,
}

impl Default for WasmDiagnosticProfile {
    fn default() -> Self {
        Self {
            protected_attributes: BTreeMap::new(),
            authored_urls: WasmAuthoredUrlPolicy::default(),
            max_diagnostics: 1_000,
            rules: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmRuleSettings {
    pub enabled: bool,
    pub severity: WasmSeverity,
}

impl Default for WasmRuleSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            severity: WasmSeverity::Warning,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmRenderPolicy {
    pub active_urls: WasmActiveUrlPolicy,
    pub external_links: WasmExternalLinkPolicy,
    pub source_languages: WasmSourceLanguagePolicy,
    pub math_languages: Vec<WasmMathLanguage>,
    pub unresolved_references: WasmUnresolvedReferencePresentation,
    pub resources: WasmResourceCapabilities,
    pub document_mode: WasmDocumentMode,
    pub stylesheets: Vec<WasmStylesheet>,
}

impl Default for WasmRenderPolicy {
    fn default() -> Self {
        Self {
            active_urls: WasmActiveUrlPolicy::default(),
            external_links: WasmExternalLinkPolicy::default(),
            source_languages: WasmSourceLanguagePolicy::default(),
            math_languages: vec![WasmMathLanguage::Latex, WasmMathLanguage::Typst],
            unresolved_references: WasmUnresolvedReferencePresentation::Target,
            resources: WasmResourceCapabilities::default(),
            document_mode: WasmDocumentMode::Fragment,
            stylesheets: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmOutputLimits {
    pub max_output_bytes: u32,
}

impl Default for WasmOutputLimits {
    fn default() -> Self {
        Self {
            max_output_bytes: 52_428_800,
        }
    }
}

/// A host-supplied stylesheet forwarded to the core stylesheet policy.
/// Rejected sources surface as `renderDiagnostics`, never as emitted CSS.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum WasmStylesheet {
    Inline { css: String },
    External { url: String },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmExternalLinkPolicy {
    pub open_in_new_context: bool,
    pub noreferrer: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmSourceLanguagePolicy {
    pub allowed: Option<Vec<String>>,
    pub unknown: WasmUnknownSourceLanguage,
}

impl Default for WasmSourceLanguagePolicy {
    fn default() -> Self {
        Self {
            allowed: None,
            unknown: WasmUnknownSourceLanguage::PreserveSanitized,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmResourceCapabilities {
    pub images: bool,
    pub media: bool,
}

impl Default for WasmResourceCapabilities {
    fn default() -> Self {
        Self {
            images: true,
            media: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmLimits {
    pub max_input_bytes: u32,
    pub max_line_bytes: u32,
    pub max_list_depth: u32,
    pub max_list_continuations: u32,
    pub max_block_depth: u32,
    pub max_inline_depth: u32,
    pub max_formula_bytes: u32,
    pub max_table_bytes: u32,
    pub max_table_cells: u32,
    pub max_table_columns: u32,
    pub max_table_depth: u32,
    pub max_catalog_entries: u32,
    pub max_catalog_bytes: u32,
    pub max_blocks: u32,
    pub max_nodes: u32,
    pub max_references: u32,
    pub max_attributes: u32,
    pub max_attribute_expansion_depth: u32,
    pub max_attribute_expansion_bytes: u32,
}

impl Default for WasmLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 10_485_760,
            max_line_bytes: 1_048_576,
            max_list_depth: 8,
            max_list_continuations: 10_000,
            max_block_depth: 32,
            max_inline_depth: 32,
            max_formula_bytes: 1_048_576,
            max_table_bytes: 5_242_880,
            max_table_cells: 100_000,
            max_table_columns: 1_000,
            max_table_depth: 8,
            max_catalog_entries: 100_000,
            max_catalog_bytes: 5_242_880,
            max_blocks: 100_000,
            max_nodes: 1_000_000,
            max_references: 100_000,
            max_attributes: 1_000,
            max_attribute_expansion_depth: 32,
            max_attribute_expansion_bytes: 1_048_576,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmAuthoredUrlPolicy {
    pub allowed_schemes: Vec<String>,
    pub allow_relative: bool,
}

impl Default for WasmAuthoredUrlPolicy {
    fn default() -> Self {
        Self {
            allowed_schemes: vec!["http".to_owned(), "https".to_owned()],
            allow_relative: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmActiveUrlPolicy {
    pub allowed_schemes: Vec<String>,
    pub allow_authored_relative: bool,
    pub allow_resolved_relative: bool,
    pub allow_resolved_root_relative: bool,
    pub allow_data_uris: bool,
}

impl Default for WasmActiveUrlPolicy {
    fn default() -> Self {
        Self {
            allowed_schemes: vec!["http".to_owned(), "https".to_owned()],
            allow_authored_relative: false,
            allow_resolved_relative: false,
            allow_resolved_root_relative: false,
            allow_data_uris: false,
        }
    }
}
