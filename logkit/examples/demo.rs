//! 冒烟测试 / 用法示例。
//!
//! ```text
//! cargo run -p logkit --example demo
//! LOG_LEVEL=trace cargo run -p logkit --example demo
//! ```

fn main() {
    let level = logkit::init();
    logkit::info!("当前日志级别：{level}");

    logkit::trace!("最细的细节，平时是看不见的");
    logkit::debug!("出问题时才有用的细节");
    logkit::warn!("磁盘已用 {}%", 91);
    logkit::error!("连不上 {}", "127.0.0.1:8080");

    // 把代价高昂的东西挡在 `enabled` 后面，关闭时就不花任何代价。
    if logkit::enabled(logkit::Level::Debug) {
        logkit::debug!("代价高昂的内容：{}", "x".repeat(32));
    }

    // 级别在运行时决定，同时关掉 target 字段。
    logkit::log_at!(logkit::Level::Info, "运行时决定的级别也能用");
    logkit::set_show_target(false);
    logkit::info!("从这里开始不再带 [模块路径]");
}
