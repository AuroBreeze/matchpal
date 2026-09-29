//! WebSocket 会话：连接、订阅、心跳、读帧
//!
//! # 三个必须照抄的细节
//!
//! 1. **鉴权靠握手时的 Cookie**（`PVP_APP_TOKEN=<token>`），帧本身不含凭据。
//! 2. **读帧超时要自己设**。Python 版注释里专门记了这个坑：`websocket-client`
//!    握手后会把 socket 重置成阻塞模式，不自己设超时 `recv()` 就永远挂着，
//!    外层的 `--timeout` 根本检查不到。这里对应 [`Session::read_frame`] 返回
//!    `Ok(None)` 表示"这一秒没数据"，和"连接断了"区分开。
//! 3. **连上先发 `ping`，再发订阅帧**，顺序是实测的。

use std::net::TcpStream;
use std::time::Duration;

use tungstenite::client::ClientRequestBuilder;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, Utf8Bytes, WebSocket};

use crate::api::ORIGIN;
use crate::model::subscribe_payload;

/// 读帧超时。**不要**改成很久 —— 它同时承担着"让外层能检查 --timeout"的职责。
pub const READ_TIMEOUT: Duration = Duration::from_secs(1);
/// 心跳间隔
pub const PING_INTERVAL: Duration = Duration::from_secs(20);

/// 连接 / 收帧失败
#[derive(Debug)]
pub enum WsError {
    /// 握手或 TLS 失败
    Connect(String),
    /// 连接被对端关掉
    Closed,
    /// 协议层或 IO 错误（非超时）
    Protocol(String),
}

impl std::fmt::Display for WsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WsError::Connect(err) => write!(f, "WebSocket 连接失败：{err}"),
            WsError::Closed => write!(f, "WebSocket 已被对端关闭"),
            WsError::Protocol(err) => write!(f, "WebSocket 出错：{err}"),
        }
    }
}

impl std::error::Error for WsError {}

/// 一条已建立的会话
pub struct Session {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
}

impl Session {
    /// 连接并用 token 完成握手鉴权
    pub fn connect(url: &str, token: &str) -> Result<Session, WsError> {
        let uri = url.parse().map_err(|err| WsError::Connect(format!("地址不合法 {url}：{err}")))?;
        let request = ClientRequestBuilder::new(uri)
            .with_header("Cookie", format!("PVP_APP_TOKEN={token}"))
            .with_header("Origin", ORIGIN);
        let (socket, _response) = tungstenite::connect(request)
            .map_err(|err| WsError::Connect(err.to_string()))?;
        // 握手完成后立刻设读超时，理由见文件头
        set_stream_read_timeout(socket.get_ref(), READ_TIMEOUT)
            .map_err(|err| WsError::Connect(format!("设置读超时失败：{err}")))?;
        Ok(Session { socket })
    }

    /// 发一段文本
    pub fn send_text(&mut self, text: &str) -> Result<(), WsError> {
        self.socket
            .send(Message::Text(Utf8Bytes::from(text)))
            .map_err(|err| WsError::Protocol(err.to_string()))
    }

    /// 应用层心跳：文本 `ping`（不是 WebSocket 的 Ping 帧）
    pub fn ping(&mut self) -> Result<(), WsError> {
        self.send_text("ping")
    }

    /// 订阅自己的对局（`messageType` 10001）
    pub fn subscribe(&mut self, steamid: &str) -> Result<(), WsError> {
        self.send_text(&subscribe_payload(steamid))
    }

    /// 读一帧文本。
    ///
    /// - `Ok(Some(text))`：收到文本帧
    /// - `Ok(None)`：这一轮没有应用层数据（读超时、Pong、二进制帧）
    /// - `Err`：连接断了，调用方该重连
    pub fn read_frame(&mut self) -> Result<Option<String>, WsError> {
        match self.socket.read() {
            Ok(Message::Text(text)) => Ok(Some(text.as_str().to_string())),
            // 对端发 WebSocket Ping：按协议回 Pong，然后继续等
            Ok(Message::Ping(payload)) => {
                let _ = self.socket.send(Message::Pong(payload));
                Ok(None)
            }
            Ok(Message::Close(_)) => Err(WsError::Closed),
            // Pong / 二进制 / 其他：都不是应用层数据
            Ok(_) => Ok(None),
            // 读超时是"没数据"，不是错误
            Err(tungstenite::Error::Io(err)) if is_timeout(&err) => Ok(None),
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Err(WsError::Closed)
            }
            Err(err) => Err(WsError::Protocol(err.to_string())),
        }
    }

    /// 主动关闭（退出前收拾干净）
    pub fn close(&mut self) {
        let _ = self.socket.close(None);
    }
}

/// 给底层 socket 设读超时。
///
/// `MaybeTlsStream` 标了 `#[non_exhaustive]`，所以必须有兜底分支 ——
/// 将来 tungstenite 加了新 TLS 后端，这里会走兜底（不发超时），而不是编译不过。
fn set_stream_read_timeout(stream: &MaybeTlsStream<TcpStream>, timeout: Duration) -> std::io::Result<()> {
    match stream {
        MaybeTlsStream::Plain(tcp) => tcp.set_read_timeout(Some(timeout)),
        MaybeTlsStream::NativeTls(tls) => tls.get_ref().set_read_timeout(Some(timeout)),
        _ => Ok(()),
    }
}

/// 读超时在 Windows 上是 `TimedOut`(10060)，别的平台可能是 `WouldBlock`
fn is_timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读超时必须真的落到 socket 上 —— 这是外层 `--timeout` 能生效的前提
    #[test]
    fn read_timeout_is_actually_applied() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("应该能监听本机端口");
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).expect("应该能连上自己");
        let stream = MaybeTlsStream::Plain(client);

        let timeout = Duration::from_millis(700);
        set_stream_read_timeout(&stream, timeout).expect("设超时不该失败");

        let MaybeTlsStream::Plain(tcp) = &stream else {
            panic!("刚才是用 Plain 包起来的");
        };
        assert_eq!(tcp.read_timeout().unwrap(), Some(timeout));
    }

    #[test]
    fn timeout_detection_covers_both_error_kinds() {
        let would_block = std::io::Error::from(std::io::ErrorKind::WouldBlock);
        let timed_out = std::io::Error::from(std::io::ErrorKind::TimedOut);
        let broken = std::io::Error::from(std::io::ErrorKind::ConnectionReset);
        assert!(is_timeout(&would_block));
        assert!(is_timeout(&timed_out));
        assert!(!is_timeout(&broken));
    }
}
