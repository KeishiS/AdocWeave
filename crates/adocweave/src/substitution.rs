//! Ordered, output-independent AsciiDoc substitution contexts and attribute evaluation.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubstitutionStep {
    SpecialCharacters,
    Quotes,
    Attributes,
    Replacements,
    Macros,
    PostReplacements,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubstitutionContext {
    Normal,
    Header,
    Verbatim,
    Pass,
    None,
}

const NORMAL: &[SubstitutionStep] = &[
    SubstitutionStep::SpecialCharacters,
    SubstitutionStep::Quotes,
    SubstitutionStep::Attributes,
    SubstitutionStep::Replacements,
    SubstitutionStep::Macros,
    SubstitutionStep::PostReplacements,
];
const HEADER: &[SubstitutionStep] = &[
    SubstitutionStep::SpecialCharacters,
    SubstitutionStep::Attributes,
];
const VERBATIM: &[SubstitutionStep] = &[SubstitutionStep::SpecialCharacters];
const PASS: &[SubstitutionStep] = &[];

impl SubstitutionContext {
    pub const fn steps(self) -> &'static [SubstitutionStep] {
        match self {
            Self::Normal => NORMAL,
            Self::Header => HEADER,
            Self::Verbatim => VERBATIM,
            Self::Pass | Self::None => PASS,
        }
    }

    pub const fn applies(self, step: SubstitutionStep) -> bool {
        let steps = self.steps();
        let mut index = 0;
        while index < steps.len() {
            if steps[index] as u8 == step as u8 {
                return true;
            }
            index += 1;
        }
        false
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttributeExpansionLimits {
    pub max_depth: u32,
    pub max_bytes: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributeExpansionError {
    Undefined,
    Cycle,
    DepthLimitExceeded,
    SizeLimitExceeded,
}

pub(crate) struct AttributeEvaluator<'a> {
    values: &'a BTreeMap<String, String>,
    limits: AttributeExpansionLimits,
}

pub(crate) fn apply_replacements(value: &str) -> String {
    let mut output = String::new();
    let mut cursor = 0;
    while cursor < value.len() {
        let rest = &value[cursor..];
        if rest.starts_with("\\'") {
            output.push('\'');
            cursor += 2;
            continue;
        }
        let escaped = rest.starts_with('\\');
        let candidate = if escaped { &rest[1..] } else { rest };
        let replacement = [
            ("(TM)", "™"),
            ("(C)", "©"),
            ("(R)", "®"),
            ("...", "…"),
            ("->", "→"),
            ("<-", "←"),
            ("=>", "⇒"),
            ("<=", "⇐"),
        ]
        .into_iter()
        .find(|(source, _)| candidate.starts_with(source));
        if let Some((source, rendered)) = replacement {
            output.push_str(if escaped { source } else { rendered });
            cursor += source.len() + usize::from(escaped);
            continue;
        }
        let character = rest.chars().next().expect("non-empty remainder");
        if character == '\'' {
            let previous_is_word = output
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
            let next_is_word = rest[1..].chars().next().is_some_and(char::is_alphanumeric);
            output.push(if previous_is_word && next_is_word {
                '’'
            } else {
                '\''
            });
        } else {
            output.push(character);
        }
        cursor += character.len_utf8();
    }
    output
}

impl<'a> AttributeEvaluator<'a> {
    pub(crate) const fn new(
        values: &'a BTreeMap<String, String>,
        limits: AttributeExpansionLimits,
    ) -> Self {
        Self { values, limits }
    }

    pub(crate) fn expand_name(&self, name: &str) -> Result<String, AttributeExpansionError> {
        let mut active = BTreeSet::new();
        self.expand_named(name, 0, &mut active)
    }

    pub(crate) fn expand_text(&self, value: &str) -> Result<String, AttributeExpansionError> {
        self.expand(value, 0, &mut BTreeSet::new())
    }

    fn expand_named(
        &self,
        name: &str,
        depth: u32,
        active: &mut BTreeSet<String>,
    ) -> Result<String, AttributeExpansionError> {
        let value = self
            .values
            .get(name)
            .ok_or(AttributeExpansionError::Undefined)?;
        if !active.insert(name.to_owned()) {
            return Err(AttributeExpansionError::Cycle);
        }
        let result = self.expand(value, depth.saturating_add(1), active);
        active.remove(name);
        result
    }

    fn expand(
        &self,
        value: &str,
        depth: u32,
        active: &mut BTreeSet<String>,
    ) -> Result<String, AttributeExpansionError> {
        if depth > self.limits.max_depth {
            return Err(AttributeExpansionError::DepthLimitExceeded);
        }
        let mut output = String::new();
        let mut cursor = 0;
        while cursor < value.len() {
            let rest = &value[cursor..];
            if rest.starts_with("\\{") {
                output.push('{');
                cursor += 2;
            } else if rest.starts_with('{') {
                let Some(close) = rest.find('}') else {
                    output.push_str(rest);
                    break;
                };
                let name = &rest[1..close];
                if name.is_empty() {
                    output.push_str("{}");
                } else {
                    output.push_str(&self.expand_named(name, depth, active)?);
                }
                cursor += close + 1;
            } else {
                let character = rest.chars().next().expect("non-empty remainder");
                output.push(character);
                cursor += character.len_utf8();
            }
            if output.len() > self.limits.max_bytes as usize {
                return Err(AttributeExpansionError::SizeLimitExceeded);
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttributeEvaluator, AttributeExpansionError, AttributeExpansionLimits, SubstitutionContext,
        SubstitutionStep,
    };
    use std::collections::BTreeMap;

    fn evaluator(values: BTreeMap<String, String>) -> AttributeEvaluator<'static> {
        AttributeEvaluator::new(
            Box::leak(Box::new(values)),
            AttributeExpansionLimits {
                max_depth: 4,
                max_bytes: 32,
            },
        )
    }

    #[test]
    fn contexts_publish_a_fixed_order() {
        assert_eq!(
            SubstitutionContext::Normal.steps().first(),
            Some(&SubstitutionStep::SpecialCharacters)
        );
        assert_eq!(
            SubstitutionContext::Normal.steps().last(),
            Some(&SubstitutionStep::PostReplacements)
        );
        assert_eq!(
            SubstitutionContext::Verbatim.steps(),
            &[SubstitutionStep::SpecialCharacters]
        );
        assert!(SubstitutionContext::Pass.steps().is_empty());
        assert!(SubstitutionContext::None.steps().is_empty());
    }

    #[test]
    fn recursive_attributes_detect_undefined_cycles_depth_and_size() {
        let evaluator = evaluator(BTreeMap::from([
            ("a".into(), "{b}".into()),
            ("b".into(), "value".into()),
            ("cycle".into(), "{cycle}".into()),
            ("long".into(), "012345678901234567890123456789012".into()),
        ]));
        assert_eq!(evaluator.expand_name("a"), Ok("value".into()));
        assert_eq!(
            evaluator.expand_name("missing"),
            Err(AttributeExpansionError::Undefined)
        );
        assert_eq!(
            evaluator.expand_name("cycle"),
            Err(AttributeExpansionError::Cycle)
        );
        assert_eq!(
            evaluator.expand_name("long"),
            Err(AttributeExpansionError::SizeLimitExceeded)
        );
        assert_eq!(evaluator.expand_text("\\{a}"), Ok("{a}".into()));
    }

    #[test]
    fn replacements_are_ordered_and_escapable() {
        assert_eq!(
            super::apply_replacements("Sam's (C) ... -> \\..."),
            "Sam’s © … → ..."
        );
    }
}
