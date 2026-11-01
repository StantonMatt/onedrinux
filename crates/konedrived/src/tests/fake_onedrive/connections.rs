//! Loopback-only HTTP/1 fault proxy for transfer tests. Unlike an HTTP error or a
//! short successful body, these faults close the socket partway through a message.
//! Dropping the owner aborts the listener and all its connections.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy)]
pub(crate) enum DropAt {
    /// Close after reading this many request-body bytes, before forwarding it.
    Request(u64),
    /// Forward the whole request and consume the response, but lose the answer.
    Answer,
    /// Send the real response headers, then only this many body bytes.
    Body(u64),
}

struct Rule {
    method: String,
    path: String,
    pass: u32,
    left: u32,
    at: DropAt,
}

#[derive(Debug, Clone)]
pub(crate) struct Asked {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) range: Option<String>,
    pub(crate) content_range: Option<String>,
}

#[derive(Default)]
struct Faults {
    rules: Vec<Rule>,
    offline: bool,
    asked: Vec<Asked>,
}

pub(crate) struct Connections {
    pub(crate) url: String,
    faults: Arc<Mutex<Faults>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Connections {
    fn drop(&mut self) { self.task.abort(); }
}

impl Connections {
    pub(crate) async fn start(upstream: std::net::SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let faults = Arc::new(Mutex::new(Faults::default()));
        let state = Arc::clone(&faults);
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let faults = Arc::clone(&state);
                        connections.spawn(async move { let _ = serve(socket, upstream, faults).await; });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self { url, faults, task }
    }

    pub(crate) fn drop_next(&self, method: &str, path: &str, pass: u32, times: u32, at: DropAt) {
        self.faults.lock().unwrap().rules.push(Rule { method: method.into(), path: path.into(), pass, left: times, at });
    }

    pub(crate) fn offline(&self, offline: bool) { self.faults.lock().unwrap().offline = offline; }

    pub(crate) fn asked(&self) -> Vec<Asked> { self.faults.lock().unwrap().asked.clone() }
}

// Read headers alone so dropping a request at a chosen body byte is deterministic.
async fn headers(socket: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(socket.read_u8().await?);
        if bytes.len() > 64 * 1024 { return Err(std::io::ErrorKind::InvalidData.into()); }
    }
    Ok(bytes)
}

fn field(headers: &str, name: &str) -> Option<String> {
    headers.lines().filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim().to_owned())
}

async fn serve(mut socket: TcpStream, upstream: std::net::SocketAddr, faults: Arc<Mutex<Faults>>) -> std::io::Result<()> {
    let head = headers(&mut socket).await?;
    let text = String::from_utf8_lossy(&head);
    let mut words = text.lines().next().unwrap_or_default().split_whitespace();
    let (method, path) = (words.next().unwrap_or_default(), words.next().unwrap_or_default());
    if konedrive_graph::test_server::is_port_probe(method, path, field(&text, "user-agent").as_deref()) { return Ok(()); }
    let at = {
        let mut faults = faults.lock().unwrap();
        faults.asked.push(Asked { method: method.into(), path: path.into(), range: field(&text, "range"), content_range: field(&text, "content-range") });
        if faults.offline { return Ok(()); }
        faults.rules.iter_mut().find(|rule| rule.method == method && path.contains(&rule.path) && rule.left > 0)
            .and_then(|rule| {
                if rule.pass > 0 { rule.pass -= 1; None }
                else { rule.left -= 1; Some(rule.at) }
            })
    };
    let length = field(&text, "content-length").and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
    if let Some(DropAt::Request(bytes)) = at {
        tokio::io::copy(&mut (&mut socket).take(bytes.min(length)), &mut tokio::io::sink()).await?;
        return Ok(());
    }
    let mut backend = TcpStream::connect(upstream).await?;
    // One request per connection keeps parsing small, even when reqwest pools sockets.
    let head = text.lines().filter(|line| !line.to_ascii_lowercase().starts_with("connection:") && !line.is_empty()).collect::<Vec<_>>().join("\r\n");
    backend.write_all(format!("{head}\r\nConnection: close\r\n\r\n").as_bytes()).await?;
    tokio::io::copy(&mut (&mut socket).take(length), &mut backend).await?;
    match at {
        Some(DropAt::Answer) => { tokio::io::copy(&mut backend, &mut tokio::io::sink()).await?; }
        Some(DropAt::Body(bytes)) => {
            let head = headers(&mut backend).await?;
            socket.write_all(&head).await?;
            tokio::io::copy(&mut backend.take(bytes), &mut socket).await?;
            socket.shutdown().await?;
        }
        _ => { tokio::io::copy(&mut backend, &mut socket).await?; }
    }
    Ok(())
}
