//! 数据导出接口
//!
//! 一次导出的内容是这个 crate 对外的**唯一数据出口**：[`MatchSnapshot`]。
//! 想接新去处(CSV、SQLite、HTTP 上报、消息队列、写回 matchpal 主程序…)
//! 只需要：
//!
//! 1. 实现 [`Exporter`]；
//! 2. 在 [`Exporters::parse_spec`] 里认领一个 `kind` 名字。
//!
//! 现有的两个实现是内建的：`json`(覆盖写一个文件，等价 Python 的 `--json-out`)
//! 和 `ndjson`(追加一行一帧，方便下游 `tail -f` 流水线)。
//!
//! 快照的字段名**刻意与 Python 版一致**(`captured_at` / `match` / `stats`)，
//! 因为下游可能已经在读这个形状了。

use std::path::PathBuf;

use serde::Serialize;

use crate::model::{MatchInfo, StatsMap};

/// 一次导出的内容
#[derive(Debug, Clone, Serialize)]
pub struct MatchSnapshot {
    /// 抓取时刻，`%Y-%m-%d %H:%M:%S`
    pub captured_at: String,
    /// 帧原文，字段名叫 `match` 是为了对齐 Python 版写出的快照
    #[serde(rename = "match")]
    pub info: MatchInfo,
    /// 战绩，未查则是空对象
    pub stats: StatsMap,
}

impl MatchSnapshot {
    /// 打上当前时间
    pub fn now(info: MatchInfo, stats: StatsMap) -> Self {
        Self {
            captured_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            info,
            stats,
        }
    }
}

/// 导出失败
#[derive(Debug)]
pub enum ExportError {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// 写法不认识，例如 `--export csv:x.csv`
    UnknownKind(String),
    /// 写法本身不合法，例如漏了路径
    BadSpec(String),
    /// 实现方自定义的失败
    Other(String),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportError::Io(err) => write!(f, "写文件失败：{err}"),
            ExportError::Json(err) => write!(f, "序列化失败：{err}"),
            ExportError::UnknownKind(kind) => {
                write!(f, "无法识别的导出类型：{kind}(目前支持 json / ndjson)")
            }
            ExportError::BadSpec(spec) => {
                write!(f, "导出目标格式不正确：{spec}(应为 json:capture/match.json 的形式)")
            }
            ExportError::Other(text) => write!(f, "{text}"),
        }
    }
}

impl std::error::Error for ExportError {}

/// 导出目标。同一个实例会被反复调用(每收到一帧调一次)。
pub trait Exporter {
    /// 类型名，日志里用来指认是谁失败了
    fn kind(&self) -> &'static str;

    /// 导出一帧
    fn export(&mut self, snapshot: &MatchSnapshot) -> Result<(), ExportError>;
}

/// 覆盖写一个 JSON 文件(等价 Python 的 `--json-out`)
pub struct JsonFile {
    path: PathBuf,
}

impl JsonFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Exporter for JsonFile {
    fn kind(&self) -> &'static str {
        "json"
    }

    fn export(&mut self, snapshot: &MatchSnapshot) -> Result<(), ExportError> {
        let text = serde_json::to_string_pretty(snapshot).map_err(ExportError::Json)?;
        crate::export::write_file(&self.path, &text)
    }
}

/// 追加一行一帧的 NDJSON(每帧一行，不覆盖历史)
pub struct JsonLines {
    path: PathBuf,
}

impl JsonLines {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Exporter for JsonLines {
    fn kind(&self) -> &'static str {
        "ndjson"
    }

    fn export(&mut self, snapshot: &MatchSnapshot) -> Result<(), ExportError> {
        let line = serde_json::to_string(snapshot).map_err(ExportError::Json)?;
        crate::export::append_line(&self.path, &line)
    }
}

/// 一组导出目标，按加入顺序依次调用
#[derive(Default)]
pub struct Exporters {
    list: Vec<Box<dyn Exporter>>,
}

impl Exporters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, exporter: Box<dyn Exporter>) {
        self.list.push(exporter);
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        self.list.iter().map(|exporter| exporter.kind()).collect()
    }

    /// 解析 `kind:path` 写法。
    ///
    /// 只按**第一个**冒号切分，所以 Windows 盘符不会被切坏：
    /// `json:C:\out\match.json` 的路径是 `C:\out\match.json`。
    pub fn parse_spec(spec: &str) -> Result<Box<dyn Exporter>, ExportError> {
        let Some((kind, path)) = spec.split_once(':') else {
            return Err(ExportError::BadSpec(spec.to_string()));
        };
        if path.trim().is_empty() {
            return Err(ExportError::BadSpec(spec.to_string()));
        }
        match kind.trim().to_ascii_lowercase().as_str() {
            "json" => Ok(Box::new(JsonFile::new(path.trim()))),
            "ndjson" | "jsonl" => Ok(Box::new(JsonLines::new(path.trim()))),
            other => Err(ExportError::UnknownKind(other.to_string())),
        }
    }

    /// 逐个导出。**一个失败不影响后面的**，失败项连名字一起返回，由调用方决定要不要报错。
    pub fn export_all(&mut self, snapshot: &MatchSnapshot) -> Vec<(&'static str, ExportError)> {
        let mut failures = Vec::new();
        for exporter in &mut self.list {
            if let Err(err) = exporter.export(snapshot) {
                failures.push((exporter.kind(), err));
            }
        }
        failures
    }
}

/// 建目录 + 覆盖写
pub fn write_file(path: &std::path::Path, text: &str) -> Result<(), ExportError> {
    ensure_parent(path)?;
    std::fs::write(path, text).map_err(ExportError::Io)
}

/// 建目录 + 追加一行
pub fn append_line(path: &std::path::Path, line: &str) -> Result<(), ExportError> {
    ensure_parent(path)?;
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(ExportError::Io)?;
    writeln!(file, "{line}").map_err(ExportError::Io)
}

fn ensure_parent(path: &std::path::Path) -> Result<(), ExportError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(ExportError::Io)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{classify, Frame};
    use serde_json::json;

    fn snapshot() -> MatchSnapshot {
        let text = json!({
            "messageType": 10002,
            "messageData": {"matchId": "9215951389778120460", "map": "de_dust2", "playerList": []}
        })
        .to_string();
        let Frame::Match(info) = classify(&text) else { panic!("样本应该是对局帧") };
        MatchSnapshot::now(*info, StatsMap::new())
    }

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("match-watcher-export-{}", std::process::id()));
        dir.join(name)
    }

    #[test]
    fn spec_parsing_accepts_both_kinds() {
        assert_eq!(Exporters::parse_spec("json:capture/a.json").unwrap().kind(), "json");
        assert_eq!(Exporters::parse_spec("ndjson:capture/a.ndjson").unwrap().kind(), "ndjson");
        assert_eq!(Exporters::parse_spec("jsonl:x.ndjson").unwrap().kind(), "ndjson");
    }

    /// Windows 盘符里的冒号不能被当成 kind/path 的分隔
    #[test]
    fn spec_parsing_keeps_windows_drive_letters() {
        let path = temp_path("drive.json");
        let spec = format!("json:{}", path.display());
        let mut exporter = Exporters::parse_spec(&spec).unwrap();
        exporter.export(&snapshot()).expect("应该能写出来");
        assert!(path.is_file(), "文件没写到 {} ", path.display());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn spec_parsing_rejects_junk() {
        assert!(matches!(Exporters::parse_spec("json"), Err(ExportError::BadSpec(_))));
        assert!(matches!(Exporters::parse_spec("json:   "), Err(ExportError::BadSpec(_))));
        assert!(matches!(Exporters::parse_spec("csv:x.csv"), Err(ExportError::UnknownKind(_))));
        // 错误信息要能直接给人看(`Box<dyn Exporter>` 不是 Debug，不能用 unwrap_err)
        let Err(err) = Exporters::parse_spec("csv:x.csv") else {
            panic!("csv 应该被拒绝");
        };
        let text = err.to_string();
        assert!(text.contains("csv"), "{text}");
        assert!(text.contains("json"), "{text}");
    }

    #[test]
    fn json_file_overwrites() {
        let path = temp_path("overwrite.json");
        let mut exporter = JsonFile::new(&path);
        exporter.export(&snapshot()).unwrap();
        exporter.export(&snapshot()).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        // 覆盖写只能有一份 JSON
        assert_eq!(text.matches("\"captured_at\"").count(), 1, "{text}");
        // 字段名与 Python 版快照一致：captured_at / match / stats
        assert!(text.contains("\"match\""));
        assert!(text.contains("\"stats\""));
        assert!(text.contains("9215951389778120460"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn json_lines_appends_one_line_per_frame() {
        let path = temp_path("frames.ndjson");
        let _ = std::fs::remove_file(&path);
        let mut exporter = JsonLines::new(&path);
        exporter.export(&snapshot()).unwrap();
        exporter.export(&snapshot()).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        // 每一行都必须是完整可解析的 JSON
        for line in text.lines() {
            let value: serde_json::Value = serde_json::from_str(line).expect("每行都该是合法 JSON");
            assert_eq!(value["match"]["matchId"], json!("9215951389778120460"));
        }
        let _ = std::fs::remove_file(&path);
    }

    /// 这是"预留接口"的关键：外部实现能插进来，失败也不影响别人
    #[derive(Default)]
    struct Counting {
        seen: usize,
        fail: bool,
    }

    impl Exporter for Counting {
        fn kind(&self) -> &'static str {
            "counting"
        }
        fn export(&mut self, _snapshot: &MatchSnapshot) -> Result<(), ExportError> {
            self.seen += 1;
            if self.fail {
                return Err(ExportError::Other("故意失败".into()));
            }
            Ok(())
        }
    }

    #[test]
    fn registry_calls_every_exporter_and_collects_failures() {
        let path = temp_path("registry.json");
        let mut exporters = Exporters::new();
        exporters.add(Box::new(Counting::default()));
        exporters.add(Box::new(Counting { seen: 0, fail: true }));
        exporters.add(Box::new(JsonFile::new(&path)));
        assert_eq!(exporters.len(), 3);
        assert_eq!(exporters.kinds(), vec!["counting", "counting", "json"]);

        let failures = exporters.export_all(&snapshot());
        // 中间那个失败了，但最后一个照样写了
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "counting");
        assert!(path.is_file(), "失败不该阻断后面的导出目标");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_registry_is_harmless() {
        let mut exporters = Exporters::new();
        assert!(exporters.is_empty());
        assert!(exporters.export_all(&snapshot()).is_empty());
    }

    #[test]
    fn export_creates_missing_directories() {
        let path = temp_path("nested/deep/frame.json");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        let mut exporter = JsonFile::new(&path);
        exporter.export(&snapshot()).expect("应自动建目录");
        assert!(path.is_file());
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }
}
