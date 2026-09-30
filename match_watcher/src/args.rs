//! 命令行参数
//!
//! 参数名尽量与 Python 版对齐，迁移过来不用重新记。
//! 抽成 `parse(&[String])` 是为了能单测 —— `parse_args()` 只负责接上
//! `std::env::args()` 和退出码。

use std::path::PathBuf;

use logkit::Level;

/// 抓到并处理完一帧
pub const EXIT_OK: i32 = 0;
/// token 无效(调 getWebsocketInfo 失败)
pub const EXIT_TOKEN_INVALID: i32 = 1;
/// 配置/参数不对
pub const EXIT_ARGS: i32 = 2;

/// 默认快照输出路径，与 Python 版一致
const DEFAULT_JSON_OUT: &str = "capture/match_snapshot.json";

#[derive(Debug, Clone)]
pub struct Args {
    pub config: PathBuf,
    pub token: String,
    pub steamid: String,
    pub platform: u32,
    pub once: bool,
    pub stats: bool,
    /// 名单人数达到这个数就出表并停止(默认 10)；0 = 不等满
    pub full: usize,
    /// 出表后不退出，继续监听(Python 版的持续模式)
    pub keep_going: bool,
    pub timeout: f64,
    /// `None` = 不写快照
    pub json_out: Option<PathBuf>,
    /// `--export kind:path`，可重复
    pub exports: Vec<String>,
    pub resubscribe: f64,
    pub retries: u32,
    pub check: bool,
    pub replay: Option<PathBuf>,
    pub verbose: bool,
    pub log_level: Option<Level>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            config: PathBuf::from("config.local.json"),
            token: String::new(),
            steamid: String::new(),
            platform: 2,
            once: false,
            stats: false,
            full: 10,
            keep_going: false,
            timeout: 0.0,
            json_out: Some(PathBuf::from(DEFAULT_JSON_OUT)),
            exports: Vec::new(),
            resubscribe: 15.0,
            retries: 5,
            check: false,
            replay: None,
            verbose: false,
            log_level: None,
        }
    }
}

const HELP: &str = "\
监听完美世界电竞当前对局：获取 matchId 与 10 人名单，可选合并战绩

  --config <文件>      token 与 steamid 的配置文件(默认 config.local.json)
  --token <token>      覆盖配置文件中的 access_token
  --steamid <id>       覆盖配置文件中的 steamid(17 位 SteamID64)
  --platform <编号>    平台编号(默认 2)
  --once               收到第一帧即退出(不等待名单满员，调试用)
  --full <人数>        名单达到该人数即输出战绩表并停止(默认 10，0 = 不限制)
  --keep-going         输出表格后不退出，继续监听(持续模式)
  --stats              额外将战绩并入表格(人满出表时已默认查询；用于 --once / 持续模式)
  --timeout <秒>       最长运行时间，0 为不限(默认 0)
  --json-out <文件>    快照写入路径(默认 capture/match_snapshot.json)
  --no-json-out        不写快照文件
  --export <写法>      追加导出目标，形如 json:文件 或 ndjson:文件；可重复
  --resubscribe <秒>   未收到对局数据时，每隔该秒数重新订阅一次，0 为关闭(默认 15)
  --retries <次数>     断线/连接失败的重试次数(默认 5)
  --check              仅校验 token 是否有效(有效 0 / 无效 1)
  --replay <文件>      离线回放已保存的推送帧(不连接 WebSocket)
  --log-level <级别>   trace/debug/info/warn/error/off(默认 info)
  --verbose            等价于 --log-level debug：打印所有 WS 帧
  --help               显示本帮助

环境变量 MATCH_WATCHER_LOG 也能设置级别。

退出码：
  0 正常结束          1 token 无效 / 未收到对局推送
  2 配置或参数错误

导出目标(--export)：
  json:<文件>          覆盖写，一帧一份完整 JSON
  ndjson:<文件>        追加，一帧一行，便于下游 tail
  Windows 盘符中的冒号不会被切坏(仅按第一个冒号切分)。
";

/// 解析结果
#[derive(Debug)]
pub enum ParseError {
    /// `--help`
    Help,
    Message(String),
}

/// 接上真实命令行。解析失败直接退出，所以返回 `Args` 而不是 Result。
pub fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match parse(&argv) {
        Ok(args) => args,
        Err(ParseError::Help) => {
            print!("{HELP}");
            std::process::exit(EXIT_OK);
        }
        Err(ParseError::Message(text)) => {
            logkit::error!("{text}(使用 --help 查看用法)");
            std::process::exit(EXIT_ARGS);
        }
    }
}

/// 纯解析，便于单测
pub fn parse(argv: &[String]) -> Result<Args, ParseError> {
    let mut args = Args::default();
    let mut index = 0;
    // 取下一个参数当值；缺了就给空串，由各分支自己判断
    let value = |i: usize| argv.get(i + 1).cloned().unwrap_or_default();

    while index < argv.len() {
        match argv[index].as_str() {
            "--config" => {
                args.config = PathBuf::from(value(index));
                index += 2;
            }
            "--token" => {
                args.token = value(index);
                index += 2;
            }
            "--steamid" => {
                args.steamid = value(index);
                index += 2;
            }
            "--platform" => {
                args.platform = parse_number(&value(index), "--platform")?;
                index += 2;
            }
            "--once" => {
                args.once = true;
                index += 1;
            }
            "--full" => {
                args.full = parse_number(&value(index), "--full")?;
                index += 2;
            }
            "--keep-going" => {
                args.keep_going = true;
                index += 1;
            }
            "--stats" => {
                args.stats = true;
                index += 1;
            }
            "--timeout" => {
                args.timeout = parse_number(&value(index), "--timeout")?;
                index += 2;
            }
            "--json-out" => {
                let path = value(index);
                if path.is_empty() {
                    return Err(ParseError::Message("--json-out 缺少文件路径".into()));
                }
                args.json_out = Some(PathBuf::from(path));
                index += 2;
            }
            "--no-json-out" => {
                args.json_out = None;
                index += 1;
            }
            "--export" => {
                let spec = value(index);
                if spec.is_empty() {
                    return Err(ParseError::Message("--export 缺少写法(形如 json:文件)".into()));
                }
                args.exports.push(spec);
                index += 2;
            }
            "--resubscribe" => {
                args.resubscribe = parse_number(&value(index), "--resubscribe")?;
                index += 2;
            }
            "--retries" => {
                args.retries = parse_number(&value(index), "--retries")?;
                index += 2;
            }
            "--check" => {
                args.check = true;
                index += 1;
            }
            "--replay" => {
                let path = value(index);
                if path.is_empty() {
                    return Err(ParseError::Message("--replay 缺少文件路径".into()));
                }
                args.replay = Some(PathBuf::from(path));
                index += 2;
            }
            "--verbose" => {
                args.verbose = true;
                args.log_level = Some(Level::Debug);
                index += 1;
            }
            "--log-level" => {
                let text = value(index);
                if text.is_empty() || text.starts_with('-') {
                    return Err(ParseError::Message(
                        "--log-level 缺少级别(可选 trace/debug/info/warn/error/off)".into(),
                    ));
                }
                match Level::parse(&text) {
                    Some(level) => args.log_level = Some(level),
                    None => {
                        return Err(ParseError::Message(format!(
                            "无法识别的日志级别：{text}(可选 trace/debug/info/warn/error/off)"
                        )));
                    }
                }
                index += 2;
            }
            "--help" | "-h" => return Err(ParseError::Help),
            other => {
                return Err(ParseError::Message(format!("未知参数：{other}")));
            }
        }
    }
    Ok(args)
}

/// 数值参数：拿不到就报错，不要静默用默认值 —— 打错一个数字却按别的值跑最坑
fn parse_number<T: std::str::FromStr>(text: &str, flag: &str) -> Result<T, ParseError> {
    if text.is_empty() || text.starts_with('-') {
        return Err(ParseError::Message(format!("{flag} 缺少数值")));
    }
    text.parse()
        .map_err(|_| ParseError::Message(format!("{flag} 的数值不合法：{text}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(argv: &[&str]) -> Args {
        let argv: Vec<String> = argv.iter().map(|item| (*item).to_string()).collect();
        parse(&argv).expect("应该能解析")
    }

    fn parse_err(argv: &[&str]) -> String {
        let argv: Vec<String> = argv.iter().map(|item| (*item).to_string()).collect();
        match parse(&argv) {
            Err(ParseError::Message(text)) => text,
            other => panic!("应该报错，实际 {other:?}"),
        }
    }

    #[test]
    fn defaults_match_the_python_version() {
        let args = parse_ok(&[]);
        assert_eq!(args.config, PathBuf::from("config.local.json"));
        assert_eq!(args.platform, 2);
        assert_eq!(args.timeout, 0.0);
        assert_eq!(args.resubscribe, 15.0);
        assert_eq!(args.retries, 5);
        assert_eq!(args.json_out, Some(PathBuf::from("capture/match_snapshot.json")));
        assert!(args.exports.is_empty());
        assert!(!args.once && !args.stats && !args.check && !args.verbose);
        assert!(args.log_level.is_none());
    }

    #[test]
    fn parses_values_and_switches() {
        let args = parse_ok(&[
            "--config", "other.json", "--token", "abc", "--steamid", "76561198000000000",
            "--platform", "3", "--once", "--stats", "--timeout", "20", "--json-out", "out.json",
            "--export", "json:a.json", "--export", "ndjson:b.ndjson",
            "--resubscribe", "0", "--retries", "2", "--replay", "frame.json", "--verbose",
        ]);
        assert_eq!(args.config, PathBuf::from("other.json"));
        assert_eq!(args.token, "abc");
        assert_eq!(args.steamid, "76561198000000000");
        assert_eq!(args.platform, 3);
        assert!(args.once && args.stats);
        assert_eq!(args.timeout, 20.0);
        assert_eq!(args.json_out, Some(PathBuf::from("out.json")));
        assert_eq!(args.exports, vec!["json:a.json", "ndjson:b.ndjson"]);
        // 0 是"关闭重订阅"，不能被当成"没给值"
        assert_eq!(args.resubscribe, 0.0);
        assert_eq!(args.retries, 2);
        assert_eq!(args.replay, Some(PathBuf::from("frame.json")));
        assert_eq!(args.log_level, Some(Level::Debug));
    }

    #[test]
    fn no_json_out_disables_the_default_file() {
        assert_eq!(parse_ok(&["--no-json-out"]).json_out, None);
    }

    /// 数值打错要当场报错，不能静默用默认值继续跑
    #[test]
    fn bad_numbers_are_rejected() {
        assert!(parse_err(&["--platform", "abc"]).contains("--platform"));
        assert!(parse_err(&["--timeout", "很久"]).contains("--timeout"));
        // 缺值也算错
        assert!(parse_err(&["--retries"]).contains("--retries"));
        assert!(parse_err(&["--retries", "--once"]).contains("--retries"));
    }

    #[test]
    fn flags_needing_a_path_reject_the_bare_form() {
        assert!(parse_err(&["--json-out"]).contains("文件路径"));
        assert!(parse_err(&["--export"]).contains("--export"));
        assert!(parse_err(&["--replay"]).contains("文件路径"));
    }

    #[test]
    fn log_level_is_validated() {
        assert_eq!(parse_ok(&["--log-level", "off"]).log_level, Some(Level::Off));
        assert_eq!(parse_ok(&["--log-level", "TRACE"]).log_level, Some(Level::Trace));
        assert!(parse_err(&["--log-level", "verbose"]).contains("verbose"));
        assert!(parse_err(&["--log-level"]).contains("缺少级别"));
        assert!(parse_err(&["--log-level", "--once"]).contains("缺少级别"));
    }

    #[test]
    fn unknown_flag_and_help() {
        assert!(parse_err(&["--nope"]).contains("--nope"));
        let argv = vec!["--help".to_string()];
        assert!(matches!(parse(&argv), Err(ParseError::Help)));
        let argv = vec!["-h".to_string()];
        assert!(matches!(parse(&argv), Err(ParseError::Help)));
    }

    /// 中文参数值不能被拆坏(配置文件名、回放路径都可能带中文)
    #[test]
    fn accepts_non_ascii_values() {
        let args = parse_ok(&["--replay", "抓包/推送样本.json", "--config", "配置.json"]);
        assert_eq!(args.replay, Some(PathBuf::from("抓包/推送样本.json")));
        assert_eq!(args.config, PathBuf::from("配置.json"));
    }
}
