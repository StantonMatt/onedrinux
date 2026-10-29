use super::*;

#[test]
fn download_links_prune_expired_entries_and_cap_their_count() {
    let cache = DownloadLinks::default();
    let link = Link { url: "https://download/unused".into(), etag: "e".into() };
    crate::panic::lock(&cache.links).insert("expired".into(), Held { link: link.clone(), since: Instant::now() - LINK_LIVES });
    cache.keep("fresh", link.clone());
    assert!(!crate::panic::lock(&cache.links).contains_key("expired"), "keep frees expired URLs");
    for n in 0..LINKS_AT_MOST {
        cache.keep(&format!("P{n}"), link.clone());
    }
    assert_eq!(crate::panic::lock(&cache.links).len(), LINKS_AT_MOST);
    assert!(cache.find("fresh").is_none(), "the oldest URL is evicted at the entry cap");
    let mut links = crate::panic::lock(&cache.links);
    links.get_mut("P1").unwrap().since = Instant::now() - LINK_LIVES;
    drop(links);
    assert!(cache.find("P1").is_none());
    assert_eq!(crate::panic::lock(&cache.links).len(), LINKS_AT_MOST - 1, "find also frees expired URLs");
}

#[tokio::test]
async fn clearing_download_links_cancels_a_background_listing() {
    use serde_json::json;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path, query_param};
    use konedrive_graph::token::StaticToken;
    let server = MockServer::start().await;
    let next = format!("{}/later", server.uri());
    Mock::given(method("GET")).and(path("/me/drive/items/F/children"))
        .and(query_param("$top", "200"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[{"id":"P0", "eTag":"e", "@microsoft.graph.downloadUrl":"https://download/unused"}], "@odata.nextLink":next})))
        .mount(&server).await;
    Mock::given(method("GET")).and(path("/later"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)).set_body_json(json!({"value":[{"id":"P1", "eTag":"e", "@microsoft.graph.downloadUrl":"https://download/unused"}]})))
        .mount(&server).await;
    let drive = DriveClient::new(url::Url::parse(&format!("{}/", server.uri())).unwrap(), Arc::new(StaticToken::new("T"))).unwrap();
    let cache = Arc::new(DownloadLinks::default());
    cache.list_folder(&drive, "F", "P0").await;
    let listing = Arc::clone(crate::panic::lock(&cache.folders).get("F").unwrap());
    assert!(!listing.task.is_finished());
    cache.clear();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !listing.task.is_finished() { tokio::task::yield_now().await; }
    }).await.expect("clearing the cache aborts a listing even when a reader still holds it");
    assert!(cache.find("P0").is_none());
    assert!(cache.find("P1").is_none(), "later pages cannot repopulate a cleared cache");
    assert!(crate::panic::lock(&cache.folders).is_empty());
}
