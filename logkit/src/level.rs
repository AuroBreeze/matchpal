use std::fmt;

/// 日志严重级别。
///
/// 大小顺序是有意义的：`Trace < Debug < Info < Warn < Error < Off`。
/// 当 `record.level >= filter_level` 时记录才会被输出，所以把过滤级别设为
/// [`Level::Warn`] 就只保留 `Warn` 和 `Error`，而 [`Level::Off`] 会彻底
/// 关闭日志器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 最细粒度的细节；正常运行时应保持关闭。
    Trace = 0,
    /// 面向开发者的细节，例如原始报文。
    Debug = 1,
    /// 正常的进度消息。
    Info = 2,
    /// 有点不对劲，但程序还能继续工作。
    Warn = 3,
    /// 操作失败了。
    Error = 4,
    /// 屏蔽一切。适合当作过滤级别的取值，永远不要用作日志记录本身的级别。
    Off = 5,
}

impl Level {
    /// 定宽(5 个字符)的大写标签，这样消息那一列永远不会错位。
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

    /// 表示该级别颜色的 ANSI 起始转义序列。
    ///
    /// 之所以保持公开，是因为调用方可能想给自己的前缀上色；但要注意，真正决定
    /// 日志器是否使用它的是 [`crate::set_color`]。
    pub fn color(self) -> &'static str {
        match self {
            Level::Trace => "\x1b[90m", // 亮黑
            Level::Debug => "\x1b[36m", // 青色
            Level::Info => "\x1b[32m",  // 绿色
            Level::Warn => "\x1b[33m",  // 黄色
            Level::Error => "\x1b[31m", // 红色
            Level::Off => "",
        }
    }

    /// 便于存进原子变量的紧凑形式。与 [`Level::from_index`] 互为逆操作。
    pub(crate) fn index(self) -> u8 {
        self as u8
    }

    /// 由 [`Level::index`] 还原出级别。任何超出范围的取值都按 [`Level::Off`]
    /// 处理，也就是刻度上安全的那一端。
    pub(crate) fn from_index(value: u8) -> Level {
        match value {
            0 => Level::Trace,
            1 => Level::Debug,
            2 => Level::Info,
            3 => Level::Warn,
            4 => Level::Error,
            _ => Level::Off,
        }
    }

    /// 解析级别名称，不区分大小写。无法识别的输入返回 `None`，这样调用方可以
    /// 自己决定是给出警告还是回退到默认值。
    pub fn parse(text: &str) -> Option<Level> {
        match text.trim().to_ascii_lowercase().as_str() {
            "trace" | "t" => Some(Level::Trace),
            "debug" | "d" => Some(Level::Debug),
            "info" | "i" => Some(Level::Info),
            // 大家最先想到的拼写是 "warning"。
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
