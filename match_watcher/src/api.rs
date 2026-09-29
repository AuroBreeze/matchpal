//! 完美世界电竞对局接口（`appactivity.wmpvp.com`）
//!
//! 请求头按抓包复刻（Android WebView / EsportsApp 4.1.2.218）。
//!
//! # 关于 TLS
//!
//! 走 `native-tls`（Windows 上是 schannel，读 **Windows 证书库**），
//! 所以本机装着中间人 CA 时能直接过 —— **不需要** Python 版那个
//! 「先正常校验，`SSLError` 就降级成不校验」的兜底。那种降级会静默关掉
//! 证书校验，等于对任何中间人都放行，能不要就不要。
//!
//! 注意 ureq 3 的 crate 级便捷函数（`ureq::get`）**不会**用 native-tls，
//! 必须在 `Agent` 上显式配 `TlsProvider::NativeTls`。

use std::time::Duration;

use serde_json::{json, Value};

use crate::model::StatsReport;

/// 拿 websocket 地址
pub const WS_INFO_URL: &str =
    "https://appactivity.wmpvp.com/steamcn/match/watchStage/getWebsocketInfo";
/// 对局战绩
pub const STATS_URL: &str =
    "https://appactivity.wmpvp.com/steamcn/match/watchStage/getPvPMatchTeamStatisticsData";
/// 请求头里的 Origin / Referer，照抄抓包
pub const ORIGIN: &str = "https://news.wmpvp.com";
pub const APP_VERSION: &str = "4.1.2.218";
/// 客户端标识。Python 版写的是 `python-match-watcher`，这里如实写 rust。
pub const DEVICE: &str = "rust-match-watcher";

const TIMEOUT: Duration = Duration::from_secs(20);
/// 报错信息里响应体最多带多少字符
const BODY_BRIEF: usize = 300;

/// 接口调用失败
#[derive(Debug)]
pub enum ApiError {
    /// 连不上 / DNS / TLS 等传输层问题
    Transport(String),
    /// HTTP 状态码不是 2xx
    Status { code: u16, body: String },
    /// 通了但业务码不是成功值（token 过期/参数不对都会走这里）
    Business { code: String, message: String },
    /// 响应不是合法 JSON
    Json(serde_json::Error),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Transport(err) => write!(f, "请求失败：{err}"),
            ApiError::Status { code, body } => write!(f, "HTTP {code}：{body}"),
            ApiError::Business { code, message } => {
                write!(f, "业务码 code={code} message={message}")
            }
            ApiError::Json(err) => write!(f, "响应不是合法 JSON：{err}"),
        }
    }
}

impl std::error::Error for ApiError {}

/// 一个新的接口客户端。`token` 必填，`steamid` 可为空（只有带 steamid 的
/// 请求才会发 `pwasteamid` 头）。
pub struct Api {
    agent: ureq::Agent,
    token: String,
    steamid: String,
}

impl Api {
    pub fn new(token: impl Into<String>, steamid: impl Into<String>) -> Self {
        let config = ureq::config::Config::builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::NativeTls)
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .timeout_global(Some(TIMEOUT))
            // 关掉"非 2xx 就当 Err"：状态码和响应体要一起给人看
            .http_status_as_error(false)
            .build();
        Self {
            agent: config.new_agent(),
            token: token.into(),
            steamid: steamid.into(),
        }
    }

    /// 抓包里的请求头。顺序不重要，内容要对。
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = vec![
            ("accessToken", self.token.clone()),
            ("device", DEVICE.to_string()),
            ("appversion", APP_VERSION.to_string()),
            ("platform", "h5_pc".to_string()),
            ("appTheme", "0".to_string()),
            ("X-Requested-With", "XMLHttpRequest".to_string()),
            ("Origin", ORIGIN.to_string()),
            ("Referer", format!("{ORIGIN}/")),
            ("Accept", "application/json, text/plain, */*".to_string()),
        ];
        if !self.steamid.is_empty() {
            headers.push(("pwasteamid", self.steamid.clone()));
        }
        headers
    }

    /// `GET getWebsocketInfo?steamId=<自己>&platform=2` → `wss://…`
    pub fn websocket_url(&self, platform: u32) -> Result<String, ApiError> {
        let mut request = self
            .agent
            .get(WS_INFO_URL)
            .query("steamId", self.steamid.as_str())
            .query("platform", platform.to_string());
        for (key, value) in self.headers() {
            request = request.header(key, value);
        }
        let value = read_json(request.call())?;
        ensure_success(&value)?;
        value
            .get("result")
            .and_then(|result| result.get("websocketUrl"))
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .map(str::to_string)
            .ok_or_else(|| ApiError::Business {
                code: code_text(&value),
                message: format!("响应里没有 websocketUrl：{}", brief(&value)),
            })
    }

    /// 查双方队伍战绩。昵称、K/D、评分、地图胜率都在响应里 ——
    /// 推送帧是没有昵称的，所以最终那张按昵称出的表必须靠这个接口。
    pub fn team_stats(
        &self,
        ct_steamids: &[String],
        t_steamids: &[String],
        map: &str,
    ) -> Result<StatsReport, ApiError> {
        let body = json!({
            "ctTeamSteamIds": ct_steamids,
            "teTeamSteamIds": t_steamids,
            "map": map,
        });
        let mut request = self.agent.post(STATS_URL);
        for (key, value) in self.headers() {
            request = request.header(key, value);
        }
        let value = read_json(request.send_json(&body))?;
        ensure_success(&value)?;
        Ok(StatsReport::from_value(&value))
    }
}

/// 状态码 → 响应体 → JSON
fn read_json(result: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Value, ApiError> {
    let mut response = result.map_err(|err| ApiError::Transport(err.to_string()))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|err| ApiError::Transport(err.to_string()))?;
    if !(200..300).contains(&status) {
        return Err(ApiError::Status { code: status, body: brief_text(&text) });
    }
    serde_json::from_str(&text).map_err(ApiError::Json)
}

/// 业务码必须等于 1（`{"code":1,"message":"success",…}`）
fn ensure_success(value: &Value) -> Result<(), ApiError> {
    if value.get("code").and_then(Value::as_i64) == Some(1) {
        return Ok(());
    }
    let code = code_text(value);
    let message = value
        .get("message")
        .or_else(|| value.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if message.is_empty() {
        return Err(ApiError::Business { code, message: brief(value) });
    }
    Err(ApiError::Business { code, message })
}

fn code_text(value: &Value) -> String {
    match value.get("code") {
        Some(code) => code.to_string(),
        None => "(无 code)".to_string(),
    }
}

fn brief(value: &Value) -> String {
    brief_text(&value.to_string())
}

/// 截断长文本，避免把整个响应体糊到日志里
fn brief_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= BODY_BRIEF {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(BODY_BRIEF).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(steamid: &str) -> Api {
        Api::new("测试token", steamid)
    }

    fn header_of<'a>(headers: &'a [(&'static str, String)], key: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn headers_match_the_captured_request() {
        let headers = api("76561198000000000").headers();
        assert_eq!(header_of(&headers, "accessToken"), Some("测试token"));
        assert_eq!(header_of(&headers, "platform"), Some("h5_pc"));
        assert_eq!(header_of(&headers, "appTheme"), Some("0"));
        assert_eq!(header_of(&headers, "Origin"), Some(ORIGIN));
        assert_eq!(header_of(&headers, "Referer"), Some("https://news.wmpvp.com/"));
        assert_eq!(header_of(&headers, "appversion"), Some(APP_VERSION));
    }

    #[test]
    fn pwasteamid_only_when_steamid_is_known() {
        // token 校验那一步（run_all 的 --check）是拿 steamid="0" 或空串发的
        assert_eq!(header_of(&api("76561198000000000").headers(), "pwasteamid"), Some("76561198000000000"));
        assert_eq!(header_of(&api("").headers(), "pwasteamid"), None);
    }

    #[test]
    fn business_code_must_be_one() {
        assert!(ensure_success(&json!({"code": 1, "message": "success"})).is_ok());

        let err = ensure_success(&json!({"code": 1002, "message": "token invalid"})).unwrap_err();
        match err {
            ApiError::Business { code, message } => {
                assert_eq!(code, "1002");
                assert_eq!(message, "token invalid");
            }
            other => panic!("应该是业务错误，实际 {other:?}"),
        }
    }

    #[test]
    fn business_error_without_message_still_says_something() {
        let err = ensure_success(&json!({"code": 500})).unwrap_err();
        assert!(err.to_string().contains("500"), "{err}");
    }

    #[test]
    fn brief_truncates_long_bodies() {
        let short = brief_text("短");
        assert_eq!(short, "短");
        let long = brief_text(&"x".repeat(1000));
        assert!(long.chars().count() <= BODY_BRIEF + 1, "实际 {} 字符", long.chars().count());
        assert!(long.ends_with('…'));
    }
}
