//! Shared parser execution state and failures.

use crate::budget::{BudgetExceeded, ParseBudget};
use crate::source::PositionError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParseFailure {
    Position(PositionError),
    Budget(BudgetExceeded),
    Cancelled,
    InternalInvariant,
}

impl From<PositionError> for ParseFailure {
    fn from(error: PositionError) -> Self {
        Self::Position(error)
    }
}

impl From<BudgetExceeded> for ParseFailure {
    fn from(error: BudgetExceeded) -> Self {
        Self::Budget(error)
    }
}

pub(crate) struct ParseState<'state> {
    pub(crate) budget: &'state mut ParseBudget,
    pub(crate) anchors: &'state mut Vec<crate::block_model::ExplicitAnchor>,
}
