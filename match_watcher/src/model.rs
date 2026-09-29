//! 推送帧与战绩的数据模型
//!
//! # 为什么把原始 JSON 一起留着
//!
//! 这是个**非官方接口**，字段类型会变（`pvpScore` 有时是数字有时是字符串，
//! `headshot` 可能是 null）。Python 版靠鸭子类型天然容忍这些，Rust 要是一上来就
//! 严格 `Deserialize` 成强类型，遇到一个意外类型就会**整帧解析失败、数据全丢**。
//!
//! 所以这里的策略是：
//!
//! - 帧原文用 [`serde_json::Value`] 原样留着（写快照、导出时按原样落盘）
//! - 需要用到的地方走**宽容访问器**（数字/数字字符串都认，取不到给 `None`）
//! - 只有渲染真正用得上的字段才抽成 [`Player`] 这种小视图

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 订阅自己的对局
pub const SUBSCRIBE_TYPE: i64 = 10001;
/// 对局数据推送（只在你正在打对局时才推）
pub const PUSH_TYPE: i64 = 10002;

/// 一帧 10002 里的 `messageData`，原文原样保留
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MatchInfo {
    pub raw: Value,
}

impl MatchInfo {
    pub fn new(raw: Value) -> Self {
        Self { raw }
    }

    fn text(&self, key: &str) -> Option<String> {
        as_text(self.raw.get(key)?)
    }

    fn number(&self, key: &str) -> Option<f64> {
        as_f64(self.raw.get(key)?)
    }

    pub fn match_id(&self) -> Option<String> {
        self.text("matchId")
    }

    pub fn map(&self) -> Option<String> {
        self.text("map")
    }

    /// 帧里叫 `type`（`skyladder` 之类），`type` 是 Rust 关键字所以访问器改名
    pub fn kind(&self) -> Option<String> {
        self.text("type")
    }

    pub fn start_time(&self) -> Option<String> {
        self.text("startTime")
    }

    pub fn ct_score(&self) -> Option<f64> {
        self.number("ctScore")
    }

    pub fn terrorist_score(&self) -> Option<f64> {
        self.number("terroristScore")
    }

    pub fn ave_score(&self) -> Option<f64> {
        self.number("aveScore")
    }

    /// 10 人名单。取不到 `playerList` 就是空
    pub fn players(&self) -> Vec<Player> {
        self.raw
            .get("playerList")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(Player::from_value).collect())
            .unwrap_or_default()
    }

    /// 按 CT / T 分组，顺带把认不出阵营的挑出来（Python 版是静默丢掉，这里留痕）
    pub fn teams(&self) -> Teams {
        split_teams(&self.players())
    }
}

/// 阵营
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Side {
    Ct,
    T,
    /// 认不出的阵营。Python 版会把这类人**静默丢掉**，表格少人也不告诉你
    Unknown(String),
}

impl Side {
    pub fn parse(text: &str) -> Side {
        let upper = text.trim().to_ascii_uppercase();
        if upper.starts_with("CT") {
            Side::Ct
        } else if upper.starts_with("TERROR") || upper.starts_with('T') {
            Side::T
        } else {
            Side::Unknown(text.to_string())
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Side::Ct => "CT",
            Side::T => "T",
            Side::Unknown(other) => other,
        }
    }
}

/// 名单里的一个人
#[derive(Debug, Clone, PartialEq)]
pub struct Player {
    pub steamid: String,
    pub side: Side,
    pub kill: Option<f64>,
    pub death: Option<f64>,
    pub assist: Option<f64>,
    pub adr: Option<f64>,
    pub headshot: Option<f64>,
    pub score: Option<f64>,
    pub alive: Option<bool>,
}

impl Player {
    pub fn from_value(value: &Value) -> Player {
        let field = |key: &str| value.get(key).and_then(as_f64);
        Player {
            steamid: value.get("steamId").and_then(as_text).unwrap_or_default(),
            side: Side::parse(&value.get("side").and_then(as_text).unwrap_or_default()),
            kill: field("kill"),
            death: field("death"),
            assist: field("assist"),
            adr: field("adr"),
            headshot: field("headshot"),
            score: field("score"),
            alive: value.get("alive").and_then(Value::as_bool),
        }
    }
}

/// 分组结果
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Teams {
    pub ct: Vec<Player>,
    pub t: Vec<Player>,
    pub unknown: Vec<Player>,
}

impl Teams {
    pub fn total(&self) -> usize {
        self.ct.len() + self.t.len() + self.unknown.len()
    }
}

/// 按阵营前缀分组，语义对齐 Python 的 `split_teams`，但多留一份 `unknown`
pub fn split_teams(players: &[Player]) -> Teams {
    let mut teams = Teams::default();
    for player in players {
        match player.side {
            Side::Ct => teams.ct.push(player.clone()),
            Side::T => teams.t.push(player.clone()),
            Side::Unknown(_) => teams.unknown.push(player.clone()),
        }
    }
    teams
}

/// 战绩接口返回的每人数据，键是 SteamID64，值是原文
pub type StatsMap = BTreeMap<String, Value>;

/// 从战绩里取一个数值字段（数字 / 数字字符串都认）
pub fn stat_number(stats: &StatsMap, steamid: &str, key: &str) -> Option<f64> {
    stats.get(steamid)?.get(key).and_then(as_f64)
}

/// 战绩接口里单个选手的数据
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerStat {
    pub steamid: String,
    pub nickname: String,
    pub avatar: String,
    pub map: String,
    pub rating_pro: Option<f64>,
    pub kd: Option<f64>,
    pub adr: Option<f64>,
    pub we: Option<f64>,
    pub map_win_rate: Option<f64>,
    pub ct_win_rate: Option<f64>,
    pub t_win_rate: Option<f64>,
    pub win: Option<f64>,
    pub lose: Option<f64>,
    pub draw: Option<f64>,
    pub pvp_score: Option<f64>,
    pub snipe_rate: Option<f64>,
    pub head_shot_rate: Option<f64>,
    pub flash_success_rate: Option<f64>,
    pub anonymous: bool,
}

impl PlayerStat {
    pub fn from_value(value: &Value) -> PlayerStat {
        let num = |key: &str| value.get(key).and_then(as_f64);
        PlayerStat {
            steamid: value.get("steamId").and_then(as_text).unwrap_or_default(),
            nickname: value.get("nickname").and_then(as_text).unwrap_or_default(),
            avatar: value.get("avatar").and_then(as_text).unwrap_or_default(),
            map: value.get("map").and_then(as_text).unwrap_or_default(),
            rating_pro: num("ratingPro"),
            kd: num("kd"),
            adr: num("adr"),
            we: num("we"),
            map_win_rate: num("mapWinRate"),
            ct_win_rate: num("ctWinRate"),
            t_win_rate: num("twinRate"),
            win: num("win"),
            lose: num("lose"),
            draw: num("draw"),
            pvp_score: num("pvpScore"),
            snipe_rate: num("snipeRate"),
            head_shot_rate: num("headShotRate"),
            flash_success_rate: num("flashSuccessRate"),
            anonymous: value.get("anonymous").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    /// 表格里显示的名字：匿名或没昵称时给个能看懂的占位，不要空着
    pub fn display_name(&self) -> String {
        if self.anonymous {
            "匿名玩家".to_string()
        } else if self.nickname.trim().is_empty() {
            "（无昵称）".to_string()
        } else {
            self.nickname.clone()
        }
    }
}

/// 队伍汇总（`ctTeamDTO` / `tteamDTO`）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TeamStat {
    pub rating_pro: Option<f64>,
    pub kd: Option<f64>,
    pub adr: Option<f64>,
    pub we: Option<f64>,
    pub win_rate: Option<f64>,
    /// 各项"最高"：字段名 → (SteamID, 值文本)。渲染时把 SteamID 换成昵称
    pub best: BTreeMap<String, (String, String)>,
}

impl TeamStat {
    pub fn from_value(value: &Value) -> TeamStat {
        let num = |key: &str| value.get(key).and_then(as_f64);
        let mut best = BTreeMap::new();
        for key in ["snipeRate", "headShotRate", "flashSuccessRate"] {
            let Some(entry) = value.get(key) else { continue };
            let first = entry.get("first").and_then(as_text);
            let second = entry.get("second").and_then(as_text);
            if let (Some(first), Some(second)) = (first, second) {
                best.insert(key.to_string(), (first, second));
            }
        }
        TeamStat {
            rating_pro: num("ratingPro"),
            kd: num("kd"),
            adr: num("adr"),
            we: num("we"),
            win_rate: num("winRate"),
            best,
        }
    }
}

/// 一次战绩查询的完整结果
#[derive(Debug, Clone, Default)]
pub struct StatsReport {
    pub ct: Vec<PlayerStat>,
    pub t: Vec<PlayerStat>,
    pub ct_team: Option<TeamStat>,
    pub t_team: Option<TeamStat>,
    /// 原文摊平后的映射（SteamID → 原文），导出用
    pub raw: StatsMap,
}

impl StatsReport {
    /// 从完整响应（`{"code":1,"result":{…}}`）解析
    pub fn from_value(value: &Value) -> StatsReport {
        let empty = serde_json::Map::new();
        let result = value.get("result").and_then(Value::as_object).unwrap_or(&empty);
        let list = |key: &str| -> Vec<PlayerStat> {
            result
                .get(key)
                .and_then(Value::as_array)
                .map(|items| items.iter().map(PlayerStat::from_value).collect())
                .unwrap_or_default()
        };
        StatsReport {
            ct: list("ctPlayerStatsDTOList"),
            // 三种拼写都见过
            t: {
                let mut t = list("tplayerStatsDTOList");
                if t.is_empty() {
                    t = list("tePlayerStatsDTOList");
                }
                t
            },
            ct_team: result.get("ctTeamDTO").map(TeamStat::from_value),
            t_team: result.get("tteamDTO").map(TeamStat::from_value),
            raw: collect_stats(value),
        }
    }

    pub fn players(&self) -> impl Iterator<Item = &PlayerStat> {
        self.ct.iter().chain(self.t.iter())
    }

    pub fn len(&self) -> usize {
        self.ct.len() + self.t.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn find(&self, steamid: &str) -> Option<&PlayerStat> {
        self.players().find(|player| player.steamid == steamid)
    }

    /// 这个 JSON 看起来是不是战绩接口的响应（而不是推送帧）
    pub fn looks_like_response(value: &Value) -> bool {
        let Some(result) = value.get("result") else { return false };
        ["ctPlayerStatsDTOList", "tplayerStatsDTOList", "tePlayerStatsDTOList"]
            .iter()
            .any(|key| result.get(*key).is_some_and(Value::is_array))
    }

    /// 从「摊平的 SteamID → 选手」映射建报告。
    ///
    /// 用途是回放**快照文件**（`{captured_at, match, stats}`）—— 无论是 Python 版
    /// 还是本工具写出来的都是这个形状，`stats` 里没有阵营，所以队伍要拿推送帧的
    /// `side` 去分。
    pub fn from_flat(stats: &Value, info: &MatchInfo) -> StatsReport {
        let empty = serde_json::Map::new();
        let entries = stats.as_object().unwrap_or(&empty);
        let mut report = StatsReport::default();
        for player in info.players() {
            let Some(entry) = entries.get(&player.steamid) else { continue };
            match player.side {
                Side::Ct => report.ct.push(PlayerStat::from_value(entry)),
                Side::T => report.t.push(PlayerStat::from_value(entry)),
                // 阵营认不出的人不进表，与实时路径保持一致
                Side::Unknown(_) => continue,
            }
        }
        report.raw = entries
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        report
    }

    /// 快照形状的 `stats` 是否可用（键是 SteamID、值是选手对象）
    pub fn flat_looks_usable(stats: &Value) -> bool {
        stats
            .as_object()
            .is_some_and(|map| map.values().any(|value| value.get("steamId").is_some()))
    }

    /// 地图名：响应里只在每个选手身上带，取第一个
    pub fn map(&self) -> Option<String> {
        self.players().map(|player| player.map.clone()).find(|map| !map.is_empty())
    }
}

/// 把响应里的三份选手统计摊平成「SteamID → 原文」
fn collect_stats(value: &Value) -> StatsMap {
    let mut stats = StatsMap::new();
    let Some(result) = value.get("result") else {
        return stats;
    };
    for key in ["ctPlayerStatsDTOList", "tplayerStatsDTOList", "tePlayerStatsDTOList"] {
        let Some(items) = result.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            if let Some(steamid) = item.get("steamId").and_then(as_text) {
                stats.insert(steamid, item.clone());
            }
        }
    }
    stats
}

/// 帧分类结果
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// 对局数据推送
    Match(Box<MatchInfo>),
    /// 别的 messageType（订阅回执之类）
    Other(Option<i64>),
    /// 不是 JSON（`pong`、心跳文本）
    NotJson,
}

/// 把一帧文本分类。抽成纯函数是为了能单测 —— 真正的收帧循环没法离线测。
pub fn classify(text: &str) -> Frame {
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        return Frame::NotJson;
    };
    let message_type = message.get("messageType").and_then(as_f64).map(|value| value as i64);
    if message_type != Some(PUSH_TYPE) {
        return Frame::Other(message_type);
    }
    match message.get("messageData") {
        Some(data) if data.is_object() => Frame::Match(Box::new(MatchInfo::new(data.clone()))),
        // 有 10002 但没有 messageData：当普通帧处理，别当成对局数据
        _ => Frame::Other(message_type),
    }
}

/// 订阅帧的 JSON 文本（发 10001）
pub fn subscribe_payload(steamid: &str) -> String {
    serde_json::json!({
        "messageType": SUBSCRIBE_TYPE,
        "messageData": { "steam_id": steamid },
    })
    .to_string()
}

/// 字符串 / 数字都转成字符串，对齐 Python 的 `str(value)`
pub fn as_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// 数字 / 数字字符串都转成 f64
pub fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 人造一帧，形状照抄真实推送（README 里的字段），但 SteamID 全是编的
    fn sample_frame() -> String {
        json!({
            "messageType": PUSH_TYPE,
            "messageData": {
                "matchId": "9215951389778120460",
                "map": "de_dust2",
                "type": "skyladder",
                "startTime": "2026-09-29 21:00:00",
                "ctScore": 7,
                "terroristScore": 5,
                "aveScore": 1.23,
                "playerList": [
                    {"steamId": "76561198000000001", "side": "CT", "kill": 12, "death": 8, "assist": 3, "adr": 95.3, "headshot": 4, "score": 1.45, "alive": true},
                    {"steamId": "76561198000000002", "side": "TERRORIST", "kill": "9", "death": 10, "assist": 2, "adr": "80", "headshot": null, "score": 0.98, "alive": false},
                    {"steamId": "76561198000000003", "side": "观察者", "kill": 0, "death": 0, "assist": 0, "adr": 0, "headshot": 0, "score": 0}
                ]
            }
        })
        .to_string()
    }

    #[test]
    fn classifies_push_frame() {
        let Frame::Match(info) = classify(&sample_frame()) else {
            panic!("应该识别成对局推送");
        };
        assert_eq!(info.match_id().as_deref(), Some("9215951389778120460"));
        assert_eq!(info.map().as_deref(), Some("de_dust2"));
        assert_eq!(info.kind().as_deref(), Some("skyladder"));
        assert_eq!(info.ct_score(), Some(7.0));
        assert_eq!(info.terrorist_score(), Some(5.0));
        assert_eq!(info.ave_score(), Some(1.23));
        assert_eq!(info.players().len(), 3);
    }

    #[test]
    fn non_push_frames_are_not_mistaken_for_matches() {
        assert_eq!(classify("pong"), Frame::NotJson);
        assert_eq!(classify("not json at all"), Frame::NotJson);
        assert_eq!(classify(r#"{"messageType":10001,"messageData":{}}"#), Frame::Other(Some(10001)));
        // 10002 但没有 messageData：别当成对局数据
        assert_eq!(classify(r#"{"messageType":10002}"#), Frame::Other(Some(10002)));
        assert_eq!(classify(r#"{"foo":1}"#), Frame::Other(None));
    }

    /// 字段类型飘了也不能丢帧：数字/字符串/null 混着来照样能读
    #[test]
    fn tolerates_loose_field_types() {
        let players = match classify(&sample_frame()) {
            Frame::Match(info) => info.players(),
            other => panic!("应该是对局帧，实际 {other:?}"),
        };
        assert_eq!(players[0].adr, Some(95.3));
        assert_eq!(players[0].alive, Some(true));
        // 字符串数字
        assert_eq!(players[1].kill, Some(9.0));
        assert_eq!(players[1].adr, Some(80.0));
        // null → None，不报错
        assert_eq!(players[1].headshot, None);
        assert_eq!(players[1].alive, Some(false));
    }

    #[test]
    fn splits_teams_and_keeps_the_unknown_ones_visible() {
        let Frame::Match(info) = classify(&sample_frame()) else { panic!("应该是对局帧") };
        let teams = info.teams();
        assert_eq!(teams.ct.len(), 1);
        assert_eq!(teams.t.len(), 1);
        // Python 版会把这个人静默丢掉，表格人数对不上还查不出原因
        assert_eq!(teams.unknown.len(), 1);
        assert_eq!(teams.total(), 3);
        assert_eq!(teams.unknown[0].side.label(), "观察者");
    }

    #[test]
    fn side_parsing_matches_python_prefix_rules() {
        assert_eq!(Side::parse("CT"), Side::Ct);
        assert_eq!(Side::parse("ct"), Side::Ct);
        assert_eq!(Side::parse("T"), Side::T);
        assert_eq!(Side::parse("TERRORIST"), Side::T);
        assert_eq!(Side::parse("t"), Side::T);
        // 空阵营也算未知，不能默默划进 T
        assert_eq!(Side::parse(""), Side::Unknown(String::new()));
    }

    #[test]
    fn subscribe_payload_shape() {
        let text = subscribe_payload("76561198000000000");
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["messageType"], json!(SUBSCRIBE_TYPE));
        assert_eq!(value["messageData"]["steam_id"], json!("76561198000000000"));
    }

    #[test]
    fn stats_lookup_reads_loose_numbers() {
        let mut stats = StatsMap::new();
        stats.insert(
            "76561198000000001".to_string(),
            json!({"ratingPro": 1.23, "pvpScore": "1800", "mapWinRate": 0.5}),
        );
        assert_eq!(stat_number(&stats, "76561198000000001", "ratingPro"), Some(1.23));
        assert_eq!(stat_number(&stats, "76561198000000001", "pvpScore"), Some(1800.0));
        assert_eq!(stat_number(&stats, "76561198000000001", "mapWinRate"), Some(0.5));
        // 查不到的人 / 查不到的字段都是 None，不是 panic
        assert_eq!(stat_number(&stats, "76561198000000009", "ratingPro"), None);
        assert_eq!(stat_number(&stats, "76561198000000001", "nope"), None);
    }

    #[test]
    fn empty_match_info_yields_nothing_instead_of_panicking() {
        let info = MatchInfo::new(json!({}));
        assert_eq!(info.match_id(), None);
        assert!(info.players().is_empty());
        assert_eq!(info.teams().total(), 0);
    }

    /// 战绩响应的形状照抄真实返回（README 记过的那份），SteamID 与昵称都是编的
    fn sample_stats_response() -> Value {
        json!({
            "code": 1,
            "message": "success",
            "result": {
                "ctPlayerStatsDTOList": [
                    {
                        "steamId": "76561198000000001", "nickname": "甲", "map": "de_dust2",
                        "kd": 1.0743, "ratingPro": 1.108, "adr": 80.1, "we": 8.8,
                        "mapWinRate": 0.5714, "ctWinRate": 0.54, "twinRate": 0.5316,
                        "win": 5, "lose": 4, "draw": 1, "pvpScore": 1725,
                        "snipeRate": 0.0802, "headShotRate": 0.5328, "flashSuccessRate": 0.8218,
                        "anonymous": false
                    }
                ],
                "tplayerStatsDTOList": [
                    {
                        "steamId": "76561198000000002", "nickname": "乙", "map": "de_dust2",
                        "kd": 0.88, "ratingPro": 0.973, "adr": 72.9, "we": 7.5,
                        "mapWinRate": 0.46, "pvpScore": 1719, "anonymous": true
                    }
                ],
                "ctTeamDTO": {
                    "ratingPro": 1.13, "kd": 1.04, "adr": 82.96, "we": 8.9, "winRate": 0.49,
                    "snipeRate": {"first": "76561198000000001", "second": "0.26"},
                    "headShotRate": {"first": "76561198000000001", "second": "0.53"}
                },
                "tteamDTO": {"ratingPro": 1.01, "kd": 0.99, "adr": 72.58, "we": 8.3, "winRate": 0.51}
            }
        })
    }

    #[test]
    fn parses_stats_report() {
        let report = StatsReport::from_value(&sample_stats_response());
        assert_eq!(report.len(), 2);
        assert!(!report.is_empty());
        assert_eq!(report.map().as_deref(), Some("de_dust2"));

        let ct = &report.ct[0];
        assert_eq!(ct.nickname, "甲");
        assert_eq!(ct.rating_pro, Some(1.108));
        assert_eq!(ct.kd, Some(1.0743));
        assert_eq!(ct.adr, Some(80.1));
        assert_eq!(ct.we, Some(8.8));
        assert_eq!(ct.map_win_rate, Some(0.5714));
        assert_eq!(ct.pvp_score, Some(1725.0));
        assert_eq!(ct.head_shot_rate, Some(0.5328));
        assert_eq!((ct.win, ct.lose, ct.draw), (Some(5.0), Some(4.0), Some(1.0)));
        assert!(!ct.anonymous);

        // 队伍汇总 + "各项最高"
        let team = report.ct_team.as_ref().expect("有 CT 队伍汇总");
        assert_eq!(team.rating_pro, Some(1.13));
        assert_eq!(team.win_rate, Some(0.49));
        assert_eq!(team.best.len(), 2, "只给了两项");
        assert_eq!(team.best["snipeRate"].1, "0.26");
        assert!(report.t_team.is_some());

        // 导出用的原文映射按 SteamID 索引，两份名单都在里面
        assert_eq!(report.raw.len(), 2);
        assert!(report.raw.contains_key("76561198000000002"));
        // 按 SteamID 反查（渲染"最高"时要用它换昵称）
        assert_eq!(report.find("76561198000000002").unwrap().display_name(), "匿名玩家");
    }

    #[test]
    fn stats_report_tolerates_junk() {
        // 空对象 / 没有 result / 列表不是数组：都不该 panic
        assert!(StatsReport::from_value(&json!({})).is_empty());
        assert!(StatsReport::from_value(&json!({"result": {}})).is_empty());
        assert!(StatsReport::from_value(&json!({"result": {"ctPlayerStatsDTOList": "不是数组"}})).is_empty());
        // 缺 steamId 的人仍会出现在表格里（只是查不到人）
        let report = StatsReport::from_value(&json!({"result": {"ctPlayerStatsDTOList": [{"nickname": "没有id"}]}}));
        assert_eq!(report.len(), 1);
        assert_eq!(report.ct[0].steamid, "");
        assert_eq!(report.ct[0].display_name(), "没有id");
    }

    /// `te…` / `t…` 两种拼写都认；`t` 有数据时不要被 `te` 覆盖
    #[test]
    fn accepts_both_t_list_spellings() {
        let with_t = StatsReport::from_value(&json!({
            "result": {"tplayerStatsDTOList": [{"steamId": "76561198000000002"}]}
        }));
        assert_eq!(with_t.t.len(), 1);

        let with_te = StatsReport::from_value(&json!({
            "result": {"tePlayerStatsDTOList": [{"steamId": "76561198000000002"}]}
        }));
        assert_eq!(with_te.t.len(), 1);
    }

    #[test]
    fn recognises_a_stats_response() {
        assert!(StatsReport::looks_like_response(&sample_stats_response()));
        // 推送帧不是响应
        let push: Value = serde_json::from_str(&sample_frame()).unwrap();
        assert!(!StatsReport::looks_like_response(&push));
        assert!(!StatsReport::looks_like_response(&json!({})));
    }

    #[test]
    fn display_name_falls_back_instead_of_showing_nothing() {
        let mut player = PlayerStat::default();
        assert_eq!(player.display_name(), "（无昵称）");
        player.nickname = "  ".into();
        assert_eq!(player.display_name(), "（无昵称）");
        player.nickname = "名字".into();
        assert_eq!(player.display_name(), "名字");
        // 匿名优先于昵称（服务端可能仍然塞了昵称）
        player.anonymous = true;
        assert_eq!(player.display_name(), "匿名玩家");
    }
}
