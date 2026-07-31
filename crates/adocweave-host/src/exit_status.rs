//! Exit status categories shared by the native host programs.
//!
//! A caller that only asks "did it work" reads zero and non-zero, and nothing
//! here changes that. A caller that has to react differently to a mistyped
//! option and to an unreadable file needs the two to be distinguishable, which
//! a single failure code cannot provide. The command-line interface and the
//! Language Server report the same numbers so the answer does not depend on
//! which of the two programs ran.

/// Reason a native host program stopped, as the number it reports.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum ExitStatus {
    /// The work finished and nothing reached the failure threshold.
    Success = 0,
    /// Diagnostics reached the configured failure threshold.
    Diagnostics = 1,
    /// The arguments or options given cannot be acted on.
    Usage = 2,
    /// Reading or writing a file, stream or resource failed.
    InputOutput = 3,
    /// A configured limit on input size or resources was exceeded.
    LimitExceeded = 4,
}

impl ExitStatus {
    /// Returns the number a caller observes.
    pub const fn code(self) -> u8 {
        self as u8
    }
}

impl From<ExitStatus> for std::process::ExitCode {
    fn from(status: ExitStatus) -> Self {
        Self::from(status.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_category_keeps_the_number_callers_depend_on() {
        // These numbers are a published contract. Changing one silently changes
        // what a caller's script concludes, so they are asserted rather than
        // left to the order of the declarations.
        assert_eq!(ExitStatus::Success.code(), 0);
        assert_eq!(ExitStatus::Diagnostics.code(), 1);
        assert_eq!(ExitStatus::Usage.code(), 2);
        assert_eq!(ExitStatus::InputOutput.code(), 3);
        assert_eq!(ExitStatus::LimitExceeded.code(), 4);
    }
}
