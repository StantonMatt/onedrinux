use super::*;
use futures_util::StreamExt;

#[tokio::test]
async fn network_down_closes_http_reads_writes_and_notification_sockets() {
    let graph = FakeGraph::start().await;
    assert_eq!(graph.server.uri(), graph.uri(), "scripted continuation links must use the client origin");
    assert_eq!(graph.server.uri(), format!("http://{}", graph.server.address()));
    graph.with(|cloud| cloud.add_file("A", ROOT, "a.txt", b"before"));
    let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
    let get = |path: &str| client.get(format!("{}/{path}", graph.uri()));
    assert!(!get("me/drive").send().await.unwrap().bytes().await.unwrap().is_empty());
    let socket_url = graph.with(|cloud| cloud.socket_url.replace("http:", "ws:"));
    let (mut socket, _) = tokio_tungstenite::connect_async(&socket_url).await.unwrap();
    assert!(socket.next().await.unwrap().is_ok(), "the Engine.IO open packet");
    graph.set_network_down(true);
    for (method, path) in [("GET", "me/drive"), ("GET", "me/drive/root/delta"), ("GET", "dl/A"), ("POST", "me/drive/root/children")] {
        assert!(client.request(method.parse().unwrap(), format!("{}/{path}", graph.uri())).send().await.is_err(), "{method} {path} must fail at transport level");
    }
    let ended = tokio::time::timeout(Duration::from_secs(2), socket.next()).await.expect("an open socket is dropped");
    assert!(!matches!(ended, Some(Ok(tokio_tungstenite::tungstenite::Message::Text(_)))));
    assert!(tokio_tungstenite::connect_async(&socket_url).await.is_err(), "new notification sockets are refused");
    graph.with(|cloud| cloud.edit("A", b"changed elsewhere while offline"));
    graph.set_network_down(false);
    assert_eq!(get("dl/A").send().await.unwrap().bytes().await.unwrap().as_ref(), b"changed elsewhere while offline");
    assert!(tokio_tungstenite::connect_async(&socket_url).await.is_ok());
}

#[tokio::test]
async fn network_down_interrupts_an_http_request_already_in_flight() {
    let graph = FakeGraph::start().await;
    graph.with(|cloud| cloud.delay("GET", "me/drive", Duration::from_secs(30), 1));
    let url = format!("{}/me/drive", graph.uri());
    let request = tokio::spawn(async move { reqwest::get(url).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while graph.with(|cloud| cloud.count("GET", "me/drive")) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("the request reached Graph before the loss");
    graph.set_network_down(true);
    assert!(tokio::time::timeout(Duration::from_secs(2), request).await.expect("the connection is cut without waiting for Graph").unwrap().is_err());
}
