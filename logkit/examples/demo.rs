//! Smoke test / usage sample.
//!
//! ```text
//! cargo run -p logkit --example demo
//! LOG_LEVEL=trace cargo run -p logkit --example demo
//! ```

fn main() {
    let level = logkit::init();
    logkit::info!("log level in force: {level}");

    logkit::trace!("finest detail, normally hidden");
    logkit::debug!("useful when something is broken");
    logkit::warn!("disk {}% full", 91);
    logkit::error!("cannot reach {}", "127.0.0.1:8080");

    // Guard anything expensive behind `enabled` so it costs nothing when off.
    if logkit::enabled(logkit::Level::Debug) {
        logkit::debug!("expensive payload: {}", "x".repeat(32));
    }

    // Level chosen at run time, and the target field turned off.
    logkit::log_at!(logkit::Level::Info, "runtime level works");
    logkit::set_show_target(false);
    logkit::info!("no [module::path] from here on");
}
