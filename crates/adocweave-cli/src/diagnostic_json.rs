//! The single JSON shape `check --format json` emits.
//!
//! Every path produced its own object before: core diagnostics carried `related`
//! and `fixes` but no `sourceId`, projected include diagnostics carried
//! `sourceId` but neither list, and local target diagnostics carried a third
//! set. Consumers that read one shape broke when a document gained an include.
//! Building every record here keeps the emitted keys identical.

use adocweave::SourceId;
use adocweave::output::diagnostics::{Diagnostic, RelatedInformation, TextEdit};
use adocweave::preprocess::{ProjectedDiagnostic, SourceOrigin};
use adocweave::text::TextRange;
use serde_json::{Map, Value, json};

/// Serializes a diagnostic of a document that was analyzed directly.
pub(crate) fn record(diagnostic: &Diagnostic, source_id: Option<&str>) -> Value {
    let related = diagnostic.related.iter().map(related_value).collect();
    let fixes = diagnostic
        .fixes
        .iter()
        .map(|fix| {
            fix_value(
                &fix.title,
                fix.applicability.as_str(),
                fix.edits().iter().map(edit_value).collect(),
            )
        })
        .collect();
    build(diagnostic, source_id, diagnostic.range, related, fixes)
}

/// Serializes one origin of a diagnostic that came through include expansion.
///
/// The related information and the fixes are taken from the projection so that
/// every range refers to the including document, not to the expanded text.
/// `source_id` is `None` only when the projected origin has no identifier.
pub(crate) fn projected_record(projected: &ProjectedDiagnostic, origin: &SourceOrigin) -> Value {
    let related = projected
        .related
        .iter()
        .flat_map(|item| {
            item.origins.iter().map(|origin| {
                json!({
                    "range": range_value(origin.range.text_range()),
                    "message": item.value.message,
                })
            })
        })
        .collect();
    let fixes = projected
        .fixes
        .iter()
        .map(|fix| {
            let edits = fix
                .edits
                .iter()
                .flat_map(|edit| {
                    edit.origins.iter().map(|origin| {
                        json!({
                            "range": range_value(origin.range.text_range()),
                            "replacement": edit.value.replacement,
                        })
                    })
                })
                .collect();
            fix_value(&fix.title, fix.applicability.as_str(), edits)
        })
        .collect();
    build(
        &projected.diagnostic,
        origin.source_id.as_ref().map(SourceId::as_str),
        origin.range.text_range(),
        related,
        fixes,
    )
}

fn build(
    diagnostic: &Diagnostic,
    source_id: Option<&str>,
    range: TextRange,
    related: Vec<Value>,
    fixes: Vec<Value>,
) -> Value {
    let mut object = Map::new();
    object.insert("id".to_owned(), json!(diagnostic.id.as_str()));
    object.insert("code".to_owned(), json!(diagnostic.code.as_str()));
    object.insert("severity".to_owned(), json!(diagnostic.severity.as_str()));
    object.insert("sourceId".to_owned(), json!(source_id));
    object.insert("range".to_owned(), range_value(range));
    object.insert("message".to_owned(), json!(diagnostic.message));
    object.insert("related".to_owned(), Value::Array(related));
    object.insert("fixes".to_owned(), Value::Array(fixes));
    Value::Object(object)
}

fn fix_value(title: &str, applicability: &str, edits: Vec<Value>) -> Value {
    json!({ "title": title, "applicability": applicability, "edits": edits })
}

/// Adds the keys every record carries to a diagnostic built outside the core.
///
/// Local target diagnostics keep their own `target`, `line` and `column` keys.
/// Those are additional information, not a different shape.
pub(crate) fn with_common_keys(mut object: Map<String, Value>) -> Value {
    object.entry("related").or_insert_with(|| json!([]));
    object.entry("fixes").or_insert_with(|| json!([]));
    Value::Object(object)
}

pub(crate) fn range_value(range: TextRange) -> Value {
    json!({ "start": range.start().to_u32(), "end": range.end().to_u32() })
}

fn related_value(related: &RelatedInformation) -> Value {
    json!({ "range": range_value(related.range), "message": related.message })
}

fn edit_value(edit: &TextEdit) -> Value {
    json!({ "range": range_value(edit.range), "replacement": edit.replacement })
}
