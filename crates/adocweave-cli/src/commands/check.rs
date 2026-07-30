use std::path::Path;

use adocweave::output::diagnostics as diagnostic;
use adocweave::preprocess::{PreprocessedAnalysis, ProjectionLimits};
use adocweave::text::{PositionEncoding, SourceDocument};
use adocweave::{AnalysisOptions, Engine, ParseError};

use crate::check_output::{
    CheckOutcome, DiagnosticCounts, DiagnosticFormat, FailOn, github_annotation,
    prefix_human_source, sarif_log, sarif_result,
};
use crate::{diagnostic_json, local_include, local_target};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Options {
    pub(crate) format: DiagnosticFormat,
    pub(crate) fail_on: FailOn,
    pub(crate) summary: bool,
    pub(crate) fix: bool,
    pub(crate) dry_run: bool,
    pub(crate) list_rules: bool,
    pub(crate) enabled_rules: Vec<diagnostic::LintRuleId>,
}

#[derive(Debug)]
pub(crate) enum Error {
    InvalidUtf8 { valid_up_to: usize },
    Analysis(ParseError),
    Position(adocweave::text::PositionError),
    Include(local_include::LocalIncludeError),
    LocalTarget(adocweave_host::LocalTargetError),
    FixConflict(diagnostic::EditConflict),
    Serialize(String),
}

pub(crate) struct LocalContext<'context> {
    pub(crate) base: &'context Path,
    pub(crate) root: &'context Path,
    pub(crate) source_id: &'context str,
}

fn analysis_options(
    base: &AnalysisOptions,
    enabled_rules: &[diagnostic::LintRuleId],
) -> AnalysisOptions {
    let mut options = base.clone();
    for rule in enabled_rules {
        let current = options.diagnostics.lint.rule(*rule);
        options.diagnostics.lint.set_rule(
            *rule,
            diagnostic::RuleSettings {
                enabled: true,
                ..current
            },
        );
    }
    options
}

pub(crate) fn apply_safe_fixes(
    input: &[u8],
    check: &Options,
    base_analysis_options: &AnalysisOptions,
) -> Result<Vec<u8>, Error> {
    let source = decode_input(input)?;
    let analysis = Engine::new(analysis_options(
        base_analysis_options,
        &check.enabled_rules,
    ))
    .analyze(source)
    .map_err(Error::Analysis)?;
    let edits = analysis
        .diagnostics()
        .iter()
        .flat_map(|diagnostic| &diagnostic.fixes)
        .filter(|fix| fix.applicability == diagnostic::Applicability::Always)
        .flat_map(|fix| fix.edits().iter().cloned())
        .collect::<Vec<_>>();
    if edits.is_empty() {
        return Ok(input.to_vec());
    }
    let fix = diagnostic::Fix::new("apply safe fixes", diagnostic::Applicability::Always, edits)
        .map_err(Error::FixConflict)?;
    let mut fixed = source.to_owned();
    for edit in fix.edits().iter().rev() {
        fixed.replace_range(
            edit.range.start().to_usize()..edit.range.end().to_usize(),
            &edit.replacement,
        );
    }
    Ok(fixed.into_bytes())
}

pub(crate) fn process(
    input: &[u8],
    check: &Options,
    source_id: &str,
    base_analysis_options: &AnalysisOptions,
    preprocess_options: &adocweave::preprocess::PreprocessOptions,
    resource_limits: adocweave_host::FilesystemReadLimits,
    local: Option<LocalContext<'_>>,
) -> Result<CheckOutcome, Error> {
    let source = decode_input(input)?;
    let analysis = Engine::new(analysis_options(
        base_analysis_options,
        &check.enabled_rules,
    ))
    .analyze(source)
    .map_err(Error::Analysis)?;
    let mut host = if let Some(local) = local {
        let mut targets = analysis.local_targets();
        let snapshot =
            std::iter::empty::<(String, adocweave::preprocess::ResourceDocument)>().collect();
        let mut local_preprocess_options = preprocess_options.clone();
        local_preprocess_options.source_id = Some(adocweave::SourceId::new(local.source_id));
        local_preprocess_options.enable_includes = false;
        let include_document =
            adocweave::preprocess::preprocess(source, &snapshot, &local_preprocess_options)
                .map_err(|error| {
                    Error::Include(local_include::LocalIncludeError::Preprocess(error))
                })?;
        let includes = include_document
            .directives
            .iter()
            .filter(|directive| directive.kind == adocweave::preprocess::DirectiveKind::Include)
            .collect::<Vec<_>>();
        let optional_ranges = includes
            .iter()
            .filter(|include| include.optional)
            .map(|include| include.target_range)
            .collect::<Vec<_>>();
        targets.extend(includes.iter().filter_map(|include| include.local_target()));
        let mut diagnostics = local_target::validate(
            &targets,
            local.base,
            local.root,
            local.source_id,
            source,
            resource_limits,
        )
        .map_err(Error::LocalTarget)?;
        diagnostics.retain(|diagnostic| {
            diagnostic.code != "local-target-missing"
                || !optional_ranges.contains(&diagnostic.range)
        });
        diagnostics
    } else {
        Vec::new()
    };
    host.sort_by(|left, right| {
        (
            left.range.start(),
            left.range.end(),
            left.code,
            left.target.as_str(),
        )
            .cmp(&(
                right.range.start(),
                right.range.end(),
                right.code,
                right.target.as_str(),
            ))
    });
    let mut counts = DiagnosticCounts::default();
    for item in analysis.diagnostics() {
        counts.add(item.severity);
    }
    counts.add_host_errors(host.len());
    let output = match check.format {
        DiagnosticFormat::Json => {
            let mut sorted = analysis.diagnostics().to_vec();
            diagnostic::sort_diagnostics(&mut sorted);
            let mut values = sorted
                .iter()
                .map(|item| diagnostic_json::record(item, Some(source_id)))
                .collect::<Vec<_>>();
            values.extend(local_target::json_values(&host));
            serde_json::to_string(&values).map_err(|error| Error::Serialize(error.to_string()))?
        }
        DiagnosticFormat::Human => {
            let core = diagnostic::render_human(
                analysis.diagnostics(),
                analysis.source_document(),
                PositionEncoding::Utf8,
            )
            .map_err(Error::Position)?;
            prefix_human_source(&core, source_id)
                + &local_target::render_human(&host, source).map_err(Error::Position)?
        }
        DiagnosticFormat::Github => {
            let document = SourceDocument::new(source).map_err(Error::Position)?;
            let mut output = String::new();
            for item in analysis.diagnostics() {
                let position = document
                    .offset_to_position(item.range.start(), PositionEncoding::Utf8)
                    .map_err(Error::Position)?;
                output.push_str(&github_annotation(
                    item.severity,
                    item.code.as_str(),
                    &item.message,
                    source_id,
                    position.line + 1,
                    position.character + 1,
                ));
            }
            for item in &host {
                output.push_str(&github_annotation(
                    diagnostic::Severity::Error,
                    item.code,
                    item.message,
                    &item.source_id,
                    item.line,
                    item.column,
                ));
            }
            output
        }
        DiagnosticFormat::Sarif => {
            let document = SourceDocument::new(source).map_err(Error::Position)?;
            let mut results = Vec::new();
            for item in analysis.diagnostics() {
                let position = document
                    .offset_to_position(item.range.start(), PositionEncoding::Utf8)
                    .map_err(Error::Position)?;
                results.push(sarif_result(
                    item.id.as_str(),
                    item.severity,
                    item.code.as_str(),
                    &item.message,
                    source_id,
                    position.line + 1,
                    position.character + 1,
                ));
            }
            results.extend(host.iter().map(|item| {
                let id = format!(
                    "{}@{}:{}:{}",
                    item.code,
                    item.source_id,
                    item.range.start().to_u32(),
                    item.range.end().to_u32()
                );
                sarif_result(
                    &id,
                    diagnostic::Severity::Error,
                    item.code,
                    item.message,
                    &item.source_id,
                    item.line,
                    item.column,
                )
            }));
            sarif_log(results)
        }
    };
    Ok(CheckOutcome {
        output,
        counts,
        fail_on: check.fail_on,
    })
}

pub(crate) fn process_preprocessed(
    prepared: &mut local_include::PreparedInput,
    check: &Options,
    base_analysis_options: &AnalysisOptions,
) -> Result<CheckOutcome, Error> {
    let (projection, mut validation) = prepared.projection_and_validation_mut();
    let engine = Engine::new(analysis_options(
        base_analysis_options,
        &check.enabled_rules,
    ));
    let analysis = engine
        .analyze(&projection.document().source)
        .map_err(|error| {
            Error::Include(local_include::LocalIncludeError::Analysis(
                error.to_string(),
            ))
        })?;
    let projected = PreprocessedAnalysis {
        document: projection.document().clone(),
        analysis,
    }
    .project_origins(ProjectionLimits::default())
    .map_err(|error| {
        Error::Include(local_include::LocalIncludeError::Analysis(
            error.to_string(),
        ))
    })?;
    let mut host = Vec::new();
    if let Some(validation) = validation.as_mut() {
        for target in &projected.local_targets {
            for origin in &target.target_origins {
                let source_id = origin
                    .source_id
                    .as_ref()
                    .map_or("<stdin>", adocweave::SourceId::as_str);
                let directive = (target.value.kind == adocweave::LocalTargetKind::Include)
                    .then(|| {
                        projected.directives.iter().find(|directive| {
                            directive
                                .source_id
                                .as_ref()
                                .map(adocweave::SourceId::as_str)
                                == Some(source_id)
                                && directive.target_range == origin.range.text_range()
                        })
                    })
                    .flatten();
                let base = if directive.is_some() {
                    projection.include_base(source_id)
                } else {
                    projection.source_base(source_id)
                }
                .ok_or_else(|| {
                    Error::Include(local_include::LocalIncludeError::MissingSource(
                        source_id.to_owned(),
                    ))
                })?;
                let optional = directive.is_some_and(|directive| directive.optional);
                let source = projection.source(source_id).ok_or_else(|| {
                    Error::Include(local_include::LocalIncludeError::MissingSource(
                        source_id.to_owned(),
                    ))
                })?;
                if let Some(error) =
                    directive.and_then(|directive| validation.include_error(&directive.target))
                {
                    if optional && matches!(error, adocweave_host::LocalTargetError::Missing(_)) {
                        continue;
                    }
                    host.push(local_target::diagnostic_from_error(
                        error,
                        source_id,
                        source,
                        origin.range.text_range(),
                        &target.value.target,
                    ));
                    continue;
                }
                if optional && target.value.syntax == adocweave::LocalTargetSyntax::Candidate {
                    match validation.session_mut().inspect(base, &target.value.path) {
                        Ok(_) | Err(adocweave_host::LocalTargetError::Missing(_)) => continue,
                        Err(_) => {}
                    }
                }
                let mut value = target.value.clone();
                value.target_range = origin.range.text_range();
                host.extend(local_target::validate_with_session(
                    std::slice::from_ref(&value),
                    base,
                    source_id,
                    source,
                    validation.session_mut(),
                ));
            }
        }
        host.sort_by(|left, right| {
            (
                left.source_id.as_str(),
                left.range.start(),
                left.range.end(),
                left.code,
                left.target.as_str(),
            )
                .cmp(&(
                    right.source_id.as_str(),
                    right.range.start(),
                    right.range.end(),
                    right.code,
                    right.target.as_str(),
                ))
        });
    }
    let mut counts = DiagnosticCounts::default();
    for item in &projected.diagnostics {
        for _ in &item.origins {
            counts.add(item.diagnostic.severity);
        }
    }
    counts.add_host_errors(host.len());
    if check.format == DiagnosticFormat::Json {
        let mut values = projected
            .diagnostics
            .iter()
            .flat_map(|diagnostic| {
                diagnostic
                    .origins
                    .iter()
                    .map(move |origin| diagnostic_json::projected_record(diagnostic, origin))
            })
            .collect::<Vec<_>>();
        values.extend(local_target::json_values(&host));
        return serde_json::to_string(&values)
            .map(|output| CheckOutcome {
                output,
                counts,
                fail_on: check.fail_on,
            })
            .map_err(|error| Error::Serialize(error.to_string()));
    }
    if check.format == DiagnosticFormat::Sarif {
        let mut results = Vec::new();
        for diagnostic in &projected.diagnostics {
            for origin in &diagnostic.origins {
                let source_id = origin
                    .source_id
                    .as_ref()
                    .map_or("<unknown>", adocweave::SourceId::as_str);
                let source = projection.source(source_id).ok_or_else(|| {
                    Error::Include(local_include::LocalIncludeError::MissingSource(
                        source_id.to_owned(),
                    ))
                })?;
                let index = SourceDocument::new(source).map_err(|error| {
                    Error::Include(local_include::LocalIncludeError::Position(error))
                })?;
                let position = index
                    .offset_to_position(origin.range.start(), PositionEncoding::Utf8)
                    .map_err(|error| {
                        Error::Include(local_include::LocalIncludeError::Position(error))
                    })?;
                results.push(sarif_result(
                    &format!(
                        "{}@{}:{}:{}",
                        diagnostic.diagnostic.code.as_str(),
                        source_id,
                        origin.range.start().to_u32(),
                        origin.range.end().to_u32()
                    ),
                    diagnostic.diagnostic.severity,
                    diagnostic.diagnostic.code.as_str(),
                    &diagnostic.diagnostic.message,
                    source_id,
                    position.line + 1,
                    position.character + 1,
                ));
            }
        }
        results.extend(host.iter().map(|diagnostic| {
            let id = format!(
                "{}@{}:{}:{}",
                diagnostic.code,
                diagnostic.source_id,
                diagnostic.range.start().to_u32(),
                diagnostic.range.end().to_u32()
            );
            sarif_result(
                &id,
                diagnostic::Severity::Error,
                diagnostic.code,
                diagnostic.message,
                &diagnostic.source_id,
                diagnostic.line,
                diagnostic.column,
            )
        }));
        return Ok(CheckOutcome {
            output: sarif_log(results),
            counts,
            fail_on: check.fail_on,
        });
    }

    let mut output = String::new();
    for diagnostic in &projected.diagnostics {
        for origin in &diagnostic.origins {
            let source_id = origin
                .source_id
                .as_ref()
                .map_or("<unknown>", adocweave::SourceId::as_str);
            let source = projection.source(source_id).ok_or_else(|| {
                Error::Include(local_include::LocalIncludeError::MissingSource(
                    source_id.to_owned(),
                ))
            })?;
            let index = SourceDocument::new(source).map_err(|error| {
                Error::Include(local_include::LocalIncludeError::Position(error))
            })?;
            let position = index
                .offset_to_position(origin.range.start(), PositionEncoding::Utf8)
                .map_err(|error| {
                    Error::Include(local_include::LocalIncludeError::Position(error))
                })?;
            if check.format == DiagnosticFormat::Github {
                output.push_str(&github_annotation(
                    diagnostic.diagnostic.severity,
                    diagnostic.diagnostic.code.as_str(),
                    &diagnostic.diagnostic.message,
                    source_id,
                    position.line + 1,
                    position.character + 1,
                ));
                continue;
            }
            use std::fmt::Write as _;
            writeln!(
                output,
                "{}:{}:{}: {}[{}]: {}",
                source_id,
                position.line + 1,
                position.character + 1,
                diagnostic.diagnostic.severity.as_str(),
                diagnostic.diagnostic.code.as_str(),
                diagnostic.diagnostic.message,
            )
            .expect("writing to a String cannot fail");
        }
    }
    for diagnostic in &host {
        if check.format == DiagnosticFormat::Github {
            output.push_str(&github_annotation(
                diagnostic::Severity::Error,
                diagnostic.code,
                diagnostic.message,
                &diagnostic.source_id,
                diagnostic.line,
                diagnostic.column,
            ));
            continue;
        }
        let source = projection.source(&diagnostic.source_id).ok_or_else(|| {
            Error::Include(local_include::LocalIncludeError::MissingSource(
                diagnostic.source_id.clone(),
            ))
        })?;
        output.push_str(
            &local_target::render_human(std::slice::from_ref(diagnostic), source).map_err(
                |error| Error::Include(local_include::LocalIncludeError::Position(error)),
            )?,
        );
    }
    Ok(CheckOutcome {
        output,
        counts,
        fail_on: check.fail_on,
    })
}

fn decode_input(input: &[u8]) -> Result<&str, Error> {
    std::str::from_utf8(input).map_err(|error| Error::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(format: DiagnosticFormat) -> Options {
        Options {
            format,
            fail_on: FailOn::Error,
            summary: false,
            fix: false,
            dry_run: false,
            list_rules: false,
            enabled_rules: Vec::new(),
        }
    }

    #[test]
    fn process_owns_input_decoding_analysis_and_diagnostic_rendering() {
        let mut check = options(DiagnosticFormat::Json);
        check.enabled_rules.push(
            diagnostic::lint_rule("trailing-whitespace")
                .expect("known lint rule")
                .id,
        );
        let outcome = process(
            b"= Title   \n",
            &check,
            "manual.adoc",
            &AnalysisOptions::default(),
            &adocweave::preprocess::PreprocessOptions::default(),
            adocweave_host::FilesystemReadLimits::default(),
            None,
        )
        .expect("check outcome");

        let diagnostics: Vec<serde_json::Value> =
            serde_json::from_str(&outcome.output).expect("JSON diagnostics");
        assert!(
            diagnostics
                .iter()
                .any(|item| item["code"] == "trailing-whitespace")
        );
    }

    #[test]
    fn safe_fixes_apply_only_always_applicable_edits() {
        let fixed = apply_safe_fixes(
            b"= Title   \n",
            &{
                let mut check = options(DiagnosticFormat::Human);
                check.enabled_rules.push(
                    diagnostic::lint_rule("trailing-whitespace")
                        .expect("known lint rule")
                        .id,
                );
                check
            },
            &AnalysisOptions::default(),
        )
        .expect("safe fixes");

        assert_eq!(fixed, b"= Title\n");
    }

    #[test]
    fn process_reports_the_invalid_utf8_offset() {
        let result = process(
            b"valid\xff",
            &options(DiagnosticFormat::Human),
            "<stdin>",
            &AnalysisOptions::default(),
            &adocweave::preprocess::PreprocessOptions::default(),
            adocweave_host::FilesystemReadLimits::default(),
            None,
        );

        assert!(matches!(result, Err(Error::InvalidUtf8 { valid_up_to: 5 })));
    }
}
