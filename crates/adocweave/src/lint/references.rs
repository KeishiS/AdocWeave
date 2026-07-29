use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use crate::diagnostic::{Applicability, RelatedInformation};
use crate::source::TextRange;
use crate::url::AuthoredUrlPolicy;

use super::{
    ASCIIDOC_FILE_LINK, DUPLICATE_ANCHOR, INVALID_ANCHOR, INVALID_CROSS_REFERENCE,
    INVALID_URL_SCHEME, LintContext, LintDiagnosticBody, LintDiagnosticSink, NON_ASCIIDOC_XREF,
    UNRESOLVED_CROSS_REFERENCE,
};

pub(super) fn lint_links_and_references(
    context: &LintContext<'_>,
    sink: &mut LintDiagnosticSink<'_>,
) {
    let document = context.document();
    let authored_url_policy = sink.config().authored_url_policy.clone();
    lint_links_and_references_with_observer(document, &authored_url_policy, sink, |_| {});
}

pub(super) fn lint_links_and_references_with_observer<'document>(
    document: &'document crate::parser::AstDocument,
    authored_url_policy: &AuthoredUrlPolicy,
    sink: &mut LintDiagnosticSink<'_>,
    mut observe: impl FnMut(crate::walker::SemanticNode<'document>),
) {
    let mut targets = BTreeSet::new();
    for target in document.identifiers().targets() {
        if sink.should_stop() {
            return;
        }
        targets.insert(target.id.clone());
    }
    fn inspect(
        inline: &crate::inline::Inline,
        targets: &BTreeSet<String>,
        authored_url_policy: &AuthoredUrlPolicy,
        sink: &mut LintDiagnosticSink<'_>,
    ) -> ControlFlow<()> {
        if sink.should_stop() {
            return ControlFlow::Break(());
        }
        use crate::inline::Inline;
        use crate::reference::ReferenceKey;
        match inline {
            Inline::Link(link) => {
                if !authored_url_policy.allows(&link.target) {
                    sink.emit(INVALID_URL_SCHEME, link.target_range, || {
                        LintDiagnosticBody::new("URL is rejected by the configured policy")
                    });
                }
                if sink.should_stop() {
                    return ControlFlow::Break(());
                }
                if link.target_expansion_error.is_none()
                    && classify_file_target(&link.target)
                        .is_some_and(|target| is_asciidoc_extension(target.extension))
                    && let Some(range) = link.macro_name_range
                {
                    sink.emit(ASCIIDOC_FILE_LINK, range, || {
                        let fix = (link.target_attributes.is_empty()
                            && is_fixable_relative_target(&link.target))
                        .then_some(("replace link with xref", range, "xref"));
                        LintDiagnosticBody::new("use xref for an AsciiDoc document target")
                            .with_optional_fix(fix, Applicability::Always)
                    });
                }
            }
            Inline::Macro(node)
                if matches!(
                    node.kind,
                    crate::inline::StandardMacroKind::Image
                        | crate::inline::StandardMacroKind::Icon
                        | crate::inline::StandardMacroKind::Audio
                        | crate::inline::StandardMacroKind::Video
                ) && !authored_url_policy.allows(&node.target) =>
            {
                sink.emit(INVALID_URL_SCHEME, node.target_range, || {
                    LintDiagnosticBody::new("resource URL is rejected by the configured policy")
                });
            }
            Inline::Reference(reference) => match &reference.target {
                Some(ReferenceKey::Local { anchor }) => {
                    if !targets.contains(anchor.as_str()) {
                        sink.emit(UNRESOLVED_CROSS_REFERENCE, reference.target_range, || {
                            LintDiagnosticBody::new("local cross reference target does not exist")
                        });
                    }
                }
                Some(ReferenceKey::Document { document, .. }) => {
                    if !valid_unresolved_relative_target(document) {
                        sink.emit(INVALID_CROSS_REFERENCE, reference.target_range, || {
                            LintDiagnosticBody::new("unsafe cross-document target")
                        });
                    }
                    if sink.should_stop() {
                        return ControlFlow::Break(());
                    }
                    if reference.target_expansion_error.is_none()
                        && classify_file_target(&reference.expanded_target)
                            .is_some_and(|target| !is_asciidoc_extension(target.extension))
                        && let Some(range) = reference.macro_name_range
                    {
                        sink.emit(NON_ASCIIDOC_XREF, range, || {
                            let fix = (reference.target_attributes.is_empty()
                                && is_fixable_relative_target(&reference.expanded_target))
                            .then_some(("replace xref with link", range, "link"));
                            LintDiagnosticBody::new("use link for a non-AsciiDoc file target")
                                .with_optional_fix(fix, Applicability::Always)
                        });
                    }
                }
                Some(ReferenceKey::Scheme {
                    scheme, locator, ..
                }) => {
                    if scheme.is_empty()
                        || locator.is_empty()
                        || locator.chars().any(char::is_control)
                    {
                        sink.emit(INVALID_CROSS_REFERENCE, reference.target_range, || {
                            LintDiagnosticBody::new("invalid scheme-based cross reference")
                        });
                    }
                }
                None => {
                    sink.emit(INVALID_CROSS_REFERENCE, reference.target_range, || {
                        LintDiagnosticBody::new("invalid cross reference")
                    });
                }
            },
            Inline::Text(_)
            | Inline::Literal { .. }
            | Inline::Styled { .. }
            | Inline::AttributeReference { .. }
            | Inline::HardBreak { .. }
            | Inline::Passthrough { .. }
            | Inline::Macro(_)
            | Inline::Formula(_) => {}
        }
        if sink.should_stop() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }
    let _: ControlFlow<()> = crate::walker::try_walk_ast(document, |node| {
        observe(node);
        if sink.should_stop() {
            return ControlFlow::Break(());
        }
        if let crate::walker::SemanticNode::Inline(inline) = node {
            inspect(inline, &targets, authored_url_policy, sink)
        } else {
            ControlFlow::Continue(())
        }
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileTarget<'a> {
    extension: &'a str,
}

fn classify_file_target(target: &str) -> Option<FileTarget<'_>> {
    let path_end = target.find(['?', '#']).unwrap_or(target.len());
    let path = &target[..path_end];
    if path.starts_with("//")
        || path.contains([
            '\\', '\0', '\r', '\n', '\t', ' ', '[', ']', '<', '>', '"', '\'',
        ])
        || has_scheme(path)
    {
        return None;
    }
    let name = path.rsplit('/').next()?;
    let (stem, extension) = name.rsplit_once('.')?;
    if stem.is_empty() || extension.is_empty() {
        return None;
    }
    Some(FileTarget { extension })
}

fn is_asciidoc_extension(extension: &str) -> bool {
    extension.eq_ignore_ascii_case("adoc") || extension.eq_ignore_ascii_case("asciidoc")
}

fn is_fixable_relative_target(target: &str) -> bool {
    classify_file_target(target).is_some() && valid_unresolved_relative_target(target)
}

fn has_scheme(target: &str) -> bool {
    target.find(':').is_some()
}

/// Checks only syntax that is safe to retain for later host resolution.
///
/// Parent segments are valid here because linting performs no filesystem
/// access. Renderers and resource providers apply their own stricter policy
/// before turning the target into an active URL or local path.
fn valid_unresolved_relative_target(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains(['\\', ':'])
        && !value.contains("//")
        && !value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '<' | '>' | '"' | '\'' | '`' | '{' | '}')
        })
        && valid_relative_percent_escapes(value)
}

fn valid_relative_percent_escapes(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return false;
        }
        let (Some(high), Some(low)) = (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
        else {
            return false;
        };
        let decoded = high * 16 + low;
        if decoded <= 0x20 || decoded == 0x7f || matches!(decoded, b'.' | b'/' | b'\\') {
            return false;
        }
        index += 3;
    }
    true
}

const fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

pub(super) fn lint_anchors(context: &LintContext<'_>, sink: &mut LintDiagnosticSink<'_>) {
    let document = context.document();
    let mut ids = BTreeMap::<String, TextRange>::new();
    for anchor in document.anchors() {
        if sink.should_stop() {
            break;
        }
        if !anchor.valid {
            sink.emit(INVALID_ANCHOR, anchor.range, || {
                LintDiagnosticBody::new("invalid or unattached explicit anchor")
            });
        }
    }
    if sink.should_stop() {
        return;
    }
    for target in document.identifiers().targets() {
        if sink.should_stop() {
            break;
        }
        if let Some(first) = ids.insert(target.id.clone(), target.id_range) {
            sink.emit(DUPLICATE_ANCHOR, target.id_range, || {
                LintDiagnosticBody::new(format!("duplicate anchor ID `{}`", target.id))
                    .with_related(vec![RelatedInformation {
                        message: "first target with this ID".to_owned(),
                        range: first,
                    }])
            });
        }
    }
}
