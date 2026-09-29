//! `logkit` —— 一个可以随手丢进任何 Rust 项目的分级日志器。
//!
//! 设计目标按优先级排列：零配置、禁用时足够廉价、绝不干扰程序本身的输出。
//!
//! # 快速开始
//!
//! ```no_run
//! logkit::init();                       // 读取 LOG_LEVEL，默认为 info
//! logkit::info!("正在监听端口 {}", 8080);
//! logkit::error!("连不上 {}", "127.0.0.1");
//! ```
//!
//! 输出大致长这样：
//!
//! ```text
//! 2026-09-29 21:26:56.123  INFO   [myapp]  正在监听端口 8080
//! ```
//!
//! # 设计遵循的几条规则
//!
//! - **先过滤，后格式化。** 低于过滤级别的日志记录会被直接丢弃，不产生任何内存
//!   分配，所以 `trace!` 可以放心留在热路径上。
//! - **默认输出目标是 stderr。** stdout 留给管道和重定向，保持干净。
//! - **记日志永不 panic。** 锁中毒、文件缺失或者管道断开，最多丢一行日志，
//!   绝不会把整个程序带崩。
//! - **只有一个全局日志器。** 对命令行工具和桌面应用来说足够了。这里有意不做
//!   按模块配置，也不做异步队列。
//!
//! # 在其他项目中复用它
//!
//! ```toml
//! [dependencies]
//! logkit = { path = "../matchpal/logkit" }
//! ```
//!
//! # 两点值得留意的地方
//!
//! 1. 颜色只在首次使用时自动检测一次：要求 stderr 是终端，在 Windows 上还会
//!    额外开启虚拟终端处理。可以用 [`set_color`] 强制指定。
//! 2. [`Sink::File`] 会无视 [`set_color`] 关闭颜色，因为日志文件里混进转义
//!    序列纯属噪音。

#![warn(missing_docs)]

mod level;
mod sink;

pub use level::Level;
pub use sink::Sink;

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

/// 每一行日志使用的时间戳格式：本地时间，毫秒精度。
const TIMESTAMP: &str = "%Y-%m-%d %H:%M:%S%.3f";

/// 过滤级别，特意放在 [`Config`] 之外，读取时无需加锁。
///
/// 过滤器存在的全部意义就是让被禁用的调用足够廉价，所以它必须在拿到 sink 互斥锁
/// *之前*就可读——否则热路径上每一次 `debug!` 都要付出一把锁外加一次系统调用级
/// 别的锁竞争。
static FILTER: AtomicU8 = AtomicU8::new(Level::Info as u8);

/// 日志器的可变状态。统一放在一把互斥锁后面：日志行都很短，锁只在写入期间持有。
struct Config {
    sink: Sink,
    color: bool,
    show_target: bool,
    /// [`Sink::File`] 的缓存句柄；首次写入时才惰性打开。
    file: Option<std::fs::File>,
}

static CONFIG: OnceLock<Mutex<Config>> = OnceLock::new();

fn config() -> &'static Mutex<Config> {
    CONFIG.get_or_init(|| {
        Mutex::new(Config {
            sink: Sink::Stderr,
            color: color_auto(),
            show_target: true,
            file: None,
        })
    })
}

fn filter() -> Level {
    Level::from_index(FILTER.load(Ordering::Relaxed))
}

// ---------------------------------------------------------------- 公共 API

/// 从 `LOG_LEVEL` 环境变量初始化，未设置或无法识别时回退到 [`Level::Info`]。
/// 返回当前生效的级别，方便在启动时打一行 "当前按 warn 级别记录日志" 之类的横幅。
///
/// 这个调用是可选的：不调用它日志器也能正常工作。
pub fn init() -> Level {
    init_from_env("LOG_LEVEL")
}

/// 与 [`init`] 类似，但读取由你指定的环境变量——当多个程序共用同一台主机、
/// 每个程序都想要自己的开关时很有用。
pub fn init_from_env(variable: &str) -> Level {
    let level = std::env::var(variable)
        .ok()
        .and_then(|text| Level::parse(&text))
        .unwrap_or(Level::Info);
    set_level(level);
    level
}

/// 设置过滤级别。低于该级别的日志记录会被丢弃。
pub fn set_level(level: Level) {
    FILTER.store(level.index(), Ordering::Relaxed);
}

/// 当前的过滤级别。
pub fn level() -> Level {
    filter()
}

/// 这一级别的日志记录会被输出吗？
///
/// 该调用无锁，所以足够廉价，可以用来保护那些构造代价高昂的参数：
///
/// ```no_run
/// # let response = "";
/// if logkit::enabled(logkit::Level::Debug) {
///     logkit::debug!("raw response: {}", response);
/// }
/// ```
pub fn enabled(level: Level) -> bool {
    level >= filter()
}

/// 重定向输出。参见 [`Sink`]。
pub fn set_sink(sink: Sink) {
    if let Ok(mut config) = config().lock() {
        // 丢掉旧句柄，这样之后换成文件 sink 时能干净地重新打开。
        config.file = None;
        config.sink = sink;
    }
}

/// 追加写入文件。等价于 `set_sink(Sink::file(path))`。
pub fn log_to_file(path: impl Into<PathBuf>) {
    set_sink(Sink::file(path));
}

/// 强制打开或关闭颜色。默认是自动检测的（参见 crate 文档）。
pub fn set_color(on: bool) {
    if let Ok(mut config) = config().lock() {
        config.color = on;
    }
}

/// 显示或隐藏 `[module::path]` 字段。默认为显示；如果程序只有一个文件、
/// target 永远相同，可以关掉它。
pub fn set_show_target(on: bool) {
    if let Ok(mut config) = config().lock() {
        config.show_target = on;
    }
}

// ---------------------------------------------------------------- 核心

/// 输出一条日志记录。由各个宏调用，不建议直接调用。
///
/// `Arguments` 会被原样透传，而且过滤检查发生在获取 sink 锁之前，所以一条被过滤
/// 掉的记录确实只花一次 relaxed load 加一次比较。
#[doc(hidden)]
pub fn log(level: Level, target: &str, args: fmt::Arguments<'_>) {
    if level < filter() {
        return;
    }
    // 锁中毒意味着另一个线程在持锁期间 panic 了。记日志不能跟着 panic，
    // 所以丢掉这一行，继续往下走。
    let Ok(mut config) = config().lock() else {
        return;
    };
    let color = config.color && !matches!(config.sink, Sink::File(_));
    let line = format_line(level, target, color, config.show_target, args);
    config.write_line(&line);
}

impl Config {
    fn write_line(&mut self, line: &str) {
        // 克隆 sink 既能让借用检查器满意，又不必在修改 `self.file` 的过程中
        // 一直持有一个引用。stderr/stdout 的克隆是零成本的，只有 File 变体
        // 会复制一次路径。
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
                    // 路径不可写（盘符有问题、权限不足）。退回到 stderr，
                    // 而不是把整行日志整个吞掉。
                    None => {
                        let _ = writeln!(std::io::stderr(), "{line}");
                    }
                }
            }
        }
    }
}

/// `2026-09-29 21:26:56.123  INFO   [target]  消息`
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
    // let-chain：需要 edition 2024，这个 crate 已经在用了
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// 只有别人看得见时，颜色才值得输出：stderr 必须是终端，在 Windows 上
/// 控制台还必须接受 ANSI 转义序列。
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

    /// `STD_ERROR_HANDLE` —— 我们要给它着色的那个输出目标。
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

// ---------------------------------------------------------------- 宏

/// 以 [`Level::Trace`] 级别记录日志。需要显式指定级别时参见 [`log_at`]。
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {
        $crate::log($crate::Level::Trace, module_path!(), format_args!($($arg)*))
    };
}

/// 以 [`Level::Debug`] 级别记录日志。
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        $crate::log($crate::Level::Debug, module_path!(), format_args!($($arg)*))
    };
}

/// 以 [`Level::Info`] 级别记录日志。
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::log($crate::Level::Info, module_path!(), format_args!($($arg)*))
    };
}

/// 以 [`Level::Warn`] 级别记录日志。
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::log($crate::Level::Warn, module_path!(), format_args!($($arg)*))
    };
}

/// 以 [`Level::Error`] 级别记录日志。
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {
        $crate::log($crate::Level::Error, module_path!(), format_args!($($arg)*))
    };
}

/// 以运行时计算出的级别记录日志。
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
    use std::time::Duration;

    /// 时间戳共 23 个字符：`YYYY-MM-DD HH:MM:SS.mmm`。
    const STAMP_LEN: usize = 23;

    fn line(level: Level, target: &str, color: bool, show_target: bool) -> String {
        format_line(level, target, color, show_target, format_args!("hello {}", 1))
    }

    #[test]
    fn layout_is_stamp_level_target_message() {
        let text = line(Level::Info, "myapp::inner", false, true);
        assert_eq!(&text[STAMP_LEN..], "  INFO   [myapp::inner]  hello 1");
        // 与其校验时间戳的具体取值，不如检查它的形状是否合理。
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
        // 与 `log` 内部的比较逻辑保持一致。
        let filter = Level::Warn;
        assert!(Level::Warn >= filter);
        assert!(Level::Error >= filter);
        assert!(!(Level::Info >= filter));
        // Off 会屏蔽一切。
        assert!(!(Level::Error >= Level::Off));
    }

    #[test]
    fn index_round_trips_through_the_atomic_representation() {
        for level in [Level::Trace, Level::Debug, Level::Info, Level::Warn, Level::Error, Level::Off] {
            assert_eq!(Level::from_index(level.index()), level);
        }
        // 超出范围时回退到刻度上安全的那一端。
        assert_eq!(Level::from_index(200), Level::Off);
    }

    /// 唯一会碰到全局状态的测试，集中放在一处，免得和其他测试抢。
    #[test]
    fn file_sink_writes_and_global_level_round_trips() {
        let path = std::env::temp_dir().join(format!("logkit-test-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);

        set_color(true);
        log_to_file(&path);
        set_level(Level::Trace);
        info!("written to a file {}", 42);
        debug!("second line");

        // 过滤读取的是原子变量，`enabled` 必须与 `log` 保持一致。
        assert_eq!(level(), Level::Trace);
        assert!(enabled(Level::Trace));
        set_level(Level::Warn);
        assert_eq!(level(), Level::Warn);
        assert!(enabled(Level::Error));
        assert!(!enabled(Level::Info));

        let text = std::fs::read_to_string(&path).expect("log file should exist");
        assert!(text.contains("INFO "), "{text}");
        assert!(text.contains("written to a file 42"), "{text}");
        assert!(text.contains("second line"), "{text}");
        // 即使颜色被强制打开，文件输出也必须屏蔽颜色。
        assert!(!text.contains('\x1b'), "file sink leaked escape codes: {text:?}");

        // 回归测试：过滤检查必须在获取 sink 锁*之前*完成。
        // 如果先加锁，一个被禁用的调用仍然要排在正在写入的线程后面，
        // 而这恰恰是过滤器要避免的事情。
        set_level(Level::Off);
        let held = config().lock().expect("sink lock");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            crate::error!("filtered out, must not wait for the lock");
            let _ = tx.send(());
        });
        let returned = rx.recv_timeout(Duration::from_secs(2)).is_ok();
        drop(held);
        assert!(returned, "a filtered-out record waited for the sink lock");

        // 恢复默认值，免得影响其他测试。
        set_sink(Sink::Stderr);
        set_level(Level::Info);
        let _ = std::fs::remove_file(&path);
    }
}
