//! novaui's one error type: what a fallible call returns for an app to report.

/// What can go wrong in novaui.
///
/// Broken invariants inside the framework are bugs, not errors: they are `debug_assert!`s and
/// never reach an app as an `Error`.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A `.nui` sheet, fragment or condition, or a strings table, that does not parse, or a
    /// template that does not build: the 1-based line (0 when it is not about one) and why.
    Parse { line: usize, msg: String },
    /// The window, the graphics device or the swapchain could not be made (`platform::run`).
    Platform(String),
}

impl Error {
    pub(crate) fn parse(line: usize, msg: impl Into<String>) -> Self {
        Error::Parse { line, msg: msg.into() }
    }

    /// The source line a `Parse` error is on.
    pub fn line(&self) -> Option<usize> {
        match self {
            Error::Parse { line, .. } if *line > 0 => Some(*line),
            _ => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse { line: 0, msg } => f.write_str(msg),
            Error::Parse { line, msg } => write!(f, "line {line}: {msg}"),
            Error::Platform(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for Error {}
