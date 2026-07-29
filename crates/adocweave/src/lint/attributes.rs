use std::collections::BTreeSet;

use crate::attributes::{AttributeBindingId, DocumentAttributeOperation};
use crate::source::TextRange;

use super::{
    ATTRIBUTE_EXPANSION, LintContext, LintDiagnosticBody, LintDiagnosticSink, PROTECTED_ATTRIBUTE,
    UNDEFINED_ATTRIBUTE, UNUSED_ATTRIBUTE,
};

pub(super) fn lint_attributes(context: &LintContext<'_>, sink: &mut LintDiagnosticSink<'_>) {
    let document = context.document();
    let protected_attributes = sink.config().protected_attributes.clone();
    for attribute in document.attributes() {
        if sink.is_full() {
            break;
        }
        if let Some(expected) = protected_attributes.get(&attribute.name) {
            let changed = match &attribute.operation {
                DocumentAttributeOperation::Set => expected
                    .as_ref()
                    .is_none_or(|expected| &attribute.value.folded_text != expected),
                DocumentAttributeOperation::Unset => expected.is_some(),
            };
            if changed {
                sink.emit(PROTECTED_ATTRIBUTE, attribute.range, || {
                    LintDiagnosticBody::new(format!(
                        "protected attribute `{}` cannot be changed",
                        attribute.name
                    ))
                });
            }
        }
    }

    if sink.is_full() {
        return;
    }
    let environment = document.attribute_environment();
    let references = document.resolved.facts().attribute_references();
    let inline_references = references
        .iter()
        .filter(|reference| {
            !environment.bindings().iter().any(|binding| {
                contains_range(
                    binding.occurrence().value.source_range,
                    reference.name_range,
                )
            })
        })
        .cloned()
        .collect();
    let mut used_bindings = BTreeSet::<AttributeBindingId>::new();
    lint_attribute_reference_uses(inline_references, &mut used_bindings, sink);
    if sink.is_full() {
        return;
    }
    for binding in environment.bindings() {
        if sink.is_full() {
            break;
        }
        let references = references
            .iter()
            .filter(|reference| {
                contains_range(
                    binding.occurrence().value.source_range,
                    reference.name_range,
                )
            })
            .collect::<Vec<_>>();
        for reference in &references {
            used_bindings.extend(reference.binding_id);
        }
        if let Err(error) = binding.value() {
            let range = references.first().map_or_else(
                || binding.occurrence().value.source_range,
                |reference| reference.name_range,
            );
            let undefined_reference =
                if error == crate::substitution::AttributeExpansionError::Undefined {
                    references.first().copied()
                } else {
                    None
                };
            let rule = if undefined_reference.is_some() {
                UNDEFINED_ATTRIBUTE
            } else {
                ATTRIBUTE_EXPANSION
            };
            sink.emit(rule, range, || {
                if let Some(reference) = undefined_reference {
                    LintDiagnosticBody::new(format!(
                        "undefined document attribute `{}`",
                        reference.name
                    ))
                } else {
                    LintDiagnosticBody::new(attribute_expansion_message(error))
                }
            });
        }
    }
    if sink.is_full() {
        return;
    }
    for binding in environment.bindings() {
        if sink.is_full() {
            break;
        }
        let occurrence = binding.occurrence();
        if binding.operation() == DocumentAttributeOperation::Set
            && !used_bindings.contains(&binding.id())
            && !protected_attributes.contains_key(&occurrence.name)
        {
            sink.emit(UNUSED_ATTRIBUTE, occurrence.name_range, || {
                LintDiagnosticBody::new(format!("unused document attribute `{}`", occurrence.name))
            });
        }
    }
}

fn lint_attribute_reference_uses(
    references: Vec<crate::attributes::AttributeReference>,
    used_bindings: &mut BTreeSet<crate::attributes::AttributeBindingId>,
    sink: &mut LintDiagnosticSink<'_>,
) {
    for reference in references {
        if sink.is_full() {
            break;
        }
        used_bindings.extend(reference.binding_id);
        match reference.value {
            Ok(Some(_)) => {}
            Ok(None) | Err(crate::substitution::AttributeExpansionError::Undefined) => {
                sink.emit(UNDEFINED_ATTRIBUTE, reference.name_range, || {
                    LintDiagnosticBody::new(format!(
                        "undefined document attribute `{}`",
                        reference.name
                    ))
                });
            }
            Err(error) => {
                sink.emit(ATTRIBUTE_EXPANSION, reference.range, || {
                    LintDiagnosticBody::new(attribute_expansion_message(error))
                });
            }
        }
    }
}

fn contains_range(outer: TextRange, inner: TextRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}

fn attribute_expansion_message(
    error: crate::substitution::AttributeExpansionError,
) -> &'static str {
    match error {
        crate::substitution::AttributeExpansionError::Undefined => {
            "document attribute expansion references an undefined attribute"
        }
        crate::substitution::AttributeExpansionError::Cycle => {
            "document attribute expansion contains a cycle"
        }
        crate::substitution::AttributeExpansionError::DepthLimitExceeded => {
            "document attribute expansion exceeds the depth limit"
        }
        crate::substitution::AttributeExpansionError::SizeLimitExceeded => {
            "document attribute expansion exceeds the size limit"
        }
    }
}
