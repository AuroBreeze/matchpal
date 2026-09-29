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
use tauri::{AppHandle, Emitter, State};

use match_watcher::config::{self, Credentials};
use match_watcher::export::Exporters;
use match_watcher::session::{self, SessionOptions};

/// 共享给监听线程的运行状态
#[derive(Default)]
struct AppState {
    /// 当前会话的停止信号；None = 没有会话在跑
    stop: Option<Arc<AtomicBool>>,
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
}

fn default_full() -> usize {
    10
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
            let _ = app.emit("watcher", &event);
        });
        if let Ok(mut st) = shared.lock() {
            st.stop = None;
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

fn main() {
    tauri::Builder::default()
        .manage(Arc::new(Mutex::new(AppState::default())))
        .invoke_handler(tauri::generate_handler![token_status, start_watch, stop_watch])
        .run(tauri::generate_context!())
        .expect("matchpal 桌面端启动失败");
}
