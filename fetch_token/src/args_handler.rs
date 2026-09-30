use std::path::PathBuf;

use logkit::Level;

use crate::pause::{self, Pause};

pub struct Args {
    pub port: u16,
    pub out: PathBuf,
    pub write_config: PathBuf,
    pub names: Vec<String>,
    pub hosts: Vec<String>,
    pub ca_store: String,
    pub timeout: u64,
    pub keep_going: bool,
    pub keep_ca: bool,
    /// 命令行显式给的日志级别；`None` = 没给，交给 `FETCH_TOKEN_LOG` / 默认 info
    pub log_level: Option<Level>,
    pub log_all: Option<PathBuf>,
    pub no_elevate: bool,
    /// WS 推送后端端口；0 = 关闭
    pub push_port: u16,
}

const HELP: &str = "\
fetch access_token

  --port <端口>           本机监听端口(默认 8080)
  --out <目录>            工作目录，放 CA 证书(默认 capture)
  --write-config <文件>   命中后写入该文件(默认 config.local.json)
  --names <列表>          要捕获的字段名，逗号分隔
  --hosts <域名列表>      在默认白名单之外再追加域名(逗号分隔)
  --any-host              不限制域名(无关噪声较多：CSRF/资讯流 token 也会被写入)
  --ca-store <machine|user> 证书装机器库(需管理员，默认)还是当前用户库
  --timeout <秒>          最长运行时间(默认 300)
  --keep-going            命中后不退出，继续运行
  --keep-ca               结束后保留根证书(默认卸载)
  --log-all <文件>        把所有经过的请求 URL 记录到该文件
  --log-level <级别>      trace/debug/info/warn/error/off(默认 info)
  --verbose               等价于 --log-level debug：打印所有经过的请求
  --no-elevate            不自动提权(自己保证管理员权限)
  --push-port <端口>      WS 推送后端端口(默认 8787，0 = 关闭)：
                          客户端连上 ws://127.0.0.1:<端口> 即可收到捕获结果
  --pause [秒]            结束前留住窗口：不带值停 10 秒，带值停 N 秒，0 = 一直等到按键
  --no-pause              结束就关窗口，不留(脚本/CI 用)
  --help                  显示本帮助

环境变量 FETCH_TOKEN_LOG 也能设置级别；双击运行时无法附加参数，可使用该环境变量。

退出码：
  0 捕获并写入配置      1 超时未命中
  2 环境/参数错误        3 提权被拒绝
  4 已捕获但写入配置失败
";

/// 解析命令行。日志级别只在这里折算成 `Option<Level>`，
/// 真正生效交给 main —— `FETCH_TOKEN_LOG` 的解析复用 logkit 自己的实现。
pub fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut args = Args {
        port: 8080,
        out: PathBuf::from("capture"),
        write_config: PathBuf::from("config.local.json"),
        names: vec![
            "access_token".into(),
            "steam_cn_token".into(),
            "accesstoken".into(),
            "pvp_app_token".into(),
            "token".into(),
        ],
        hosts: default_hosts(),
        ca_store: "machine".into(),
        timeout: 300,
        keep_going: false,
        keep_ca: false,
        log_level: None,
        log_all: None,
        no_elevate: false,
        push_port: 8787,
    };
    let mut index = 0;
    let value = |i: usize| argv.get(i + 1).cloned().unwrap_or_default();
    while index < argv.len() {
        match argv[index].as_str() {
            "--port" => {
                args.port = value(index).parse().unwrap_or(8080);
                index += 2;
            }
            "--out" => {
                args.out = PathBuf::from(value(index));
                index += 2;
            }
            "--write-config" => {
                args.write_config = PathBuf::from(value(index));
                index += 2;
            }
            "--names" => {
                args.names = value(index).split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
                index += 2;
            }
            "--ca-store" => {
                args.ca_store = value(index);
                index += 2;
            }
            "--hosts" => {
                // 追加而不是替换：漏抓时用户往往只想补一个域名
                // 替换语义会逼他把默认 11 个再抄一遍，抄漏了就是新的漏抓
                args.hosts.extend(normalize_hosts(&value(index)));
                index += 2;
            }
            "--any-host" => {
                args.hosts.clear();
                index += 1;
            }
            "--timeout" => {
                args.timeout = value(index).parse().unwrap_or(300);
                index += 2;
            }
            "--log-all" => {
                args.log_all = Some(PathBuf::from(value(index)));
                index += 2;
            }
            "--keep-going" => {
                args.keep_going = true;
                index += 1;
            }
            "--keep-ca" => {
                args.keep_ca = true;
                index += 1;
            }
            "--verbose" => {
                // 老旗标保留：等价于 --log-level debug
                args.log_level = Some(Level::Debug);
                index += 1;
            }
            "--log-level" => {
                let text = value(index);
                // 下一个参数是别的旗标(或已经是最后一个参数)都算没给值：
                // 否则会报「无法识别的日志级别：--no-pause」这种莫名其妙的错
                if text.is_empty() || text.starts_with('-') {
                    logkit::error!("--log-level 缺少级别(可选 trace/debug/info/warn/error/off)");
                    pause::exit_with(2);
                }
                match Level::parse(&text) {
                    Some(level) => args.log_level = Some(level),
                    None => {
                        // 这里用 logkit 的 error 宏：级别还没生效，但 Error 永远打得出来
                        logkit::error!("无法识别的日志级别：{text}(可选 trace/debug/info/warn/error/off)");
                        pause::exit_with(2);
                    }
                }
                index += 2;
            }
            "--no-elevate" => {
                args.no_elevate = true;
                index += 1;
            }
            "--push-port" => {
                args.push_port = value(index).parse().unwrap_or(8787);
                index += 2;
            }
            "--pause" => {
                // 带值 `--pause 5`：最多停 5 秒；不带值 `--pause`：停默认 10 秒。
                // 下一个参数不是数字就当作不带值(比如 `--pause --verbose`)。
                let seconds = argv.get(index + 1).and_then(|s| s.parse::<u64>().ok());
                let pause = match seconds {
                    Some(0) => Pause::UntilKey,
                    Some(n) => Pause::Secs(n),
                    None => Pause::Secs(pause::DEFAULT_PAUSE_SECS),
                };
                // 立刻生效：这样后面即使紧跟一个非法参数、走到 exit 也照样留窗
                pause::set(pause);
                index += if seconds.is_some() { 2 } else { 1 };
            }
            "--no-pause" => {
                pause::set(Pause::Off);
                index += 1;
            }
            "--help" | "-h" => {
                print!("{HELP}");
                pause::exit_with(0);
            }
            other => {
                logkit::error!("未知参数：{other}(使用 --help 查看用法)");
                pause::exit_with(2);
            }
        }
    }
    args
}

// ---------------------------------------------------------------- 主机白名单
/// 默认只认这些域名：完美世界竞技平台(wmpvp)+ Steam 系
///
/// `wmpvp.com` / `pwesports.cn` 是**实测的命中域名**(见仓库根 README 的抓取记录)：
/// `pwaweblogin.wmpvp.com` 下发/携带 `steam_cn_token`、`appactivity.wmpvp.com` 是对战接口、
/// `gwapi.pwesports.cn` 把 token 挂在 URL 上。少了它们就是完全抓不到
///
/// 另外这道闸也是防误收的：`assets.msn.cn`(Windows 小组件资讯流)会送来一个叫
/// `userauthtoken` 的 JWT，落盘后还会被别名逻辑写成 `access_token`，下游拿着它请求必然失败
pub fn default_hosts() -> Vec<String> {
    [
        "wmpvp.com",
        "pwesports.cn",
        "wanmei.com",
        "wanmei.com.cn",
        "wanmei.net",
        "perfectworld.com",
        "perfectworld.com.cn",
        "perfectworld.net",
        "pwrd.com",
        "steampowered.com",
        "steamcommunity.com",
        "steamgames.com",
        "steamstatic.com",
        "steamchina.com",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

pub fn normalize_hosts(list: &str) -> Vec<String> {
    list.split(',')
        .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}