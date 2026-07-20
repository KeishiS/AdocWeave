//! Deterministic resource and syntax-policy limits for public processing.
//!
//! The core is pure: it does not read files, environment variables, clocks,
//! networks, or execute external commands. Hosts provide all input explicitly.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxMode {
    Permissive,
    Strict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessingLimits {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_line_bytes: usize,
    pub max_list_depth: usize,
    pub max_inline_depth: usize,
    pub max_formula_bytes: usize,
    pub max_blocks: usize,
    pub max_nodes: usize,
    pub max_references: usize,
    pub max_attributes: usize,
    pub max_diagnostics: usize,
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
