//! Inline grammar boundary used by block parsing and semantic lowering.

use crate::budget::{BudgetExceeded, ParseBudget};
use crate::inline::{InlineParseConfig, InlineParseOutput};
use crate::source::TextRange;

/// Parses one inline sequence through the dedicated inline grammar component.
pub(crate) fn parse(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    budget: &mut ParseBudget,
) -> Result<InlineParseOutput, BudgetExceeded> {
    crate::inline::parse_with_budget_impl(value, range, config, budget)
}
