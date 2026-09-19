//! worker 角色的字段可见性策略。
//!
//! `WorkerConfig` 是一个带多个可选字段的扁平结构，历史上“哪个角色能配哪个字段”
//! 散落在运行时校验与 CLI 分支里，导致把凭据、行情规格或名义额上限配到不会读取
//! 它的角色上时，配置照样通过校验并在运行时静默失效。本模块把这张判定表收进
//! `WorkerRole` 所在的类型系统：每个角色对每个字段只有 Required / Allowed /
//! Forbidden 三种可见性，非法组合在配置反序列化与启动校验阶段即失败。

use crate::{CredentialEnv, CredentialFiles, WorkerConfig, WorkerRole};

/// 全部 `WorkerRole` 变体。新增角色必须在此登记，`field_scopes` 与角色谓词的
/// 覆盖由测试核对，避免 CLI 侧另抄一份角色清单。
pub const ALL_WORKER_ROLES: &[WorkerRole] = &[
    WorkerRole::Api,
    WorkerRole::MarketData,
    WorkerRole::UserStream,
    WorkerRole::Execution,
    WorkerRole::SpreadRecovery,
    WorkerRole::Scheduler,
    WorkerRole::Reconciler,
    WorkerRole::Strategy,
    WorkerRole::OutboxRelay,
    WorkerRole::EventConsumer,
];

/// 字段在某个角色下的可见性。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldScope {
    /// 该角色必须提供该字段，缺失即启动失败。
    Required,
    /// 该角色会读取该字段；省略时走默认路径。
    Allowed,
    /// 该角色的运行路径永不读取该字段；配置它说明参数被绑到了错误的 worker。
    Forbidden,
}

/// 一个角色对其可见字段集合的声明。新增 `WorkerConfig` 可选字段时必须在此补一项，
/// 并在 [`WorkerRole::field_scopes`] 的每一行里给出判定。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WorkerRoleFieldScopes {
    pub account_id: FieldScope,
    pub venue_id: FieldScope,
    pub endpoint: FieldScope,
    pub symbols: FieldScope,
    pub settlement_currency: FieldScope,
    /// `credential_env` 与 `credential_files` 共用一个可见性：它们只是同一份
    /// 凭据的两种来源。
    pub credentials: FieldScope,
    pub instrument_spec_path: FieldScope,
    pub paper_initial_cash_raw: FieldScope,
    /// `max_order_notional_raw` 与 `max_position_notional_raw` 共用一个可见性。
    pub order_notional_limits: FieldScope,
}

impl WorkerRoleFieldScopes {
    /// 该角色的凭据来源是否需要成对校验（Forbidden 时凭据本身已非法）。
    pub const fn credentials_scoped(self) -> bool {
        !matches!(self.credentials, FieldScope::Forbidden)
    }
}

/// 基础设施角色：只跑控制面/队列，不绑定任何 Venue 账户。
const INFRA_SCOPES: WorkerRoleFieldScopes = WorkerRoleFieldScopes {
    account_id: FieldScope::Forbidden,
    venue_id: FieldScope::Forbidden,
    endpoint: FieldScope::Forbidden,
    symbols: FieldScope::Forbidden,
    settlement_currency: FieldScope::Forbidden,
    credentials: FieldScope::Forbidden,
    instrument_spec_path: FieldScope::Forbidden,
    paper_initial_cash_raw: FieldScope::Forbidden,
    order_notional_limits: FieldScope::Forbidden,
};

/// 只读的公共行情角色。
const MARKET_DATA_SCOPES: WorkerRoleFieldScopes = WorkerRoleFieldScopes {
    account_id: FieldScope::Allowed,
    venue_id: FieldScope::Allowed,
    endpoint: FieldScope::Required,
    symbols: FieldScope::Allowed,
    settlement_currency: FieldScope::Allowed,
    // CCXT 私有端点（例如带权限的 OHLCV）可以按 worker 提供凭据引用。
    credentials: FieldScope::Allowed,
    instrument_spec_path: FieldScope::Forbidden,
    paper_initial_cash_raw: FieldScope::Forbidden,
    order_notional_limits: FieldScope::Forbidden,
};

/// 下单角色：Execution 与 SpreadRecovery 共享同一套账户级边界。
const TRADING_SCOPES: WorkerRoleFieldScopes = WorkerRoleFieldScopes {
    account_id: FieldScope::Required,
    venue_id: FieldScope::Required,
    endpoint: FieldScope::Allowed,
    symbols: FieldScope::Allowed,
    settlement_currency: FieldScope::Allowed,
    credentials: FieldScope::Allowed,
    // 非 production 的 Paper smoke 可以兼容省略规格文件，由校验方按环境放行。
    instrument_spec_path: FieldScope::Required,
    paper_initial_cash_raw: FieldScope::Allowed,
    order_notional_limits: FieldScope::Allowed,
};

impl WorkerRole {
    /// 该角色的字段可见性表。必须逐个角色显式声明，不允许使用通配臂，
    /// 这样新增角色会在编译期强制补表。
    pub const fn field_scopes(self) -> WorkerRoleFieldScopes {
        match self {
            Self::Api | Self::Scheduler | Self::OutboxRelay | Self::EventConsumer => INFRA_SCOPES,
            Self::Strategy => WorkerRoleFieldScopes {
                // Strategy worker 只声明交易对象绑定，不持有凭据与执行边界。
                account_id: FieldScope::Allowed,
                venue_id: FieldScope::Allowed,
                endpoint: FieldScope::Forbidden,
                symbols: FieldScope::Allowed,
                settlement_currency: FieldScope::Forbidden,
                credentials: FieldScope::Forbidden,
                instrument_spec_path: FieldScope::Forbidden,
                paper_initial_cash_raw: FieldScope::Forbidden,
                order_notional_limits: FieldScope::Forbidden,
            },
            Self::MarketData => MARKET_DATA_SCOPES,
            Self::UserStream => WorkerRoleFieldScopes {
                endpoint: FieldScope::Required,
                // 用户流回报会复用同一份冻结规格做精度预检。
                instrument_spec_path: FieldScope::Allowed,
                paper_initial_cash_raw: FieldScope::Forbidden,
                order_notional_limits: FieldScope::Forbidden,
                ..TRADING_SCOPES
            },
            Self::Execution => TRADING_SCOPES,
            Self::SpreadRecovery => WorkerRoleFieldScopes {
                paper_initial_cash_raw: FieldScope::Forbidden,
                ..TRADING_SCOPES
            },
            Self::Reconciler => WorkerRoleFieldScopes {
                account_id: FieldScope::Required,
                venue_id: FieldScope::Required,
                endpoint: FieldScope::Allowed,
                symbols: FieldScope::Allowed,
                settlement_currency: FieldScope::Allowed,
                credentials: FieldScope::Allowed,
                instrument_spec_path: FieldScope::Forbidden,
                paper_initial_cash_raw: FieldScope::Forbidden,
                order_notional_limits: FieldScope::Forbidden,
            },
        }
    }
}

impl WorkerRole {
    /// 可以承载 Venue 绑定的角色白名单。由类型系统给出，新增角色必须显式归类，
    /// 避免 CLI 与运行时各留一份会漂移的角色清单。
    pub const fn is_venue_role(self) -> bool {
        match self {
            Self::MarketData
            | Self::UserStream
            | Self::Execution
            | Self::SpreadRecovery
            | Self::Reconciler => true,
            Self::Api
            | Self::Scheduler
            | Self::Strategy
            | Self::OutboxRelay
            | Self::EventConsumer => false,
        }
    }

    /// 该角色是否以账户身份访问 Venue 私有接口。
    pub const fn uses_private_venue(self) -> bool {
        match self {
            Self::UserStream | Self::Execution | Self::SpreadRecovery | Self::Reconciler => true,
            Self::Api
            | Self::MarketData
            | Self::Scheduler
            | Self::Strategy
            | Self::OutboxRelay
            | Self::EventConsumer => false,
        }
    }
}

fn filled(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|text| !text.trim().is_empty())
}

fn env_ready(credentials: Option<&CredentialEnv>) -> bool {
    credentials
        .map(|credentials| {
            [credentials.api_key.as_str(), credentials.secret.as_str()]
                .iter()
                .all(|name| {
                    !name.trim().is_empty()
                        && name
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                })
        })
        .unwrap_or(false)
}

fn files_ready(credentials: Option<&CredentialFiles>) -> bool {
    credentials
        .map(|credentials| {
            [credentials.api_key.as_str(), credentials.secret.as_str()]
                .iter()
                .all(|path| !path.trim().is_empty())
        })
        .unwrap_or(false)
}

/// 校验 `WorkerConfig` 的字段是否符合角色可见性策略所需的单项判定结果。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoleFieldStatus {
    Ok,
    /// 配置了该角色永不读取的字段。
    Forbidden {
        field: &'static str,
    },
    /// 必需字段缺失或为空。
    Missing {
        field: &'static str,
    },
    /// 凭据来源同时提供或提供后内容无效。
    CredentialSource,
    /// 该角色没有 Paper 虚拟账户语义却声明了初始资金。
    PaperCashOnRealVenue,
}

impl WorkerConfig {
    /// 该角色的凭据来源是否已按“env 与 files 二选一且内容有效”提供。
    pub fn has_valid_credential_env(&self) -> bool {
        env_ready(self.credential_env.as_ref())
    }

    pub fn has_valid_credential_files(&self) -> bool {
        files_ready(self.credential_files.as_ref())
    }

    /// 按 [`WorkerRole::field_scopes`] 逐项判定该 worker 的字段组合。
    ///
    /// 判定与 Venue 无关：把凭据或名义额上限配给不读取它的角色，在任何 Venue
    /// 下都是同一种配置错误，不能因为不是某个 Venue 就被放过。
    pub fn role_field_status(&self) -> RoleFieldStatus {
        let scopes = self.role.field_scopes();
        let required = [
            ("account_id", scopes.account_id, filled(&self.account_id)),
            ("venue_id", scopes.venue_id, filled(&self.venue_id)),
            ("endpoint", scopes.endpoint, filled(&self.endpoint)),
        ];
        for (field, scope, present) in required {
            if scope == FieldScope::Required && !present {
                return RoleFieldStatus::Missing { field };
            }
        }
        let optional: [(&str, FieldScope, bool); 11] = [
            ("account_id", scopes.account_id, filled(&self.account_id)),
            ("venue_id", scopes.venue_id, filled(&self.venue_id)),
            ("endpoint", scopes.endpoint, filled(&self.endpoint)),
            ("symbols", scopes.symbols, !self.symbols.is_empty()),
            (
                "settlement_currency",
                scopes.settlement_currency,
                filled(&self.settlement_currency),
            ),
            (
                "credential_env",
                scopes.credentials,
                self.credential_env.is_some(),
            ),
            (
                "credential_files",
                scopes.credentials,
                self.credential_files.is_some(),
            ),
            (
                "instrument_spec_path",
                scopes.instrument_spec_path,
                self.instrument_spec_path.is_some(),
            ),
            (
                "paper_initial_cash_raw",
                scopes.paper_initial_cash_raw,
                self.paper_initial_cash_raw.is_some(),
            ),
            (
                "max_order_notional_raw",
                scopes.order_notional_limits,
                self.max_order_notional_raw.is_some(),
            ),
            (
                "max_position_notional_raw",
                scopes.order_notional_limits,
                self.max_position_notional_raw.is_some(),
            ),
        ];
        for (field, scope, configured) in optional {
            if scope == FieldScope::Forbidden && configured {
                return RoleFieldStatus::Forbidden { field };
            }
        }
        if scopes.credentials_scoped() {
            let env = self.credential_env.is_some();
            let files = self.credential_files.is_some();
            // 同时提供两套来源时，读取顺序会静默决定使用哪一份；只允许一种。
            if env && files
                || env && !env_ready(self.credential_env.as_ref())
                || files && !files_ready(self.credential_files.as_ref())
            {
                return RoleFieldStatus::CredentialSource;
            }
        }
        if self.paper_initial_cash_raw.is_some()
            && !self
                .venue_id
                .as_deref()
                .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
        {
            return RoleFieldStatus::PaperCashOnRealVenue;
        }
        RoleFieldStatus::Ok
    }
}
