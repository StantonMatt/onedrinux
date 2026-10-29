use super::*;
use konedrive_graph::token::StaticToken;
use std::sync::Mutex;
use url::Url;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn the_remote_check_completes_descendant_create_edit_and_remove_in_order() {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new((0, false, Vec::new())));
    let observed = Arc::clone(&state);
    let base = server.uri();
    Mock::given(|_: &wiremock::Request| true).respond_with(move |request: &wiremock::Request| {
        let mut state = observed.lock().unwrap();
        let path = request.url.path();
        let file = || json!({"id":"FILE", "name":"descendant.txt", "eTag":"file-tag", "file":{}, "parentReference":{"id":"INNER"}});
        let folder = |id, name, parent| json!({"id":id, "name":name, "folder":{}, "parentReference":{"id":parent}, "cTag":format!("tag-{}",state.0)});
        match (request.method.as_str(), path) {
            ("POST", "/me/drive/items/RUN/children") => ResponseTemplate::new(201).set_body_json(folder("OUTER", "ctag-parent", "RUN")),
            ("POST", "/me/drive/items/OUTER/children") => ResponseTemplate::new(201).set_body_json(folder("INNER", "child", "OUTER")),
            ("GET", "/me/drive/items/OUTER") => ResponseTemplate::new(200).set_body_json(folder("OUTER", "ctag-parent", "RUN")),
            ("PUT", path) if path.contains("descendant.txt") => {
                assert_eq!(state.0, 0);
                state.0 = 1; state.1 = true; state.2.push("create");
                ResponseTemplate::new(201).set_body_json(file())
            }
            ("GET", path) if path.contains("descendant.txt") => {
                assert!(state.1, "the descendant must remain until the removal operation");
                ResponseTemplate::new(200).set_body_json(file())
            }
            ("POST", "/me/drive/items/FILE/createUploadSession") => {
                assert_eq!(state.0, 1); assert!(state.1);
                ResponseTemplate::new(200).set_body_json(json!({"uploadUrl":format!("{base}/upload")}))
            }
            ("PUT", "/upload") => {
                assert_eq!(state.0, 1); assert_eq!(request.body, b"changed");
                state.0 = 2; state.2.push("edit");
                ResponseTemplate::new(200).set_body_json(file())
            }
            ("PATCH", "/me/drive/items/FILE") => ResponseTemplate::new(200).set_body_json(file()),
            ("DELETE", "/me/drive/items/FILE") => {
                assert_eq!(state.0, 2); assert!(state.1);
                state.0 = 3; state.1 = false; state.2.push("remove");
                ResponseTemplate::new(204)
            }
            ("DELETE", "/me/drive/items/OUTER") => {
                assert_eq!(request.headers.get("if-match").unwrap().to_str().unwrap(), format!("tag-{}", state.0 - 1));
                ResponseTemplate::new(412).set_body_json(json!({"error":{"code":"preconditionFailed"}}))
            }
            ("GET", "/delta") => ResponseTemplate::new(200).set_body_json(json!({"value":[folder("OUTER", "ctag-parent", "RUN")], "@odata.deltaLink":format!("{base}/delta")})),
            _ => panic!("unexpected request: {} {path}", request.method),
        }
    }).mount(&server).await;
    let options = Options {
        upstream: Url::parse(&format!("{}/", server.uri())).unwrap(),
        token: "mock".into(),
        read_only_token: None,
        test_drive: "mock".into(),
        config: konedrived::config::Config::default(),
        caps: crate::guard::Caps::default(),
        run_id: "mock".into(),
        delta_wait: Duration::from_millis(1),
        large: false,
        only: None,
    };
    let drive =
        DriveClient::new(options.upstream.clone(), Arc::new(StaticToken::new("mock"))).unwrap();
    let api = Api::new(options.upstream.clone(), "mock");
    let guard = Arc::new(Guard::new(options.caps.clone(), options.run_id.clone()));
    let mut run = Run::new(
        drive,
        None,
        api,
        guard,
        "RUN".into(),
        &options,
        format!("{}/delta", server.uri()),
    );
    let outcome = run.remote_safety().await;
    assert!(matches!(outcome, Outcome::Pass(_)), "{outcome:?}");
    assert_eq!(state.lock().unwrap().2, ["create", "edit", "remove"]);
}
