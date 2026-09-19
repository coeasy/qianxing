//! `WorkerRole::field_scopes` 角色字段可见性策略的集成用例。
//!
//! 这些判定只用 `qx-runtime` 的公开 API，放在 `tests/` 而不是 `src` 内，
//! 是为了同时证明策略表对外可依赖：调用方不需要 crate 内部可见性就能复核
//! “哪个角色能配哪个字段”这条启动门禁。

use qx_runtime::{
    CredentialEnv, CredentialFiles, FieldScope, RoleFieldStatus, WorkerConfig, WorkerRole,
    ALL_WORKER_ROLES,
};

fn worker(role: WorkerRole) -> WorkerConfig {
    WorkerConfig {
        id: "probe".into(),
        role,
        enabled: true,
        account_id: None,
        venue_id: None,
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    }
}

/// 策略表必须覆盖全部角色，且每个角色都要给出 9 个字段的判定。
#[test]
fn every_role_declares_every_field_scope() {
    assert_eq!(ALL_WORKER_ROLES.len(), 10);
    for role in ALL_WORKER_ROLES {
        let scopes = role.field_scopes();
        for scope in [
            scopes.account_id,
            scopes.venue_id,
            scopes.endpoint,
            scopes.symbols,
            scopes.settlement_currency,
            scopes.credentials,
            scopes.instrument_spec_path,
            scopes.paper_initial_cash_raw,
            scopes.order_notional_limits,
        ] {
            assert!(matches!(
                scope,
                FieldScope::Required | FieldScope::Allowed | FieldScope::Forbidden
            ));
        }
    }
}

/// 角色谓词与字段可见性必须一致：基础设施角色不得被当作 Venue 角色，
/// 私有接口角色必须是 Venue 角色。
#[test]
fn role_predicts_agree_with_field_scopes() {
    for role in ALL_WORKER_ROLES {
        let scopes = role.field_scopes();
        if !role.is_venue_role() {
            assert_eq!(scopes.credentials, FieldScope::Forbidden, "{role:?}");
            assert_eq!(scopes.endpoint, FieldScope::Forbidden, "{role:?}");
        }
        assert_eq!(
            scopes.credentials == FieldScope::Forbidden,
            !role.uses_private_venue() && !matches!(role, WorkerRole::MarketData),
            "{role:?}"
        );
    }
    assert_eq!(
        ALL_WORKER_ROLES
            .iter()
            .filter(|role| role.uses_private_venue())
            .count(),
        4
    );
}

#[test]
fn infra_roles_reject_venue_bound_fields() {
    for role in [
        WorkerRole::Api,
        WorkerRole::Scheduler,
        WorkerRole::OutboxRelay,
        WorkerRole::EventConsumer,
    ] {
        let mut probe = worker(role);
        assert_eq!(probe.role_field_status(), RoleFieldStatus::Ok);
        probe.symbols = vec!["BTCUSDT.BINANCE".into()];
        assert_eq!(
            probe.role_field_status(),
            RoleFieldStatus::Forbidden { field: "symbols" }
        );
        let mut probe = worker(role);
        probe.credential_env = Some(CredentialEnv {
            api_key: "QX_KEY".into(),
            secret: "QX_SECRET".into(),
        });
        assert_eq!(
            probe.role_field_status(),
            RoleFieldStatus::Forbidden {
                field: "credential_env"
            }
        );
    }
}

#[test]
fn strategy_role_cannot_hold_credentials_or_execution_bounds() {
    let mut probe = worker(WorkerRole::Strategy);
    probe.account_id = Some("acct".into());
    probe.venue_id = Some("paper".into());
    probe.symbols = vec!["BTCUSDT.BINANCE".into()];
    assert_eq!(probe.role_field_status(), RoleFieldStatus::Ok);
    probe.max_order_notional_raw = Some(1_000);
    assert_eq!(
        probe.role_field_status(),
        RoleFieldStatus::Forbidden {
            field: "max_order_notional_raw"
        }
    );
    let mut probe = worker(WorkerRole::Strategy);
    probe.endpoint = Some("deploy/qianxing.ccxt.exchange.example.json".into());
    assert_eq!(
        probe.role_field_status(),
        RoleFieldStatus::Forbidden { field: "endpoint" }
    );
}

#[test]
fn trading_roles_require_account_and_credential_source_is_single() {
    let mut probe = worker(WorkerRole::Execution);
    assert_eq!(
        probe.role_field_status(),
        RoleFieldStatus::Missing {
            field: "account_id"
        }
    );
    probe.account_id = Some("main".into());
    assert_eq!(
        probe.role_field_status(),
        RoleFieldStatus::Missing { field: "venue_id" }
    );
    probe.venue_id = Some("binance-testnet".into());
    probe.endpoint = Some("https://testnet.binance.vision".into());
    probe.instrument_spec_path = Some("deploy/spec.json".into());
    assert_eq!(probe.role_field_status(), RoleFieldStatus::Ok);
    probe.credential_env = Some(CredentialEnv {
        api_key: "QX_KEY".into(),
        secret: "QX_SECRET".into(),
    });
    assert_eq!(probe.role_field_status(), RoleFieldStatus::Ok);
    probe.credential_files = Some(CredentialFiles {
        api_key: "secrets/key".into(),
        secret: "secrets/secret".into(),
    });
    assert_eq!(probe.role_field_status(), RoleFieldStatus::CredentialSource);
}

/// 非 Binance Venue 同样不得留下空的或半空的凭据引用。
#[test]
fn half_empty_credential_source_fails_on_any_venue() {
    let mut probe = worker(WorkerRole::Execution);
    probe.account_id = Some("main".into());
    probe.venue_id = Some("okx".into());
    probe.endpoint = Some("https://www.okx.com".into());
    probe.instrument_spec_path = Some("deploy/spec.json".into());
    probe.credential_env = Some(CredentialEnv {
        api_key: "QX_OKX_KEY".into(),
        secret: "  ".into(),
    });
    assert_eq!(probe.role_field_status(), RoleFieldStatus::CredentialSource);
}

#[test]
fn paper_cash_stays_on_paper_execution_workers() {
    let mut probe = worker(WorkerRole::Execution);
    probe.account_id = Some("main".into());
    probe.venue_id = Some("binance".into());
    probe.endpoint = Some("https://api.binance.com".into());
    probe.instrument_spec_path = Some("deploy/spec.json".into());
    probe.credential_env = Some(CredentialEnv {
        api_key: "QX_KEY".into(),
        secret: "QX_SECRET".into(),
    });
    probe.paper_initial_cash_raw = Some(1_000_000);
    assert_eq!(
        probe.role_field_status(),
        RoleFieldStatus::PaperCashOnRealVenue
    );
    probe.venue_id = Some("paper".into());
    assert_eq!(probe.role_field_status(), RoleFieldStatus::Ok);
    let mut probe = worker(WorkerRole::Reconciler);
    probe.account_id = Some("main".into());
    probe.venue_id = Some("paper".into());
    probe.paper_initial_cash_raw = Some(1);
    assert_eq!(
        probe.role_field_status(),
        RoleFieldStatus::Forbidden {
            field: "paper_initial_cash_raw"
        }
    );
}

#[test]
fn market_data_requires_endpoint_and_never_holds_execution_bounds() {
    let mut probe = worker(WorkerRole::MarketData);
    assert_eq!(
        probe.role_field_status(),
        RoleFieldStatus::Missing { field: "endpoint" }
    );
    probe.endpoint = Some("https://api.binance.com".into());
    probe.instrument_spec_path = Some("deploy/spec.json".into());
    assert_eq!(
        probe.role_field_status(),
        RoleFieldStatus::Forbidden {
            field: "instrument_spec_path"
        }
    );
}
