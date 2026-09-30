//! 事件驱动的监听会话：CLI 与桌面端共用的编排层
//!
//! 监听主循环原来写在 bin 里，桌面端来了之后有两个消费者：
//! CLI 要"按老样子打日志"，GUI 要"拿到结构化的进度和表格"。
//! 所以把循环搬进 lib，对外只发 [`WatcherEvent`]：
//!
//! - 结构化事件([`WatcherEvent::Account`] / [`Connected`] / [`Progress`] /
//!   [`Resubscribed`] / [`Report`])给 GUI 渲染界面用；
//! - 其余瞬时消息统一走 [`WatcherEvent::Notice`](级别 + 现成文案)，
//!   CLI 原样输出，GUI 挑着显示；
//! - 循环内部**不做任何输出**，要不要打印、怎么打印全由消费者决定。
//!
//! [`Connected`]: WatcherEvent::Connected

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::api::Api;
use crate::config::Credentials;
use crate::export::{Exporters, MatchSnapshot};
use crate::model::{classify, Frame, MatchInfo, StatsMap, StatsReport};
use crate::render::{render_match, render_report, render_unknown_sides};
use crate::ws::{Session, PING_INTERVAL};

/// 正常结束
pub const EXIT_OK: i32 = 0;
/// token 无效(调 getWebsocketInfo 失败)
pub const EXIT_TOKEN_INVALID: i32 = 1;
/// 跑完了但没收到对局推送。Python 版同样返回 1，这里分开命名只为可读
pub const EXIT_NO_PUSH: i32 = 1;
/// 配置/参数不对
pub const EXIT_ARGS: i32 = 2;

/// 会话过程中发生的事。除 [`WatcherEvent::Notice`] 外都是结构化的，
/// 消费者拿到就能用，不必从文案里抠数字。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum WatcherEvent {
    /// 监听开始时的账号信息
    Account {
        steamid: String,
        masked_token: String,
        token_len: usize,
    },
    /// WebSocket 已连接并完成订阅
    Connected {
        full: usize,
    },
    /// 名单进度(名单没攒够时每帧发一次)
    Progress {
        loaded: usize,
        full: usize,
    },
    /// 尚未收到对局数据，重新订阅了一次
    Resubscribed {
        waited_secs: f64,
        interval_secs: f64,
    },
    /// 瞬时消息：连接重试、心跳失败、超时、导出失败等。
    /// `level` 是 logkit 级别名(info/warn/error/debug)，
    /// `message` 是最终文案，消费者可以直接展示。
    Notice {
        level: &'static str,
        message: String,
    },
    /// 一张渲染完成的表格(人满出表 / 超时兜底 / 回放)。
    /// `text` 是与 CLI 输出一致的成品，`data` 是结构化数据，消费者二选一。
    Report {
        text: String,
        data: GuiReport,
    },
    /// 原始 JSON 数据，未经任何加工：
    /// - `source = "push_frame"`：对局推送帧(messageType 10002 的完整信封)
    /// - `source = "stats_response"`：战绩接口的原始响应
    /// 每收到一份就推一次，与 [`WatcherEvent::Report`] 的加工数据并存。
    Raw {
        source: &'static str,
        payload: serde_json::Value,
    },
    /// 会话结束，`code` 与 CLI 退出码含义一致
    Finished {
        code: i32,
    },
}

/// 一名玩家的可展示数据。字段随来源可缺省：战绩接口给全量，
/// 推送帧兜底时只有 steamid 和实时 K/D / ADR。
#[derive(Debug, Clone, Serialize)]
pub struct GuiRow {
    pub side: String,
    pub steamid: String,
    pub nickname: Option<String>,
    pub rating_pro: Option<f64>,
    pub kd: Option<f64>,
    pub adr: Option<f64>,
    pub we: Option<f64>,
    pub map_win_rate: Option<f64>,
    pub head_shot_rate: Option<f64>,
    pub snipe_rate: Option<f64>,
    pub flash_success_rate: Option<f64>,
    pub pvp_score: Option<f64>,
}

/// [`WatcherEvent::Report`] 的结构化载荷：GUI 据此画真正的表格，
/// 不必去解析渲染好的 ASCII 文本。
#[derive(Debug, Clone, Serialize, Default)]
pub struct GuiReport {
    pub map: Option<String>,
    pub ct: Vec<GuiRow>,
    pub t: Vec<GuiRow>,
    /// 阵营识别不出的玩家数(未计入 ct / t)
    pub unknown: usize,
}

/// 战绩接口行 → 展示行
fn gui_row_from_stat(stat: &crate::model::PlayerStat, side: &str) -> GuiRow {
    GuiRow {
        side: side.to_string(),
        steamid: stat.steamid.clone(),
        nickname: (!stat.nickname.is_empty()).then(|| stat.nickname.clone()),
        rating_pro: stat.rating_pro,
        kd: stat.kd,
        adr: stat.adr,
        we: stat.we,
        map_win_rate: stat.map_win_rate,
        head_shot_rate: stat.head_shot_rate,
        snipe_rate: stat.snipe_rate,
        flash_success_rate: stat.flash_success_rate,
        pvp_score: stat.pvp_score,
    }
}

/// 推送帧兜底：没有战绩数据时用实时名单凑展示行
fn gui_rows_from_info(info: &MatchInfo) -> (Vec<GuiRow>, Vec<GuiRow>) {
    let teams = info.teams();
    let convert = |players: &[crate::model::Player], side: &str| {
        players
            .iter()
            .map(|player| {
                let kd = match (player.kill, player.death) {
                    (Some(k), Some(d)) if d > 0.0 => Some(k / d),
                    _ => None,
                };
                GuiRow {
                    side: side.to_string(),
                    steamid: player.steamid.clone(),
                    nickname: None,
                    rating_pro: None,
                    kd,
                    adr: player.adr,
                    we: None,
                    map_win_rate: None,
                    head_shot_rate: None,
                    snipe_rate: None,
                    flash_success_rate: None,
                    pvp_score: None,
                }
            })
            .collect()
    };
    (convert(&teams.ct, "CT"), convert(&teams.t, "T"))
}

/// 监听会话的运行参数。字段与 CLI 一一对应，GUI 按需填。
pub struct SessionOptions {
    /// 名单人数达到这个数就出表并停止(默认 10)；0 = 不等满
    pub full: usize,
    /// 收到第一帧就出表(`--once`)
    pub once: bool,
    /// 出表后不退出，继续监听(`--keep-going`)
    pub keep_going: bool,
    /// 最长运行时间(秒)，0 为不限
    pub timeout: f64,
    /// 还没收到对局数据时，每隔这么多秒重新订阅一次，0 为关闭
    pub resubscribe: f64,
    /// 断线/连接失败的重试次数
    pub retries: u32,
    /// 快照导出目标；空 = 不导出
    pub exporters: Exporters,
    /// 外部停止信号。GUI 传一个 `AtomicBool`，置 true 后会话在下一圈退出；CLI 传 `None`
    pub stop: Option<Arc<AtomicBool>>,
    /// WS 推送后端。Some 时每个事件序列化成 JSON 广播给已连接的客户端
    pub push: Option<crate::push::PushHub>,
}

/// 什么时候停下来出最终那张表
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// 名单人数够了(默认行为)
    WhenFull,
    /// 收到第一帧就出(`--once`)
    AfterFirst,
    /// 不出最终表，一直监听(`--keep-going`)
    Never,
}

/// 运行一轮监听会话：取 ws 地址 → 连接 → 订阅 → 等名单满 → 出表。
///
/// 阻塞直到会话结束，返回值与 CLI 退出码含义一致。
/// token / steamid 的存在性检查由调用方负责(报错文案随调用场景不同)。
pub fn run_session(
    credentials: &Credentials,
    platform: u32,
    mut opts: SessionOptions,
    on_event: &mut dyn FnMut(WatcherEvent),
) -> i32 {
    // WS 后端：每个事件先序列化广播给已连接的客户端，再交给本地消费者。
    // 客户端拿到的事件与 CLI 打印的是同一份，不另起一套协议。
    let push = opts.push.take();
    let mut emit = move |event: WatcherEvent| {
        if let Some(hub) = &push
            && let Ok(text) = serde_json::to_string(&event)
        {
            hub.broadcast(text);
        }
        on_event(event);
    };

    let api = Api::new(credentials.token.clone(), credentials.steamid.clone());
    emit(WatcherEvent::Account {
        steamid: credentials.steamid.clone(),
        masked_token: credentials.masked_token(),
        token_len: credentials.token.len(),
    });

    let ws_url = match api.websocket_url(platform) {
        Ok(url) => url,
        Err(err) => {
            emit(WatcherEvent::Notice {
                level: "error",
                message: format!("获取 websocketUrl 失败：{err}"),
            });
            return EXIT_TOKEN_INVALID;
        }
    };
    emit(WatcherEvent::Notice {
        level: "info",
        message: format!("WebSocket: {ws_url}"),
    });
    if !opts.exporters.is_empty() {
        emit(WatcherEvent::Notice {
            level: "info",
            message: format!("导出目标：{}", opts.exporters.kinds().join(", ")),
        });
    }

    watch_loop(credentials, &api, &ws_url, opts, &mut emit)
}

/// 主循环：连接 → 订阅 → 等名单满 → 出战绩表 → 停止
fn watch_loop(
    credentials: &Credentials,
    api: &Api,
    ws_url: &str,
    mut opts: SessionOptions,
    on_event: &mut dyn FnMut(WatcherEvent),
) -> i32 {
    let stop_at = if opts.keep_going {
        Stop::Never
    } else if opts.once {
        Stop::AfterFirst
    } else {
        Stop::WhenFull
    };
    let stop_requested = || opts.stop.as_ref().is_some_and(|f| f.load(Ordering::Relaxed));

    let started = Instant::now();
    let mut last_ping = Instant::now();
    let mut last_subscribe: Option<Instant> = None;
    let mut attempts: u32 = 0;
    let mut session: Option<Session> = None;
    let mut got_push = false;
    // 最后见到的那一帧，超时退出时拿它兜底出表
    let mut last_info: Option<MatchInfo> = None;

    let code = loop {
        if stop_requested() {
            on_event(WatcherEvent::Notice {
                level: "info",
                message: "收到停止请求，即将退出".into(),
            });
            break EXIT_OK;
        }
        if opts.timeout > 0.0 && started.elapsed().as_secs_f64() > opts.timeout {
            on_event(WatcherEvent::Notice {
                level: "info",
                message: format!("已达到超时时间 {}s，即将退出", opts.timeout),
            });
            break EXIT_OK;
        }

        // ---- 连接(含断线重连)
        if session.is_none() {
            match Session::connect(ws_url, &credentials.token) {
                Ok(mut ws) => {
                    // 顺序照抄 Python：先 ping，再订阅
                    if let Err(err) = ws.ping() {
                        on_event(WatcherEvent::Notice {
                            level: "debug",
                            message: format!("初始心跳发送失败：{err}"),
                        });
                    }
                    if let Err(err) = ws.subscribe(&credentials.steamid) {
                        on_event(WatcherEvent::Notice {
                            level: "warn",
                            message: format!("订阅失败：{err}"),
                        });
                    }
                    last_ping = Instant::now();
                    last_subscribe = Some(Instant::now());
                    attempts = 0;
                    session = Some(ws);
                    on_event(WatcherEvent::Connected { full: opts.full });
                }
                Err(err) => {
                    attempts += 1;
                    if attempts > opts.retries {
                        on_event(WatcherEvent::Notice {
                            level: "error",
                            message: format!("连接失败 {attempts} 次，停止重试：{err}"),
                        });
                        break EXIT_OK;
                    }
                    on_event(WatcherEvent::Notice {
                        level: "warn",
                        message: format!("连接失败({err})，3 秒后重试"),
                    });
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
                    on_event(WatcherEvent::Notice {
                        level: "debug",
                        message: format!("心跳发送失败：{err}"),
                    });
                }
                last_ping = Instant::now();
            }
            let need_resubscribe = opts.resubscribe > 0.0
                && !got_push
                && last_subscribe.is_none_or(|at| at.elapsed().as_secs_f64() > opts.resubscribe);
            if need_resubscribe {
                if let Err(err) = ws.subscribe(&credentials.steamid) {
                    on_event(WatcherEvent::Notice {
                        level: "debug",
                        message: format!("重新订阅失败：{err}"),
                    });
                } else {
                    on_event(WatcherEvent::Resubscribed {
                        waited_secs: last_subscribe.map(|at| at.elapsed().as_secs_f64()).unwrap_or(0.0),
                        interval_secs: opts.resubscribe,
                    });
                }
                last_subscribe = Some(Instant::now());
            }
        }

        // ---- 读一帧(读超时 = 这一轮没数据，不当作断开)
        let outcome = session.as_mut().expect("同上").read_frame();
        let text = match outcome {
            Ok(None) => continue,
            Ok(Some(text)) => text,
            Err(err) => {
                if let Some(mut ws) = session.take() {
                    ws.close();
                }
                attempts += 1;
                if attempts > opts.retries {
                    on_event(WatcherEvent::Notice {
                        level: "error",
                        message: format!("连接断开 {attempts} 次，停止重连：{err}"),
                    });
                    break EXIT_OK;
                }
                on_event(WatcherEvent::Notice {
                    level: "warn",
                    message: format!("连接断开：{err}，3 秒后进行第 {attempts} 次重连"),
                });
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
        };

        let Frame::Match(info) = classify(&text) else {
            on_event(WatcherEvent::Notice {
                level: "debug",
                message: format!("跳过非对局帧：{}", short(&text)),
            });
            continue;
        };
        got_push = true;
        let loaded = info.players().len();
        last_info = Some((*info).clone());

        // 原始推送帧：原样透传给 WS 客户端，不做过任何字段挑选
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&text) {
            on_event(WatcherEvent::Raw {
                source: "push_frame",
                payload,
            });
        }

        // 名单还没攒够：报个进度继续等(这就是"人满才出表"的等待过程)
        let full = opts.full == 0 || loaded >= opts.full;
        if !full && stop_at == Stop::WhenFull {
            on_event(WatcherEvent::Progress {
                loaded,
                full: opts.full,
            });
            continue;
        }

        let stats = fetch_report(api, &info, on_event);
        emit_final(&info, &stats, loaded, opts.full, &mut opts.exporters, on_event);

        if stop_at != Stop::Never {
            break EXIT_OK;
        }
        // --keep-going：继续监听，下一帧重新出表
    };

    if !got_push {
        // 一帧都没收到：如果之前攒了半份名单，也出一张(人数不足会写明)
        if let Some(info) = last_info.take() {
            let loaded = info.players().len();
            on_event(WatcherEvent::Notice {
                level: "warn",
                message: format!("未收到完整推送，按现有名单 {} 人生成表格", loaded),
            });
            let stats = fetch_report(api, &info, on_event);
            emit_final(&info, &stats, loaded, opts.full, &mut opts.exporters, on_event);
            on_event(WatcherEvent::Finished { code: EXIT_OK });
            return EXIT_OK;
        }
        on_event(WatcherEvent::Notice {
            level: "error",
            message: "未收到对局推送(messageType 10002)。可能原因：\n  \
                 · 当前不在对局中 —— 该推送仅在比赛进行期间出现\n  \
                 · token 已失效(请重新运行 fetch_token)\n  \
                 · steamid 不正确(须为 17 位 SteamID64，且为本账号)"
                .into(),
        });
        on_event(WatcherEvent::Finished {
            code: EXIT_NO_PUSH,
        });
        return EXIT_NO_PUSH;
    }
    on_event(WatcherEvent::Finished { code });
    code
}

/// 查战绩。失败只警告不中断 —— 名单本身还是有价值的。
///
/// 公开给回放路径复用：bin 的 `--replay` 在解析出名单后也走这里查战绩。
pub fn fetch_report(
    api: &Api,
    info: &MatchInfo,
    on_event: &mut dyn FnMut(WatcherEvent),
) -> StatsReport {
    let teams = info.teams();
    let ct: Vec<String> = teams.ct.iter().map(|player| player.steamid.clone()).collect();
    let t: Vec<String> = teams.t.iter().map(|player| player.steamid.clone()).collect();
    if ct.is_empty() && t.is_empty() {
        on_event(WatcherEvent::Notice {
            level: "warn",
            message: "名单为空，跳过战绩查询".into(),
        });
        return StatsReport::default();
    }
    match api.team_stats(&ct, &t, info.map().unwrap_or_default().as_str()) {
        Ok(report) => {
            on_event(WatcherEvent::Notice {
                level: "info",
                message: format!(
                    "战绩接口返回 {} 人的数据(CT {} / T {})",
                    report.len(),
                    report.ct.len(),
                    report.t.len()
                ),
            });
            report
        }
        Err(err) => {
            on_event(WatcherEvent::Notice {
                level: "warn",
                message: format!("战绩接口调用失败(名单信息仍可查看)：{err}"),
            });
            StatsReport::default()
        }
    }
}

/// 出最终那张表：有昵称就用战绩表，没有就退回按 SteamID 的实时表。
///
/// 公开给回放路径复用。
pub fn emit_final(
    info: &MatchInfo,
    report: &StatsReport,
    loaded: usize,
    expected: usize,
    exporters: &mut Exporters,
    on_event: &mut dyn FnMut(WatcherEvent),
) {
    let teams = info.teams();
    if let Some(warning) = render_unknown_sides(&teams) {
        on_event(WatcherEvent::Notice {
            level: "warn",
            message: warning,
        });
    }

    // 表格是"产品"，通过 Report 事件交给消费者；日志走 Notice，两者不混。
    // 同一份数据给两次：text 供 CLI 直接打印，data 供 GUI 画真正的表格。
    let (text, data) = if report.is_empty() {
        on_event(WatcherEvent::Notice {
            level: "warn",
            message: "无战绩数据，回退为按 SteamID 显示的实时表格(昵称依赖战绩接口)".into(),
        });
        let (ct, t) = gui_rows_from_info(info);
        (
            render_match(info, &StatsMap::new(), false),
            GuiReport {
                map: info.map(),
                ct,
                t,
                unknown: teams.unknown.len(),
            },
        )
    } else {
        // 人数以推送帧为准(战绩接口只回它认识的)，所以两个数字都传进去
        let shown = if loaded == 0 { report.len() } else { loaded };
        (
            render_report(Some(info), report, shown, expected),
            GuiReport {
                map: report.map().or_else(|| info.map()),
                ct: report.ct.iter().map(|s| gui_row_from_stat(s, "CT")).collect(),
                t: report.t.iter().map(|s| gui_row_from_stat(s, "T")).collect(),
                unknown: teams.unknown.len(),
            },
        )
    };
    on_event(WatcherEvent::Report { text, data });

    let snapshot = MatchSnapshot::now(info.clone(), report.raw.clone());
    // 战绩接口的原始响应同样原样透传(非完整响应来源时是 Null，跳过)
    if !report.raw_response.is_null() {
        on_event(WatcherEvent::Raw {
            source: "stats_response",
            payload: report.raw_response.clone(),
        });
    }
    for (kind, err) in exporters.export_all(&snapshot) {
        on_event(WatcherEvent::Notice {
            level: "error",
            message: format!("导出({kind})失败：{err}"),
        });
    }
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
