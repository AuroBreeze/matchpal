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

mod args;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use logkit::{debug, error, info, warn};
use serde_json::{json, Value};

use match_watcher::api::Api;
use match_watcher::config::{self, Credentials};
use match_watcher::export::{Exporters, JsonFile, MatchSnapshot};
use match_watcher::model::{classify, Frame, MatchInfo, StatsMap, StatsReport};
use match_watcher::render::{render_match, render_report, render_unknown_sides};
use match_watcher::ws::{Session, PING_INTERVAL};

use crate::args::{Args, EXIT_ARGS, EXIT_NO_PUSH, EXIT_OK, EXIT_TOKEN_INVALID};

/// 什么时候停下来出最终那张表
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// 名单人数够了（默认行为）
    WhenFull,
    /// 收到第一帧就出（`--once`）
    AfterFirst,
    /// 不出最终表，一直监听（`--keep-going`）
    Never,
}

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

    info!(
        "账号 SteamID64: {}    token: {}（{} 字符）",
        credentials.steamid,
        credentials.masked_token(),
        credentials.token.len()
    );

    let api = Api::new(credentials.token.clone(), credentials.steamid.clone());
    let ws_url = match api.websocket_url(args.platform) {
        Ok(url) => url,
        Err(err) => {
            error!("获取 websocketUrl 失败：{err}");
            return EXIT_TOKEN_INVALID;
        }
    };
    info!("WebSocket: {ws_url}");

    let mut exporters = match build_exporters(args) {
        Ok(exporters) => exporters,
        Err(err) => {
            error!("{err}");
            return EXIT_ARGS;
        }
    };
    if !exporters.is_empty() {
        info!("导出目标：{}", exporters.kinds().join(", "));
    }

    watch(args, &credentials, &api, &ws_url, &mut exporters)
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

/// 主循环：连接 → 订阅 → 等名单满 → 出战绩表 → 停止
fn watch(
    args: &Args,
    credentials: &Credentials,
    api: &Api,
    ws_url: &str,
    exporters: &mut Exporters,
) -> i32 {
    let stop = if args.keep_going {
        Stop::Never
    } else if args.once {
        Stop::AfterFirst
    } else {
        Stop::WhenFull
    };

    let started = Instant::now();
    let mut last_ping = Instant::now();
    let mut last_subscribe: Option<Instant> = None;
    let mut attempts: u32 = 0;
    let mut session: Option<Session> = None;
    let mut got_push = false;
    // 最后见到的那一帧，超时退出时拿它兜底出表
    let mut last_info: Option<MatchInfo> = None;

    loop {
        if args.timeout > 0.0 && started.elapsed().as_secs_f64() > args.timeout {
            info!("已达到超时时间 {}s，即将退出", args.timeout);
            break;
        }

        // ---- 连接（含断线重连）
        if session.is_none() {
            match Session::connect(ws_url, &credentials.token) {
                Ok(mut ws) => {
                    // 顺序照抄 Python：先 ping，再订阅
                    if let Err(err) = ws.ping() {
                        debug!("初始心跳发送失败：{err}");
                    }
                    if let Err(err) = ws.subscribe(&credentials.steamid) {
                        warn!("订阅失败：{err}");
                    }
                    last_ping = Instant::now();
                    last_subscribe = Some(Instant::now());
                    attempts = 0;
                    session = Some(ws);
                    info!("WebSocket 已连接并完成订阅，等待名单达到 {} 人", args.full);
                }
                Err(err) => {
                    attempts += 1;
                    if attempts > args.retries {
                        error!("连接失败 {attempts} 次，停止重试：{err}");
                        break;
                    }
                    warn!("连接失败（{err}），3 秒后重试");
                    std::thread::sleep(Duration::from_secs(3));
                    continue;
                }
            }
        }

        // ---- 心跳 & 还没收到推送就定期重新订阅
        {
            let ws = session.as_mut().expect("上面刚确保是 Some");
            if last_ping.elapsed() >= PING_INTERVAL {
                if let Err(err) = ws.ping() {
                    debug!("心跳发送失败：{err}");
                }
                last_ping = Instant::now();
            }
            let need_resubscribe = args.resubscribe > 0.0
                && !got_push
                && last_subscribe.is_none_or(|at| at.elapsed().as_secs_f64() > args.resubscribe);
            if need_resubscribe {
                if let Err(err) = ws.subscribe(&credentials.steamid) {
                    debug!("重新订阅失败：{err}");
                } else {
                    info!("尚未收到对局数据，重新发送订阅");
                }
                last_subscribe = Some(Instant::now());
            }
        }

        // ---- 读一帧（读超时 = 这一轮没数据，不当作断开）
        let outcome = session.as_mut().expect("同上").read_frame();
        let text = match outcome {
            Ok(None) => continue,
            Ok(Some(text)) => text,
            Err(err) => {
                if let Some(mut ws) = session.take() {
                    ws.close();
                }
                attempts += 1;
                if attempts > args.retries {
                    error!("连接断开 {attempts} 次，停止重连：{err}");
                    break;
                }
                warn!("连接断开：{err}，3 秒后进行第 {attempts} 次重连");
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
        };

        let Frame::Match(info) = classify(&text) else {
            debug!("跳过非对局帧：{}", short(&text));
            continue;
        };
        got_push = true;
        let loaded = info.players().len();
        last_info = Some((*info).clone());

        // 名单还没攒够：报个进度继续等（这就是"人满才出表"的等待过程）
        let full = args.full == 0 || loaded >= args.full;
        if !full && stop == Stop::WhenFull {
            info!("名单 {loaded}/{}，继续等待", args.full);
            continue;
        }

        let stats = fetch_report(api, &info);
        emit_final(&info, &stats, loaded, args.full, exporters);

        if stop != Stop::Never {
            break;
        }
        // --keep-going：继续监听，下一帧重新出表
    }

    if !got_push {
        // 一帧都没收到：如果之前攒了半份名单，也出一张（人数不足会写明）
        if let Some(info) = last_info.take() {
            let loaded = info.players().len();
            warn!("未收到完整推送，按现有名单 {} 人生成表格", loaded);
            let stats = fetch_report(api, &info);
            emit_final(&info, &stats, loaded, args.full, exporters);
            return EXIT_OK;
        }
        error!(
            "未收到对局推送（messageType 10002）。可能原因：\n  \
             · 当前不在对局中 —— 该推送仅在比赛进行期间出现\n  \
             · token 已失效（请重新运行 fetch_token）\n  \
             · steamid 不正确（须为 17 位 SteamID64，且为本账号）"
        );
        return EXIT_NO_PUSH;
    }
    EXIT_OK
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
        emit_final(&info, &report, loaded, loaded, &mut exporters);
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
        fetch_report(&api, &info)
    } else {
        warn!("缺少 token，无法生成昵称表（昵称仅存在于战绩接口返回中）");
        StatsReport::default()
    };
    emit_final(&info, &report, loaded, args.full, &mut exporters);
    EXIT_OK
}

/// 查战绩。失败只警告不中断 —— 名单本身还是有价值的。
fn fetch_report(api: &Api, info: &MatchInfo) -> StatsReport {
    let teams = info.teams();
    let ct: Vec<String> = teams.ct.iter().map(|player| player.steamid.clone()).collect();
    let t: Vec<String> = teams.t.iter().map(|player| player.steamid.clone()).collect();
    if ct.is_empty() && t.is_empty() {
        warn!("名单为空，跳过战绩查询");
        return StatsReport::default();
    }
    match api.team_stats(&ct, &t, info.map().unwrap_or_default().as_str()) {
        Ok(report) => {
            info!("战绩接口返回 {} 人的数据（CT {} / T {}）", report.len(), report.ct.len(), report.t.len());
            report
        }
        Err(err) => {
            warn!("战绩接口调用失败（名单信息仍可查看）：{err}");
            StatsReport::default()
        }
    }
}

/// 出最终那张表：有昵称就用战绩表，没有就退回按 SteamID 的实时表
fn emit_final(
    info: &MatchInfo,
    report: &StatsReport,
    loaded: usize,
    expected: usize,
    exporters: &mut Exporters,
) {
    let teams = info.teams();
    if let Some(warning) = render_unknown_sides(&teams) {
        warn!("{warning}");
    }

    // 表格是"产品"，走 stdout；日志走 stderr，两者不混
    if report.is_empty() {
        warn!("无战绩数据，回退为按 SteamID 显示的实时表格（昵称依赖战绩接口）");
        print!("{}", render_match(info, &StatsMap::new(), false));
    } else {
        // 人数以推送帧为准（战绩接口只回它认识的），所以 two 个数字都传进去
        let shown = if loaded == 0 { report.len() } else { loaded };
        print!("{}", render_report(Some(info), report, shown, expected));
    }

    let snapshot = MatchSnapshot::now(info.clone(), report.raw.clone());
    for (kind, err) in exporters.export_all(&snapshot) {
        error!("导出（{kind}）失败：{err}");
    }
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

/// 日志里截断长帧，别把整帧糊进去
fn short(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= 120 {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(120).collect();
    format!("{head}…")
}
