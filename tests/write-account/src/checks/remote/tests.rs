use super::*;
use konedrive_graph::token::StaticToken;
use std::sync::Mutex;
use url::Url;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The fake answers as OneDrive personal did on 2026-10-29: folders with no cTag, an eTag
/// that only the folder's own rename and move change, 412 for a stale eTag and 204 for the
/// current one.
#[tokio::test]
async fn the_remote_check_completes_descendant_create_edit_and_remove_then_rename_move_and_both_deletes_in_order() {
    let server = MockServer::start().await;
    // The step reached, whether the descendant exists, what was done, the version of the outer folder's eTag.
    let state = Arc::new(Mutex::new((0, false, Vec::new(), 1)));
    let observed = Arc::clone(&state);
    let base = server.uri();
    Mock::given(|_: &wiremock::Request| true).respond_with(move |request: &wiremock::Request| {
        let mut state = observed.lock().unwrap();
        let path = request.url.path();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let file = || json!({"id":"FILE", "name":"descendant.txt", "eTag":"file-tag", "cTag":"file-ctag", "file":{}, "parentReference":{"id":"INNER"}});
        let folder = |id: &str, name: &str, parent: &str, version: u32| json!({"id":id, "name":name, "folder":{}, "parentReference":{"id":parent}, "eTag":format!("\"{{{id}}},{version}\"")});
        let outer = |state: &(u32, bool, Vec<&str>, u32)| folder("OUTER", "etag-parent", "RUN", state.3);
        match (request.method.as_str(), path) {
            ("POST", "/me/drive/items/RUN/children") if body["name"] == "etag-sibling" => ResponseTemplate::new(201).set_body_json(folder("SIBLING", "etag-sibling", "RUN", 1)),
            ("POST", "/me/drive/items/RUN/children") => ResponseTemplate::new(201).set_body_json(outer(&state)),
            ("POST", "/me/drive/items/OUTER/children") => ResponseTemplate::new(201).set_body_json(folder("INNER", "child", "OUTER", 1)),
            ("GET", "/me/drive/items/OUTER") => ResponseTemplate::new(200).set_body_json(outer(&state)),
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
            ("GET", "/delta") => ResponseTemplate::new(200).set_body_json(json!({"value":[outer(&state)], "@odata.deltaLink":format!("{base}/delta")})),
            ("PATCH", "/me/drive/items/OUTER") => {
                assert_eq!(request.headers.get("if-match").unwrap().to_str().unwrap(), format!("\"{{OUTER}},{}\"", state.3));
                state.3 += 1;
                state.2.push(if body["parentReference"]["id"] == "SIBLING" { "move" } else { "rename" });
                ResponseTemplate::new(200).set_body_json(outer(&state))
            }
            ("DELETE", "/me/drive/items/OUTER") => {
                let tag = request.headers.get("if-match").unwrap().to_str().unwrap().to_owned();
                if tag == format!("\"{{OUTER}},{}\"", state.3) {
                    state.2.push("delete");
                    ResponseTemplate::new(204)
                } else {
                    state.2.push("stale delete");
                    ResponseTemplate::new(412).set_body_json(json!({"error":{"code":"resourceModified"}}))
                }
            }
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
    assert_eq!(state.lock().unwrap().2, ["create", "edit", "remove", "rename", "move", "stale delete", "delete"]);
}
