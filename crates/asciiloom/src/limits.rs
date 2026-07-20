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
    /// Reserved for list parsing; enforced when list nodes are enabled.
    pub max_list_depth: usize,
    pub max_inline_depth: usize,
    /// Reserved for document attributes; enforced when attributes are enabled.
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
