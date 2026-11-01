use std::sync::Arc;

use super::MockServer;
use crate::drive::DriveClient;
use crate::token::StaticToken;

#[tokio::test]
async fn port_probes_are_ignored_but_extra_client_requests_are_counted() {
    let server = MockServer::start().await;
    let probe = reqwest::Client::builder().user_agent("node").build().unwrap();
    probe.get(server.uri()).send().await.unwrap();
    reqwest::get(server.uri()).await.unwrap();
    probe.get(server.uri()).header("user-agent", "another-ide").send().await.unwrap();
    assert!(server.received_requests().await.unwrap().is_empty(), "the IDE's GET / is not our request");

    let client = DriveClient::new(format!("{}/", server.uri()).parse().unwrap(), Arc::new(StaticToken::new("T"))).unwrap();
    // Even GET / must remain visible when our client sends it (e.g. a download URL).
    for count in 1..=2 {
        let _ = client.download(&server.uri(), 0, None).await;
        assert_eq!(server.received_requests().await.unwrap().len(), count, "an extra client request must fail a request budget");
    }
    let bare = reqwest::Client::new();
    bare.post(format!("{}/token", server.uri())).send().await.unwrap();
    probe.get(format!("{}/unexpected", server.uri())).send().await.unwrap();
    probe.get(format!("{}/?unexpected=1", server.uri())).send().await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 5, "keep OAuth without a UA and every other request shape");
}
