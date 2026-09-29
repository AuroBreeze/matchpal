//! 从 `config.local.json` 里读 token 与 steamid
//!
//! 键名候选顺序是照抄 Python 版踩出来的 —— 客户端、网页端、安卓 App 写出来的
//! 键名不一样（`access_token` / `Access_Token` / `steam_cn_token` …），
//! 换顺序就可能读不到。

use std::path::Path;

use serde_json::Value;

/// 日志里脱敏用：保留前 10 个字符
const MASK_KEEP: usize = 10;

/// token 的候选键名，顺序与 Python 版一致
const TOKEN_KEYS: &[&str] = &[
    "access_token",
    "Access_Token",
    "steam_cn_token",
    "Steam_Cn_Token",
    "token",
];

/// steamid 的候选键名，顺序与 Python 版一致
const STEAMID_KEYS: &[&str] = &["uid", "steamid", "steamId", "loginSteamId", "pwasteamid"];

/// 账号凭据。缺失就是空串，由调用方决定该报什么错。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub token: String,
    pub steamid: String,
}

impl Credentials {
    pub fn has_token(&self) -> bool {
        !self.token.is_empty()
    }

    pub fn has_steamid(&self) -> bool {
        !self.steamid.is_empty()
    }

    /// 脱敏后的 token，给日志用
    pub fn masked_token(&self) -> String {
        mask(&self.token)
    }
}

/// 读配置失败：只有"读不了"和"不是合法 JSON"两种，文件不存在不算错
#[derive(Debug)]
pub enum ConfigError {
    Read(std::io::Error),
    Parse(serde_json::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read(err) => write!(f, "读配置文件失败：{err}"),
            ConfigError::Parse(err) => write!(f, "配置文件不是合法 JSON：{err}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// 读配置。**文件不存在返回空凭据而不是错误** —— 等价于"还没抓过 token"，
/// 让调用方去决定是提示先跑抓取、还是直接报缺 token。
pub fn load(path: &Path) -> Result<Credentials, ConfigError> {
    if !path.is_file() {
        return Ok(Credentials::default());
    }
    let text = std::fs::read_to_string(path).map_err(ConfigError::Read)?;
    parse(&text)
}

/// 从 JSON 文本解析凭据（抽出来是为了能单测）
pub fn parse(text: &str) -> Result<Credentials, ConfigError> {
    let value: Value = serde_json::from_str(text).map_err(ConfigError::Parse)?;
    Ok(extract(&value))
}

/// 按候选键序取第一个非空值，取不到再退回 `source_url` 查询串里找 steamid
pub fn extract(value: &Value) -> Credentials {
    let token = TOKEN_KEYS
        .iter()
        .find_map(|key| value.get(*key).and_then(scalar_text))
        .unwrap_or_default();

    let steamid = STEAMID_KEYS
        .iter()
        .find_map(|key| value.get(*key).and_then(scalar_text))
        .filter(|text| is_steamid(text))
        .or_else(|| {
            let url = value.get("source_url").and_then(scalar_text)?;
            steamid_from_query(&url)
        })
        .unwrap_or_default();

    Credentials { token, steamid }
}

/// JSON 里可能是字符串也可能是数字，Python 用 `str()` 一把梭，这里对齐这个语义
fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// SteamID64：正好 17 位纯数字
pub fn is_steamid(text: &str) -> bool {
    text.len() == 17 && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// 从 `https://…?uid=7656119…` 这类网址里捞出 17 位数字
///
/// 只做 `&`/`=` 切分，不做百分号解码 —— 目标是纯数字，解不解码结果一样。
fn steamid_from_query(url: &str) -> Option<String> {
    let (_, query) = url.split_once('?')?;
    query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .map(|(_, value)| value.trim())
        .find(|value| is_steamid(value))
        .map(|value| value.to_string())
}

fn mask(token: &str) -> String {
    if token.chars().count() > MASK_KEEP {
        let head: String = token.chars().take(MASK_KEEP).collect();
        format!("{head}…")
    } else {
        token.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_not_an_error() {
        let path = std::env::temp_dir().join("match-watcher-绝不存在的配置.json");
        let credentials = load(&path).expect("缺文件不该报错");
        assert_eq!(credentials, Credentials::default());
        assert!(!credentials.has_token());
    }

    #[test]
    fn reads_token_and_steamid_from_the_usual_keys() {
        let credentials = parse(r#"{"access_token":"abc123","steamid":"76561198000000000"}"#).unwrap();
        assert_eq!(credentials.token, "abc123");
        assert_eq!(credentials.steamid, "76561198000000000");
    }

    /// 键名候选顺序来自 Python 版：先进先出，第一个非空的赢
    #[test]
    fn token_key_order_is_respected() {
        let text = r#"{"token":"最后兜底","steam_cn_token":"次选","access_token":"首选"}"#;
        assert_eq!(parse(text).unwrap().token, "首选");

        // 首选缺失时落到下一个
        let text = r#"{"steam_cn_token":"次选","token":"最后兜底"}"#;
        assert_eq!(parse(text).unwrap().token, "次选");

        // 大小写变体
        let text = r#"{"Access_Token":"变体"}"#;
        assert_eq!(parse(text).unwrap().token, "变体");
    }

    #[test]
    fn numeric_token_is_accepted_like_python_str() {
        // Python 版是 str(data[key])，数字也能用
        let credentials = parse(r#"{"uid":76561198000000000,"access_token":123456}"#).unwrap();
        assert_eq!(credentials.token, "123456");
        assert_eq!(credentials.steamid, "76561198000000000");
    }

    #[test]
    fn steamid_must_be_17_digits() {
        assert_eq!(parse(r#"{"uid":"12345"}"#).unwrap().steamid, "");
        assert_eq!(parse(r#"{"uid":"7656119800000000x"}"#).unwrap().steamid, "");
        assert_eq!(parse(r#"{"uid":"76561198000000000"}"#).unwrap().steamid, "76561198000000000");
    }

    /// 实测抓包里 steamid 常常只在 source_url 的查询串上
    #[test]
    fn falls_back_to_source_url_query() {
        let text = r#"{"access_token":"abc","source_url":"https://pwaweblogin.wmpvp.com/match-api/calendar?a=20000&uid=76561198000000000&r=1"}"#;
        let credentials = parse(text).unwrap();
        assert_eq!(credentials.steamid, "76561198000000000");

        // 显式字段优先于 URL 兜底
        let text = r#"{"steamid":"76561198111111111","source_url":"https://x/y?uid=76561198000000000"}"#;
        assert_eq!(parse(text).unwrap().steamid, "76561198111111111");
    }

    #[test]
    fn broken_json_is_an_error() {
        assert!(matches!(parse("{ 不是 JSON"), Err(ConfigError::Parse(_))));
    }

    #[test]
    fn masked_token_keeps_only_the_head() {
        // 值与真实 token 无关，只保留「40 位十六进制」的形态
        let credentials = Credentials {
            token: "0123456789abcdef0123456789abcdef01234567".into(),
            steamid: String::new(),
        };
        assert_eq!(credentials.masked_token(), "0123456789…");
        // 短 token 原样返回，不要弄出个看不懂的省略号
        let short = Credentials { token: "abc".into(), steamid: String::new() };
        assert_eq!(short.masked_token(), "abc");
    }
}
