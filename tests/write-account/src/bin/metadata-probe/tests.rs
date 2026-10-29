use super::*;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn the_probe_counts_metadata_without_any_content_or_write_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/me/drive/root/delta"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [
                {"id": "folder-a", "folder": {}, "cTag": "c1"},
                {"id": "folder-b", "folder": {}},
                {"id": "file-a", "file": {"hashes": {"quickXorHash": "hash"}}, "@microsoft.graph.downloadUrl": "https://never-follow.invalid/content"},
                {"id": "file-b", "file": {}},
                {"id": "file-a", "file": {}},
                {"id": "deleted", "deleted": {}, "file": {}}
            ],
            "@odata.deltaLink": format!("{}/delta/end", server.uri())
        }))).expect(1).mount(&server).await;
    let drive = DriveClient::new(Url::parse(&format!("{}/", server.uri())).unwrap(), Arc::new(StaticToken::new("fake"))).unwrap();
    assert_eq!(sample_metadata(&drive, 10).await.unwrap(), (2, 1, 2, 1, true));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests.iter().all(|request| request.method.as_str() == "GET" && request.url.path().ends_with("/delta")));
}

#[test]
fn the_probe_refuses_an_empty_or_unbounded_sample() {
    for sample in ["0", "10001"] {
        assert!(Args::try_parse_from(["metadata-probe", "--graph-drive", "D", "--graph-token", "token", "--sample", sample]).is_err());
    }
}
