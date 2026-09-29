use std::fmt;

/// Log severity.
///
/// Ordering is meaningful: `Trace < Debug < Info < Warn < Error < Off`.
/// A record is emitted when `record.level >= filter_level`, so setting the
/// filter to [`Level::Warn`] keeps `Warn` and `Error` only, and
/// [`Level::Off`] silences the logger completely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Finest-grained detail; expected to be off in normal runs.
    Trace = 0,
    /// Developer-facing detail, e.g. raw payloads.
    Debug = 1,
    /// Normal progress messages.
    Info = 2,
    /// Something is off but the program keeps working.
    Warn = 3,
    /// The operation failed.
    Error = 4,
    /// Silence everything. Useful as a filter value, never as a record level.
    Off = 5,
}

impl Level {
    /// Fixed-width (5 char) uppercase tag, so the message column never shifts.
    pub fn tag(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
            Level::Off => "OFF  ",
        }
    }

    /// ANSI escape that starts this level's colour.
    ///
    /// Kept public because a caller may want to colour its own prefixes, but
    /// note that [`crate::set_color`] is what decides whether the logger uses
    /// it at all.
    pub fn color(self) -> &'static str {
        match self {
            Level::Trace => "\x1b[90m", // bright black
            Level::Debug => "\x1b[36m", // cyan
            Level::Info => "\x1b[32m",  // green
            Level::Warn => "\x1b[33m",  // yellow
            Level::Error => "\x1b[31m", // red
            Level::Off => "",
        }
    }

    /// Parse a level name, case-insensitively. Returns `None` for anything
    /// unrecognised so the caller can decide whether to warn or fall back.
    pub fn parse(text: &str) -> Option<Level> {
        match text.trim().to_ascii_lowercase().as_str() {
            "trace" | "t" => Some(Level::Trace),
            "debug" | "d" => Some(Level::Debug),
            "info" | "i" => Some(Level::Info),
            // "warning" is the spelling people reach for first.
            "warn" | "warning" | "w" => Some(Level::Warn),
            "error" | "err" | "e" => Some(Level::Error),
            "off" | "none" | "silent" | "quiet" => Some(Level::Off),
            _ => None,
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag().trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_by_severity() {
        assert!(Level::Trace < Level::Debug);
        assert!(Level::Debug < Level::Info);
        assert!(Level::Info < Level::Warn);
        assert!(Level::Warn < Level::Error);
        assert!(Level::Error < Level::Off);
    }

    #[test]
    fn parse_accepts_aliases_and_rejects_garbage() {
        assert_eq!(Level::parse("INFO"), Some(Level::Info));
        assert_eq!(Level::parse("  warn  "), Some(Level::Warn));
        assert_eq!(Level::parse("Warning"), Some(Level::Warn));
        assert_eq!(Level::parse("off"), Some(Level::Off));
        assert_eq!(Level::parse("verbose"), None);
        assert_eq!(Level::parse(""), None);
    }

    #[test]
    fn every_tag_is_five_chars_wide() {
        for level in [Level::Trace, Level::Debug, Level::Info, Level::Warn, Level::Error, Level::Off] {
            assert_eq!(level.tag().len(), 5, "{level} tag is not 5 chars");
        }
    }
}
