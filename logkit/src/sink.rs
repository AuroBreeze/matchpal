use std::path::PathBuf;

/// 日志行写入到哪里。
///
/// 默认为 [`Sink::Stderr`]：命令行工具的 stdout 经常被管道或重定向接管
/// (JSON 输出、下游消费者)，把日志行混进去会污染数据。只有当有人正盯着
/// 屏幕看时，才应该改用 stdout。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sink {
    /// 默认值。为管道和重定向保持 stdout 干净。
    Stderr,
    /// 适用于 stdout *就是*日志流的交互式工具。
    Stdout,
    /// 追加写入文件，文件(及其父目录)不存在时自动创建。
    /// 使用这个输出目标时颜色会自动关闭。
    File(PathBuf),
}

impl Sink {
    /// 便捷构造函数，调用方不必再导入 [`PathBuf`]。
    pub fn file(path: impl Into<PathBuf>) -> Sink {
        Sink::File(path.into())
    }
}
