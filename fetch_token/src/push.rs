//! 本地 WebSocket 推送后端
//!
//! fetch_token 命中 token 后会把结果写进 config.local.json；这个模块把
//! **同一份 JSON** 同时广播给所有已连接的 WS 客户端，让 GUI / 前端可以把
//! fetch_token 当后端用——连上来等推送即可，不必轮询文件。
//!
//! 协议（服务端 → 客户端的单行 JSON 文本帧；客户端发来的帧一律忽略，
//! 这是单向推送通道）：
//!
//! ```text
//! 连上即推   {"type":"hello","push_port":8787}
//! 命中写盘   {"type":"captured","config":{...与 config.local.json 相同...}}
//! 超时未命中 {"type":"timeout"}
//! ```
//!
//! 只监听 127.0.0.1，不对外网暴露；对端的 WebSocket Ping 由 `read()`
//! 自动回 Pong（每轮循环 flush 带出）。实现上是每客户端一个非阻塞轮询
//! 线程（50ms 一圈）：读负责收 Close / Ping，写负责排空广播队列——
//! 本地就几个客户端，轮询比上异步框架便宜得多。

use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use tokio::sync::broadcast;
use tungstenite::{Message, Utf8Bytes, WebSocket};

/// 轮询间隔：一圈内检查一遍广播队列和读帧
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 推送枢纽：`broadcast` 的同步封装，main 持有它往里塞消息
pub struct PushHub {
    tx: broadcast::Sender<String>,
}

impl PushHub {
    /// 在 127.0.0.1:port 起推送服务；port 传 0 由系统分配，返回实际端口
    pub fn spawn(port: u16) -> std::io::Result<(Self, u16)> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let actual = listener.local_addr()?.port();
        let (tx, _) = broadcast::channel(16);
        let greeting = format!("{{\"type\":\"hello\",\"push_port\":{actual}}}");
        let tx_thread = tx.clone();
        std::thread::spawn(move || accept_loop(listener, tx_thread, greeting));
        Ok((Self { tx }, actual))
    }

    /// 向所有已连接客户端广播一条文本消息（没有客户端时是空操作）
    pub fn broadcast(&self, message: String) {
        let _ = self.tx.send(message);
    }
}

fn accept_loop(listener: TcpListener, tx: broadcast::Sender<String>, greeting: String) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        // 握手失败的连接直接丢弃：这是本地工具通道，不值得打断抓包主流程
        let Ok(ws) = tungstenite::accept(stream) else { continue };
        // 订阅必须在 spawn 之前：客户端收到 hello 即代表订阅已生效，
        // 之后广播的消息一条不漏（测试依赖这个时序）
        let rx = tx.subscribe();
        let greeting = greeting.clone();
        std::thread::spawn(move || serve_client(ws, rx, greeting));
    }
}

fn serve_client(mut ws: WebSocket<TcpStream>, mut rx: broadcast::Receiver<String>, greeting: String) {
    if ws.get_ref().set_nonblocking(true).is_err() {
        return;
    }
    let _ = ws.send(Message::Text(Utf8Bytes::from(greeting)));
    let _ = ws.flush();
    loop {
        // 排空广播队列；Lagged = 客户端太慢丢了旧消息，只保证收到最新的
        loop {
            match rx.try_recv() {
                Ok(message) => {
                    if ws.send(Message::Text(Utf8Bytes::from(message))).is_err() {
                        return;
                    }
                    if ws.flush().is_err() {
                        return;
                    }
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(broadcast::error::TryRecvError::Empty) => break,
                // 所有发送端都 drop 了(hub 已被丢弃)：没有后续消息，收摊
                Err(broadcast::error::TryRecvError::Closed) => return,
            }
        }
        match ws.read() {
            // 对端主动关：礼貌回 Close 再收摊
            Ok(Message::Close(_)) => {
                let _ = ws.close(None);
                return;
            }
            // Ping 的 Pong 由 tungstenite 在 read 时排队，flush 带出
            Ok(_) => {}
            // 非阻塞模式下的"暂时没数据"
            Err(tungstenite::Error::Io(err))
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => return,
        }
        let _ = ws.flush();
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tungstenite::stream::MaybeTlsStream;

    type Client = WebSocket<MaybeTlsStream<TcpStream>>;

    fn connect(port: u16) -> Client {
        let (ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}")).expect("客户端应能连上");
        ws
    }

    /// 阻塞读到一条文本帧。消息必然到达（hello 同步了订阅时序），
    /// 超时说明服务端坏了，让 connect 的 socket 超时兜底报错。
    fn read_text(ws: &mut Client) -> String {
        if let MaybeTlsStream::Plain(tcp) = ws.get_ref() {
            tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        loop {
            match ws.read().expect("连接不应中断") {
                Message::Text(text) => return text.to_string(),
                Message::Close(_) => panic!("服务端提前关闭"),
                _ => continue,
            }
        }
    }

    fn set_client_nonblocking(ws: &mut Client) {
        if let MaybeTlsStream::Plain(tcp) = ws.get_ref() {
            tcp.set_nonblocking(true).unwrap();
        }
    }

    /// 读一帧，读不到（超时）返回 None —— 用来断言"没有多余消息"
    fn try_read_text(ws: &mut Client) -> Option<String> {
        set_client_nonblocking(ws);
        let result = match ws.read() {
            Ok(Message::Text(text)) => Some(text.to_string()),
            Ok(_) => None,
            Err(tungstenite::Error::Io(err))
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                None
            }
            Err(err) => panic!("读帧失败：{err}"),
        };
        if let MaybeTlsStream::Plain(tcp) = ws.get_ref() {
            tcp.set_nonblocking(false).unwrap();
        }
        result
    }

    fn hub_with_two_clients() -> (PushHub, Client, Client) {
        let (hub, port) = PushHub::spawn(0).expect("端口 0 应能绑定");
        let mut a = connect(port);
        let mut b = connect(port);
        // 读到 hello 即代表两端的订阅都已生效，之后的广播不会漏
        assert!(read_text(&mut a).contains("\"type\":\"hello\""));
        assert!(read_text(&mut b).contains("\"type\":\"hello\""));
        (hub, a, b)
    }

    #[test]
    fn spawns_on_ephemeral_port() {
        let (hub, port) = PushHub::spawn(0).expect("应能绑定");
        assert!(port > 0, "系统分配的端口应非 0");
        drop(hub);
    }

    #[test]
    fn pushes_captured_payload_to_every_client() {
        let (hub, mut a, mut b) = hub_with_two_clients();
        hub.broadcast(r#"{"type":"captured","config":{"access_token":"abc"}}"#.into());
        let from_a = read_text(&mut a);
        let from_b = read_text(&mut b);
        assert!(from_a.contains("\"captured\"") && from_a.contains("access_token"));
        assert_eq!(from_a, from_b, "两个客户端应收到相同内容");
    }

    #[test]
    fn timeout_notice_reaches_clients() {
        let (hub, mut a, mut b) = hub_with_two_clients();
        hub.broadcast(r#"{"type":"timeout"}"#.into());
        assert!(read_text(&mut a).contains("\"timeout\""));
        assert!(read_text(&mut b).contains("\"timeout\""));
    }

    /// 没有任何广播时，客户端不该收到 hello 之外的帧（协议是纯推送）
    #[test]
    fn no_spurious_frames_between_broadcasts() {
        let (hub, mut a, _b) = hub_with_two_clients();
        std::thread::sleep(Duration::from_millis(150));
        assert!(try_read_text(&mut a).is_none(), "不应有自发帧");
        hub.broadcast(r#"{"type":"captured"}"#.into());
        assert!(read_text(&mut a).contains("\"captured\""));
    }
}
