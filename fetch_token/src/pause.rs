//! 退出前把控制台窗口留住
//!
//! 双击 exe 时，控制台是系统**为这个进程临时创建**的 —— 进程一退，窗口立刻消失，
//! 用户根本看不到"命中 token / 写到哪个文件 / 为什么失败"。而从已有终端（cmd / pwsh /
//! cargo run）启动时，控制台是复用的，窗口不会消失，这时平白多等 10 秒反而碍事
//!
//! 所以默认值按"窗口会不会消失"自动选：
//!   - 独立控制台（双击 exe、ShellExecuteW runas 提权后新开的窗口）→ 停 10 秒，按键可提前关
//!   - 复用终端（cmd / pwsh / cargo run）→ 不停，保持脚本友好
//! 再用 `--pause [秒]` / `--no-pause` 显式覆盖

use std::sync::OnceLock;
use std::time::Duration;

/// 独立控制台下的默认停留秒数
pub const DEFAULT_PAUSE_SECS: u64 = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pause {
    /// 不停留，直接退出
    Off,
    /// 一直等到按键（`--pause 0`）
    UntilKey,
    /// 最多等 n 秒，按键可提前结束
    Secs(u64),
}

static SETTING: OnceLock<Pause> = OnceLock::new();

/// 记录设置。OnceLock 只认第一次，所以 `--pause` 在参数解析阶段就能立刻生效 ——
/// 即使后面紧跟一个非法参数导致报错退出，窗口也一样留得住
pub fn set(pause: Pause) {
    let _ = SETTING.set(pause);
}

/// 显式设置优先（`--pause` / `--no-pause`）；没设置过就按"窗口会不会消失"自动判断，
/// 这样连 `--help`、非法参数这些解析期的早退路径也能对
pub fn setting() -> Pause {
    match SETTING.get() {
        Some(pause) => *pause,
        None => default_setting(),
    }
}

/// 用户没显式指定时的默认值：只有控制台会随进程消失才停留
pub fn default_setting() -> Pause {
    if crate::platform::console_would_vanish() {
        Pause::Secs(DEFAULT_PAUSE_SECS)
    } else {
        Pause::Off
    }
}

/// **所有**退出路径都走这里：先把窗口留住，再退出。
/// 直接调 `std::process::exit` 会绕过停留，双击场景就白做了
pub fn exit_with(code: i32) -> ! {
    wait();
    std::process::exit(code)
}

fn wait() {
    // 这里刻意不走 logkit：倒计时用 `\r` 原地刷新同一行，套上时间戳和级别前缀
    // 会变成每秒刷出一行，反而看不清。这是交互式 UI，不是日志。
    match setting() {
        Pause::Off => {}
        Pause::UntilKey => {
            println!("\n按任意键关闭窗口…");
            crate::platform::wait_any_key();
        }
        Pause::Secs(total) => countdown(total),
    }
}

/// 每秒刷一次倒计时；另起线程阻塞等键，谁先到听谁的。
/// 不用 `WaitForSingleObject` 等控制台句柄是因为窗口大小变化、鼠标移动也会送来
/// INPUT_RECORD，会把"按键"误判成"已按键"提前关窗
fn countdown(total: u64) {
    use std::io::Write;
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        crate::platform::wait_any_key();
        let _ = tx.send(());
    });

    let mut left = total;
    loop {
        print!("\r窗口将在 {left} 秒后自动关闭，按任意键立即关闭… ");
        let _ = std::io::stdout().flush();
        if left == 0 {
            println!();
            return;
        }
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(()) => {
                println!("\r已按键，关闭窗口。");
                return;
            }
            Err(_) => left -= 1,
        }
    }
}
