//! 监听完美世界电竞当前对局：人满（默认 10 人）就出一张按昵称的战绩表，然后停止
//!
//! ```text
//! match_watcher                              # 等到 10 人齐 → 出表 → 退出
//! match_watcher --full 6                     # 6 人就出表
//! match_watcher --check                      # 只校验接口通不通（有效 0 / 无效 1）
//! match_watcher --once                       # 第一帧就出表（不等人齐，调试用）
//! match_watcher --keep-going                 # 持续监听（Python 版的模式）
//! match_watcher --replay capture/push.json       # 离线回放一帧推送
//! match_watcher --replay capture/stats.json      # 离线渲染一份战绩响应
//! ```
//!
//! 输出分工（有意分开的）：
//!
//! - **表格与快照走 stdout** —— 那是程序的产品，可能被重定向/管道消费
//! - **诊断信息走 logkit（stderr）** —— 时间戳、级别过滤，`--log-level off` 能全关掉
//!
//! 昵称只在**战绩接口**的返回里（推送帧没有昵称），所以最终那张表以战绩接口为主
//! 数据源，推送帧只提供 matchId / 地图 / 比分 / 名单人数。
//!
//! 监听主循环在 `match_watcher::session`（lib）里，以 [`WatcherEvent`] 事件流出结果；
//! 这个 bin 只做三件事：解析参数、离线回放、把事件映射成日志/打印。

mod args;

use std::path::PathBuf;

use logkit::{debug, error, info, warn};
use serde_json::{json, Value};

use match_watcher::api::Api;
use match_watcher::config::{self, Credentials};
use match_watcher::export::{Exporters, JsonFile};
use match_watcher::model::{MatchInfo, StatsReport};
use match_watcher::session::{self, emit_final, fetch_report, SessionOptions, WatcherEvent};

use crate::args::{Args, EXIT_ARGS, EXIT_OK, EXIT_TOKEN_INVALID};

fn main() {
    logkit::set_show_target(false);
    let args = args::parse_args();
    logkit::init_from_env("MATCH_WATCHER_LOG");
    if let Some(level) = args.log_level {
        logkit::set_level(level);
    }

    let code = run(&args);
    std::process::exit(code);
}

fn run(args: &Args) -> i32 {
    let credentials = match config::load(&args.config) {
        Ok(credentials) => credentials,
        Err(err) => {
            error!("{err}");
            return EXIT_ARGS;
        }
    };
    let credentials = Credentials {
        token: if args.token.is_empty() { credentials.token } else { args.token.clone() },
        steamid: if args.steamid.is_empty() { credentials.steamid } else { args.steamid.clone() },
    };

    if let Some(path) = &args.replay {
        return replay(args, path, &credentials);
    }

    if !credentials.has_token() {
        error!(
            "缺少 token：请先运行 fetch_token 获取，或通过 --token 指定（读取 {}）",
            args.config.display()
        );
        return EXIT_ARGS;
    }

    if args.check {
        return check(args, &credentials);
    }

    if !credentials.has_steamid() {
        error!("缺少 steamid：配置文件中缺少 17 位 SteamID64，请通过 --steamid 指定");
        return EXIT_ARGS;
    }

    let exporters = match build_exporters(args) {
        Ok(exporters) => exporters,
        Err(err) => {
            error!("{err}");
            return EXIT_ARGS;
        }
    };

    watch(args, &credentials, exporters)
}

/// `--check` 只说明"接口通不通"。
///
/// **注意：这个接口不校验 token** —— 实测拿垃圾 token、甚至 `steamid=123`
/// 也照样返回 `code=1` 和一个合法 ws 地址；WS 握手同样放行。所以这里返回 0
/// 不代表 token 有效。详见 README/提交说明。
fn check(args: &Args, credentials: &Credentials) -> i32 {
    let api = Api::new(credentials.token.clone(), credentials.steamid.clone());
    match api.websocket_url(args.platform) {
        Ok(url) => {
            info!(
                "接口连通（{}），ws={url}",
                credentials.masked_token()
            );
            warn!("注意：getWebsocketInfo 不校验 token，此结果仅表明网络与接口可用");
            EXIT_OK
        }
        Err(err) => {
            error!("获取 websocketUrl 失败：{err}");
            EXIT_TOKEN_INVALID
        }
    }
}

/// 把 lib 发出的事件映射成 CLI 输出：日志级别与文案保持与事件化之前一致，
/// 表格原样进 stdout。
fn handle_event(event: WatcherEvent) {
    match event {
        WatcherEvent::Account { steamid, masked_token, token_len } => {
            info!("账号 SteamID64: {}    token: {}（{} 字符）", steamid, masked_token, token_len);
        }
        WatcherEvent::Connected { full } => {
            info!("WebSocket 已连接并完成订阅，等待名单达到 {full} 人");
        }
        WatcherEvent::Progress { loaded, full } => {
            info!("名单 {loaded}/{full}，继续等待");
        }
        WatcherEvent::Resubscribed { waited_secs, interval_secs } => {
            info!("已等待 {:.0} 秒未收到对局数据，重新订阅（之后每 {} 秒一次）", waited_secs, interval_secs);
        }
        WatcherEvent::Notice { level, message } => match level {
            "error" => error!("{message}"),
            "warn" => warn!("{message}"),
            "debug" => debug!("{message}"),
            _ => info!("{message}"),
        },
        WatcherEvent::Report { text, .. } => print!("{text}"),
        WatcherEvent::Finished { .. } => {}
    }
}

/// CLI 侧的监听入口：参数搬进 SessionOptions，交给 lib 的事件会话
fn watch(args: &Args, credentials: &Credentials, exporters: Exporters) -> i32 {
    session::run_session(
        credentials,
        args.platform,
        SessionOptions {
            full: args.full,
            once: args.once,
            keep_going: args.keep_going,
            timeout: args.timeout,
            resubscribe: args.resubscribe,
            retries: args.retries,
            exporters,
            stop: None,
        },
        &mut handle_event,
    )
}

/// 离线回放。自动认出两种文件：
///
/// - **推送帧**（有 `playerList`）：走完整的"解析名单 → 查战绩 → 出表"
/// - **战绩响应**（有 `result.ctPlayerStatsDTOList`）：直接出表，不联网
fn replay(args: &Args, path: &PathBuf, credentials: &Credentials) -> i32 {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            error!("无法读取回放文件 {}：{err}", path.display());
            return EXIT_ARGS;
        }
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            error!("回放文件不是合法 JSON：{err}");
            return EXIT_ARGS;
        }
    };

    let mut exporters = match build_exporters(args) {
        Ok(exporters) => exporters,
        Err(err) => {
            error!("{err}");
            return EXIT_ARGS;
        }
    };

    // ---- 战绩响应：不联网，直接出表
    if StatsReport::looks_like_response(&value) {
        let report = StatsReport::from_value(&value);
        let loaded = report.len();
        info!("回放战绩响应：{} 人", loaded);
        let info = MatchInfo::new(json!({ "map": report.map() }));
        emit_final(&info, &report, loaded, loaded, &mut exporters, &mut handle_event);
        return EXIT_OK;
    }

    // ---- 推送帧：解析名单（可选联网查战绩）
    let data = value
        .get("messageData")
        .or_else(|| value.get("match"))
        .unwrap_or(&value)
        .clone();
    let info = MatchInfo::new(data);
    let loaded = info.players().len();
    if loaded == 0 {
        error!("回放文件中既无 playerList，也无法识别为战绩响应");
        return EXIT_ARGS;
    }
    let report = if let Some(stats) = value.get("stats")
        && StatsReport::flat_looks_usable(stats)
    {
        // 快照文件（push + stats 都在里面）：完全离线，不用联网
        info!("使用快照内包含的战绩数据（离线）");
        StatsReport::from_flat(stats, &info)
    } else if credentials.has_token() {
        let api = Api::new(credentials.token.clone(), credentials.steamid.clone());
        fetch_report(&api, &info, &mut handle_event)
    } else {
        warn!("缺少 token，无法生成昵称表（昵称仅存在于战绩接口返回中）");
        StatsReport::default()
    };
    emit_final(&info, &report, loaded, args.full, &mut exporters, &mut handle_event);
    EXIT_OK
}

/// 组装导出目标：`--json-out`（兼容 Python 的默认行为）+ 若干 `--export kind:path`
fn build_exporters(args: &Args) -> Result<Exporters, String> {
    let mut exporters = Exporters::new();
    if let Some(path) = &args.json_out {
        exporters.add(Box::new(JsonFile::new(path)));
    }
    for spec in &args.exports {
        let exporter = Exporters::parse_spec(spec).map_err(|err| err.to_string())?;
        exporters.add(exporter);
    }
    Ok(exporters)
}
