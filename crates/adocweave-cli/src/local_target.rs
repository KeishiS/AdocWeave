//! Explicit filesystem validation for typed local targets.

use std::path::Path;

use adocweave::text::{PositionEncoding, SourceDocument};
use adocweave::{LocalTargetReference, LocalTargetSyntax};
use adocweave_host::{LocalTargetError, LocalTargetPolicy, LocalTargetSession, ResourceLimits};

#[derive(Clone, Debug)]
pub struct HostDiagnostic {
    pub code: &'static str,
    pub message: &'static str,
    pub source_id: String,
    pub range: adocweave::text::TextRange,
    pub target: String,
    pub line: u32,
    pub column: u32,
}

pub fn validate(
    targets: &[LocalTargetReference],
    base: &Path,
    project_root: &Path,
    source_id: &str,
    source: &str,
    limits: ResourceLimits,
) -> Result<Vec<HostDiagnostic>, LocalTargetError> {
    let policy = LocalTargetPolicy::new(project_root)?;
    let mut session = LocalTargetSession::new(policy, limits.max_files, limits);
    Ok(validate_with_session(
        targets,
        base,
        source_id,
        source,
        &mut session,
    ))
}

pub fn validate_with_session(
    targets: &[LocalTargetReference],
    base: &Path,
    source_id: &str,
    source: &str,
    session: &mut LocalTargetSession,
) -> Vec<HostDiagnostic> {
    targets
        .iter()
        .filter_map(|target| {
            let error = if target.syntax == LocalTargetSyntax::Unverifiable {
                LocalTargetError::Unverifiable(target.target.clone())
            } else {
                match session.inspect(base, &target.path) {
                    Ok(_) => return None,
                    Err(error) => error,
                }
            };
            let (line, column) = line_column(source, target.target_range.start().to_u32() as usize);
            Some(HostDiagnostic {
                code: error.diagnostic_code(),
                message: message(&error),
                source_id: source_id.to_owned(),
                range: target.target_range,
                target: target.target.clone(),
                line,
                column,
            })
        })
        .collect()
}

pub fn diagnostic_from_error(
    error: &LocalTargetError,
    source_id: &str,
    source: &str,
    range: adocweave::text::TextRange,
    target: &str,
) -> HostDiagnostic {
    let (line, column) = line_column(source, range.start().to_u32() as usize);
    HostDiagnostic {
        code: error.diagnostic_code(),
        message: message(error),
        source_id: source_id.to_owned(),
        range,
        target: target.to_owned(),
        line,
        column,
    }
}

pub fn render_human(
    diagnostics: &[HostDiagnostic],
    source: &str,
) -> Result<String, adocweave::text::PositionError> {
    use std::fmt::Write as _;

    let document = SourceDocument::new(source)?;
    let mut output = String::new();
    for diagnostic in diagnostics {
        let _ = document.offset_to_position(diagnostic.range.start(), PositionEncoding::Utf8)?;
        let source_id = visible(&diagnostic.source_id);
        let target = visible(&diagnostic.target);
        writeln!(
            output,
            "{}:{}:{}: error[{}]: {} (target: {})",
            source_id,
            diagnostic.line,
            diagnostic.column,
            diagnostic.code,
            diagnostic.message,
            target
        )
        .expect("writing to a String cannot fail");
    }
    Ok(output)
}

fn visible(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        if character.is_control() {
            output.extend(character.escape_default());
        } else {
            output.push(character);
        }
    }
    output
}

pub fn json_values(diagnostics: &[HostDiagnostic]) -> Vec<serde_json::Value> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            serde_json::json!({
                "id": format!(
                    "{}@{}:{}:{}",
                    diagnostic.code,
                    diagnostic.source_id,
                    diagnostic.range.start().to_u32(),
                    diagnostic.range.end().to_u32()
                ),
                "code": diagnostic.code,
                "severity": "error",
                "message": diagnostic.message,
                "sourceId": diagnostic.source_id,
                "range": {
                    "start": diagnostic.range.start().to_u32(),
                    "end": diagnostic.range.end().to_u32()
                },
                "target": diagnostic.target
                ,"line": diagnostic.line
                ,"column": diagnostic.column
            })
        })
        .collect()
}

fn line_column(source: &str, offset: usize) -> (u32, u32) {
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32 + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, tail)| tail)
        .len() as u32
        + 1;
    (line, column)
}

const fn message(error: &LocalTargetError) -> &'static str {
    match error {
        LocalTargetError::Missing(_) => "local target does not exist",
        LocalTargetError::OutsideRoot(_) => "local target is outside the project root",
        LocalTargetError::NotFile(_) | LocalTargetError::NotDirectory(_) => {
            "local target is not a regular file"
        }
        LocalTargetError::PermissionDenied(_) => "local target cannot be read",
        LocalTargetError::LimitExceeded { .. } => "local target inspection limit exceeded",
        LocalTargetError::Unverifiable(_)
        | LocalTargetError::ResourceTooLarge(_)
        | LocalTargetError::ReadLimitExceeded => "local target cannot be verified",
    }
}
