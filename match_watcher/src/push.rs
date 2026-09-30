//! 本地 WebSocket 推送后端
//!
//! match_watcher 作为后端：把监听会话产生的 [`crate::session::WatcherEvent`]
//! 事件流(序列化后的 JSON)实时推给所有已连接的客户端。GUI / 前端连上
//! `ws://127.0.0.1:<端口>` 即可拿到名单进度、重订阅、对局推送和最终表格，
//! 不必轮询文件或解析 stdout。
//!
//! 协议(服务端 → 客户端的单行 JSON 文本帧；客户端发来的帧一律忽略，
//! 这是单向推送通道)：
//!
//! ```text
//! 连上即推   {"type":"hello","service":"match_watcher","push_port":8788}
//! 会话事件   {"type":"Progress","data":{"loaded":3,"full":10}}
//!            {"type":"Report","data":{"text":"...","data":{...结构化表格...}}}
//!            其余事件同 WatcherEvent 的 serde 形态
//! ```
//!
//! 只监听 127.0.0.1，不对外网暴露。与 fetch_token 的推送后端同构，
//! 但这个 crate 是纯同步的：枢纽用 std mpsc 通道列表，广播时逐个
//! 发送、清理已断开的客户端。

use std::net::{TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use tungstenite::{Message, Utf8Bytes, WebSocket};

/// 轮询间隔：一圈内检查一遍推送队列和读帧
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 推送枢纽：main 持有它往里塞消息
pub struct PushHub {
    clients: Arc<Mutex<Vec<mpsc::Sender<String>>>>,
}

impl PushHub {
    /// 在 127.0.0.1:port 起推送服务；port 传 0 由系统分配，返回实际端口
    pub fn spawn(port: u16) -> std::io::Result<(Self, u16)> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let actual = listener.local_addr()?.port();
        let clients: Arc<Mutex<Vec<mpsc::Sender<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let greeting =
            format!("{{\"type\":\"hello\",\"service\":\"match_watcher\",\"push_port\":{actual}}}");
        let clients_thread = Arc::clone(&clients);
        std::thread::spawn(move || accept_loop(listener, clients_thread, greeting));
        Ok((
            Self {
                clients: Arc::clone(&clients),
            },
            actual,
        ))
    }

    /// 向所有已连接客户端广播一条文本消息；发送失败(对端已断开)的
    /// 客户端顺手从列表里清掉
    pub fn broadcast(&self, message: String) {
        let Ok(mut clients) = self.clients.lock() else { return };
        clients.retain(|client| client.send(message.clone()).is_ok());
    }
}

fn accept_loop(
    listener: TcpListener,
    clients: Arc<Mutex<Vec<mpsc::Sender<String>>>>,
    greeting: String,
) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        // 握手失败的连接直接丢弃：这是本地工具通道，不值得打断监听主流程
        let Ok(ws) = tungstenite::accept(stream) else { continue };
        let (tx, rx) = mpsc::channel::<String>();
        // 先注册再 spawn：客户端收到 hello 即代表注册已生效，
        // 之后广播的消息一条不漏(测试依赖这个时序)
        if let Ok(mut list) = clients.lock() {
            list.push(tx);
        }
        let greeting = greeting.clone();
        std::thread::spawn(move || serve_client(ws, rx, greeting));
    }
}

fn serve_client(mut ws: WebSocket<TcpStream>, rx: mpsc::Receiver<String>, greeting: String) {
    if ws.get_ref().set_nonblocking(true).is_err() {
        return;
    }
    let _ = ws.send(Message::Text(Utf8Bytes::from(greeting)));
    let _ = ws.flush();
    loop {
        // 排空推送队列
        while let Ok(message) = rx.try_recv() {
            if ws.send(Message::Text(Utf8Bytes::from(message))).is_err() {
                return;
            }
            if ws.flush().is_err() {
                return;
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

    /// 阻塞读到一条文本帧。消息必然到达(hello 同步了注册时序)，
    /// 超时说明服务端坏了，靠 connect 的 socket 超时兜底报错。
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

    /// 读一帧，读不到(超时)返回 None —— 用来断言"没有多余消息"
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
        // 读到 hello 即代表两端的注册都已生效，之后的广播不会漏
        let hello_a = read_text(&mut a);
        let hello_b = read_text(&mut b);
        assert!(hello_a.contains("\"match_watcher\""));
        assert!(hello_b.contains("\"match_watcher\""));
        (hub, a, b)
    }

    #[test]
    fn spawns_on_ephemeral_port() {
        let (hub, port) = PushHub::spawn(0).expect("应能绑定");
        assert!(port > 0, "系统分配的端口应非 0");
        drop(hub);
    }

    #[test]
    fn pushes_events_to_every_client() {
        let (hub, mut a, mut b) = hub_with_two_clients();
        hub.broadcast(
            r#"{"type":"Progress","data":{"loaded":3,"full":10}}"#.into(),
        );
        let from_a = read_text(&mut a);
        let from_b = read_text(&mut b);
        assert!(from_a.contains("\"Progress\"") && from_a.contains("\"loaded\":3"));
        assert_eq!(from_a, from_b, "两个客户端应收到相同内容");
    }

    /// 断开的客户端会被清理，不影响后续广播
    #[test]
    fn drops_disconnected_clients_and_keeps_broadcasting() {
        let (hub, mut a, mut b) = hub_with_two_clients();
        b.close(None).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        hub.broadcast(r#"{"type":"Notice","data":{"level":"info","message":"x"}}"#.into());
        // a 照常收到；b 已经不在列表里，即便它 socket 还没完全关掉也不报错
        assert!(read_text(&mut a).contains("\"Notice\""));
    }

    /// 没有任何广播时，客户端不该收到 hello 之外的帧(协议是纯推送)
    #[test]
    fn no_spurious_frames_between_broadcasts() {
        let (hub, mut a, _b) = hub_with_two_clients();
        std::thread::sleep(Duration::from_millis(150));
        assert!(try_read_text(&mut a).is_none(), "不应有自发帧");
        hub.broadcast(r#"{"type":"Connected","data":{"full":10}}"#.into());
        assert!(read_text(&mut a).contains("\"Connected\""));
    }
}
