//! 监听完美世界电竞当前对局
//!
//! 链路：
//!
//! ```text
//! config.local.json(access_token + steamid)
//!   ↓
//! GET  appactivity.wmpvp.com/steamcn/match/watchStage/getWebsocketInfo
//!          ?steamId=<自己>&platform=2          请求头 accessToken: <token>
//!      → {"result":{"websocketUrl":"wss://wss-csgo-pwa.wmpvp.com"}}
//!   ↓
//! WS   wss://wss-csgo-pwa.wmpvp.com          握手带 Cookie: PVP_APP_TOKEN=<token>
//!      发 ping                                → 收 pong
//!      发 {"messageType":10001,...}           ← 订阅自己的对局
//!      收 {"messageType":10002,"messageData":{matchId, map, playerList[10], ...}}
//!   ↓
//! (--stats)POST …/getPvPMatchTeamStatisticsData
//!      → 每人 ratingPro / PP分 / 地图胜率
//! ```
//!
//! crate 拆成 lib + bin 两部分：lib 里的解析、分组、渲染、导出都能单测，
//! bin 只做参数解析和编排 —— 顺带避免 `fetch_token` 踩过的那个坑
//! (`main` 写在 `lib.rs` 里会没有 bin target，`cargo run` 直接报错)。

pub mod api;
pub mod config;
pub mod export;
pub mod model;
pub mod push;
pub mod render;
pub mod session;
pub mod ws;
