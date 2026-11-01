use super::*;
use futures_util::StreamExt;
use std::time::Duration;

/// The FUSE VM harness serves SyncService directly without an Account object or D-Bus.
/// Exercise the actual daemon exporter and generated client here on a private bus.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_refused_signal_coalesces_a_burst_and_matches_the_client_contract() {
    let bus = konedrive_dbus::testing::TestBus::start();
    let connection = bus.connect().await;
    connection
        .request_name(konedrive_dbus::SERVICE_NAME)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let endpoint = "http://127.0.0.1:1/".parse().unwrap();
    let service = crate::account::testing::single_account(
        dir.path(),
        konedrive_graph::oauth::Endpoints {
            authority: endpoint,
            graph: "http://127.0.0.1:1/".parse().unwrap(),
        },
        Arc::new(konedrive_graph::secret::MemoryStore::default()),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    let path =
        ObjectPath::try_from(format!("/org/konedrive/Accounts/{}", service.id().as_str())).unwrap();
    let reports = service.config().write_refusals(service.id());
    let task = export(&connection, &path, service).await.unwrap();
    let client = bus.connect().await;
    let proxy = konedrive_dbus::accounts::AccountProxy::new(&client, path.clone())
        .await
        .unwrap();
    let mut signals = proxy.receive_write_refused().await.unwrap();
    let pid = std::process::id();
    let expected_app = crate::download_guard::Callers::default()
        .identify(pid)
        .unwrap()
        .identity;
    reports.record("Outside/first.txt".into(), "Outside".into(), pid);
    let signal = tokio::time::timeout(Duration::from_secs(2), signals.next())
        .await
        .unwrap()
        .unwrap();
    let args = signal.args().unwrap();
    assert_eq!(
        (*args.path(), *args.folder(), *args.app(), *args.count()),
        ("Outside/first.txt", "Outside", expected_app.as_str(), 1)
    );
    for n in 0..1000 {
        reports.record(
            format!("top-{n}").into(),
            std::path::PathBuf::new(),
            u32::MAX,
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), signals.next())
            .await
            .is_err()
    );
    let signal = tokio::time::timeout(Duration::from_secs(12), signals.next())
        .await
        .unwrap()
        .unwrap();
    let args = signal.args().unwrap();
    assert_eq!(
        (*args.path(), *args.folder(), *args.app(), *args.count()),
        ("top-0", "", "", 1000)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), signals.next())
            .await
            .is_err()
    );
    task.abort();
    let _ = task.await;
    unexport(&connection, &path, false).await.unwrap();
}

/// The derived consent property is exported per account and notifies clients when
/// scopes, a wider refresh grant, mode or sign-out change its value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_still_granted_emits_properties_changed() {
    use crate::account::state::SignInState;
    use crate::config::Mode;
    let bus = konedrive_dbus::testing::TestBus::start();
    let connection = bus.connect().await;
    connection.request_name(konedrive_dbus::SERVICE_NAME).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let service = crate::account::testing::single_account(
        dir.path(),
        konedrive_graph::oauth::Endpoints {
            authority: "http://127.0.0.1:1/".parse().unwrap(),
            graph: "http://127.0.0.1:1/".parse().unwrap(),
        },
        Arc::new(konedrive_graph::secret::MemoryStore::default()),
        Duration::from_secs(1),
    ).await.unwrap();
    let path = ObjectPath::try_from(format!("/org/konedrive/Accounts/{}", service.id().as_str())).unwrap();
    let task = export(&connection, &path, Arc::clone(&service)).await.unwrap();
    let client = bus.connect().await;
    let properties = zbus::fdo::PropertiesProxy::builder(&client)
        .destination(konedrive_dbus::SERVICE_NAME).unwrap()
        .path(path.clone()).unwrap().build().await.unwrap();
    let mut signals = properties.receive_properties_changed().await.unwrap();
    let token = AccountSnapshot {
        state: SignInState::SignedIn,
        granted_scopes: "Files.ReadWrite".into(),
        ..AccountSnapshot::default()
    };
    let wider = AccountSnapshot {
        granted_scopes: "Files.Read".into(),
        wider_grant: "Files.ReadWrite".into(),
        ..token.clone()
    };
    let mut signed_out = wider.clone();
    signed_out.clear_account();
    signed_out.state = SignInState::SignedOut;
    for (snapshot, expected) in [
        (token.clone(), true),
        (AccountSnapshot { mode: Mode::ReadWrite, ..token }, false),
        (wider.clone(), true),
        (signed_out, false),
        (wider.clone(), true),
        (AccountSnapshot { wider_grant: String::new(), ..wider }, false),
    ] {
        service.state().update(|s| *s = snapshot);
        let observed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let signal = signals.next().await.unwrap();
                let args = signal.args().unwrap();
                if let Some(value) = args.changed_properties().get("WriteStillGranted") {
                    break bool::try_from(value.clone()).unwrap();
                }
            }
        }).await.expect("WriteStillGranted PropertiesChanged");
        assert_eq!(observed, expected);
    }
    task.abort();
    let _ = task.await;
    unexport(&connection, &path, false).await.unwrap();
}
