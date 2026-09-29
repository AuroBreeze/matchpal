//! matchpal 桌面端壳。
//!
//! 解耦约定（与仓库 README 一致）：
//! - 只链接 `match_watcher` 的 lib（解析 / 渲染 / 事件会话）；
//! - `fetch_token` 永远不进本进程 —— 抓 token 需要独立 UAC 提权，
//!   后续做成「拉起外部进程 + 展示其日志」的向导；
//! - UI 是无构建依赖的静态页面（`ui/`），通过 Tauri 事件流消费
//!   `WatcherEvent`，和 CLI 消费的是同一份数据。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, State};

use match_watcher::config::{self, Credentials};
use match_watcher::export::Exporters;
use match_watcher::model::{MatchInfo, StatsReport};
use match_watcher::session::{self, SessionOptions, WatcherEvent};

/// 悬浮窗的三种形态：胶囊条（等待）/ 小圆钮（收缩）/ 数据面板（出表）
const FLOATING_PILL: (f64, f64) = (272.0, 74.0);
const FLOATING_DOT: (f64, f64) = (56.0, 56.0);
const FLOATING_PANEL: (f64, f64) = (322.0, 402.0);

/// 共享给监听线程的运行状态
#[derive(Default)]
struct AppState {
    /// 当前会话的停止信号；None = 没有会话在跑
    stop: Option<Arc<AtomicBool>>,
    /// 最近一次事件：悬浮窗中途打开时补发，让它立刻跟上进度
    last_event: Option<WatcherEvent>,
}

/// 前端可见的 token 状态
#[derive(Serialize)]
struct TokenStatus {
    config_found: bool,
    has_token: bool,
    has_steamid: bool,
    masked_token: String,
    steamid: String,
    config_path: String,
    error: Option<String>,
}

#[derive(Deserialize)]
struct StartWatchArgs {
    /// 名单满几人出表，0 = 不等满
    #[serde(default = "default_full")]
    full: usize,
    /// 出表后继续监听
    #[serde(default)]
    keep_going: bool,
    /// 匹配成立 / 出表时自动弹出悬浮窗（游戏里看，不切窗口）
    #[serde(default = "default_true")]
    auto_floating: bool,
}

fn default_full() -> usize {
    10
}

fn default_true() -> bool {
    true
}

/// 配置文件定位：先看工作目录，再看 exe 所在目录（双击启动时的工作目录不可控）
fn config_path() -> PathBuf {
    let local = PathBuf::from("config.local.json");
    if local.exists() {
        return local;
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let beside = dir.join("config.local.json");
        if beside.exists() {
            return beside;
        }
    }
    local
}

#[tauri::command]
fn token_status() -> TokenStatus {
    let path = config_path();
    let path_display = path.display().to_string();
    match config::load(&path) {
        Ok(c) => TokenStatus {
            config_found: true,
            has_token: c.has_token(),
            has_steamid: c.has_steamid(),
            masked_token: c.masked_token(),
            steamid: c.steamid.clone(),
            config_path: path_display,
            error: None,
        },
        Err(err) => TokenStatus {
            config_found: false,
            has_token: false,
            has_steamid: false,
            masked_token: String::new(),
            steamid: String::new(),
            config_path: path_display,
            error: Some(err.to_string()),
        },
    }
}

/// 启动一轮监听。同一时间只允许一个会话；结果通过 `watcher` 事件流推给前端。
#[tauri::command]
fn start_watch(
    app: AppHandle,
    state: State<'_, Arc<Mutex<AppState>>>,
    args: StartWatchArgs,
) -> Result<(), String> {
    {
        let st = state.lock().map_err(|_| "状态锁已损坏")?;
        if st.stop.is_some() {
            return Err("已有监听会话在运行".into());
        }
    }

    let credentials: Credentials = config::load(&config_path()).map_err(|err| err.to_string())?;
    if !credentials.has_token() {
        return Err("缺少 token：请先运行 fetch_token 获取".into());
    }
    if !credentials.has_steamid() {
        return Err("缺少 steamid：配置文件中需要 17 位 SteamID64".into());
    }

    let stop = Arc::new(AtomicBool::new(false));
    state.lock().map_err(|_| "状态锁已损坏")?.stop = Some(stop.clone());

    let shared = state.inner().clone();
    let auto_floating = args.auto_floating;
    std::thread::spawn(move || {
        let opts = SessionOptions {
            full: args.full,
            once: false,
            keep_going: args.keep_going,
            timeout: 0.0,
            resubscribe: 15.0,
            retries: 5,
            exporters: Exporters::new(),
            stop: Some(stop),
        };
        let _ = session::run_session(&credentials, 2, opts, &mut |event| {
            // 匹配成立 / 出表的瞬间自动亮出悬浮窗——这是它存在的意义：
            // 主窗口收起来，游戏里直接看
            if auto_floating
                && matches!(event, WatcherEvent::Connected { .. } | WatcherEvent::Report { .. })
                && let Some(win) = app.get_webview_window("floating")
            {
                let _ = win.show();
            }
            if let Ok(mut st) = shared.lock() {
                st.last_event = Some(event.clone());
            }
            let _ = app.emit("watcher", &event);
        });
        if let Ok(mut st) = shared.lock() {
            st.stop = None;
            st.last_event = None;
        }
    });
    Ok(())
}

#[tauri::command]
fn stop_watch(state: State<'_, Arc<Mutex<AppState>>>) -> Result<(), String> {
    let st = state.lock().map_err(|_| "状态锁已损坏")?;
    match st.stop.as_ref() {
        Some(stop) => {
            stop.store(true, Ordering::Relaxed);
            Ok(())
        }
        None => Err("当前没有监听会话".into()),
    }
}

/// 显示 / 隐藏悬浮窗。显示时补发最近一次事件，让小窗立刻跟上进度。
#[tauri::command]
fn set_floating_visible(app: AppHandle, visible: bool) -> Result<(), String> {
    let win = app
        .get_webview_window("floating")
        .ok_or_else(|| "悬浮窗不存在".to_string())?;
    if visible {
        win.show().map_err(|err| err.to_string())?;
        win.set_focus().map_err(|err| err.to_string())?;
    } else {
        win.hide().map_err(|err| err.to_string())?;
    }
    Ok(())
}

/// 悬浮窗在胶囊条 / 小圆钮 / 数据面板之间切换：改窗口尺寸，并通知页面换视图。
#[tauri::command]
fn float_set_view(app: AppHandle, view: String) -> Result<(), String> {
    let win = app
        .get_webview_window("floating")
        .ok_or_else(|| "悬浮窗不存在".to_string())?;
    let (w, h) = match view.as_str() {
        "dot" => FLOATING_DOT,
        "panel" => FLOATING_PANEL,
        _ => FLOATING_PILL,
    };
    win.set_size(LogicalSize::new(w, h)).map_err(|err| err.to_string())?;
    app.emit_to("floating", "floating-view", view)
        .map_err(|err| err.to_string())?;
    Ok(())
}

/// 点击悬浮窗回到主窗口
#[tauri::command]
fn open_main(app: AppHandle) -> Result<(), String> {
    let win = app
        .get_webview_window("main")
        .ok_or_else(|| "主窗口不存在".to_string())?;
    win.show().map_err(|err| err.to_string())?;
    win.unminimize().map_err(|err| err.to_string())?;
    win.set_focus().map_err(|err| err.to_string())?;
    Ok(())
}

/// 一键演示：把仓库自带的完整测试样本（10 人战绩响应）灌进与生产
/// 完全相同的渲染链路（emit_final → Report 事件），主窗口与悬浮窗
/// 同时收到一份真实形状的对局数据，便于离线检查 UI 与链路。
#[tauri::command]
fn run_demo(app: AppHandle) -> Result<(), String> {
    const SAMPLE: &str = include_str!("../../match_watcher/src/testdata/stats_response.json");
    let value: serde_json::Value =
        serde_json::from_str(SAMPLE).map_err(|err| format!("样本不是合法 JSON：{err}"))?;
    let report = StatsReport::from_value(&value);
    if report.is_empty() {
        return Err("样本解析结果为空".into());
    }
    let info = MatchInfo::new(serde_json::json!({ "map": report.map() }));
    let mut exporters = Exporters::new();
    session::emit_final(&info, &report, report.len(), report.len(), &mut exporters, &mut |event| {
        let _ = app.emit("watcher", &event);
    });
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .manage(Arc::new(Mutex::new(AppState::default())))
        .invoke_handler(tauri::generate_handler![
            token_status,
            start_watch,
            stop_watch,
            run_demo,
            set_floating_visible,
            float_set_view,
            open_main
        ])
        .run(tauri::generate_context!())
        .expect("matchpal 桌面端启动失败");
}
