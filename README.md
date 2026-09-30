# matchpal

完美世界电竞(CS)赛前三查工具。在匹配成立、地图尚未加载时，自动获取当前对局十名玩家的历史数据(ratingPro、K/D、ADR、爆头率、狙击率、闪光成功率、地图胜率、PP 分)，按阵营渲染为一张战绩表，帮助玩家在开局前判断对局质量、选择战术与心态预期。

基于完美世界电竞平台官方接口实现，本地运行，Rust 编写，无运行时依赖。

## 定位

| | 说明 |
| --- | --- |
| 解决的问题 | 匹配成立后，玩家需要手动打开观将台逐个搜索十名玩家的历史战绩，全程约一到两分钟，经常错过开局。 |
| 产品形态 | 一套 Windows 命令行工具，随对局自动完成"获取数据 → 等人齐 → 出表"，通常在对局开始前完成。 |
| 适用人群 | 使用完美世界电竞平台的天梯玩家；希望将战绩数据接入其他工具链(ndjson 导出)的用户。 |
| 不做的事 | 不采集任何平台未提供的数据，不影响对局进程，不修改客户端，不自动化任何游戏内操作。 |

## 与其他方案对比

| 方案 | 数据获取 | 单局耗时 | 依赖 | 自动化程度 | 数据留存 |
| --- | --- | --- | --- | --- | --- |
| 官方观将台 | 手动逐个搜索 | 1–2 分钟 | 浏览器登录 | 全手动 | 无 |
| **matchpal(本项目)** | 自动查十人 | 约 3–5 秒 | 单个可执行文件 | 全自动，人满即出表 | JSON / NDJSON 快照 |

## 工作区结构

| 成员 | 说明 |
| --- | --- |
| `fetch_token` | 本地代理工具，从完美世界客户端流量中捕获 access_token，生成配置文件；内置 WS 推送后端 |
| `match_watcher` | 主程序：订阅对局推送，等名单满员，查询战绩并渲染表格；内置 WS 推送后端 |
| `logkit` | 两个程序共用的分级日志库(时间戳、级别过滤、颜色、文件输出) |

构建要求：Rust 1.85 及以上(edition 2024)，Windows 10/11。

## 快速开始

```bash
# 第一步：获取 token(每个登录会话执行一次，token 过期后重新执行)
cargo run -p fetch_token --release

# 第二步：监听对局并输出战绩表
cargo run -p match_watcher --release
```

第一步运行时会弹出 UAC 提权确认，批准后进入拦截状态；此时在完美世界客户端中登录或进入个人页面，程序命中 token 后自动写入 `config.local.json` 并退出，随后自动还原系统代理、卸载临时证书。

第二步运行后连接对局推送通道，等待当前对局名单满 10 人(可通过 `--full` 调整)，查询战绩接口并输出表格，快照默认写入 `capture/match_snapshot.json`。

## 使用指南

### fetch_token

执行流程：自动提权(UAC)→ 运行时生成一把仅存在于本机的 CA 证书 → 安装到 Windows 根证书库 → 将系统代理临时指向 `127.0.0.1:8080` → 拦截并检查 HTTP/S 流量中的 token 字段(URL、请求头、Cookie、请求体、JSON 响应)→ 命中后写入配置文件 → 还原系统代理并卸载证书。

- 仅检查完美世界 / Steam 系域名白名单内的流量，白名单外原样转发。实测命中的域名：

  | 域名 | 角色 |
  | --- | --- |
  | `pwaweblogin.wmpvp.com` | 下发 / 携带 `steam_cn_token` |
  | `appactivity.wmpvp.com` | 对战接口(`getWebsocketInfo` 等) |
  | `gwapi.pwesports.cn` | token 挂在 URL 查询串上 |
  | `wss-csgo-pwa.wmpvp.com` | 对局 WebSocket 握手 |

- 若程序报告存在"字段名命中但域名不在白名单"的候选，可用 `--hosts <域名>` 追加后重跑。
- 正常退出时自动清理；若进程被强制终止，可能残留名为 `WMPVP Token Sniffer CA` 的证书或代理设置，前者可用 `certutil -delstore Root WMPVP Token Sniffer CA` 手动删除，后者在系统代理设置中关闭即可。

常用参数：

| 参数 | 说明 |
| --- | --- |
| `--port <端口>` | 本地监听端口(默认 8080) |
| `--hosts <域名>` | 在默认白名单之外追加域名 |
| `--timeout <秒>` | 最长等待时间(默认 300) |
| `--keep-ca` | 退出时保留根证书(默认卸载) |
| `--log-all <文件>` | 记录所有经过的请求 URL，用于排查抓取失败 |
| `--push-port <端口>` | WS 推送后端端口(默认 8787，`0` 关闭)，见下文「作为后端使用」 |
| `--no-pause` | 结束后不留窗(脚本 / CI 使用) |

退出码：`0` 捕获并写入 · `1` 超时未命中 · `2` 环境或参数错误 · `3` 提权被拒绝 · `4` 捕获成功但写入失败。

### match_watcher

数据链路(自抓包实测还原)：

```text
config.local.json(access_token + steamid)
  ↓  GET  getWebsocketInfo?steamId=<本机账号>&platform=2，请求头携带 accessToken
  ↓  WS   wss://wss-csgo-pwa.wmpvp.com，握手携带 Cookie: PVP_APP_TOKEN
  ↓  订阅后接收推送 messageType 10002(matchId / 地图 / 比分 / playerList)
  ↓  POST 按两队 steamid 查询 getTeamStatisticsData(玩家昵称仅该接口返回)
输出：表格 → stdout；诊断日志 → stderr；快照 → capture/match_snapshot.json
```

stdout 与 stderr 有意分离：表格是产品输出，可重定向或进入管道；日志用于人工诊断。

常用参数：

| 参数 | 说明 |
| --- | --- |
| `--full <人数>` | 名单达到该人数即出表并停止(默认 10，`0` 表示不等满) |
| `--once` | 收到第一帧即出表，不等待满员(调试用) |
| `--keep-going` | 出表后不退出，持续监听 |
| `--stats` | 将战绩并入表格(用于 `--once` 与持续模式) |
| `--timeout <秒>` | 最长运行时间，`0` 为不限 |
| `--json-out <文件>` / `--no-json-out` | 快照写入路径(默认 `capture/match_snapshot.json`)/ 关闭 |
| `--export <写法>` | 追加导出目标：`json:<文件>` 或 `ndjson:<文件>`，可重复 |
| `--resubscribe <秒>` | 未收到推送时重新订阅的间隔(默认 15，`0` 关闭) |
| `--retries <次数>` | 断线重连次数(默认 5) |
| `--replay <文件>` | 离线回放推送帧或战绩响应，不连接网络 |
| `--push-port <端口>` | WS 推送后端端口(默认 8788，`0` 关闭)，见下文「作为后端使用」 |
| `--log-level <级别>` | trace / debug / info / warn / error / off |

退出码：`0` 正常结束 · `1` token 无效或未收到对局推送 · `2` 配置或参数错误。

离线回放示例(仓库自带一份 10 人齐全的样本数据，steamId 与昵称已随机化)：

```bash
cargo run -p match_watcher --release -- --replay match_watcher/src/testdata/stats_response.json
```

## 作为后端使用(WS 推送)

两个程序都内置本地 WebSocket 推送服务，可以当作后端供 GUI / 前端消费，
不必轮询文件或解析 stdout。服务只监听 `127.0.0.1`，不对外网暴露；
单向推送，客户端发来的帧一律忽略。

| 服务 | 默认地址 | 关闭方式 | 内容 |
| --- | --- | --- | --- |
| fetch_token | `ws://127.0.0.1:8787` | `--push-port 0` | token 捕获结果 |
| match_watcher | `ws://127.0.0.1:8788` | `--push-port 0` | 对局监听事件流 |

fetch_token 协议(服务端 → 客户端的单行 JSON 文本帧)：

```text
连上即推   {"type":"hello","service":"fetch_token","push_port":8787}
命中写盘   {"type":"captured","config":{...与 config.local.json 完全相同...}}
超时未命中 {"type":"timeout"}
```

match_watcher 协议：连上先收 `hello`(带 `service: "match_watcher"`)，
之后是完整的监听事件流，形态为 `{"type":<事件名>,"data":{...}}`：

| 事件 | 说明 |
| --- | --- |
| `Connected` | WebSocket 已连接并完成订阅 |
| `Progress` | 名单进度(`loaded` / `full`) |
| `Resubscribed` | 尚未收到对局数据，重新订阅 |
| `Notice` | 诊断消息(`level` + 现成文案) |
| `Report` | 最终表格：`text` 为 CLI 同款文本，`data` 为结构化两队数据 |
| `Raw` | 原始 JSON:`source` 为 `push_frame`(对局推送帧)或 `stats_response`(战绩接口响应)，`payload` 为未加工数据 |
| `Finished` | 会话结束，`code` 与 CLI 退出码一致 |

各事件的示例帧(省略号表示实际内容更长)：

```jsonc
// hello —— 连上即推
{"type":"hello","service":"match_watcher","push_port":8788}

// Connected —— WebSocket 已连接并完成订阅
{"type":"Connected","data":{"full":10}}

// Progress —— 每收到一帧推送就报一次名单进度
{"type":"Progress","data":{"loaded":3,"full":10}}

// Resubscribed —— 超过间隔还没收到对局数据，重新订阅
{"type":"Resubscribed","data":{"waited_secs":15.2,"interval_secs":15.0}}

// Notice —— 诊断消息，level 与 CLI 日志级别一致，message 可直接展示
{"type":"Notice","data":{"level":"warn","message":"连接失败(连接超时)，3 秒后重试"}}

// Raw —— 原始推送帧，未经任何加工
{"type":"Raw","data":{"source":"push_frame","payload":{
  "messageType":10002,
  "messageData":{"matchId":"9215951389778120460","map":"de_dust2","playerList":[ … ]} 
}}}

// Raw —— 战绩接口的原始响应
{"type":"Raw","data":{"source":"stats_response","payload":{
  "code":1,"message":"success","result":{"ctPlayerStatsDTOList":[ … ],"ctTeamDTO":{ … }} 
}}}

// Report —— 最终表格：text 是 CLI 同款文本，data 是结构化的两队数据
{"type":"Report","data":{
  "text":"============================(表格原文)============================",
  "data":{
    "map":"de_dust2",
    "unknown":0,
    "ct":[{
      "side":"CT","steamid":"76561198000000001","nickname":"测试玩家01",
      "rating_pro":1.108,"kd":1.07,"adr":80.1,"we":8.8,
      "map_win_rate":0.571,"head_shot_rate":0.533,
      "snipe_rate":0.080,"flash_success_rate":0.822,"pvp_score":1725
    }],
    "t":[ … ]
  }
}}

// Finished —— 会话结束，code 含义与 CLI 退出码一致
{"type":"Finished","data":{"code":0}}
```

前端接入示例(浏览器 / Node 通用)：

```js
const ws = new WebSocket("ws://127.0.0.1:8788");
ws.onmessage = (e) => {
  const event = JSON.parse(e.data);
  if (event.type === "Report") render(event.data.data);   // 结构化两队数据
};
```

## 配置文件

`config.local.json` 由 fetch_token 生成、match_watcher 消费：

| 字段 | 说明 |
| --- | --- |
| `access_token` | 登录凭证。兼容 `steam_cn_token` 等别名键，优先级见 `match_watcher/src/config.rs` |
| `steamid` | 本机账号的 17 位 SteamID64 |
| `captured_at` / `captured_by` / `source_url` / `uid` / `host` / `path` | 抓取来源记录 |

注意：该文件包含明文 token，已被 `.gitignore` 排除，请勿提交或外发。

## 故障排查

| 现象 | 原因与处理 |
| --- | --- |
| 已连接但长时间无推送 | 推送仅在正在对局时出现。程序每 15 秒自动重新订阅并提示等待时长；若持续无推送且不在对局中，属正常现象 |
| 提示"缺少 token" | 重新运行 fetch_token，或用 `--token` 直接指定 |
| 提示 steamid 错误 | 需要本机账号的 17 位 SteamID64 |
| `--check` 通过但实际无数据 | `getWebsocketInfo` 接口不校验 token，该检查仅验证网络连通性 |
| 表格退回按 SteamID 显示 | 玩家昵称仅存在于战绩接口，通常是 token 已失效，重新执行 fetch_token |
| fetch_token 报"未捕获 token" | 按提示依次检查：客户端是否使用系统代理、根证书是否安装成功、用 `--log-all` 或 `--verbose` 复查流量 |

## 测试

```bash
cargo test --workspace
```

- `fetch_token`：token 字段识别规则、域名白名单边界(近似域名、后缀匹配)、请求体可读性判定等回归测试。
- `match_watcher`：推送帧解析、阵营分组、表格渲染(中英文混排宽度对齐)、导出器、WebSocket 超时判定，以及基于 `src/testdata/stats_response.json` 完整样本的解析与渲染链路测试。
- `logkit`：级别过滤与格式化单元测试，可运行 `cargo run -p logkit --example demo` 查看各级别输出效果。

## 安全与隐私说明

- fetch_token 运行期间，本机走系统代理的 HTTPS 流量会经过本地代理并被解密检查；白名单之外的内容不做任何记录。
- CA 证书每次运行现场生成、正常退出即卸载；私钥不落盘分发。
- `config.local.json`(明文 token)与 `capture/`(CA 证书、抓包日志、快照)均在 `.gitignore` 中，其中的 URL 与快照可能直接包含 token，请勿提交或外发。
- 本项目为个人工具，接口自抓包实测还原，平台协议变更可能导致功能失效。

## 许可证

GPLv3，见仓库根 `LICENSE`。
