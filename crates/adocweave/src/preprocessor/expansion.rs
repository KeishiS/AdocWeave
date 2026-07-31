use std::collections::BTreeMap;

use crate::attributes::{
    DocumentAttributeOccurrence, ExternalAttributes, SequentialAttributeState,
};
use crate::core::SourceId;
use crate::substitution::AttributeExpansionLimits;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExpansionLimit {
    Includes,
    Nodes,
}

pub(super) struct ExpansionState {
    includes: u64,
    nodes: u64,
    attributes: SequentialAttributeState,
    attribute_delimiters: Vec<String>,
    attribute_position: bool,
}

impl ExpansionState {
    pub(super) fn new(
        attributes: &ExternalAttributes,
        attribute_limits: AttributeExpansionLimits,
    ) -> Self {
        Self {
            includes: 0,
            nodes: 0,
            attributes: SequentialAttributeState::with_locked_values(attributes, attribute_limits),
            attribute_delimiters: Vec::new(),
            attribute_position: true,
        }
    }

    pub(super) fn register_include(&mut self, maximum: u32) -> Result<(), ExpansionLimit> {
        self.includes += 1;
        if self.includes > u64::from(maximum) {
            return Err(ExpansionLimit::Includes);
        }
        Ok(())
    }

    pub(super) fn register_node(&mut self, maximum: u32) -> Result<(), ExpansionLimit> {
        self.nodes += 1;
        if self.nodes > u64::from(maximum) {
            return Err(ExpansionLimit::Nodes);
        }
        Ok(())
    }

    pub(super) fn attributes(&self) -> &BTreeMap<String, String> {
        self.attributes.values()
    }

    pub(super) const fn attribute_limits(&self) -> AttributeExpansionLimits {
        self.attributes.limits()
    }

    pub(super) fn observe_delimiter(&mut self, content: &str) -> bool {
        if crate::delimiter::spec(content).is_none() {
            return false;
        }
        if self
            .attribute_delimiters
            .last()
            .is_some_and(|open| open == content)
        {
            self.attribute_delimiters.pop();
        } else {
            self.attribute_delimiters.push(content.to_owned());
        }
        true
    }

    pub(super) fn accepts_attribute(&self, delimiter: bool) -> bool {
        !delimiter && self.attribute_delimiters.is_empty() && self.attribute_position
    }

    pub(super) fn apply_attribute(&mut self, occurrence: &DocumentAttributeOccurrence) {
        let _ = self.attributes.apply(occurrence);
    }

    pub(super) fn finish_line(&mut self, document_attribute: bool, content: &str) {
        self.attribute_position = document_attribute
            || content.trim_matches([' ', '\t']).is_empty()
            || content.starts_with("//");
    }

    pub(super) fn finish_directive_output(&mut self) {
        self.attribute_position = false;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct IncludeFrame {
    source_id: Option<SourceId>,
    depth: u32,
    base_uri: Option<String>,
    active_targets: Vec<String>,
}

impl IncludeFrame {
    pub(super) fn root(source_id: Option<SourceId>, base_uri: Option<&str>) -> Self {
        Self {
            source_id,
            depth: 0,
            base_uri: base_uri.map(str::to_owned),
            active_targets: Vec::new(),
        }
    }

    pub(super) fn child(
        &self,
        target: String,
        source_id: SourceId,
        base_uri: Option<String>,
    ) -> Self {
        let mut active_targets = self.active_targets.clone();
        active_targets.push(target);
        Self {
            source_id: Some(source_id),
            // The caller rejects a frame that already reached the configured
            // depth, and the include count runs out long before this could
            // wrap. Saturating keeps that reasoning from becoming a silent
            // wrap if either bound is later raised or removed.
            depth: self.depth.saturating_add(1),
            base_uri,
            active_targets,
        }
    }

    pub(super) fn source_id(&self) -> Option<SourceId> {
        self.source_id.clone()
    }

    pub(super) const fn depth(&self) -> u32 {
        self.depth
    }

    pub(super) fn base_uri(&self) -> Option<&str> {
        self.base_uri.as_deref()
    }

    pub(super) fn contains_target(&self, target: &str) -> bool {
        self.active_targets.iter().any(|active| active == target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attribute_limits() -> AttributeExpansionLimits {
        AttributeExpansionLimits {
            max_depth: 16,
            max_bytes: 1024,
        }
    }

    #[test]
    fn counters_reject_the_first_item_beyond_each_limit() {
        let mut state = ExpansionState::new(&ExternalAttributes::new(), attribute_limits());
        assert_eq!(state.register_include(1), Ok(()));
        assert_eq!(state.register_include(1), Err(ExpansionLimit::Includes));
        assert_eq!(state.register_node(2), Ok(()));
        assert_eq!(state.register_node(2), Ok(()));
        assert_eq!(state.register_node(2), Err(ExpansionLimit::Nodes));
    }

    #[test]
    fn include_frames_own_depth_base_and_cycle_ancestry() {
        let root = IncludeFrame::root(Some(SourceId::new("root")), Some("file:///guide"));
        assert_eq!(root.depth(), 0);
        assert_eq!(root.base_uri(), Some("file:///guide"));
        assert!(!root.contains_target("chapter.adoc"));

        let chapter = root.child(
            "chapter.adoc".to_owned(),
            SourceId::new("chapter"),
            Some("file:///guide/chapter".to_owned()),
        );
        assert_eq!(chapter.depth(), 1);
        assert_eq!(chapter.source_id(), Some(SourceId::new("chapter")));
        assert_eq!(chapter.base_uri(), Some("file:///guide/chapter"));
        assert!(chapter.contains_target("chapter.adoc"));
        assert!(!root.contains_target("chapter.adoc"));
    }

    #[test]
    fn delimiter_and_content_transitions_control_attribute_positions() {
        let mut state = ExpansionState::new(&ExternalAttributes::new(), attribute_limits());
        assert!(state.accepts_attribute(false));
        assert!(state.observe_delimiter("----"));
        assert!(!state.accepts_attribute(true));
        assert!(!state.accepts_attribute(false));
        assert!(state.observe_delimiter("----"));
        assert!(state.accepts_attribute(false));
        state.finish_directive_output();
        assert!(!state.accepts_attribute(false));
        state.finish_line(false, "");
        assert!(state.accepts_attribute(false));
    }
}
