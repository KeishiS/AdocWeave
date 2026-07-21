//! Backend-neutral, fully resolved inputs consumed by pure renderers.

use std::collections::BTreeMap;

use crate::reference::ResolvedReference;
use crate::resource::ResolvedResource;
use crate::source::TextRange;

/// An owned snapshot of every host resolution result used during rendering.
///
/// Construction performs no I/O. Exact-range indexes are built once so every
/// consumer observes the same order-independent duplicate semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderInputs {
    references: Vec<ResolvedReference>,
    resources: Vec<ResolvedResource>,
    reference_index: BTreeMap<TextRange, Vec<usize>>,
    resource_index: BTreeMap<TextRange, Vec<usize>>,
}

impl Default for RenderInputs {
    fn default() -> Self {
        Self::new(Vec::new(), Vec::new())
    }
}

impl RenderInputs {
    pub fn new(references: Vec<ResolvedReference>, resources: Vec<ResolvedResource>) -> Self {
        let reference_index = range_index(&references, |resolution| resolution.source_range);
        let resource_index = range_index(&resources, |resolution| resolution.source_range);
        Self {
            references,
            resources,
            reference_index,
            resource_index,
        }
    }

    pub fn references(&self) -> &[ResolvedReference] {
        &self.references
    }

    pub fn resources(&self) -> &[ResolvedResource] {
        &self.resources
    }

    pub fn reference_at(&self, range: TextRange) -> ResolutionMatch<'_, ResolvedReference> {
        lookup(&self.references, &self.reference_index, range)
    }

    pub fn resource_at(&self, range: TextRange) -> ResolutionMatch<'_, ResolvedResource> {
        lookup(&self.resources, &self.resource_index, range)
    }

    pub fn track_usage(&self) -> RenderInputUsage<'_> {
        RenderInputUsage {
            inputs: self,
            used_references: vec![false; self.references.len()],
            used_resources: vec![false; self.resources.len()],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionMatch<'a, T> {
    Missing,
    Unique(&'a T),
    Duplicate,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RenderInputDomain {
    Reference,
    Resource,
}

impl RenderInputDomain {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Resource => "resource",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RenderInputProblemKind {
    Duplicate,
    Unused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderInputProblem {
    pub kind: RenderInputProblemKind,
    pub domain: RenderInputDomain,
    pub range: TextRange,
}

pub struct RenderInputUsage<'a> {
    inputs: &'a RenderInputs,
    used_references: Vec<bool>,
    used_resources: Vec<bool>,
}

impl<'a> RenderInputUsage<'a> {
    pub fn reference_at(&mut self, range: TextRange) -> ResolutionMatch<'a, ResolvedReference> {
        mark_and_lookup(
            &self.inputs.references,
            &self.inputs.reference_index,
            &mut self.used_references,
            range,
        )
    }

    pub fn resource_at(&mut self, range: TextRange) -> ResolutionMatch<'a, ResolvedResource> {
        mark_and_lookup(
            &self.inputs.resources,
            &self.inputs.resource_index,
            &mut self.used_resources,
            range,
        )
    }

    pub fn finish(mut self) -> Vec<RenderInputProblem> {
        let mut problems = Vec::new();
        record_duplicates(
            &self.inputs.reference_index,
            &mut self.used_references,
            RenderInputDomain::Reference,
            &mut problems,
        );
        record_duplicates(
            &self.inputs.resource_index,
            &mut self.used_resources,
            RenderInputDomain::Resource,
            &mut problems,
        );
        for (resolution, used) in self.inputs.references.iter().zip(self.used_references) {
            if !used {
                problems.push(RenderInputProblem {
                    kind: RenderInputProblemKind::Unused,
                    domain: RenderInputDomain::Reference,
                    range: resolution.source_range,
                });
            }
        }
        for (resolution, used) in self.inputs.resources.iter().zip(self.used_resources) {
            if !used {
                problems.push(RenderInputProblem {
                    kind: RenderInputProblemKind::Unused,
                    domain: RenderInputDomain::Resource,
                    range: resolution.source_range,
                });
            }
        }
        problems.sort_by_key(|problem| {
            (
                problem.range.start(),
                problem.range.end(),
                problem.domain,
                problem.kind,
            )
        });
        problems
    }
}

fn range_index<T>(
    values: &[T],
    range: impl Fn(&T) -> TextRange,
) -> BTreeMap<TextRange, Vec<usize>> {
    let mut index = BTreeMap::<_, Vec<usize>>::new();
    for (position, value) in values.iter().enumerate() {
        index.entry(range(value)).or_default().push(position);
    }
    index
}

fn lookup<'a, T>(
    values: &'a [T],
    index: &BTreeMap<TextRange, Vec<usize>>,
    range: TextRange,
) -> ResolutionMatch<'a, T> {
    match index.get(&range).map(Vec::as_slice) {
        None | Some([]) => ResolutionMatch::Missing,
        Some([position]) => ResolutionMatch::Unique(&values[*position]),
        Some(_) => ResolutionMatch::Duplicate,
    }
}

fn mark_and_lookup<'a, T>(
    values: &'a [T],
    index: &BTreeMap<TextRange, Vec<usize>>,
    used: &mut [bool],
    range: TextRange,
) -> ResolutionMatch<'a, T> {
    if let Some(positions) = index.get(&range) {
        for position in positions {
            used[*position] = true;
        }
    }
    lookup(values, index, range)
}

fn record_duplicates(
    index: &BTreeMap<TextRange, Vec<usize>>,
    used: &mut [bool],
    domain: RenderInputDomain,
    problems: &mut Vec<RenderInputProblem>,
) {
    for (range, positions) in index {
        if positions.len() < 2 {
            continue;
        }
        for position in positions {
            used[*position] = true;
        }
        problems.push(RenderInputProblem {
            kind: RenderInputProblemKind::Duplicate,
            domain,
            range: *range,
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::source::{TextRange, TextSize};

    use super::*;

    #[test]
    fn indexes_are_order_independent_and_usage_is_audited_once() {
        let range = TextRange::new(TextSize::ZERO, TextSize::new(1).expect("size")).expect("range");
        let reference = ResolvedReference::resolved(range, "https://example/reference");
        let inputs = RenderInputs::new(
            vec![reference.clone(), reference],
            vec![ResolvedResource::resolved(
                range,
                "https://example/image.png",
                Some("image/png".to_owned()),
                Some(42),
            )],
        );

        assert_eq!(inputs.references().len(), 2);
        assert!(matches!(
            inputs.reference_at(range),
            ResolutionMatch::Duplicate
        ));
        let mut usage = inputs.track_usage();
        assert!(matches!(
            usage.resource_at(range),
            ResolutionMatch::Unique(_)
        ));
        assert_eq!(
            usage.finish(),
            [RenderInputProblem {
                kind: RenderInputProblemKind::Duplicate,
                domain: RenderInputDomain::Reference,
                range,
            }]
        );
    }
}
