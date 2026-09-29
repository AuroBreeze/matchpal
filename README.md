# matchpal

完美世界电竞（CS）对局数据工具集。抓取自己的登录 token，监听当前对局，人满即出一张
按昵称汇总的战绩表——无需打开观将台，开打前几秒就能看到对面和队友的成色。

## 工作区结构

| 成员 | 说明 |
| --- | --- |
| `fetch_token` | 本地 MITM 代理，从完美世界客户端流量中捕获 access_token，写入 `config.local.json` |
| `match_watcher` | 主程序：连接对局 WebSocket，等名单满 → 查战绩 → 输出表格 |
| `logkit` | 两个程序共用的分级日志库（时间戳 / 级别过滤 / 颜色 / 文件 sink） |
| （根包 `matchpal`） | 仅占位，暂无功能 |

构建要求：Rust 1.85+（edition 2024），Windows 10/11。

## 快速开始

```bash
# 1. 抓 token（自动提权弹 UAC → 现场生成 CA → 装证书 → 设系统代理 → 拦截 → 自动清理）
cargo run -p fetch_token --release
#    运行后在完美世界客户端里登录 / 点一下头像即命中，自动写 config.local.json 并收工

# 2. 监听对局
cargo run -p match_watcher --release
#    进对局等人数到 10 → 输出战绩表 → 退出；快照写在 capture/match_snapshot.json
```

token 失效时（日志会提示）重跑第 1 步即可，抓到的就是新的。

## fetch_token

执行流程：自动提权（UAC）→ 运行时用 rcgen 现场生成一把**本机自有** CA（私钥绝不分发）
→ 装入 Windows 根证书库 → 把系统代理临时指向 `127.0.0.1:8080` → 拦截 HTTP/S 扫描
URL / 请求头 / Cookie / 请求体 / JSON 响应里的 token 字段 → 命中写盘 → 还原代理并卸载证书。

- 默认只认完美世界 / Steam 系域名白名单（`wmpvp.com`、`pwesports.cn`、`wanmei.com`、
  `steampowered.com` 等）。白名单外的候选会在退出时列出，可用 `--hosts <域名>` 追加后重跑。
- 上游 TLS 走 schannel（读 Windows 证书库），所以必须装 CA 才能解开 HTTPS。
- 正常退出即自动清理；异常退出（断电 / 强杀）可能残留证书或代理设置，
  残留证书名 `WMPVP Token Sniffer CA`，可用 `certutil -delstore Root <名称>` 手动卸载。

实测的 token 命中记录（白名单就是按它定的）：

| 域名 | 角色 |
| --- | --- |
| `pwaweblogin.wmpvp.com` | 下发 / 携带 `steam_cn_token` |
| `appactivity.wmpvp.com` | 对战接口（`getWebsocketInfo` 等） |
| `gwapi.pwesports.cn` | token 直接挂在 URL 查询串上 |
| `wss-csgo-pwa.wmpvp.com` | 对局 WebSocket 握手 |

常用参数：

```bash
cargo run -p fetch_token --release -- --help
#  --keep-ca              结束后保留根证书（默认卸载）
#  --any-host             不限制域名（噪声大，一般不用）
#  --log-all <文件>       把所有经过的请求 URL 记录到该文件（排查抓不到时用）
#  --no-pause             结束即关窗口（脚本/CI 用）
```

退出码：`0` 捕获并写入 · `1` 超时未命中 · `2` 环境/参数错误 · `3` 提权被拒绝 · `4` 已捕获但写入失败。

## match_watcher

链路（从实测 HAR 还原）：

```text
config.local.json（access_token + steamid）
  ↓  GET  getWebsocketInfo?steamId=<自己>&platform=2（请求头 accessToken）
  ↓  WS   wss://wss-csgo-pwa.wmpvp.com（Cookie: PVP_APP_TOKEN）
  ↓  订阅 → 收推送 messageType 10002（matchId / 地图 / 比分 / playerList）
  ↓  POST getPvPMatchTeamStatisticsData（按两队 steamid 查战绩，昵称只在这里有）
表格 → stdout；日志 → stderr；快照 → capture/match_snapshot.json
```

默认行为：等名单满 10 人 → 出表 → 退出。常用参数：

```bash
cargo run -p match_watcher --release -- --help
#  --once                 收到第一帧即出表（调试）
#  --full <人数>          攒够几人出表（默认 10，0 = 不等）
#  --keep-going           出表后继续监听（持续模式）
#  --stats                把战绩并入表格（给 --once / 持续模式用）
#  --timeout <秒>         最长运行时间
#  --export <写法>        追加导出目标：json:文件 / ndjson:文件（可重复）
#  --check                只测接口连通性（注意：该接口不校验 token）
#  --replay <文件>        离线回放推送帧或战绩响应（不联网）
#  --log-level <级别>     trace/debug/info/warn/error/off
```

退出码：`0` 正常 · `1` token 无效或未收到对局推送 · `2` 配置/参数错误。

离线回放示例（仓库自带一份 10 人齐全的样本，steamId/昵称已随机化）：

```bash
cargo run -p match_watcher --release -- --replay match_watcher/src/testdata/stats_response.json
```

## 配置文件

`config.local.json` 由 fetch_token 生成，match_watcher 消费：

| 字段 | 说明 |
| --- | --- |
| `access_token` | 登录凭证（主键；也兼容 `steam_cn_token` 等别名，见 `match_watcher/src/config.rs`） |
| `steamid` | 17 位 SteamID64，必须是**本账号**的 |
| `captured_at` / `source_url` / `uid` | 抓取来源记录 |

⚠️ 该文件是**明文** token，已被 `.gitignore` 排除，不要提交、不要外发。

## 测试

```bash
cargo test --workspace
```

- `fetch_token`：token 字段识别、域名白名单边界（近似域名、后缀匹配）、body 可读性规则等回归。
- `match_watcher`：推送帧解析、队伍分组、表格渲染（中英文宽度对齐）、导出器、WS 超时判定，
  以及基于 `src/testdata/stats_response.json` 的完整解析 + 渲染链路。
- `logkit`：`cargo run -p logkit --example demo` 可看各级别输出效果。

## 安全与隐私说明

- 抓 token 期间**本机所有走系统代理的 HTTPS 流量**都会经过本地代理并被解密检查；
  白名单保证只有命中域名的内容会被写盘，其余原样转发。
- CA 每次运行现场生成、退出即卸载；`--keep-ca` 仅在清楚后果时使用。
- `capture/`（CA 证书、抓包日志、快照）与 `config.local.json` 均在 `.gitignore` 中，
  里面的 URL 和快照可能直接带着 token。
