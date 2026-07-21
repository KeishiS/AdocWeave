//! Construction-time resource accounting shared by syntax and semantic builders.

use crate::limits::ProcessingLimits;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BudgetExceeded {
    pub resource: &'static str,
    pub limit: u32,
    pub actual: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct ParseBudget {
    limits: ProcessingLimits,
    blocks: u32,
    nodes: u32,
    references: u32,
    attributes: u32,
    list_continuations: u32,
}

impl ParseBudget {
    pub(crate) fn new(limits: ProcessingLimits) -> Result<Self, BudgetExceeded> {
        let mut budget = Self {
            limits,
            blocks: 0,
            nodes: 0,
            references: 0,
            attributes: 0,
            list_continuations: 0,
        };
        budget.consume_node()?;
        Ok(budget)
    }

    #[cfg(test)]
    pub(crate) fn unlimited() -> Self {
        Self::new(ProcessingLimits {
            max_blocks: u32::MAX,
            max_nodes: u32::MAX,
            max_references: u32::MAX,
            max_attributes: u32::MAX,
            ..ProcessingLimits::default()
        })
        .expect("an unlimited budget accepts the document node")
    }

    pub(crate) fn consume_block(&mut self) -> Result<(), BudgetExceeded> {
        consume(&mut self.blocks, self.limits.max_blocks, "blocks")
    }

    pub(crate) fn consume_node(&mut self) -> Result<(), BudgetExceeded> {
        consume(&mut self.nodes, self.limits.max_nodes, "nodes")
    }

    pub(crate) fn consume_reference(&mut self) -> Result<(), BudgetExceeded> {
        consume(
            &mut self.references,
            self.limits.max_references,
            "references",
        )
    }

    pub(crate) fn consume_attribute(&mut self) -> Result<(), BudgetExceeded> {
        consume(
            &mut self.attributes,
            self.limits.max_attributes,
            "document attributes",
        )
    }

    pub(crate) fn consume_list_continuation(&mut self) -> Result<(), BudgetExceeded> {
        consume(
            &mut self.list_continuations,
            self.limits.max_list_continuations,
            "list continuations",
        )
    }
}

fn consume(current: &mut u32, limit: u32, resource: &'static str) -> Result<(), BudgetExceeded> {
    let actual = u64::from(*current) + 1;
    if actual > u64::from(limit) {
        return Err(BudgetExceeded {
            resource,
            limit,
            actual,
        });
    }
    *current += 1;
    Ok(())
}
