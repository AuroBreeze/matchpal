//! 把对局信息渲染成等宽表格
//!
//! 与 Python 版的差别（都是有意修的，不是漏抄）：
//!
//! - **按显示宽度对齐**：Python 直接拿 f-string 的 `<5` / `:>8` 对齐，那是按**字符数**算的，
//!   于是"队伍""爆头"这些中文表头都会歪。这里统一按显示宽度补空格（中文算 2 列）。
//! - 缺字段显示 `-` 而不是 Python 的 `None`。
//! - `headshot` 是 0 时显示 `0` —— Python 写的 `value or '-'` 会把 0 也变成 `-`。

use crate::model::{stat_number, MatchInfo, Player, Teams, PUSH_TYPE};

/// 分隔线宽度，与 Python 版一致
const RULE: usize = 92;

/// 各列宽度，顺序与 Python 版一致
const COL_TEAM: usize = 5;
const COL_STEAMID: usize = 19;
const COL_KILL: usize = 3;
const COL_DEATH: usize = 4;
const COL_ASSIST: usize = 4;
const COL_ADR: usize = 6;
const COL_HEADSHOT: usize = 8;
const COL_SCORE: usize = 6;
const COL_RATING: usize = 11;
const COL_PVP: usize = 7;
const COL_WINRATE: usize = 10;

/// 显示宽度：东亚字符占 2 列（阈值抄 Python 的 `ord(ch) > 0x2E80`）
pub fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(ch: char) -> usize {
    if (ch as u32) > 0x2E80 { 2 } else { 1 }
}

/// 按显示宽度截断，截了就补一个省略号（`…` 只占 1 列）
///
/// 昵称里中文很长很常见（"汉东省刑侦大队第一狙击手祁同伟" 是 34 列），
/// 不截断整张表会被一个人撑爆。
pub fn truncate_display(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    let budget = max.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = char_width(ch);
        if used + width > budget {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}

/// 左对齐补齐到指定显示宽度
pub fn pad_left(text: &str, width: usize) -> String {
    let mut out = text.to_string();
    out.push_str(&" ".repeat(width.saturating_sub(display_width(text))));
    out
}

/// 右对齐补齐到指定显示宽度
pub fn pad_right(text: &str, width: usize) -> String {
    let mut out = " ".repeat(width.saturating_sub(display_width(text)));
    out.push_str(text);
    out
}

/// 数字渲染：缺值给 `-`，整数不带小数点
fn fmt_num(value: Option<f64>) -> String {
    match value {
        None => "-".to_string(),
        Some(number) if number.fract() == 0.0 => format!("{number:.0}"),
        Some(number) => format!("{number}"),
    }
}

/// 比分 / 平均分这类顶层数字
fn fmt_header_num(value: Option<f64>) -> String {
    match value {
        None => "-".to_string(),
        Some(number) => fmt_num(Some(number)),
    }
}

/// 渲染一帧对局数据。`stats` 为空时只出基础列。
pub fn render_match(info: &MatchInfo, stats: &crate::model::StatsMap, show_stats: bool) -> String {
    let teams = info.teams();
    let mut out = String::new();

    out.push('\n');
    out.push_str(&"=".repeat(RULE));
    out.push('\n');
    out.push_str(&format!(
        "对局 {}  |  {}  |  类型 {}  |  开始 {}  |  比分 CT {} : {} T  |  平均分 {}",
        info.match_id().unwrap_or_else(|| "-".into()),
        info.map().unwrap_or_else(|| "-".into()),
        info.kind().unwrap_or_else(|| "-".into()),
        info.start_time().unwrap_or_else(|| "-".into()),
        fmt_header_num(info.ct_score()),
        fmt_header_num(info.terrorist_score()),
        fmt_header_num(info.ave_score()),
    ));
    out.push('\n');
    out.push_str(&"=".repeat(RULE));
    out.push('\n');

    // 表头
    let mut header = String::new();
    header.push_str(&pad_left("队伍", COL_TEAM));
    header.push_str(&pad_left("SteamID", COL_STEAMID));
    header.push_str(&pad_right("K", COL_KILL));
    header.push_str(&pad_right("D", COL_DEATH));
    header.push_str(&pad_right("A", COL_ASSIST));
    header.push_str(&pad_right("ADR", COL_ADR));
    header.push_str(&pad_right("爆头", COL_HEADSHOT));
    header.push_str(&pad_right("评分", COL_SCORE));
    if show_stats {
        header.push_str(&pad_right("ratingPro", COL_RATING));
        header.push_str(&pad_right("PP分", COL_PVP));
        header.push_str(&pad_right("地图胜率", COL_WINRATE));
    }
    out.push_str(&header);
    out.push('\n');
    out.push_str(&"-".repeat(RULE));
    out.push('\n');

    for (side, group) in [("CT", &teams.ct), ("T", &teams.t)] {
        for (index, player) in group.iter().enumerate() {
            out.push_str(&render_player_row(side, index == 0, player, stats, show_stats));
            out.push('\n');
        }
        out.push_str(&"-".repeat(RULE));
        out.push('\n');
    }

    out.push_str(&format!("名单人数：CT {} / T {}", teams.ct.len(), teams.t.len()));
    out.push('\n');
    out
}

fn render_player_row(
    side: &str,
    first_of_group: bool,
    player: &Player,
    stats: &crate::model::StatsMap,
    show_stats: bool,
) -> String {
    let mut row = String::new();
    row.push_str(&pad_left(if first_of_group { side } else { "" }, COL_TEAM));
    row.push_str(&pad_left(&player.steamid, COL_STEAMID));
    row.push_str(&pad_right(&fmt_num(player.kill), COL_KILL));
    row.push_str(&pad_right(&fmt_num(player.death), COL_DEATH));
    row.push_str(&pad_right(&fmt_num(player.assist), COL_ASSIST));
    row.push_str(&pad_right(&fmt_num(player.adr), COL_ADR));
    row.push_str(&pad_right(&fmt_num(player.headshot), COL_HEADSHOT));
    row.push_str(&pad_right(&fmt_num(player.score), COL_SCORE));

    if show_stats {
        let rating = stat_number(stats, &player.steamid, "ratingPro");
        let pvp = stats
            .get(&player.steamid)
            .and_then(|item| item.get("pvpScore"))
            .and_then(crate::model::as_text);
        let win_rate = stat_number(stats, &player.steamid, "mapWinRate");

        row.push_str(&pad_right(
            &match rating {
                Some(value) => format!("{value:.2}"),
                None => "-".to_string(),
            },
            COL_RATING,
        ));
        row.push_str(&pad_right(pvp.as_deref().unwrap_or("-"), COL_PVP));
        row.push_str(&pad_right(
            &match win_rate {
                Some(value) => format!("{:.0}%", value * 100.0),
                None => "-".to_string(),
            },
            COL_WINRATE,
        ));
    }

    row
}

/// 有人的阵营认不出来时给一句提示。
/// Python 版会静默丢掉这些人，表格人数对不上还查不出原因。
pub fn render_unknown_sides(teams: &Teams) -> Option<String> {
    if teams.unknown.is_empty() {
        return None;
    }
    let list: Vec<String> = teams
        .unknown
        .iter()
        .map(|player| format!("{}（{}）", player.steamid, player.side.label()))
        .collect();
    Some(format!(
        "有 {} 人的阵营认不出来，未计入表格（推送里的 side 可能变了）：{}",
        teams.unknown.len(),
        list.join("、")
    ))
}

/// 只在调试时用：说明这一帧是什么类型
pub fn describe_push_type() -> i64 {
    PUSH_TYPE
}

// ---------------------------------------------------------------- 战绩表（按昵称）
//
// 推送帧里**没有昵称**，昵称、K/D、评分、地图胜率全都只在战绩接口的返回里，
// 所以"人满之后出的那张表"必须以战绩接口为主数据源，推送帧只提供
// matchId / 地图 / 比分 / 名单人数。

/// 昵称列宽度。中文占 2 列，26 列 ≈ 13 个汉字，超了截断加省略号。
const COL_NICK: usize = 26;
const COL_RATING_PRO: usize = 9;
const COL_KD: usize = 6;
const COL_WE: usize = 5;
const COL_RATE: usize = 7;
/// 各列加起来正好 92，与分隔线同宽
const COL_WIN_RATE: usize = 8;
const RPT_PVP: usize = 6;

/// 渲染"按昵称出的战绩表"。
///
/// `info` 为 `None` 时只出地图与人数（离线回放一份战绩响应就是这种情况）。
pub fn render_report(
    info: Option<&MatchInfo>,
    report: &crate::model::StatsReport,
    loaded: usize,
    expected: usize,
) -> String {
    let map = report
        .map()
        .or_else(|| info.and_then(|info| info.map()))
        .unwrap_or_else(|| "-".into());

    let mut out = String::new();
    out.push('\n');
    out.push_str(&"=".repeat(RULE));
    out.push('\n');
    match info {
        Some(info) => out.push_str(&format!(
            "对局 {}  |  {}  |  类型 {}  |  比分 CT {} : {} T  |  名单 {loaded}/{expected}",
            info.match_id().unwrap_or_else(|| "-".into()),
            map,
            info.kind().unwrap_or_else(|| "-".into()),
            fmt_header_num(info.ct_score()),
            fmt_header_num(info.terrorist_score()),
        )),
        None => out.push_str(&format!("地图 {map}  |  名单 {loaded}/{expected}")),
    }
    out.push('\n');
    out.push_str(&"=".repeat(RULE));
    out.push('\n');

    let mut header = String::new();
    header.push_str(&pad_left("队伍", COL_TEAM));
    header.push_str(&pad_left("昵称", COL_NICK));
    header.push_str(&pad_right("ratingPro", COL_RATING_PRO));
    header.push_str(&pad_right("K/D", COL_KD));
    header.push_str(&pad_right("ADR", COL_ADR));
    header.push_str(&pad_right("WE", COL_WE));
    header.push_str(&pad_right("爆头率", COL_RATE));
    header.push_str(&pad_right("狙击率", COL_RATE));
    header.push_str(&pad_right("闪光率", COL_RATE));
    header.push_str(&pad_right("地图胜率", COL_WIN_RATE));
    header.push_str(&pad_right("PP分", RPT_PVP));
    out.push_str(&header);
    out.push('\n');
    out.push_str(&"-".repeat(RULE));
    out.push('\n');

    for (side, group) in [("CT", &report.ct), ("T", &report.t)] {
        for (index, player) in group.iter().enumerate() {
            out.push_str(&render_stat_row(side, index == 0, player));
            out.push('\n');
        }
        out.push_str(&"-".repeat(RULE));
        out.push('\n');
    }

    out.push_str(&format!(
        "名单人数：CT {} / T {}（共 {}）",
        report.ct.len(),
        report.t.len(),
        report.len()
    ));
    out.push('\n');

    if let Some(team) = &report.ct_team {
        out.push_str(&render_team_summary("CT", team, report));
        out.push('\n');
    }
    if let Some(team) = &report.t_team {
        out.push_str(&render_team_summary("T", team, report));
        out.push('\n');
    }
    out
}

fn render_stat_row(side: &str, first_of_group: bool, player: &crate::model::PlayerStat) -> String {
    let mut row = String::new();
    row.push_str(&pad_left(if first_of_group { side } else { "" }, COL_TEAM));
    row.push_str(&pad_left(
        &truncate_display(&player.display_name(), COL_NICK),
        COL_NICK,
    ));
    row.push_str(&pad_right(&fmt_fixed(player.rating_pro, 2), COL_RATING_PRO));
    row.push_str(&pad_right(&fmt_fixed(player.kd, 2), COL_KD));
    row.push_str(&pad_right(&fmt_fixed(player.adr, 1), COL_ADR));
    row.push_str(&pad_right(&fmt_fixed(player.we, 1), COL_WE));
    row.push_str(&pad_right(&fmt_pct(player.head_shot_rate), COL_RATE));
    row.push_str(&pad_right(&fmt_pct(player.snipe_rate), COL_RATE));
    row.push_str(&pad_right(&fmt_pct(player.flash_success_rate), COL_RATE));
    row.push_str(&pad_right(&fmt_pct(player.map_win_rate), COL_WIN_RATE));
    row.push_str(&pad_right(&fmt_fixed(player.pvp_score, 0), RPT_PVP));
    row
}

/// 队伍汇总一行。`best` 里存的是 SteamID，这里换成昵称 —— 用户明确不要看 SteamID。
fn render_team_summary(side: &str, team: &crate::model::TeamStat, report: &crate::model::StatsReport) -> String {
    let mut line = format!(
        "{side} 队伍汇总：ratingPro {}  K/D {}  ADR {}  WE {}  胜率 {}",
        fmt_fixed(team.rating_pro, 2),
        fmt_fixed(team.kd, 2),
        fmt_fixed(team.adr, 1),
        fmt_fixed(team.we, 1),
        fmt_pct(team.win_rate),
    );
    for (field, (steamid, value)) in &team.best {
        let who = report
            .find(steamid)
            .map(|player| player.display_name())
            .unwrap_or_else(|| "?".to_string());
        let label = match field.as_str() {
            "snipeRate" => "狙击率最高",
            "headShotRate" => "爆头率最高",
            "flashSuccessRate" => "闪光成功率最高",
            other => other,
        };
        // 响应里给的是小数文本，统一按百分比显示
        let shown = value
            .parse::<f64>()
            .map(|number| format!("{:.1}%", number * 100.0))
            .unwrap_or_else(|_| value.clone());
        line.push_str(&format!("  {label} {shown}（{who}）"));
    }
    line
}

/// 固定小数位；缺值给 `-`
fn fmt_fixed(value: Option<f64>, digits: usize) -> String {
    match value {
        Some(number) => format!("{number:.digits$}"),
        None => "-".to_string(),
    }
}

/// 小数 → 百分比；缺值给 `-`
fn fmt_pct(value: Option<f64>) -> String {
    match value {
        Some(number) => format!("{:.1}%", number * 100.0),
        None => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{classify, Frame, StatsMap};
    use serde_json::json;

    fn sample() -> MatchInfo {
        let text = json!({
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
                    {"steamId": "76561198000000001", "side": "CT", "kill": 12, "death": 8, "assist": 3, "adr": 95.3, "headshot": 4, "score": 1.45},
                    {"steamId": "76561198000000002", "side": "TERRORIST", "kill": 9, "death": 10, "assist": 2, "adr": 80, "headshot": 0, "score": 0.98}
                ]
            }
        })
        .to_string();
        let Frame::Match(info) = classify(&text) else { panic!("样本应该是对局帧") };
        *info
    }

    #[test]
    fn display_width_counts_cjk_as_two() {
        assert_eq!(display_width("ab"), 2);
        assert_eq!(display_width("中文"), 4);
        assert_eq!(display_width("CT 中文"), 7);
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn padding_uses_display_width_not_char_count() {
        assert_eq!(pad_left("中文", 6), "中文  ");
        assert_eq!(pad_left("abc", 5), "abc  ");
        assert_eq!(pad_right("中文", 6), "  中文");
        // 已经超过宽度就不截断，宁可撑开也不要截数据
        assert_eq!(pad_left("很长的中文内容", 4), "很长的中文内容");
    }

    /// 关键回归：表头和数据行必须等宽，中文表头以前是歪的
    #[test]
    fn header_and_rows_line_up() {
        let text = render_match(&sample(), &StatsMap::new(), false);
        let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
        let header = lines.iter().find(|line| line.starts_with("队伍")).expect("有表头");
        let ct_row = lines
            .iter()
            .find(|line| line.starts_with("CT") && line.contains("76561198000000001"))
            .expect("有 CT 行");
        assert_eq!(
            display_width(header),
            display_width(ct_row),
            "表头与数据行显示宽度不一致：\n{header}\n{ct_row}"
        );
    }

    #[test]
    fn renders_basic_columns_without_stats() {
        let text = render_match(&sample(), &StatsMap::new(), false);
        assert!(text.contains("对局 9215951389778120460"));
        assert!(text.contains("de_dust2"));
        assert!(text.contains("比分 CT 7 : 5 T"));
        assert!(text.contains("平均分 1.23"));
        assert!(text.contains("76561198000000001"));
        assert!(text.contains("95.3"));
        assert!(text.contains("名单人数：CT 1 / T 1"));
        // 不开 --stats 就不该有这几列
        assert!(!text.contains("ratingPro"));
    }

    #[test]
    fn merges_stats_into_the_table() {
        let mut stats = StatsMap::new();
        stats.insert(
            "76561198000000001".to_string(),
            json!({"ratingPro": 1.234, "pvpScore": "1800", "mapWinRate": 0.5}),
        );
        let text = render_match(&sample(), &stats, true);
        assert!(text.contains("ratingPro"));
        assert!(text.contains("1.23"));
        assert!(text.contains("1800"));
        assert!(text.contains("50%"));
        // 没有战绩的那个人给 "-"，不是崩掉
        let rows: Vec<&str> = text.lines().filter(|line| line.contains("76561198000000002")).collect();
        assert!(rows[0].ends_with("-"), "缺战绩的人应补 -，实际：{}", rows[0]);
    }

    #[test]
    fn missing_fields_render_as_dash_not_none() {
        let mut info = sample();
        // 把人造帧里的 headshot/adr 抹掉，模拟字段缺失
        if let Some(players) = info.raw.get_mut("playerList").and_then(|v| v.as_array_mut()) {
            players[0].as_object_mut().unwrap().remove("headshot");
        }
        let text = render_match(&info, &StatsMap::new(), false);
        let row = text
            .lines()
            .find(|line| line.starts_with("CT"))
            .expect("有 CT 行");
        assert!(!row.contains("None"), "不该出现 Python 的 None：{row}");
        assert!(row.contains('-'), "缺字段应显示 -：{row}");
    }

    #[test]
    fn headshot_zero_is_not_hidden() {
        let text = render_match(&sample(), &StatsMap::new(), false);
        let row = text
            .lines()
            .find(|line| line.contains("76561198000000002"))
            .expect("有 T 行");
        // Python 的 `value or '-'` 会把 0 变成 '-'，这里应该显示 0
        assert!(row.contains('0'), "headshot=0 应该显示 0：{row}");
    }

    #[test]
    fn unknown_sides_are_reported() {
        let mut info = sample();
        if let Some(players) = info.raw.get_mut("playerList").and_then(|v| v.as_array_mut()) {
            players[0]["side"] = json!("旁观");
        }
        let teams = info.teams();
        assert_eq!(teams.unknown.len(), 1);
        let warning = render_unknown_sides(&teams).expect("应该给提示");
        assert!(warning.contains("76561198000000001"));
        assert!(warning.contains("旁观"));
        // 都认得出来时不该有提示
        assert!(render_unknown_sides(&sample().teams()).is_none());
    }

    #[test]
    fn empty_frame_renders_without_panicking() {
        let info = MatchInfo::new(json!({}));
        let text = render_match(&info, &StatsMap::new(), false);
        assert!(text.contains("名单人数：CT 0 / T 0"));
    }

    // ---------------------------------------------------------- 战绩表（按昵称）

    fn sample_report() -> crate::model::StatsReport {
        crate::model::StatsReport::from_value(&json!({
            "code": 1,
            "result": {
                "ctPlayerStatsDTOList": [{
                    "steamId": "76561198000000001", "nickname": "甲", "map": "de_dust2",
                    "kd": 1.0743, "ratingPro": 1.108, "adr": 80.1, "we": 8.8,
                    "mapWinRate": 0.5714, "snipeRate": 0.0802, "headShotRate": 0.5328,
                    "flashSuccessRate": 0.8218, "pvpScore": 1725
                }],
                "tplayerStatsDTOList": [{
                    "steamId": "76561198000000002",
                    "nickname": "汉东省刑侦大队第一狙击手祁同伟", "map": "de_dust2",
                    "kd": 0.88, "ratingPro": 0.973, "adr": 72.9, "we": 7.5,
                    "mapWinRate": 0.46, "pvpScore": 1719
                }],
                "ctTeamDTO": {
                    "ratingPro": 1.13, "kd": 1.04, "adr": 82.96, "we": 8.9, "winRate": 0.49,
                    "snipeRate": {"first": "76561198000000001", "second": "0.26"}
                }
            }
        }))
    }

    #[test]
    fn truncate_keeps_display_width_within_budget() {
        assert_eq!(truncate_display("短名字", 10), "短名字");
        let long = "汉东省刑侦大队第一狙击手祁同伟";
        let cut = truncate_display(long, COL_NICK);
        assert!(display_width(&cut) <= COL_NICK, "截断后 {cut} 宽 {} 列", display_width(&cut));
        assert!(cut.ends_with('…'));
        // 不能把双宽字符切成半个
        assert!(long.starts_with(cut.trim_end_matches('…')));
        // 刚好放得下就不动它
        assert_eq!(truncate_display("12345", 5), "12345");
    }

    #[test]
    fn report_table_shows_nicknames_and_no_steamids() {
        let report = sample_report();
        let text = render_report(None, &report, 10, 10);

        // 表头与列
        assert!(text.contains("地图 de_dust2  |  名单 10/10"), "{text}");
        for column in ["昵称", "ratingPro", "K/D", "ADR", "WE", "爆头率", "狙击率", "闪光率", "地图胜率", "PP分"] {
            assert!(text.contains(column), "少了列 {column}");
        }

        // 昵称在，SteamID 一个都不能出现 —— 这是用户明确要求的
        assert!(text.contains('甲'));
        assert!(!text.contains("7656119"), "不该输出 SteamID：\n{text}");

        // 数值格式
        assert!(text.contains("1.11"), "ratingPro 两位小数");
        assert!(text.contains("53.3%"), "爆头率按百分比");
        assert!(text.contains("57.1%"), "地图胜率按百分比");
        assert!(text.contains("1725"), "PP分原样");

        // 长昵称被截断，没有把表撑爆
        for line in text.lines() {
            assert!(display_width(line) <= RULE, "有行超宽（{} 列）：{line}", display_width(line));
        }
    }

    #[test]
    fn report_summary_resolves_best_player_to_nickname() {
        let text = render_report(None, &sample_report(), 10, 10);
        assert!(text.contains("CT 队伍汇总"));
        assert!(text.contains("ratingPro 1.13"));
        assert!(text.contains("胜率 49.0%"));
        // "最高"里的 SteamID 要换成昵称
        assert!(text.contains("狙击率最高 26.0%（甲）"), "{text}");
        assert!(!text.contains("7656119"));
    }

    #[test]
    fn report_accepts_a_push_header_when_available() {
        let report = sample_report();
        let text = render_report(Some(&sample()), &report, 10, 10);
        assert!(text.contains("对局 9215951389778120460"));
        assert!(text.contains("比分 CT 7 : 5 T"));
        assert!(text.contains("名单 10/10"));
    }

    /// 回归：战绩表的表头与数据行必须等宽。
    /// 之前表头的 PP分 用了实时表那套 7 宽的常量、数据行用 6 宽，整整错开 1 列。
    #[test]
    fn report_header_and_rows_line_up() {
        let report = sample_report();
        let text = render_report(None, &report, 10, 10);
        let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
        let header = lines.iter().find(|line| line.starts_with("队伍")).expect("有表头");
        let rows: Vec<&&str> = lines
            .iter()
            .filter(|line| {
                // 只挑表格里的选手行；"CT 队伍汇总…" 是自由格式的一行，不参与对齐
                (line.starts_with("CT") || line.starts_with("T ")) && !line.contains("汇总")
            })
            .collect();
        assert!(!rows.is_empty(), "应该有数据行");
        for row in rows {
            assert_eq!(
                display_width(header),
                display_width(row),
                "表头与数据行不等宽：\n{header}\n{row}"
            );
        }
    }

    /// 每一格必须正好填满它的列宽，多一列少一列都会让整张表错位
    #[test]
    fn report_header_cells_fill_their_columns() {
        let cells: [(&str, usize); 11] = [
            ("队伍", COL_TEAM),
            ("昵称", COL_NICK),
            ("ratingPro", COL_RATING_PRO),
            ("K/D", COL_KD),
            ("ADR", COL_ADR),
            ("WE", COL_WE),
            ("爆头率", COL_RATE),
            ("狙击率", COL_RATE),
            ("闪光率", COL_RATE),
            ("地图胜率", COL_WIN_RATE),
            ("PP分", RPT_PVP),
        ];
        for (text, width) in cells {
            assert_eq!(display_width(&pad_right(text, width)), width, "「{text}」这一格宽度不对");
        }
        let total: usize = cells.iter().map(|(_, width)| *width).sum();
        assert_eq!(total, RULE, "列宽总和必须等于分隔线宽度，否则表格右边会多出一截");
    }

    #[test]
    fn report_with_empty_stats_still_renders() {
        let text = render_report(None, &crate::model::StatsReport::default(), 0, 10);
        assert!(text.contains("名单 0/10"));
        assert!(text.contains("昵称"));
        // 一行数据都没有，但表头必须在（调用方据此判断"还没数据"）
    }
}
