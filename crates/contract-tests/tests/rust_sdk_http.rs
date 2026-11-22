use qx_api::{
    load_mtls_identity_policy_from_pem, load_mtls_server_config_from_pem, ApiPolicy, ApiService,
    ApiState, MtlsIdentityStore, TlsConfigStore,
};
use qx_control::{CommandKind, ControlCommand, Permission};
use qx_sdk_http::{HttpRemoteTransport, RemoteApiClient, RemoteTransport};
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn rust_sdk_connects_to_real_api_over_loopback() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stopped);
    let worker = thread::spawn(move || {
        ApiService::new(ApiState::default())
            .serve(listener, || 1, || worker_stop.load(Ordering::Acquire))
            .unwrap();
    });
    let transport =
        HttpRemoteTransport::loopback("127.0.0.1", port, Duration::from_secs(5)).unwrap();
    let invalid = transport.post("/app/provider-backtest", "{}").unwrap();
    assert!((400..500).contains(&invalid.status), "{}", invalid.body);
    let client = RemoteApiClient::new(transport);
    let recipes = client.builtin_strategy_recipes().unwrap();
    assert_eq!(recipes["schema_version"], 1);
    assert!(recipes["recipes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["name"] == "macd"));
    let strategies = client.builtin_strategy_catalog().unwrap();
    assert!(strategies
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "sma_cross"));
    stopped.store(true, Ordering::Release);
    worker.join().unwrap();
}

#[test]
fn rust_sdk_mtls_authenticates_server_and_maps_client_identity() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mtls");
    let ca = fixtures.join("ca.pem");
    let cert = fixtures.join("client.pem");
    let key = fixtures.join("client-key.pem");
    let server = load_mtls_server_config_from_pem(
        fixtures.join("server.pem"),
        fixtures.join("server-key.pem"),
        &ca,
    )
    .unwrap();
    let identities = load_mtls_identity_policy_from_pem(&BTreeMap::from([(
        "sdk-operator".to_string(),
        cert.clone(),
    )]))
    .unwrap();
    let policy = ApiPolicy::new().grant("sdk-operator", Permission::Trading);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stopped);
    let worker = thread::spawn(move || {
        let configs = TlsConfigStore::new(server);
        let identities = MtlsIdentityStore::new(identities);
        ApiService::with_policy(ApiState::default(), policy)
            .serve_tls_mtls_with_stores(
                listener,
                &configs,
                &identities,
                || 1,
                || worker_stop.load(Ordering::Acquire),
            )
            .unwrap();
    });
    let timeout = Duration::from_secs(5);
    let authorized =
        HttpRemoteTransport::mtls("127.0.0.1", port, timeout, &ca, &cert, &key).unwrap();
    let recipes = RemoteApiClient::new(
        HttpRemoteTransport::mtls("127.0.0.1", port, timeout, &ca, &cert, &key).unwrap(),
    )
    .builtin_strategy_recipes()
    .unwrap();
    assert_eq!(recipes["schema_version"], 1);
    let command = ControlCommand {
        command_id: 1,
        request_id: "mtls-sdk-test".into(),
        operator_id: "untrusted-body".into(),
        reason: "exercise identity mapping".into(),
        kind: CommandKind::PauseStrategy,
        target: "test-strategy".into(),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: false,
    };
    let body = serde_json::to_string(&command).unwrap();
    let accepted = authorized.post("/control/commands", &body).unwrap();
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    let receipt: serde_json::Value = serde_json::from_str(&accepted.body).unwrap();
    assert_eq!(receipt["operator_id"], "sdk-operator");

    let unmapped = HttpRemoteTransport::mtls(
        "127.0.0.1",
        port,
        timeout,
        &ca,
        fixtures.join("unmapped-client.pem"),
        &key,
    )
    .unwrap();
    let rejected = unmapped.post("/control/commands", &body).unwrap();
    assert_eq!(rejected.status, 403, "{}", rejected.body);
    let wrong_ca =
        HttpRemoteTransport::mtls("127.0.0.1", port, timeout, &cert, &cert, &key).unwrap();
    assert!(wrong_ca.get("/app/strategy-recipes").is_err());
    let public_roots = HttpRemoteTransport::https("127.0.0.1", port, timeout).unwrap();
    assert!(public_roots.get("/app/strategy-recipes").is_err());
    stopped.store(true, Ordering::Release);
    worker.join().unwrap();
}
