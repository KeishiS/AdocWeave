//! Strict, versioned project configuration shared by AdocWeave consumers.
//!
//! Parsing a project file never grants filesystem or network authority. The
//! resolved paths and limits remain inputs to a host policy that must restrict
//! them to an independently trusted workspace boundary.
#![warn(missing_docs)]

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use adocweave::output::diagnostics::{LintConfig, RuleSettings, Severity, lint_rule};
use adocweave::output::formatter::{FormatConfig, NewlineStyle};
use adocweave::output::html::{HtmlDocumentMode, RenderPolicy};
use adocweave::preprocess::PreprocessOptions;
use adocweave::{AnalysisOptions, SyntaxMode};
use adocweave_host::ResourceLimits;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Conventional project configuration filename.
pub const FILE_NAME: &str = ".adocweave.toml";
/// Configuration schema version accepted by this package.
pub const SCHEMA_VERSION: u32 = 1;

/// Stable category for configuration failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigErrorCode {
    /// Configuration or search path could not be read safely.
    ReadFailed,
    /// Search start lies outside its trusted boundary.
    OutsideBoundary,
    /// TOML is malformed or contains an unknown field.
    InvalidToml,
    /// Schema version is not supported.
    UnsupportedSchema,
    /// Lint rule identifier is unknown.
    InvalidRule,
    /// External attribute has an ambiguous value.
    InvalidAttribute,
    /// Configured processing limit is invalid.
    InvalidLimit,
    /// Configured path is absolute or escapes its configuration directory.
    InvalidPath,
}

impl ConfigErrorCode {
    /// Returns the stable kebab-case code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadFailed => "read-failed",
            Self::OutsideBoundary => "outside-boundary",
            Self::InvalidToml => "invalid-toml",
            Self::UnsupportedSchema => "unsupported-schema",
            Self::InvalidRule => "invalid-rule",
            Self::InvalidAttribute => "invalid-attribute",
            Self::InvalidLimit => "invalid-limit",
            Self::InvalidPath => "invalid-path",
        }
    }
}

/// Configuration error that never contains authored attribute values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigError {
    /// Stable error category.
    pub code: ConfigErrorCode,
    /// Schema field associated with the error, when available.
    pub field: Option<String>,
    message: &'static str,
}

impl ConfigError {
    const fn new(code: ConfigErrorCode, message: &'static str) -> Self {
        Self {
            code,
            field: None,
            message,
        }
    }

    fn at(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(field) = &self.field {
            write!(
                formatter,
                "configuration {} at {field}: {}",
                self.code.as_str(),
                self.message
            )
        } else {
            write!(
                formatter,
                "configuration {}: {}",
                self.code.as_str(),
                self.message
            )
        }
    }
}

impl Error for ConfigError {}

/// One immutable, content-addressed view of a project configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigSnapshot {
    /// Canonical path of the selected configuration.
    pub path: PathBuf,
    /// SHA-256 digest of the exact UTF-8 configuration content.
    pub content_sha256: [u8; 32],
    /// Fully resolved typed configuration.
    pub config: ResolvedProjectConfig,
}

impl ConfigSnapshot {
    /// Loads an explicitly selected project configuration.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let path = fs::canonicalize(path).map_err(|_| {
            ConfigError::new(
                ConfigErrorCode::ReadFailed,
                "the project file cannot be resolved",
            )
        })?;
        let directory = path.parent().ok_or_else(|| {
            ConfigError::new(
                ConfigErrorCode::ReadFailed,
                "the project file has no parent directory",
            )
        })?;
        let source = fs::read_to_string(&path).map_err(|_| {
            ConfigError::new(
                ConfigErrorCode::ReadFailed,
                "the project file cannot be read as UTF-8",
            )
        })?;
        let content_sha256 = Sha256::digest(source.as_bytes()).into();
        let config = ResolvedProjectConfig::parse(&source, directory)?;
        Ok(Self {
            path,
            content_sha256,
            config,
        })
    }
}

/// Finds the nearest project file without searching above `boundary`.
///
/// Both paths must already exist. A file `start` searches from its parent.
pub fn discover(start: &Path, boundary: &Path) -> Result<Option<PathBuf>, ConfigError> {
    let boundary = fs::canonicalize(boundary).map_err(|_| {
        ConfigError::new(
            ConfigErrorCode::ReadFailed,
            "the search boundary cannot be resolved",
        )
    })?;
    let start = fs::canonicalize(start).map_err(|_| {
        ConfigError::new(
            ConfigErrorCode::ReadFailed,
            "the search start cannot be resolved",
        )
    })?;
    let mut directory = if start.is_dir() {
        start
    } else {
        start
            .parent()
            .expect("a canonical file path has a parent")
            .to_path_buf()
    };
    if !directory.starts_with(&boundary) {
        return Err(ConfigError::new(
            ConfigErrorCode::OutsideBoundary,
            "the search start is outside the trusted boundary",
        ));
    }

    loop {
        let candidate = directory.join(FILE_NAME);
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ConfigError::new(
                    ConfigErrorCode::ReadFailed,
                    "the discovered project configuration cannot be a symbolic link",
                ));
            }
            Ok(metadata) if metadata.is_file() => return Ok(Some(candidate)),
            Ok(_) => {
                return Err(ConfigError::new(
                    ConfigErrorCode::ReadFailed,
                    "the project configuration path is not a file",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(ConfigError::new(
                    ConfigErrorCode::ReadFailed,
                    "the project configuration path cannot be inspected",
                ));
            }
        }
        if directory == boundary {
            return Ok(None);
        }
        if !directory.pop() {
            return Ok(None);
        }
    }
}

/// Discovers and loads one project configuration snapshot.
pub fn discover_and_load(
    start: &Path,
    boundary: &Path,
) -> Result<Option<ConfigSnapshot>, ConfigError> {
    discover(start, boundary)?
        .map(|path| ConfigSnapshot::load(&path))
        .transpose()
}

/// Include policy and bounded local resource settings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResourceSettings {
    /// Whether include preprocessing is enabled.
    pub include: bool,
    /// Configuration-relative roots proposed to the host policy.
    pub roots: Vec<PathBuf>,
    /// Resource limits no greater than built-in ceilings.
    pub limits: ResourceLimits,
}

/// Local target validation settings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalTargetSettings {
    /// Whether local target validation is enabled.
    pub enabled: bool,
    /// Configuration-relative project root proposed to the host policy.
    pub project_root: Option<PathBuf>,
}

/// Complete-document rendering and stylesheet settings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HtmlSettings {
    /// Deterministic HTML rendering policy.
    pub policy: RenderPolicy,
    /// Configuration-relative stylesheet files.
    pub stylesheet_files: Vec<PathBuf>,
    /// Authored stylesheet URLs, subject to the active URL policy.
    pub stylesheet_urls: Vec<String>,
}

/// Fully typed schema-version-1 project configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProjectConfig {
    /// Parsed schema version.
    pub schema_version: u32,
    /// Core syntax, attribute, and diagnostic options.
    pub analysis: AnalysisOptions,
    /// Include preprocessing options shared with analysis attributes.
    pub preprocess: PreprocessOptions,
    /// Local resource settings.
    pub resources: ResourceSettings,
    /// Local target validation settings.
    pub local_targets: LocalTargetSettings,
    /// Formatter settings.
    pub format: FormatConfig,
    /// Whether `format.newline` was present in the project file.
    pub format_newline_explicit: bool,
    /// Whether `format.final-newline` was present in the project file.
    pub format_final_newline_explicit: bool,
    /// HTML and stylesheet settings.
    pub html: HtmlSettings,
}

impl Default for ResolvedProjectConfig {
    fn default() -> Self {
        let preprocess = PreprocessOptions {
            enable_includes: false,
            ..PreprocessOptions::default()
        };
        Self {
            schema_version: SCHEMA_VERSION,
            analysis: AnalysisOptions::default(),
            preprocess,
            resources: ResourceSettings::default(),
            local_targets: LocalTargetSettings::default(),
            format: FormatConfig::default(),
            format_newline_explicit: false,
            format_final_newline_explicit: false,
            html: HtmlSettings::default(),
        }
    }
}

impl ResolvedProjectConfig {
    /// Parses strict TOML and resolves relative paths against `directory`.
    pub fn parse(source: &str, directory: &Path) -> Result<Self, ConfigError> {
        let wire: ProjectConfigWire = toml::from_str(source).map_err(|_| {
            ConfigError::new(
                ConfigErrorCode::InvalidToml,
                "the project file is not valid strict TOML",
            )
        })?;
        wire.resolve(directory)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ProjectConfigWire {
    schema_version: u32,
    #[serde(default)]
    analysis: AnalysisWire,
    #[serde(default)]
    lint: LintWire,
    #[serde(default)]
    resources: ResourcesWire,
    #[serde(default)]
    local_targets: LocalTargetsWire,
    #[serde(default)]
    format: FormatWire,
    #[serde(default)]
    html: HtmlWire,
}

impl ProjectConfigWire {
    fn resolve(self, directory: &Path) -> Result<ResolvedProjectConfig, ConfigError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ConfigError::new(
                ConfigErrorCode::UnsupportedSchema,
                "only schema version 1 is supported",
            )
            .at("schema-version"));
        }

        let mut resolved = ResolvedProjectConfig::default();
        resolved.analysis.syntax.syntax_mode = self.analysis.syntax_mode.into();
        for (name, attribute) in self.analysis.attributes {
            let value = attribute.resolve(&format!("analysis.attributes.{name}"))?;
            resolved.analysis.attributes.insert(name, value);
        }
        resolved.preprocess.attributes = resolved.analysis.attributes.clone();
        self.lint.apply(&mut resolved.analysis.diagnostics.lint)?;
        resolved.resources = self.resources.resolve(directory)?;
        resolved.preprocess.enable_includes = resolved.resources.include;
        resolved.preprocess.max_total_bytes =
            u32::try_from(resolved.resources.limits.max_total_bytes).map_err(|_| {
                ConfigError::new(ConfigErrorCode::InvalidLimit, "limit exceeds u32")
                    .at("resources.max-total-bytes")
            })?;
        resolved.local_targets = self.local_targets.resolve(directory)?;
        resolved.format_newline_explicit = self.format.newline.is_some();
        resolved.format_final_newline_explicit = self.format.final_newline.is_some();
        resolved.format = self.format.resolve()?;
        resolved.html = self.html.resolve(directory)?;
        Ok(resolved)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum SyntaxModeWire {
    #[default]
    Permissive,
    Strict,
}

impl From<SyntaxModeWire> for SyntaxMode {
    fn from(value: SyntaxModeWire) -> Self {
        match value {
            SyntaxModeWire::Permissive => Self::Permissive,
            SyntaxModeWire::Strict => Self::Strict,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct AnalysisWire {
    #[serde(default)]
    syntax_mode: SyntaxModeWire,
    #[serde(default)]
    attributes: BTreeMap<String, AttributeWire>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct AttributeWire {
    value: Option<String>,
    unset: Option<bool>,
}

impl AttributeWire {
    fn resolve(self, field: &str) -> Result<Option<String>, ConfigError> {
        match (self.value, self.unset) {
            (Some(value), None) => Ok(Some(value)),
            (None, Some(true)) => Ok(None),
            _ => Err(ConfigError::new(
                ConfigErrorCode::InvalidAttribute,
                "set exactly one of value or unset=true",
            )
            .at(field)),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct LintWire {
    #[serde(default)]
    rules: BTreeMap<String, RuleWire>,
    max_line_length: Option<usize>,
    max_consecutive_blank_lines: Option<usize>,
    max_diagnostics: Option<usize>,
}

impl LintWire {
    fn apply(self, config: &mut LintConfig) -> Result<(), ConfigError> {
        if let Some(value) = self.max_line_length {
            ensure_positive(value, "lint.max-line-length")?;
            config.max_line_length = value;
        }
        if let Some(value) = self.max_consecutive_blank_lines {
            ensure_positive(value, "lint.max-consecutive-blank-lines")?;
            config.max_consecutive_blank_lines = value;
        }
        if let Some(value) = self.max_diagnostics {
            ensure_positive(value, "lint.max-diagnostics")?;
            config.max_diagnostics = value;
        }
        for (name, rule) in self.rules {
            let Some(descriptor) = lint_rule(&name) else {
                return Err(
                    ConfigError::new(ConfigErrorCode::InvalidRule, "unknown lint rule")
                        .at(format!("lint.rules.{name}")),
                );
            };
            let current = config.rule(descriptor.id);
            config.set_rule(
                descriptor.id,
                RuleSettings {
                    enabled: rule.enabled.unwrap_or(current.enabled),
                    severity: rule
                        .severity
                        .map(SeverityWire::into)
                        .unwrap_or(current.severity),
                },
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum SeverityWire {
    Error,
    Warning,
    Information,
    Hint,
}

impl From<SeverityWire> for Severity {
    fn from(value: SeverityWire) -> Self {
        match value {
            SeverityWire::Error => Self::Error,
            SeverityWire::Warning => Self::Warning,
            SeverityWire::Information => Self::Information,
            SeverityWire::Hint => Self::Hint,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RuleWire {
    enabled: Option<bool>,
    severity: Option<SeverityWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ResourcesWire {
    #[serde(default)]
    include: bool,
    #[serde(default)]
    roots: Vec<PathBuf>,
    max_files: Option<usize>,
    max_total_bytes: Option<u64>,
    max_resource_bytes: Option<u64>,
}

impl ResourcesWire {
    fn resolve(self, directory: &Path) -> Result<ResourceSettings, ConfigError> {
        let ceiling = ResourceLimits::default();
        let limits = ResourceLimits {
            max_files: bounded(self.max_files, ceiling.max_files, "resources.max-files")?,
            max_total_bytes: bounded(
                self.max_total_bytes,
                ceiling.max_total_bytes,
                "resources.max-total-bytes",
            )?,
            max_resource_bytes: bounded(
                self.max_resource_bytes,
                ceiling.max_resource_bytes,
                "resources.max-resource-bytes",
            )?,
        };
        if limits.max_resource_bytes > limits.max_total_bytes {
            return Err(ConfigError::new(
                ConfigErrorCode::InvalidLimit,
                "resource limit exceeds the total byte limit",
            )
            .at("resources.max-resource-bytes"));
        }
        let roots = self
            .roots
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                resolve_relative(directory, path, format!("resources.roots.{index}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ResourceSettings {
            include: self.include,
            roots,
            limits,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct LocalTargetsWire {
    #[serde(default)]
    enabled: bool,
    project_root: Option<PathBuf>,
}

impl LocalTargetsWire {
    fn resolve(self, directory: &Path) -> Result<LocalTargetSettings, ConfigError> {
        let project_root = self
            .project_root
            .map(|path| resolve_relative(directory, path, "local-targets.project-root"))
            .transpose()?;
        if self.enabled && project_root.is_none() {
            return Err(ConfigError::new(
                ConfigErrorCode::InvalidPath,
                "enabled local target checks require project-root",
            )
            .at("local-targets.project-root"));
        }
        Ok(LocalTargetSettings {
            enabled: self.enabled,
            project_root,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum NewlineWire {
    #[default]
    Lf,
    CrLf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct FormatWire {
    newline: Option<NewlineWire>,
    final_newline: Option<bool>,
    #[serde(default = "default_blank_lines")]
    max_consecutive_blank_lines: usize,
}

impl Default for FormatWire {
    fn default() -> Self {
        Self {
            newline: None,
            final_newline: None,
            max_consecutive_blank_lines: default_blank_lines(),
        }
    }
}

impl FormatWire {
    fn resolve(self) -> Result<FormatConfig, ConfigError> {
        ensure_positive(
            self.max_consecutive_blank_lines,
            "format.max-consecutive-blank-lines",
        )?;
        Ok(FormatConfig {
            newline: match self.newline.unwrap_or_default() {
                NewlineWire::Lf => NewlineStyle::Lf,
                NewlineWire::CrLf => NewlineStyle::CrLf,
            },
            final_newline: self.final_newline.unwrap_or_else(default_true),
            max_consecutive_blank_lines: self.max_consecutive_blank_lines,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct HtmlWire {
    #[serde(default)]
    complete: bool,
    #[serde(default)]
    stylesheet_files: Vec<PathBuf>,
    #[serde(default)]
    stylesheet_urls: Vec<String>,
}

impl HtmlWire {
    fn resolve(self, directory: &Path) -> Result<HtmlSettings, ConfigError> {
        let mut policy = RenderPolicy {
            document_mode: if self.complete {
                HtmlDocumentMode::Complete
            } else {
                HtmlDocumentMode::Fragment
            },
            ..RenderPolicy::default()
        };
        policy.stylesheets.sources.clear();
        let stylesheet_files = self
            .stylesheet_files
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                resolve_relative(directory, path, format!("html.stylesheet-files.{index}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(HtmlSettings {
            policy,
            stylesheet_files,
            stylesheet_urls: self.stylesheet_urls,
        })
    }
}

fn default_true() -> bool {
    true
}

fn default_blank_lines() -> usize {
    1
}

fn ensure_positive(value: usize, field: &str) -> Result<(), ConfigError> {
    if value == 0 {
        return Err(
            ConfigError::new(ConfigErrorCode::InvalidLimit, "limit must be positive").at(field),
        );
    }
    Ok(())
}

fn bounded<T>(value: Option<T>, ceiling: T, field: &str) -> Result<T, ConfigError>
where
    T: Copy + Ord + From<u8>,
{
    let value = value.unwrap_or(ceiling);
    if value < T::from(1) || value > ceiling {
        return Err(ConfigError::new(
            ConfigErrorCode::InvalidLimit,
            "limit must be positive and cannot exceed the host ceiling",
        )
        .at(field));
    }
    Ok(value)
}

fn resolve_relative(
    directory: &Path,
    path: PathBuf,
    field: impl Into<String>,
) -> Result<PathBuf, ConfigError> {
    if path.as_os_str().is_empty() {
        return Err(
            ConfigError::new(ConfigErrorCode::InvalidPath, "path must not be empty").at(field),
        );
    }
    if path.is_absolute() {
        return Err(ConfigError::new(
            ConfigErrorCode::InvalidPath,
            "project settings cannot grant an absolute path",
        )
        .at(field));
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err(ConfigError::new(
            ConfigErrorCode::InvalidPath,
            "project settings cannot escape their directory",
        )
        .at(field));
    }
    Ok(directory.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "adocweave-config-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove test directory");
        }
    }

    #[test]
    fn strict_project_config_resolves_shared_consumer_options() {
        let config = ResolvedProjectConfig::parse(
            r#"
schema-version = 1

[analysis]
syntax-mode = "strict"

[analysis.attributes.release]
value = "draft"

[analysis.attributes.hidden]
unset = true

[lint.rules.macro-boundary]
enabled = true
severity = "error"

[lint.rules.trailing-whitespace]
enabled = false
severity = "hint"

[resources]
include = true
roots = ["docs"]
max-files = 20
max-total-bytes = 4096
max-resource-bytes = 2048

[local-targets]
enabled = true
project-root = "docs"

[format]
newline = "cr-lf"
final-newline = false
max-consecutive-blank-lines = 2

[html]
complete = true
stylesheet-files = ["styles/manual.css"]
stylesheet-urls = ["https://example.test/manual.css"]
"#,
            Path::new("/workspace"),
        )
        .expect("valid config");

        assert_eq!(config.analysis.syntax.syntax_mode, SyntaxMode::Strict);
        assert_eq!(
            config.analysis.attributes.get("release"),
            Some(&Some("draft".to_owned()))
        );
        assert_eq!(config.analysis.attributes.get("hidden"), Some(&None));
        assert_eq!(config.preprocess.attributes, config.analysis.attributes);
        assert!(
            config
                .analysis
                .diagnostics
                .lint
                .rule(lint_rule("macro-boundary").expect("known rule").id)
                .enabled
        );
        let trailing = config
            .analysis
            .diagnostics
            .lint
            .rule(lint_rule("trailing-whitespace").expect("known rule").id);
        assert!(!trailing.enabled);
        assert_eq!(trailing.severity, Severity::Hint);
        assert_eq!(config.resources.roots, [PathBuf::from("/workspace/docs")]);
        assert_eq!(
            config.local_targets.project_root,
            Some(PathBuf::from("/workspace/docs"))
        );
        assert_eq!(config.format.newline, NewlineStyle::CrLf);
        assert!(!config.format.final_newline);
        assert_eq!(config.html.policy.document_mode, HtmlDocumentMode::Complete);
    }

    #[test]
    fn rejects_unknown_fields_versions_rules_and_ambiguous_attributes() {
        for (source, code) in [
            ("schema-version = 2", ConfigErrorCode::UnsupportedSchema),
            (
                "schema-version = 1\nunknown = true",
                ConfigErrorCode::InvalidToml,
            ),
            (
                "schema-version = 1\n[lint.rules.unknown]\nenabled = true",
                ConfigErrorCode::InvalidRule,
            ),
            (
                "schema-version = 1\n[analysis.attributes.secret]\nvalue = \"x\"\nunset = true",
                ConfigErrorCode::InvalidAttribute,
            ),
            (
                "schema-version = 1\n[analysis.attributes.secret]\nvalue = \"x\"\nunset = false",
                ConfigErrorCode::InvalidAttribute,
            ),
        ] {
            assert_eq!(
                ResolvedProjectConfig::parse(source, Path::new("/workspace"))
                    .expect_err("invalid config")
                    .code,
                code
            );
        }
    }

    #[test]
    fn project_config_cannot_expand_host_authority() {
        for source in [
            "schema-version = 1\n[resources]\nroots = [\"../private\"]",
            "schema-version = 1\n[resources]\nroots = [\"/private\"]",
            "schema-version = 1\n[resources]\nmax-files = 10001",
            "schema-version = 1\n[resources]\nmax-total-bytes = 10\nmax-resource-bytes = 11",
        ] {
            assert!(ResolvedProjectConfig::parse(source, Path::new("/workspace")).is_err());
        }
    }

    #[test]
    fn errors_never_echo_attribute_values() {
        let error = ResolvedProjectConfig::parse(
            "schema-version = 1\n[analysis.attributes.secret]\nvalue = \"do-not-log\"\nunset = true",
            Path::new("/workspace"),
        )
        .expect_err("ambiguous attribute");
        assert!(!error.to_string().contains("do-not-log"));
    }

    #[test]
    fn discovery_stops_at_boundary_and_loads_content_addressed_snapshot() {
        let root = TestDirectory::new();
        let nested = root.0.join("docs/guide");
        fs::create_dir_all(&nested).expect("create nested directory");
        let config_path = root.0.join(FILE_NAME);
        fs::write(&config_path, "schema-version = 1\n").expect("write config");
        let input = nested.join("index.adoc");
        fs::write(&input, "= Guide\n").expect("write input");

        assert_eq!(
            discover(&input, &root.0).expect("discover config"),
            Some(config_path.canonicalize().expect("canonical config"))
        );
        let first = discover_and_load(&input, &root.0)
            .expect("load config")
            .expect("found config");
        let second = ConfigSnapshot::load(&config_path).expect("reload config");
        assert_eq!(first, second);
        assert_ne!(first.content_sha256, [0; 32]);
    }

    #[test]
    fn discovery_rejects_starts_outside_boundary() {
        let root = TestDirectory::new();
        let other = TestDirectory::new();
        assert_eq!(
            discover(&other.0, &root.0)
                .expect_err("outside boundary")
                .code,
            ConfigErrorCode::OutsideBoundary
        );
    }

    #[test]
    #[cfg(unix)]
    fn discovery_rejects_a_symbolic_linked_project_file() {
        use std::os::unix::fs::symlink;

        let project = TestDirectory::new();
        let outside = TestDirectory::new();
        fs::write(outside.0.join("config.toml"), "schema-version = 1\n").expect("outside config");
        symlink(outside.0.join("config.toml"), project.0.join(FILE_NAME)).expect("config symlink");

        let error = discover_and_load(&project.0, &project.0).expect_err("symlink rejected");
        assert_eq!(error.code, ConfigErrorCode::ReadFailed);
    }
}
