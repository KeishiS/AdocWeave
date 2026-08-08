use std::error::Error;
use std::fmt;

use crate::cancellation::CancellationCheckpoint;
use crate::core::{CancellationCheck, NeverCancel};
use crate::diagnostic::{Diagnostic, RelatedInformation, TextEdit};
use crate::document::DocumentSymbol;
use crate::inline_model::Reference;
use crate::resource::ResourceReference;
use crate::source::TextRange;

use super::{
    Directive, ExpandedRange, OriginRange, PreprocessedAnalysis, PreprocessedDocument, SourceOrigin,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Originated<T> {
    pub origins: Vec<SourceOrigin>,
    pub value: T,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedFix {
    pub title: String,
    pub applicability: crate::diagnostic::Applicability,
    pub applicable: bool,
    pub edits: Vec<Originated<TextEdit>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedDiagnostic {
    pub diagnostic: Diagnostic,
    pub origins: Vec<SourceOrigin>,
    pub related: Vec<Originated<RelatedInformation>>,
    pub fixes: Vec<ProjectedFix>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedDocumentSymbol {
    pub symbol: DocumentSymbol,
    pub origins: Vec<SourceOrigin>,
    pub selection_origins: Vec<SourceOrigin>,
    pub children: Vec<ProjectedDocumentSymbol>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedReference {
    pub value: Reference,
    pub origins: Vec<SourceOrigin>,
    pub target_origins: Vec<SourceOrigin>,
    pub editable_anchor_origin: Option<SourceOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedLocalTarget {
    pub value: crate::local_target::LocalTargetReference,
    pub origins: Vec<SourceOrigin>,
    pub target_origins: Vec<SourceOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedResource {
    pub value: ResourceReference,
    pub origins: Vec<SourceOrigin>,
    pub target_origins: Vec<SourceOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedDocumentAttribute {
    pub value: crate::attributes::DocumentAttributeOccurrence,
    pub origins: Vec<SourceOrigin>,
    pub name_origins: Vec<SourceOrigin>,
    pub value_origins: Vec<SourceOrigin>,
    pub value_lines: Vec<ProjectedDocumentAttributeValueLine>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedDocumentAttributeValueLine {
    pub value: crate::attributes::DocumentAttributeValueLine,
    pub origins: Vec<SourceOrigin>,
    pub indent_origins: Vec<SourceOrigin>,
    pub content_origins: Vec<SourceOrigin>,
    pub ending_origins: Vec<SourceOrigin>,
    pub continuation_origins: Vec<SourceOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedAttributeBinding {
    pub value: crate::attributes::AttributeBinding,
    pub origins: Vec<SourceOrigin>,
    pub name_origins: Vec<SourceOrigin>,
    pub value_origins: Vec<SourceOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedAttributeReference {
    pub value: crate::attributes::AttributeReference,
    pub origins: Vec<SourceOrigin>,
    pub name_origins: Vec<SourceOrigin>,
}

/// All editor-facing facts from an expanded analysis, projected to original sources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalysisProjection {
    pub attribute_bindings: Vec<ProjectedAttributeBinding>,
    pub attribute_occurrences: Vec<ProjectedDocumentAttribute>,
    pub attribute_references: Vec<ProjectedAttributeReference>,
    pub directives: Vec<Directive>,
    pub diagnostics: Vec<ProjectedDiagnostic>,
    pub local_targets: Vec<ProjectedLocalTarget>,
    pub references: Vec<ProjectedReference>,
    pub resources: Vec<ProjectedResource>,
    pub symbols: Vec<ProjectedDocumentSymbol>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionLimits {
    pub max_origin_segments: u32,
}

impl Default for ProjectionLimits {
    fn default() -> Self {
        Self {
            max_origin_segments: 1_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionError {
    pub limit: u32,
    pub actual: u64,
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "projection origin segment limit exceeded (limit {}, actual {})",
            self.limit, self.actual
        )
    }
}

impl Error for ProjectionError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionFailure {
    LimitExceeded(ProjectionError),
    Cancelled,
}

impl fmt::Display for ProjectionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitExceeded(error) => error.fmt(formatter),
            Self::Cancelled => formatter.write_str("source origin projection was cancelled"),
        }
    }
}

impl Error for ProjectionFailure {}

impl From<ProjectionError> for ProjectionFailure {
    fn from(error: ProjectionError) -> Self {
        Self::LimitExceeded(error)
    }
}

impl PreprocessedAnalysis {
    pub fn project_origins(
        &self,
        limits: ProjectionLimits,
    ) -> Result<AnalysisProjection, ProjectionError> {
        match self.project_origins_cancellable(limits, &NeverCancel) {
            Ok(projection) => Ok(projection),
            Err(ProjectionFailure::LimitExceeded(error)) => Err(error),
            Err(ProjectionFailure::Cancelled) => {
                unreachable!("NeverCancel cannot cancel source origin projection")
            }
        }
    }

    /// Projects source origins with cooperative cancellation.
    pub fn project_origins_cancellable(
        &self,
        limits: ProjectionLimits,
        cancellation: &dyn CancellationCheck,
    ) -> Result<AnalysisProjection, ProjectionFailure> {
        project_origins(self, limits, cancellation)
    }
}

fn project_origins(
    input: &PreprocessedAnalysis,
    limits: ProjectionLimits,
    cancellation: &dyn CancellationCheck,
) -> Result<AnalysisProjection, ProjectionFailure> {
    let map = &input.document;
    let mut projected_segments = 0_u64;
    let mut checkpoint = CancellationCheckpoint::new(cancellation);
    if checkpoint.is_cancelled() {
        return Err(ProjectionFailure::Cancelled);
    }
    let attribute_occurrences = input
        .analysis
        .document_attribute_occurrences()
        .iter()
        .cloned()
        .map(|value| {
            let origins = project_attribute_range(
                map,
                value.range,
                value.range,
                &mut projected_segments,
                limits,
                &mut checkpoint,
            )?;
            let name_origins = project_attribute_range(
                map,
                value.name_range,
                value.range,
                &mut projected_segments,
                limits,
                &mut checkpoint,
            )?;
            let value_origins = project_attribute_range(
                map,
                value.value.source_range,
                value.range,
                &mut projected_segments,
                limits,
                &mut checkpoint,
            )?;
            let value_lines = value
                .value
                .lines
                .iter()
                .cloned()
                .map(|line| {
                    let origins = project_attribute_range(
                        map,
                        line.range,
                        value.range,
                        &mut projected_segments,
                        limits,
                        &mut checkpoint,
                    )?;
                    let indent_origins = project_attribute_range(
                        map,
                        line.indent_range,
                        value.range,
                        &mut projected_segments,
                        limits,
                        &mut checkpoint,
                    )?;
                    let content_origins = project_attribute_range(
                        map,
                        line.content_range,
                        value.range,
                        &mut projected_segments,
                        limits,
                        &mut checkpoint,
                    )?;
                    let ending_origins = project_attribute_range(
                        map,
                        line.ending_range,
                        value.range,
                        &mut projected_segments,
                        limits,
                        &mut checkpoint,
                    )?;
                    let continuation_origins = line
                        .continuation
                        .map(|continuation| {
                            project_attribute_range(
                                map,
                                continuation.range,
                                value.range,
                                &mut projected_segments,
                                limits,
                                &mut checkpoint,
                            )
                        })
                        .transpose()?
                        .unwrap_or_default();
                    Ok(ProjectedDocumentAttributeValueLine {
                        value: line,
                        origins,
                        indent_origins,
                        content_origins,
                        ending_origins,
                        continuation_origins,
                    })
                })
                .collect::<Result<Vec<_>, ProjectionFailure>>()?;
            Ok(ProjectedDocumentAttribute {
                value,
                origins,
                name_origins,
                value_origins,
                value_lines,
            })
        })
        .collect::<Result<Vec<_>, ProjectionFailure>>()?;
    let attribute_bindings = input
        .analysis
        .attribute_environment()
        .bindings()
        .iter()
        .cloned()
        .map(|value| {
            let occurrence = value.occurrence();
            Ok(ProjectedAttributeBinding {
                origins: project_attribute_range(
                    map,
                    occurrence.range,
                    occurrence.range,
                    &mut projected_segments,
                    limits,
                    &mut checkpoint,
                )?,
                name_origins: project_attribute_range(
                    map,
                    occurrence.name_range,
                    occurrence.range,
                    &mut projected_segments,
                    limits,
                    &mut checkpoint,
                )?,
                value_origins: project_attribute_range(
                    map,
                    occurrence.value.source_range,
                    occurrence.range,
                    &mut projected_segments,
                    limits,
                    &mut checkpoint,
                )?,
                value,
            })
        })
        .collect::<Result<Vec<_>, ProjectionFailure>>()?;
    let attribute_references = input
        .analysis
        .attribute_references()
        .iter()
        .cloned()
        .map(|value| {
            let origins = project_attribute_range(
                map,
                value.range,
                value.range,
                &mut projected_segments,
                limits,
                &mut checkpoint,
            )?;
            let name_origins = project_attribute_range(
                map,
                value.name_range,
                value.range,
                &mut projected_segments,
                limits,
                &mut checkpoint,
            )?;
            Ok(ProjectedAttributeReference {
                value,
                origins,
                name_origins,
            })
        })
        .collect::<Result<Vec<_>, ProjectionFailure>>()?;
    let mut project = |range| {
        if checkpoint.is_cancelled() {
            return Err(ProjectionFailure::Cancelled);
        }
        let origins = map
            .origins_for_range_cancellable(ExpandedRange::new(range), &mut checkpoint)
            .map_err(|()| ProjectionFailure::Cancelled)?;
        projected_segments = projected_segments.saturating_add(origins.len() as u64);
        if projected_segments > u64::from(limits.max_origin_segments) {
            Err(ProjectionFailure::LimitExceeded(ProjectionError {
                limit: limits.max_origin_segments,
                actual: projected_segments,
            }))
        } else {
            Ok(origins)
        }
    };
    let diagnostics = input
        .analysis
        .diagnostics()
        .iter()
        .cloned()
        .map(|diagnostic| {
            let origins = project(diagnostic.range)?;
            let related = diagnostic
                .related
                .iter()
                .cloned()
                .map(|value| {
                    Ok(Originated {
                        origins: project(value.range)?,
                        value,
                    })
                })
                .collect::<Result<Vec<_>, ProjectionFailure>>()?;
            let fixes = diagnostic
                .fixes
                .iter()
                .cloned()
                .map(|fix| -> Result<_, ProjectionFailure> {
                    let edits: Vec<_> = fix
                        .edits()
                        .iter()
                        .cloned()
                        .map(|value| {
                            Ok(Originated {
                                origins: project(value.range)?,
                                value,
                            })
                        })
                        .collect::<Result<_, ProjectionFailure>>()?;
                    let applicable = edits.iter().all(|edit| edit.origins.len() == 1)
                        && edits.iter().all(|edit| {
                            map.mapping_is_identity(ExpandedRange::new(edit.value.range))
                        });
                    Ok(ProjectedFix {
                        title: fix.title,
                        applicability: fix.applicability,
                        applicable,
                        edits,
                    })
                })
                .collect::<Result<Vec<_>, ProjectionFailure>>()?;
            Ok(ProjectedDiagnostic {
                diagnostic,
                origins,
                related,
                fixes,
            })
        })
        .collect::<Result<Vec<_>, ProjectionFailure>>()?;
    let mut local_targets = Vec::new();
    for link in input.analysis.links() {
        let Some(value) = crate::local_target::LocalTargetReference::from_link(link) else {
            continue;
        };
        local_targets.push(ProjectedLocalTarget {
            origins: project(value.range)?,
            target_origins: project(value.target_range)?,
            value,
        });
    }
    let mut references = Vec::new();
    for value in input.analysis.references() {
        let origins = project(value.range)?;
        let target_origins = project(value.target_range)?;
        let editable_anchor_origin = if let Some(range) = value.authored_anchor_range()
            && map.mapping_is_identity(ExpandedRange::new(range))
        {
            let mut origins = project(range)?;
            (origins.len() == 1).then(|| origins.remove(0))
        } else {
            None
        };
        if let Some(local) = crate::local_target::LocalTargetReference::from_reference(value) {
            let local_target_origins = project(local.target_range)?;
            local_targets.push(ProjectedLocalTarget {
                value: local,
                origins: origins.clone(),
                target_origins: local_target_origins,
            });
        }
        references.push(ProjectedReference {
            origins,
            target_origins,
            editable_anchor_origin,
            value: value.clone(),
        });
    }
    let mut resources = Vec::new();
    for value in input.analysis.resources() {
        let origins = project(value.range())?;
        let target_origins = project(value.target_range())?;
        if let Some(local) = crate::local_target::LocalTargetReference::from_resource(value) {
            local_targets.push(ProjectedLocalTarget {
                value: local,
                origins: origins.clone(),
                target_origins: target_origins.clone(),
            });
        }
        resources.push(ProjectedResource {
            origins,
            target_origins,
            value: value.clone(),
        });
    }
    let mut directives = Vec::with_capacity(input.document.directives.len());
    for directive in &input.document.directives {
        if cancellation.is_cancelled() {
            return Err(ProjectionFailure::Cancelled);
        }
        directives.push(directive.clone());
        let Some(value) = directive.local_target() else {
            continue;
        };
        let origin = SourceOrigin {
            source_id: directive.source_id.clone(),
            range: OriginRange::new(directive.range),
        };
        let target_origin = SourceOrigin {
            source_id: directive.source_id.clone(),
            range: OriginRange::new(directive.target_range),
        };
        local_targets.push(ProjectedLocalTarget {
            value,
            origins: vec![origin],
            target_origins: vec![target_origin],
        });
    }
    let mut symbol_checkpoint = CancellationCheckpoint::new(cancellation);
    let symbols = crate::document::document_symbols_cancellable(
        input.analysis.document(),
        &mut symbol_checkpoint,
    )
    .map_err(|()| ProjectionFailure::Cancelled)?
    .into_iter()
    .map(|symbol| project_symbol(symbol, &mut project))
    .collect::<Result<Vec<_>, ProjectionFailure>>()?;
    if cancellation.is_cancelled() {
        return Err(ProjectionFailure::Cancelled);
    }
    Ok(AnalysisProjection {
        attribute_bindings,
        attribute_occurrences,
        attribute_references,
        directives,
        diagnostics,
        local_targets,
        references,
        resources,
        symbols,
    })
}

fn project_attribute_range(
    map: &PreprocessedDocument,
    range: TextRange,
    occurrence_range: TextRange,
    projected_segments: &mut u64,
    limits: ProjectionLimits,
    checkpoint: &mut CancellationCheckpoint<'_>,
) -> Result<Vec<SourceOrigin>, ProjectionFailure> {
    if checkpoint.is_cancelled() {
        return Err(ProjectionFailure::Cancelled);
    }
    let origins = if range.is_empty() {
        map.origins_for_empty_range_within_cancellable(
            ExpandedRange::new(range),
            ExpandedRange::new(occurrence_range),
            checkpoint,
        )
        .map_err(|()| ProjectionFailure::Cancelled)?
    } else {
        map.origins_for_range_cancellable(ExpandedRange::new(range), checkpoint)
            .map_err(|()| ProjectionFailure::Cancelled)?
    };
    *projected_segments = projected_segments.saturating_add(origins.len() as u64);
    if *projected_segments > u64::from(limits.max_origin_segments) {
        Err(ProjectionFailure::LimitExceeded(ProjectionError {
            limit: limits.max_origin_segments,
            actual: *projected_segments,
        }))
    } else {
        Ok(origins)
    }
}

fn project_symbol(
    mut symbol: DocumentSymbol,
    project: &mut impl FnMut(TextRange) -> Result<Vec<SourceOrigin>, ProjectionFailure>,
) -> Result<ProjectedDocumentSymbol, ProjectionFailure> {
    let children = std::mem::take(&mut symbol.children)
        .into_iter()
        .map(|child| project_symbol(child, project))
        .collect::<Result<_, _>>()?;
    Ok(ProjectedDocumentSymbol {
        origins: project(symbol.range)?,
        selection_origins: project(symbol.selection_range)?,
        symbol,
        children,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::core::{AnalysisOptions, CancellationCheck, Engine, SourceId};
    use crate::preprocessor::{
        PreprocessOptions, ResourceDocument, ResourceSnapshot, preprocess_and_analyze,
    };
    use std::sync::Arc;

    #[test]
    fn origin_projection_cancels_at_a_bounded_range_checkpoint() {
        struct CancelAfterFirstCheckpoint(AtomicUsize);

        impl CancellationCheck for CancelAfterFirstCheckpoint {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) >= 1
            }
        }

        let source = "xref:target[Target]\n\n".repeat(crate::cancellation::CHECKPOINT_INTERVAL * 2);
        let input = preprocess_and_analyze(
            &Engine::new(AnalysisOptions::default()),
            &source,
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
        )
        .expect("analysis");
        let cancellation = CancelAfterFirstCheckpoint(AtomicUsize::new(0));

        let failure = input
            .project_origins_cancellable(ProjectionLimits::default(), &cancellation)
            .expect_err("projection should be cancelled");

        assert_eq!(failure, ProjectionFailure::Cancelled);
        assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn never_cancel_projection_preserves_success_and_limit_errors() {
        let input = preprocess_and_analyze(
            &Engine::new(AnalysisOptions::default()),
            "== Section\n\nxref:missing[Missing]\n",
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
        )
        .expect("analysis");

        assert_eq!(
            input
                .project_origins_cancellable(ProjectionLimits::default(), &NeverCancel)
                .expect("cancellable projection"),
            input
                .project_origins(ProjectionLimits::default())
                .expect("compatibility projection")
        );

        let limits = ProjectionLimits {
            max_origin_segments: 0,
        };
        let expected = input
            .project_origins(limits)
            .expect_err("compatibility projection limit");
        assert_eq!(
            input
                .project_origins_cancellable(limits, &NeverCancel)
                .expect_err("cancellable projection limit"),
            ProjectionFailure::LimitExceeded(expected)
        );
    }

    #[test]
    fn feature_products_are_projected_from_one_fixed_document() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "part.adoc".to_owned(),
            ResourceDocument {
                source_id: SourceId::new("part"),
                source: Arc::from(
                    ":name: World\n\n== Included\n\nHello {name} \n\nimage::picture.png[]\n\nxref:target[Target]\n",
                ),
            },
        );
        let input = preprocess_and_analyze(
            &Engine::new(crate::core::AnalysisOptions::default()),
            "include::part.adoc[]\n",
            &snapshot,
            &PreprocessOptions {
                source_id: Some(SourceId::new("root")),
                ..PreprocessOptions::default()
            },
        )
        .expect("analysis");

        let projection = input
            .project_origins(ProjectionLimits::default())
            .expect("projection");
        assert!(!projection.attribute_occurrences.is_empty());
        assert!(!projection.attribute_bindings.is_empty());
        assert!(!projection.attribute_references.is_empty());
        assert!(!projection.directives.is_empty());
        assert!(!projection.diagnostics.is_empty());
        assert!(!projection.local_targets.is_empty());
        assert!(!projection.references.is_empty());
        assert!(!projection.resources.is_empty());
        assert!(!projection.symbols.is_empty());
        assert!(
            projection
                .symbols
                .iter()
                .all(|symbol| !symbol.origins.is_empty())
        );
        assert!(
            projection
                .references
                .iter()
                .all(|reference| !reference.origins.is_empty())
        );
    }
}
