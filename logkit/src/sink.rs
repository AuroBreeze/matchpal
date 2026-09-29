use std::path::PathBuf;

/// Where log lines are written.
///
/// Defaults to [`Sink::Stderr`]: a CLI's stdout is often piped or redirected
/// (JSON output, downstream consumers), and mixing log lines into it corrupts
/// the data. Move to stdout only when a human is watching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sink {
    /// Default. Keeps stdout clean for pipes and redirects.
    Stderr,
    /// For interactive tools where stdout *is* the log stream.
    Stdout,
    /// Append to a file, creating it (and its parent directories) if missing.
    /// Colour is disabled automatically for this sink.
    File(PathBuf),
}

impl Sink {
    /// Convenience constructor so callers do not have to import [`PathBuf`].
    pub fn file(path: impl Into<PathBuf>) -> Sink {
        Sink::File(path.into())
    }
}
