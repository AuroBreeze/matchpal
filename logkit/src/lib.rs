//! `logkit` — a leveled logger you can drop into any Rust project.
//!
//! Goals, in order: zero setup, cheap when disabled, and never in the way of
//! your actual output.
//!
//! # Quick start
//!
//! ```no_run
//! logkit::init();                       // reads LOG_LEVEL, defaults to info
//! logkit::info!("listening on port {}", 8080);
//! logkit::error!("cannot reach {}", "127.0.0.1");
//! ```
//!
//! Output looks like this:
//!
//! ```text
//! 2026-09-29 21:26:56.123  INFO   [myapp]  listening on port 8080
//! ```
//!
//! # Rules the design follows
//!
//! - **Filtering happens before formatting.** A record below the filter level
//!   is dropped without allocating, so `trace!` calls can stay in hot paths.
//! - **Default sink is stderr.** stdout stays clean for pipes and redirects.
//! - **Logging never panics.** A poisoned lock, a missing file or a broken pipe
//!   loses a line at most; it never takes the program down.
//! - **One global logger.** Enough for CLIs and desktop apps. There is no
//!   per-module configuration and no async queue on purpose.
//!
//! # Reusing it from another project
//!
//! ```toml
//! [dependencies]
//! logkit = { path = "../matchpal/logkit" }
//! ```
//!
//! # Two things worth knowing
//!
//! 1. Colour is auto-detected once, on first use: it requires stderr to be a
//!    terminal, and on Windows it also turns on virtual terminal processing.
//!    Force it with [`set_color`].
//! 2. [`Sink::File`] disables colour regardless of [`set_color`], because
//!    escape codes in a log file are noise.

#![warn(missing_docs)]

mod level;
mod sink;

pub use level::Level;
pub use sink::Sink;

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Timestamp format used for every line: local time, millisecond precision.
const TIMESTAMP: &str = "%Y-%m-%d %H:%M:%S%.3f";

/// Mutable logger state. Kept behind one mutex: log lines are short and the
/// lock is only held for the duration of a write.
struct Config {
    level: Level,
    sink: Sink,
    color: bool,
    show_target: bool,
    /// Cached handle for [`Sink::File`]; opened lazily on first write.
    file: Option<std::fs::File>,
}

static CONFIG: OnceLock<Mutex<Config>> = OnceLock::new();

fn config() -> &'static Mutex<Config> {
    CONFIG.get_or_init(|| {
        Mutex::new(Config {
            level: Level::Info,
            sink: Sink::Stderr,
            color: color_auto(),
            show_target: true,
            file: None,
        })
    })
}

// ---------------------------------------------------------------- public API

/// Initialise from the `LOG_LEVEL` environment variable, falling back to
/// [`Level::Info`] if it is unset or unrecognised. Returns the level in force,
/// which is handy for a one-line "logging at warn" startup banner.
///
/// Calling this is optional: the logger works without it.
pub fn init() -> Level {
    init_from_env("LOG_LEVEL")
}

/// Like [`init`], but reads a variable you name — useful when several programs
/// share a host and each wants its own knob.
pub fn init_from_env(variable: &str) -> Level {
    let level = std::env::var(variable)
        .ok()
        .and_then(|text| Level::parse(&text))
        .unwrap_or(Level::Info);
    set_level(level);
    level
}

/// Set the filter level. Records below it are dropped.
pub fn set_level(level: Level) {
    if let Ok(mut config) = config().lock() {
        config.level = level;
    }
}

/// Current filter level.
pub fn level() -> Level {
    config().lock().map(|config| config.level).unwrap_or(Level::Off)
}

/// Is a record at this level going to be emitted?
///
/// Use it to guard arguments that are expensive to build:
///
/// ```no_run
/// # let response = "";
/// if logkit::enabled(logkit::Level::Debug) {
///     logkit::debug!("raw response: {}", response);
/// }
/// ```
pub fn enabled(level: Level) -> bool {
    config().lock().map(|config| level >= config.level).unwrap_or(false)
}

/// Redirect output. See [`Sink`].
pub fn set_sink(sink: Sink) {
    if let Ok(mut config) = config().lock() {
        // Drop the old handle so a subsequent file sink reopens cleanly.
        config.file = None;
        config.sink = sink;
    }
}

/// Append to a file. Shorthand for `set_sink(Sink::file(path))`.
pub fn log_to_file(path: impl Into<PathBuf>) {
    set_sink(Sink::file(path));
}

/// Force colour on or off. By default it is auto-detected (see the crate docs).
pub fn set_color(on: bool) {
    if let Ok(mut config) = config().lock() {
        config.color = on;
    }
}

/// Show or hide the `[module::path]` field. Defaults to on; turn it off for
/// single-file programs where the target is always the same.
pub fn set_show_target(on: bool) {
    if let Ok(mut config) = config().lock() {
        config.show_target = on;
    }
}

// ---------------------------------------------------------------- core

/// Emit one record. Called by the macros; not meant to be called directly.
///
/// The `Arguments` is passed straight through, so a filtered-out record costs
/// a comparison and nothing else.
#[doc(hidden)]
pub fn log(level: Level, target: &str, args: fmt::Arguments<'_>) {
    // A poisoned lock means another thread panicked while holding it. Logging
    // must not panic in turn, so lose the line and carry on.
    let Ok(mut config) = config().lock() else {
        return;
    };
    if level < config.level {
        return;
    }
    let color = config.color && !matches!(config.sink, Sink::File(_));
    let line = format_line(level, target, color, config.show_target, args);
    config.write_line(&line);
}

impl Config {
    fn write_line(&mut self, line: &str) {
        // Cloning the sink keeps the borrow checker happy without holding a
        // reference across the mutation of `self.file`. Stderr/stdout clones
        // are free; only the file variant copies a path.
        match self.sink.clone() {
            Sink::Stderr => {
                let _ = writeln!(std::io::stderr(), "{line}");
            }
            Sink::Stdout => {
                let _ = writeln!(std::io::stdout(), "{line}");
            }
            Sink::File(path) => {
                if self.file.is_none() {
                    self.file = open_append(&path);
                }
                match &mut self.file {
                    Some(file) => {
                        let _ = writeln!(file, "{line}");
                    }
                    // Unwritable path (bad drive, permissions). Fall back to
                    // stderr rather than swallowing the line entirely.
                    None => {
                        let _ = writeln!(std::io::stderr(), "{line}");
                    }
                }
            }
        }
    }
}

/// `2026-09-29 21:26:56.123  INFO   [target]  message`
fn format_line(
    level: Level,
    target: &str,
    color: bool,
    show_target: bool,
    args: fmt::Arguments<'_>,
) -> String {
    let now = chrono::Local::now().format(TIMESTAMP);
    let head = if color {
        format!("{now}  {}{}\x1b[0m", level.color(), level.tag())
    } else {
        format!("{now}  {}", level.tag())
    };
    let tail = if show_target && !target.is_empty() {
        format!("  [{target}]")
    } else {
        String::new()
    };
    format!("{head}{tail}  {args}")
}

fn open_append(path: &Path) -> Option<std::fs::File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// Colour is only worth emitting when someone can see it: stderr must be a
/// terminal, and on Windows the console must accept ANSI escapes.
fn color_auto() -> bool {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return false;
    }
    enable_ansi()
}

#[cfg(windows)]
fn enable_ansi() -> bool {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
    }

    /// `STD_ERROR_HANDLE` — the sink we colour.
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    unsafe {
        let handle = GetStdHandle(STD_ERROR_HANDLE);
        let mut mode = 0u32;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

#[cfg(not(windows))]
fn enable_ansi() -> bool {
    true
}

// ---------------------------------------------------------------- macros

/// Log at [`Level::Trace`]. See [`log_at`] for an explicit level.
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {
        $crate::log($crate::Level::Trace, module_path!(), format_args!($($arg)*))
    };
}

/// Log at [`Level::Debug`].
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        $crate::log($crate::Level::Debug, module_path!(), format_args!($($arg)*))
    };
}

/// Log at [`Level::Info`].
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::log($crate::Level::Info, module_path!(), format_args!($($arg)*))
    };
}

/// Log at [`Level::Warn`].
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::log($crate::Level::Warn, module_path!(), format_args!($($arg)*))
    };
}

/// Log at [`Level::Error`].
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {
        $crate::log($crate::Level::Error, module_path!(), format_args!($($arg)*))
    };
}

/// Log at a level computed at run time.
///
/// ```no_run
/// let level = logkit::Level::Warn;
/// logkit::log_at!(level, "disk {}% full", 91);
/// ```
#[macro_export]
macro_rules! log_at {
    ($level:expr, $($arg:tt)*) => {
        $crate::log($level, module_path!(), format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Timestamps are 23 chars: `YYYY-MM-DD HH:MM:SS.mmm`.
    const STAMP_LEN: usize = 23;

    fn line(level: Level, target: &str, color: bool, show_target: bool) -> String {
        format_line(level, target, color, show_target, format_args!("hello {}", 1))
    }

    #[test]
    fn layout_is_stamp_level_target_message() {
        let text = line(Level::Info, "myapp::inner", false, true);
        assert_eq!(&text[STAMP_LEN..], "  INFO   [myapp::inner]  hello 1");
        // Sanity-check the timestamp shape rather than its value.
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[10..11], " ");
        assert_eq!(&text[13..14], ":");
        assert_eq!(&text[19..20], ".");
    }

    #[test]
    fn target_can_be_hidden() {
        let text = line(Level::Warn, "myapp", false, false);
        assert_eq!(&text[STAMP_LEN..], "  WARN   hello 1");
    }

    #[test]
    fn colour_wraps_the_level_only() {
        let coloured = line(Level::Error, "myapp", true, true);
        assert!(coloured.contains("\x1b[31mERROR\x1b[0m"));
        assert!(coloured.ends_with("[myapp]  hello 1"));
        assert!(!line(Level::Error, "myapp", false, true).contains('\x1b'));
    }

    #[test]
    fn level_filter_is_inclusive_of_the_boundary() {
        // Mirrors the comparison inside `log`.
        let filter = Level::Warn;
        assert!(Level::Warn >= filter);
        assert!(Level::Error >= filter);
        assert!(!(Level::Info >= filter));
        // Off silences everything.
        assert!(!(Level::Error >= Level::Off));
    }

    /// The only test that touches global state, kept in one place so it cannot
    /// race with others.
    #[test]
    fn file_sink_writes_and_escapes_nothing() {
        let path = std::env::temp_dir().join(format!("logkit-test-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);

        set_color(true);
        log_to_file(&path);
        set_level(Level::Trace);
        info!("written to a file {}", 42);
        debug!("second line");

        let text = std::fs::read_to_string(&path).expect("log file should exist");
        assert!(text.contains("INFO "), "{text}");
        assert!(text.contains("written to a file 42"), "{text}");
        assert!(text.contains("second line"), "{text}");
        // Colour must be suppressed for file output even though it is forced on.
        assert!(!text.contains('\x1b'), "file sink leaked escape codes: {text:?}");

        // Restore defaults so other tests are unaffected.
        set_sink(Sink::Stderr);
        set_level(Level::Info);
        let _ = std::fs::remove_file(&path);
    }
}
