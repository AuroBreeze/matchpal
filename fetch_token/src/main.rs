//! access_token
//!
//! 执行流程:
//!   1. 自动提权(ShellExecuteW runas，弹一次 UAC)
//!   2. 运行时用 rcgen **现场生成一把自己的 CA**(每个用户各自一把，绝不分发私钥)
//!   3. 把 CA 装进根证书库(certutil)
//!   4. 把系统代理临时指向本机监听端口(winreg + InternetSetOptionW 通知)
//!   5. hudsucker 拦截 HTTP/S，扫描 URL / 请求头 / Set-Cookie 里的 token
//!   6. 命中 → 写 config.local.json → 还原系统代理 + 卸载证书
//!
//! NOTE: 上游 TLS 用 `with_native_tls_connector()`(Windows 上是 schannel)
//! 它读的是 **Windows 证书库** —— 本机装了 HTTPS 中间人 CA，只有走系统库才连得上
//! 换成 rustls 的默认根证书会报 `invalid peer certificate: UnknownIssuer`
//!
//! 用法：
//!   cargo run -p fetch_token --release -- --help
//!   cargo run -p fetch_token --release                   # 一键：提权 + 装证书 + 设代理 + 抓 + 清理
//!   cargo run -p fetch_token --release -- --verbose --log-all capture/flows.log

mod platform;
mod args_handler;
mod ca;
mod proxy;
mod pause;
mod push;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::BodyExt;
use hudsucker::hyper::{HeaderMap, Request, Response};
use hudsucker::{Body, HttpContext, HttpHandler, Proxy, RequestOrResponse};
use logkit::{debug, error, info, trace, warn};
use tokio::sync::mpsc;

use crate::ca::{Guard, creat_user_ca};


/// 从 `https://host:port/path?query` 取 host；origin-form(`/path`)取不到，返回 None
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split(['/', '?']).next().unwrap_or("");

    // https://user:password@example.com:8080/foo 
    let authority = authority.rsplit('@').next().unwrap_or(authority);

    // IPV4
    // example.com:8080
    // 192.168.1.10:8080
    // IPV6 
    // [::1]:8080
    // 区分IPV4与IPV6
    let host = if let Some(end) = authority.find(']') {
        &authority[..=end]
    } else {
        authority.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

// ---------------------------------------------------------------- token 识别
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn parse_pairs(text: &str) -> Vec<(String, String)> {
    text.split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let (key, value) = part.split_once('=')?;
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

/// 从 URL 查询串里取某个键的值
fn url_query_value(url: &str, name: &str) -> Option<String> {
    let (_, query) = url.split_once('?')?;
    parse_pairs(query)
        .into_iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
}

fn strong_names() -> &'static [&'static str] {
    &["access_token", "steam_cn_token", "accesstoken", "pvp_app_token"]
}

/// 弱匹配(token)容易撞上 CSRF/一次性 token，用长度+字符集过滤
fn plausible(key: &str, value: &str) -> bool {
    if value.is_empty() || value.len() > 2048 {
        return false;
    }
    if strong_names().contains(&key.to_ascii_lowercase().as_str()) {
        return true;
    }
    value.len() >= 24 && value.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn is_steamid(value: &str) -> bool {
    value.len() == 17 && value.chars().all(|c| c.is_ascii_digit())
}

/// 账号 id 的字段名
///
/// 只按「17 位数字」认会把无关站点上的订单号、缓存键也记成 steamid，而 extras 是
/// **跨请求共享**的一次污染就会把错的 id 写进最终配置
const COMPANION_NAMES: &[&str] = &["loginsteamid", "pwasteamid", "steamid", "uid"];

fn companion_steamid(key: &str, value: &str) -> Option<String> {
    let key = key.to_ascii_lowercase();
    if COMPANION_NAMES.contains(&key.as_str()) && is_steamid(value) {
        Some(value.to_string())
    } else {
        None
    }
}

/// 单次解析的 body 上限: 再大就不是接口响应，而是文件下载了
const BODY_TEXT_LIMIT: u64 = 512_000;

/// 递归吐出 JSON 里的 (键, 字符串值)。深度和数量都设上限，防止大响应拖慢抓包
fn walk_json_pairs(node: &serde_json::Value, depth: usize, out: &mut Vec<(String, String)>) {
    if depth > 4 {
        return;
    }
    match node {
        serde_json::Value::Object(map) => {
            for (key, value) in map.iter().take(200) {
                if let Some(text) = value.as_str()
                    && !text.is_empty()
                {
                    out.push((key.clone(), text.to_string()));
                }
                walk_json_pairs(value, depth + 1, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter().take(50) {
                walk_json_pairs(item, depth + 1, out);
            }
        }
        _ => {}
    }
}

/// JSON 文本 → (键, 字符串值)。先做一次 "token" 预筛：字段名不含 token 就不可能命中，
/// 没必要把每个响应都解析一遍。
fn json_pairs(text: &str) -> Vec<(String, String)> {
    if !text.to_ascii_lowercase().contains("token") {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_json_pairs(&value, 0, &mut out);
    out
}

/// 按 Content-Type 解析 body 文本(表单 / JSON)。
fn parse_text_pairs(content_type: &str, text: &str) -> Vec<(String, String)> {
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("x-www-form-urlencoded") {
        parse_pairs(text)
    } else if content_type.contains("json") {
        json_pairs(text)
    } else {
        Vec::new()
    }
}

fn header_text(headers: &HeaderMap, name: &str) -> String {
    headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
}

fn content_length_of(headers: &HeaderMap) -> Option<u64> {
    header_text(headers, "content-length").trim().parse::<u64>().ok()
}

fn body_is_text_like(content_type: &str) -> bool {
    let content_type = content_type.to_ascii_lowercase();
    content_type.contains("json") || content_type.contains("x-www-form-urlencoded")
}

/// body 能不能安全地读出来
///
/// - **必须自带 Content-Length**：没有长度的是 chunked / 流式 body。读它有两种坏结果 ——
///   流式响应(SSE、长轮询)会把连接一直挂住；chunked 请求读完后 body 变成定长，
///   与原有的 `Transfer-Encoding: chunked` 头对不上
/// - **不超上限**：超过 512KB 的不是接口响应，而是文件下载
fn body_is_readable(content_type: &str, content_length: Option<u64>) -> bool {
    body_is_text_like(content_type) && content_length.is_some_and(|len| len <= BODY_TEXT_LIMIT)
}

/// 请求体：在 body_is_readable 之上再限定「会带 body 的方法」
fn request_body_is_readable(method: &str, content_type: &str, content_length: Option<u64>) -> bool {
    matches!(method, "POST" | "PUT" | "PATCH") && body_is_readable(content_type, content_length)
}

/// 响应体：只认 JSON(表单响应对抓 token 没意义)
fn response_body_is_readable(content_type: &str, content_length: Option<u64>) -> bool {
    content_type.to_ascii_lowercase().contains("json") && content_length.is_some_and(|len| len <= BODY_TEXT_LIMIT)
}

/// 收集 body 文本并**原样重建** body(hudsucker 的 Body 实现了 HttpBody，可从 Bytes 还原)
/// 收集失败时原 body 已被消费、无法恢复，返回 None，调用方用空 body 兜底
async fn collect_body_text(body: Body) -> Option<(String, Body)> {
    let collected = body.collect().await.ok()?;
    let bytes = collected.to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    Some((text, Body::from(bytes)))
}

#[derive(Debug, Clone)]
struct Hit {
    url: String,
    fields: Vec<(String, String)>,
    steamid: Option<String>,
    from_response: bool,
}

// ---------------------------------------------------------------- 拦截处理器
#[derive(Clone)]
struct TokenHandler {
    names: Arc<Vec<String>>,
    /// 空 = 不过滤(`--any-host`) 非空时只收这些域名及其子域
    hosts: Arc<Vec<String>>,
    tx: mpsc::UnboundedSender<Hit>,
    extras: Arc<Mutex<HashMap<String, String>>>,
    log_all: Option<Arc<Mutex<std::fs::File>>>,
    last_url: Arc<Mutex<String>>,
    /// 字段名命中、但域名被白名单挡下的候选(去重)，退出时汇总给用户看
    ignored: Arc<Mutex<Vec<String>>>,
}

/// 字段名是否算命中。泛化短名(如 `token`)只允许精确匹配：它一旦参与后缀匹配，
/// `userauthtoken` / `csrftoken` / `xsrf_token` 之类全都撞进来
fn name_matches(names: &[String], key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    names.iter().any(|n| {
        if key == *n {
            return true;
        }
        let generic = n.len() < 8 && !n.contains('_') && !n.contains('-');
        !generic && key.ends_with(n.as_str())
    })
}

/// 主机是否在白名单内(空名单 = 放行)。取不到主机时也放行，宁可多扫，不要漏抓
fn host_matches(hosts: &[String], url: &str) -> bool {
    if hosts.is_empty() {
        return true;
    }
    let Some(host) = host_of(url) else { return true };
    //  必须带上 "." 前缀比较，否则 evilwanmei.com 也会被 wanmei.com 放行
    hosts
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

impl TokenHandler {
    fn is_token_name(&self, key: &str) -> bool {
        name_matches(&self.names, key)
    }

    fn host_allowed(&self, url: &str) -> bool {
        host_matches(&self.hosts, url)
    }

    /// 字段名命中了、但主机不在白名单：记下来(去重)，debug 级别下当场说一声。
    /// 只记不报的话，没开 debug 的用户看到「没抓到」时分不清是白名单太窄还是没流量
    fn report_ignored(&self, url: &str, fields: &[(String, String)]) {
        let host = host_of(url).unwrap_or_else(|| url.to_string());
        let names: Vec<&str> = fields.iter().map(|(key, _)| key.as_str()).collect();
        let line = format!("{host} 的 {}", names.join(", "));
        if let Ok(mut seen) = self.ignored.lock()
            && seen.len() < 20
            && !seen.contains(&line)
        {
            seen.push(line.clone());
        }
        debug!("已忽略：{line}(主机不在白名单；请通过 --hosts 追加或使用 --any-host 取消限制)");
    }

    fn log_line(&self, line: &str) {
        if let Some(file) = &self.log_all
            && let Ok(mut handle) = file.lock()
        {
            use std::io::Write;
            let _ = writeln!(handle, "{line}");
        }
    }

    fn remember(&self, key: &str, value: &str) {
        if let Ok(mut extras) = self.extras.lock() {
            extras.entry(key.to_string()).or_insert_with(|| value.to_string());
        }
    }

    /// `body` 是 (content-type, 文本)，由调用方读好传进来 —— 读 body 是异步的，
    /// 这一层保持同步，便于测试
    fn scan_request(&self, req: &Request<Body>, body: Option<(&str, &str)>) -> Hit {
        let url = req.uri().to_string();
        let mut fields: Vec<(String, String)> = Vec::new();
        let mut steamid: Option<String> = None;

        if let Some(query) = req.uri().query() {
            for (key, value) in parse_pairs(query) {
                if let Some(sid) = companion_steamid(&key, &value) {
                    steamid.get_or_insert(sid);
                }
                if self.is_token_name(&key) && plausible(&key, &value) {
                    fields.push((key, value));
                }
            }
        }

        for (name, value) in req.headers() {
            let name_str = name.as_str();
            let Ok(value_str) = value.to_str() else { continue };
            if name_str.eq_ignore_ascii_case("cookie") {
                for (key, cookie_value) in parse_pairs(&value_str.replace("; ", "&")) {
                    if let Some(sid) = companion_steamid(&key, &cookie_value) {
                        steamid.get_or_insert(sid);
                    }
                    if self.is_token_name(&key) && plausible(&key, &cookie_value) {
                        fields.push((key, cookie_value));
                    }
                }
                continue;
            }
            if let Some(sid) = companion_steamid(name_str, value_str) {
                steamid.get_or_insert(sid);
            }
            if self.is_token_name(name_str) && plausible(name_str, value_str) {
                fields.push((name_str.to_string(), value_str.to_string()));
            }
        }

        // 请求体：表单 / JSON。Python 版实测 POST /user-info 的 body 里就带着 access_token，
        // 只扫 URL / 头 / Cookie 会漏掉这一路
        if let Some((content_type, text)) = body {
            for (key, value) in parse_text_pairs(content_type, text) {
                if let Some(sid) = companion_steamid(&key, &value) {
                    steamid.get_or_insert(sid);
                }
                if self.is_token_name(&key) && plausible(&key, &value) {
                    fields.push((key, value));
                }
            }
        }

        // steamid 也要过白名单：17 位纯数字在无关站点上很常见(订单号、缓存键)，
        // 让它污染 extras 会导致最终写进配置的 steamid 是别人的
        if self.host_allowed(&url)
            && let Some(sid) = &steamid
        {
            self.remember("steamid", sid);
        }
        Hit { url, fields, steamid, from_response: false }
    }

    fn scan_response(&self, res: &Response<Body>) -> Vec<(String, String)> {
        let mut fields = Vec::new();
        for value in res.headers().get_all("set-cookie").iter() {
            let Ok(text) = value.to_str() else { continue };
            for part in text.split(';') {
                let Some((name, cookie_value)) = part.trim().split_once('=') else {
                    continue;
                };
                if self.is_token_name(name) && plausible(name, cookie_value) {
                    fields.push((name.to_string(), cookie_value.to_string()));
                }
            }
        }
        fields
    }
}

impl HttpHandler for TokenHandler {
    async fn handle_request(
        &mut self,
        _ctx: &HttpContext,
        req: Request<Body>,
    ) -> RequestOrResponse {
        let (parts, incoming) = req.into_parts();
        let content_type = header_text(&parts.headers, "content-type");
        let content_length = content_length_of(&parts.headers);
        let method = parts.method.as_str().to_string();

        // WebSocket 升级请求绝不能动 body，否则握不上手
        //(客户端连 wss-csgo-pwa.wmpvp.com 就靠这个请求)
        let upgradable = parts.headers.contains_key("upgrade");
        let mut body = incoming;
        let mut body_text: Option<String> = None;
        if !upgradable && request_body_is_readable(&method, &content_type, content_length) {
            match collect_body_text(body).await {
                Some((text, rebuilt)) => {
                    body_text = Some(text);
                    body = rebuilt;
                }
                // 收集失败：原 body 已被消费，只能用空 body 放行
                None => body = Body::empty(),
            }
        }
        let req = Request::from_parts(parts, body);

        let hit = self.scan_request(
            &req,
            body_text.as_deref().map(|text| (content_type.as_str(), text)),
        );
        let url = hit.url.clone();
        debug!("经过：{} {}", method, url);
        self.log_line(&format!("-> {} {}", method, url));
        if let Ok(mut last) = self.last_url.lock() {
            *last = url;
        }
        if !hit.fields.is_empty() {
            if self.host_allowed(&hit.url) {
                let _ = self.tx.send(hit);
            } else {
                self.report_ignored(&hit.url, &hit.fields);
            }
        }
        req.into()
    }

    async fn handle_response(&mut self, _ctx: &HttpContext, res: Response<Body>) -> Response<Body> {
        let (parts, incoming) = res.into_parts();
        let content_type = header_text(&parts.headers, "content-type");
        let content_length = content_length_of(&parts.headers);
        let status = parts.status;

        let mut body = incoming;
        let mut body_text: Option<String> = None;
        // 101 = 协议切换(WebSocket)，之后的 body 是隧道，不能碰
        if status.as_u16() != 101 && response_body_is_readable(&content_type, content_length) {
            match collect_body_text(body).await {
                Some((text, rebuilt)) => {
                    body_text = Some(text);
                    body = rebuilt;
                }
                None => body = Body::empty(),
            }
        }
        let res = Response::from_parts(parts, body);

        trace!("响应：{} {}", res.status(), content_type);
        let mut fields = self.scan_response(&res);
        // 响应体 JSON 是"签发点"：token 在登录流程里很可能是这一侧下发的
        if let Some(text) = &body_text {
            for (key, value) in json_pairs(text) {
                if self.is_token_name(&key) && plausible(&key, &value) {
                    fields.push((key, value));
                }
            }
        }
        if !fields.is_empty() {
            let url = self.last_url.lock().map(|u| u.clone()).unwrap_or_default();
            if self.host_allowed(&url) {
                let steamid = self.extras.lock().ok().and_then(|e| e.get("steamid").cloned());
                let _ = self.tx.send(Hit { url, fields, steamid, from_response: true });
            } else {
                self.report_ignored(&url, &fields);
            }
        }
        res
    }
}

// ---------------------------------------------------------------- 写配置
/// 把捕获结果组装成配置 JSON：写进 `path`，同时返回同一份文本给
/// WS 推送层广播 —— 落盘和推送的内容永远一致。
fn write_config(path: &Path, hit: &Hit, extras: &HashMap<String, String>) -> std::io::Result<String> {
    let mut map = serde_json::Map::new();
    map.insert(
        "captured_at".into(),
        serde_json::json!(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()),
    );
    map.insert("captured_by".into(), serde_json::json!("rust-fetch-token"));
    map.insert("source_url".into(), serde_json::json!(hit.url));
    if let Some(rest) = hit.url.split("://").nth(1) {
        let mut parts = rest.splitn(2, '/');
        map.insert("host".into(), serde_json::json!(parts.next().unwrap_or("")));
        map.insert("path".into(), serde_json::json!(format!("/{}", parts.next().unwrap_or("").split('?').next().unwrap_or(""))));
    }
    // uid：Python 版从 source_url 的查询串里取，match_watcher 靠它把 token 跟账号对上
    if let Some(uid) = url_query_value(&hit.url, "uid") {
        map.insert("uid".into(), serde_json::json!(uid));
    }
    if hit.from_response {
        map.insert("from_response".into(), serde_json::json!(true));
    }
    for (key, value) in &hit.fields {
        map.insert(key.clone(), serde_json::json!(value));
    }
    // 别名：下游脚本一般按 access_token 取值，而抓到的键名可能是 steam_cn_token 等
    let has_access_token = hit
        .fields
        .iter()
        .any(|(k, _)| k.to_ascii_lowercase().replace('_', "") == "accesstoken");
    if !has_access_token
        && let Some((_, value)) = hit.fields.first()
    {
        map.insert("access_token".into(), serde_json::json!(value));
    }
    if let Some(steamid) = hit.steamid.clone().or_else(|| extras.get("steamid").cloned()) {
        map.insert("steamid".into(), serde_json::json!(steamid));
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&serde_json::Value::Object(map))?;
    std::fs::write(path, &json)?;
    Ok(json)
}

// ---------------------------------------------------------------- 主流程
#[tokio::main]
async fn main() {
    // 放在解析参数之前：解析期就可能报错(未知参数/未知级别)，
    // 晚设的话那几条会带着模块路径，和后面的输出风格不一致。
    // 这个工具的每条消息都是自成一句话，模块路径纯属噪声。
    logkit::set_show_target(false);

    let args = args_handler::parse_args();

    // 默认 info；FETCH_TOKEN_LOG 可覆盖，命令行再覆盖环境变量。
    // 放在解析之后是有意的：这样 `--log-level off` 也不会把参数报错一起吞掉。
    logkit::init_from_env("FETCH_TOKEN_LOG");
    if let Some(level) = args.log_level {
        logkit::set_level(level);
    }

    if !platform::is_admin() && !args.no_elevate {
        if platform::elevate() {
            info!("已请求提权，请在 UAC 弹窗中点击“是”，后续操作将在新窗口中继续");
            // 提权后的新窗口是独立控制台，那边的 exit_with 会自动留窗。
            // 这里也得走闸门：万一用户是从双击的窗口启动的，这个窗口马上要关了。
            pause::exit_with(0);
        }
        error!("提权被拒绝；请以管理员身份运行后重试");
        pause::exit_with(3);
    }

    let out_dir = args.out.clone();
    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        error!("创建工作目录失败：{err}");
        pause::exit_with(2);
    }
    let cert_path = out_dir.join("fetch-token-ca.pem");
    let ca_name = "WMPVP Token Sniffer CA".to_string();
    // 生成一把自己的 CA
    let ca = creat_user_ca(&cert_path, &ca_name);

    let mut guard = Guard { proxy_prev: None, ca: None };

    // ---- 装证书
    match ca::install_ca(&cert_path, &args.ca_store) {
        Ok(()) => {
            info!("根证书已装入 {} 根证书库", args.ca_store);
            if !args.keep_ca {
                guard.ca = Some((ca_name.clone(), args.ca_store.clone()));
            }
        }
        // 不致命：只是 HTTPS 解不开，继续跑让用户自己决定
        Err(err) => warn!("安装根证书失败(HTTPS 流量将无法解密)：{err}"),
    }

    // ---- 设系统代理
    match proxy::set_system_proxy(args.port) {
        Ok(prev) => {
            info!(
                "系统代理已临时指向 127.0.0.1:{}(原值 {}，退出时自动还原)",
                args.port,
                if prev.1.is_empty() { "未启用".into() } else { prev.1.clone() }
            );
            guard.proxy_prev = Some(prev);
        }
        Err(err) => warn!("设置系统代理失败(请手动将系统代理指向 127.0.0.1:{})：{err}", args.port),
    }

    // ---- 启动代理
    let names = Arc::new(args.names.clone());
    let hosts = Arc::new(args.hosts.clone());
    let (tx, mut rx) = mpsc::unbounded_channel::<Hit>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let extras = Arc::new(Mutex::new(HashMap::new()));
    let ignored: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let log_file = args.log_all.as_ref().and_then(|path| {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(|file| Arc::new(Mutex::new(file)))
    });

    let handler = TokenHandler {
        names: names.clone(),
        hosts: hosts.clone(),
        tx,
        extras: extras.clone(),
        log_all: log_file,
        last_url: Arc::new(Mutex::new(String::new())),
        ignored: ignored.clone(),
    };

    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    let proxy = match Proxy::builder()
        .with_addr(addr)
        .with_ca(ca)
        .with_native_tls_connector()
        .with_http_handler(handler)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        })
        .build()
    {
        Ok(proxy) => proxy,
        Err(err) => {
            error!("构建代理失败：{err}");
            pause::exit_with(2);
        }
    };

    info!("本地代理已监听 http://127.0.0.1:{}", args.port);
    info!("请在客户端中进行操作(登录或进入个人页面即可)，命中 {} 后将自动完成并退出", args.names.join(", "));
    if args.hosts.is_empty() {
        warn!("注意：未限制域名(--any-host)，CSRF、资讯流等无关的一次性 token 也可能被写入");
    } else {
        info!("仅接受以下域名的字段：{}", args.hosts.join(", "));
    }

    // ---- WS 推送后端：把命中结果当作后端事件推给已连接的客户端
    let push = if args.push_port > 0 {
        match push::PushHub::spawn(args.push_port) {
            Ok((hub, port)) => {
                info!("WS 推送后端已就绪：ws://127.0.0.1:{port}(客户端连上即收推送，--push-port 0 可关闭)");
                Some(hub)
            }
            Err(err) => {
                // 不致命：端口被占只是少一路推送，写文件照旧
                warn!("WS 推送后端启动失败({err})；仍会写入配置文件，但不推送");
                None
            }
        }
    } else {
        None
    };

    let server = tokio::spawn(async move {
        if let Err(err) = proxy.start().await {
            error!("代理运行出错：{err}");
        }
    });

    let mut captured = false;
    let mut write_failed = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.timeout);
    // 用循环 + select，而不是在 select 分支里再借用 rx —— 那样会撞上"二次可变借用"
    loop {
        tokio::select! {
            maybe_hit = rx.recv() => {
                let Some(hit) = maybe_hit else { break };
                let extras_map = extras.lock().map(|e| e.clone()).unwrap_or_default();
                info!(
                    "命中 token！来源：{}({})",
                    hit.url,
                    if hit.from_response { "响应侧" } else { "请求侧" }
                );
                for (key, value) in &hit.fields {
                    let masked = if value.len() > 10 {
                        format!("{}…({} 字符)", &value[..10], value.len())
                    } else {
                        value.clone()
                    };
                    info!("    {key} = {masked}");
                }
                match write_config(&args.write_config, &hit, &extras_map) {
                    Ok(config_json) => {
                        info!("已写入：{}", args.write_config.display());
                        captured = true;
                        // 同一份 JSON 走 WS 推给客户端，落盘与推送永远一致
                        if let Some(hub) = &push {
                            hub.broadcast(format!("{{\"type\":\"captured\",\"config\":{config_json}}}"));
                            info!("已通过 WS 推送给已连接的客户端");
                        }
                    }
                    // 抓到了但没落盘：绝不能算成功。这工具唯一的产物就是这个文件，
                    // 报 0 会让脚本以为拿到了 token。
                    Err(err) => {
                        error!("写配置失败 {}：{err}", args.write_config.display());
                        write_failed = true;
                    }
                }
                if !args.keep_going {
                    break;
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                warn!("已达到超时时间 {}s，未命中", args.timeout);
                if let Some(hub) = &push {
                    hub.broadcast("{\"type\":\"timeout\"}".into());
                }
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                info!("收到 Ctrl+C，即将退出");
                break;
            }
        }
    }

    let _ = shutdown_tx.send(());
    let _ = server.await;
    drop(guard);

    // write_failed 也要排除掉：命中过但没写成时 captured 仍是 false，
    // 不排除就会走进下面这段，报"没抓到"——那是错的，明明抓到了。
    if !captured && !write_failed {
        let seen = ignored.lock().map(|list| list.clone()).unwrap_or_default();
        if !seen.is_empty() {
            warn!("以下候选字段名命中，但域名不在白名单，已忽略：");
            for line in &seen {
                warn!("- {line}");
            }
            warn!("若其中包含目标接口，请通过 --hosts <域名> 追加后重新运行。");
        }
        error!("未捕获 token。请排查：1) 客户端是否使用系统代理 2) 根证书是否已安装 3) 添加 --verbose 查看请求");
        pause::exit_with(1);
    }

    // 4 = 抓到了但没写成：跟"没抓到"分开，脚本能据此重试
    if write_failed {
        error!("已捕获 token，但未能写入配置文件(见上方错误)");
        pause::exit_with(4);
    }

    // 成功路径也要留窗：双击运行时"已写入 config.local.json"这行字同样一闪而过
    pause::exit_with(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_names() -> Vec<String> {
        ["access_token", "steam_cn_token", "accesstoken", "pvp_app_token", "token"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    /// 回归：MSN 资讯流的 userauthtoken 曾被当成命中并写成 access_token。
    #[test]
    fn generic_name_is_exact_match_only() {
        let names = default_names();
        assert!(name_matches(&names, "token"));
        assert!(name_matches(&names, "TOKEN"));
        assert!(!name_matches(&names, "userauthtoken"));
        assert!(!name_matches(&names, "csrfToken"));
        assert!(!name_matches(&names, "xsrf-token"));
    }

    #[test]
    fn specific_names_still_suffix_match() {
        let names = default_names();
        assert!(name_matches(&names, "access_token"));
        assert!(name_matches(&names, "user_access_token"));
        assert!(name_matches(&names, "gameaccesstoken"));
        assert!(name_matches(&names, "steam_cn_token"));
        assert!(name_matches(&names, "my_pvp_app_token"));
        // 后缀匹配不该退化成子串匹配
        assert!(!name_matches(&names, "access_token_backup"));
    }

    #[test]
    fn host_of_handles_authority_forms() {
        assert_eq!(host_of("https://assets.msn.cn/service/x?y=1").as_deref(), Some("assets.msn.cn"));
        assert_eq!(host_of("http://user@Example.COM:8080/x").as_deref(), Some("example.com"));
        assert_eq!(host_of("http://[::1]:8080/x").as_deref(), Some("[::1]"));
        // origin-form：MITM 之后也可能出现，取不到就交给调用方放行
        assert_eq!(host_of("/service/x"), None);
    }

    #[test]
    fn host_whitelist_blocks_noise_and_lookalikes() {
        let hosts = args_handler::default_hosts();
        assert!(host_matches(&hosts, "https://pvp.wanmei.com/api/login"));
        assert!(host_matches(&hosts, "https://wanmei.com/x"));
        assert!(host_matches(&hosts, "https://api.steampowered.com/x"));

        assert!(!host_matches(&hosts, "https://assets.msn.cn/service/news"));
        // 关键陷阱：后缀匹配必须带 "." 边界，别放行近似域名
        assert!(!host_matches(&hosts, "https://evilwanmei.com/x"));
        assert!(!host_matches(&hosts, "https://wanmei.com.evil.com/x"));

        // --any-host：空名单一律放行
        assert!(host_matches(&[], "https://assets.msn.cn/x"));
    }

    #[test]
    fn normalize_hosts_trims_and_lowercases() {
        assert_eq!(
            args_handler::normalize_hosts(" .WanMei.COM , steamcommunity.com ,, "),
            vec!["wanmei.com".to_string(), "steamcommunity.com".to_string()]
        );
    }

    /// 回归：真实命中域名来自仓库根 README 的抓取记录，
    /// 白名单漏了 wmpvp.com / pwesports.cn 就是完全抓不到。
    #[test]
    fn real_target_hosts_are_allowed() {
        let hosts = args_handler::default_hosts();
        assert!(host_matches(&hosts, "https://pwaweblogin.wmpvp.com/match-api/calendar?a=1"));
        assert!(host_matches(
            &hosts,
            "https://appactivity.wmpvp.com/steamcn/match/watchStage/getWebsocketInfo"
        ));
        assert!(host_matches(&hosts, "https://gwapi.pwesports.cn/x?token=1"));
        assert!(host_matches(&hosts, "https://wss-csgo-pwa.wmpvp.com/x"));
        // 噪声域仍然被挡住
        assert!(!host_matches(&hosts, "https://assets.msn.cn/service/news"));
    }

    #[test]
    fn json_pairs_walks_nested_objects_and_arrays() {
        let text = r#"{"result":{"steam_cn_token":"abc123"},"list":[{"access_token":"def456"}]}"#;
        let pairs = json_pairs(text);
        assert!(pairs.contains(&("steam_cn_token".to_string(), "abc123".to_string())));
        assert!(pairs.contains(&("access_token".to_string(), "def456".to_string())));
    }

    #[test]
    fn json_pairs_rejects_non_candidates() {
        // 没有 "token" 字样就不必解析
        assert!(json_pairs(r#"{"uid":"76561198000000000"}"#).is_empty());
        // 有 token 字样但不是 JSON
        assert!(json_pairs("token=abc").is_empty());
    }

    #[test]
    fn parse_text_pairs_handles_form_and_json() {
        let form =
            parse_text_pairs("application/x-www-form-urlencoded", "access_token=aaa&uid=76561198000000000");
        assert_eq!(form[0], ("access_token".to_string(), "aaa".to_string()));
        assert_eq!(form[1], ("uid".to_string(), "76561198000000000".to_string()));

        let json = parse_text_pairs("application/json; charset=utf-8", r#"{"steam_cn_token":"bbb"}"#);
        assert_eq!(json, vec![("steam_cn_token".to_string(), "bbb".to_string())]);

        // 不是文本 body 就什么都不解析
        assert!(parse_text_pairs("image/png", "access_token=aaa").is_empty());
    }

    /// 回归：以前只按「17 位数字」认 steamid，任何接口的订单号都会污染共享的 extras。
    #[test]
    fn companion_steamid_requires_known_field_name() {
        let sid = "76561198000000000";
        assert_eq!(companion_steamid("steamid", sid).as_deref(), Some(sid));
        assert_eq!(companion_steamid("loginSteamId", sid).as_deref(), Some(sid));
        assert_eq!(companion_steamid("pwasteamid", sid).as_deref(), Some(sid));
        assert_eq!(companion_steamid("uid", sid).as_deref(), Some(sid));
        // 字段名不认识 → 不认
        assert_eq!(companion_steamid("orderId", sid), None);
        // 位数不对 → 不认
        assert_eq!(companion_steamid("steamid", "12345"), None);
    }

    #[test]
    fn body_readability_rules() {
        let over = Some(BODY_TEXT_LIMIT + 1);
        // 请求体：带长度且是文本 body 才读
        assert!(request_body_is_readable("POST", "application/json", Some(1024)));
        assert!(request_body_is_readable("POST", "application/x-www-form-urlencoded", Some(10)));
        assert!(!request_body_is_readable("GET", "application/json", Some(10)));
        assert!(!request_body_is_readable("POST", "image/png", Some(10)));
        assert!(!request_body_is_readable("POST", "application/json", over));
        // 没有长度不读：chunked 请求读完后 body 变定长，与 transfer-encoding 头对不上
        assert!(!request_body_is_readable("POST", "application/json", None));

        // 响应体：只认 JSON，同样要求自带长度
        assert!(response_body_is_readable("application/json", Some(1024)));
        assert!(!response_body_is_readable("application/json", None));
        assert!(!response_body_is_readable("text/html", Some(1024)));
        assert!(!response_body_is_readable("application/json", over));
    }

    /// 回归：用仓库根 `config.local.json` 里那份**真实命中样本**的形状做验证
    /// (`gwapi.pwesports.cn` + `steam_cn_token` + `steamid`)。
    /// 值和真实 token 无关，只保留等长十六进制形态。
    #[test]
    fn real_capture_sample_is_recognized() {
        let hosts = args_handler::default_hosts();
        let names = default_names();
        let url = "https://gwapi.pwesports.cn/acty/community/moments/getAllPvpPosts";
        assert!(host_matches(&hosts, url));
        assert!(name_matches(&names, "steam_cn_token"));
        // 40 位十六进制：既过强名检查，也过弱名的长度+字符集检查
        assert!(plausible("steam_cn_token", "0123456789abcdef0123456789abcdef01234567"));
        assert_eq!(
            companion_steamid("steamid", "76561198000000000").as_deref(),
            Some("76561198000000000")
        );
    }

    #[test]
    fn url_query_value_extracts_uid() {
        let url = "https://pwaweblogin.wmpvp.com/match-api/calendar?a=20000&uid=76561198000000000&r=1";
        assert_eq!(url_query_value(url, "uid").as_deref(), Some("76561198000000000"));
        assert_eq!(url_query_value(url, "UID").as_deref(), Some("76561198000000000"));
        assert_eq!(url_query_value("https://x.com/y", "uid"), None);
        assert_eq!(url_query_value("https://x.com/y?uid=", "uid"), None);
    }
}
