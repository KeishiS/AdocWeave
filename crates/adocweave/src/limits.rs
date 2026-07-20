//! Deterministic resource and syntax-policy limits for public processing.
//!
//! The core is pure: it does not read files, environment variables, clocks,
//! networks, or execute external commands. Hosts provide all input explicitly.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SyntaxMode {
    #[default]
    Permissive,
    Strict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessingLimits {
    pub max_input_bytes: u32,
    pub max_output_bytes: u32,
    pub max_line_bytes: u32,
    pub max_list_depth: u32,
    pub max_inline_depth: u32,
    pub max_formula_bytes: u32,
    pub max_blocks: u32,
    pub max_nodes: u32,
    pub max_references: u32,
    pub max_attributes: u32,
    pub max_diagnostics: u32,
}

impl Default for ProcessingLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 10 * 1024 * 1024,
            max_output_bytes: 50 * 1024 * 1024,
            max_line_bytes: 1024 * 1024,
            max_list_depth: 8,
            max_inline_depth: 32,
            max_formula_bytes: 1024 * 1024,
            max_blocks: 100_000,
            max_nodes: 1_000_000,
            max_references: 100_000,
            max_attributes: 1_000,
            max_diagnostics: 1_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessConfig {
    pub limits: ProcessingLimits,
    pub syntax_mode: SyntaxMode,
}

impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            limits: ProcessingLimits::default(),
            syntax_mode: SyntaxMode::Permissive,
        }
    }
}
