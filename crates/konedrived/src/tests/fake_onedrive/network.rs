//! A transport gate before wiremock: an offline drive gives no HTTP answer, including
//! on already pooled connections. The control channel stays out of band.
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// A gated wiremock server. Dereferencing exposes its mock/recording operations,
/// while `uri()` and `address()` always name the transport clients must use.
pub struct Server {
    inner: Arc<konedrive_graph::test_server::MockServer>,
    address: std::net::SocketAddr,
    down: Arc<watch::Sender<bool>>,
    listener: JoinHandle<()>,
    uri: String,
}

impl Server {
    pub(super) async fn start() -> Self {
        let inner = Arc::new(konedrive_graph::test_server::MockServer::start().await);
        let serving = Arc::clone(&inner);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("the fake Graph transport gate");
        let address = listener.local_addr().unwrap();
        let uri = format!("http://{address}");
        let down = Arc::new(watch::channel(false).0);
        let changes = Arc::clone(&down);
        let listener = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut client, _)) = accepted else { break };
                        let mut down = changes.subscribe();
                        if *down.borrow_and_update() { continue; }
                        // Hold wiremock out of its reuse pool until this connection closes.
                        let upstream = Arc::clone(&serving);
                        connections.spawn(async move {
                            tokio::select! {
                                _ = async {
                                    if let Ok(mut server) = TcpStream::connect(*upstream.address()).await {
                                        let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                                    }
                                } => {},
                                _ = down.changed() => {},
                            }
                        });
                    },
                    _ = connections.join_next(), if !connections.is_empty() => {},
                }
            }
        });
        Self { inner, address, down, listener, uri }
    }

    pub fn uri(&self) -> String {
        self.uri.clone()
    }

    pub fn address(&self) -> &std::net::SocketAddr {
        &self.address
    }

    pub(super) fn set_down(&self, down: bool) {
        self.down.send_if_modified(|current| {
            if *current == down { false } else { *current = down; true }
        });
    }
}

impl std::ops::Deref for Server {
    type Target = konedrive_graph::test_server::MockServer;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // The listener and accepted connections retain the backend until they stop:
        // no late forwarded request can reach a wiremock server reused by another test.
        self.down.send_replace(true);
        self.listener.abort();
    }
}
