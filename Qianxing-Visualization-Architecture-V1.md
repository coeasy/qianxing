# Qianxing Visualization Architecture V1

> **V1.1 审计修订说明**：本文是可视化与控制面设计输入，不是实现完成声明。以下修订以当前仓库实际存在的 `qx-api`、`qx-control`、`qx-protocol`、`qx-runtime`、`qx-storage` 和 `qx-cli serve` 为事实基线；原文中未落地的 `qx-schema`、`qx-viz`、`qx-server`、`qx-backtest` 和 `/ws` 不再视为必须创建的独立模块或路径。

可视化只能消费由 EventLog 派生的只读投影，不能成为第二个事实源，也不能绕过 EngineCommand 修改交易状态。

## 1. Overview

Qianxing Visualization Platform (QX-Viz) is the visualization layer for
the Qianxing quantitative infrastructure.

The goal is to provide:

-   Independent desktop client
-   Web server deployment
-   Notebook and SDK integration
-   Plugin-based visualization extensions
-   Multi-asset quantitative research visualization

Core principle:

> Visualization is separated from computation. One result model supports
> multiple clients.

------------------------------------------------------------------------

# 2. Overall Architecture

    Users

    Desktop Client      Web Client      Notebook / SDK

            \              |              /

              QX Visualization Layer

                      |

                 QX API Gateway

                      |

        --------------------------------

        Runtime   Research   Data   Plugin

        qx-core   qx-factor  qx-data

V1.1 authoritative flow:

    Users
       |
    Desktop / Web / Notebook / SDK
       |
    Query API + EngineCommand API
       |
    qx-api  ->  Auth / Permission / Rate Limit / Audit
       |
    StateReplica + AuditStream + Snapshot/Cursor Projection
       |
    EventLog  <-  EventEngine  <-  Data / Strategy / Execution Facts
       |
    ExecutionEngine -> OMS -> Risk -> Venue Adapter -> External Venue

Research artifacts from finkit enter through `ResearchBinding`;
visualization never calls a mutable Runtime or Ledger directly.

------------------------------------------------------------------------

# 3. Architecture Principles

## 3.1 Compute and Visualization Separation

The engine does not depend on UI.

Flow:

    Strategy
       |
    Runtime
       |
    Result Schema
       |
    Visualization API
       |
    Multiple Clients

------------------------------------------------------------------------

## 3.2 Unified Data Contract

All visualization clients consume the same schema.

Examples:

-   Market data
-   Factor result
-   Backtest result
-   Portfolio result
-   Risk metrics

------------------------------------------------------------------------

# 4. Repository Architecture

Target structure:

    qianxing

    ├── crates
    │
    ├── qx-core
    ├── qx-domain
    ├── qx-runtime
    ├── qx-data
    ├── qx-factor
    ├── qx-xingban (backtest)
    │
    ├── services
    │
    ├── qx-api
    ├── qx-api / qx-cli
    │
    ├── visualization
    │
    ├── frontend visualization
    │
    ├── apps
    │
    ├── desktop
    │   ├── tauri
    │   └── web-runtime
    │
    ├── web
    │
    └── sdk
        └── python

The repository mapping above is conceptual only. The V1.1 implementation
mapping is:

    crates/qx-core          deterministic kernel, EventLog, replay primitives
    crates/qx-domain        stable domain/read-model boundary
    crates/qx-application   ports and application contracts
    crates/qx-runtime       node composition, workers and LiveEventPipeline adapter
    crates/qx-data          data ingestion, quality and catalog
    crates/qx-strategy      strategy contracts and decisions
    crates/qx-portfolio     target allocation and rebalance plans
    crates/qx-risk          order and portfolio risk decisions
    crates/qx-oms           order lifecycle and idempotency
    crates/qx-execution     execution orchestration and venue boundary
    crates/qx-adapter       market/private/execution/reconcile adapters
    crates/qx-storage       EventLog, snapshot, outbox and query backends
    crates/qx-protocol      versioned wire/account snapshot contracts
    crates/qx-control       authenticated command and audit contracts
    crates/qx-api           HTTP/WebSocket query and command boundary
    crates/qx-cli           composition and operational entry points

`qx-schema`, `qx-viz`, `qx-server` and `qx-backtest` are logical capabilities,
not new crates required by this revision. A future UI may live in a separate
frontend workspace and must consume `qx-api`/`qx-protocol` rather than import
Rust runtime internals.

------------------------------------------------------------------------

# 5. Visualization Layer

Logical client capability:

    frontend visualization packages

Responsibilities:

-   Charts
-   Dashboards
-   Reports
-   Visualization plugins
-   Themes

Modules:

    chart / dashboard / report / plugin / theme packages

    ├── chart
    ├── dashboard
    ├── report
    ├── plugin
    └── theme

In V1.1 this is a logical client capability, not a mandatory Rust crate.
The visualization layer owns `ChartScene`, `PanelSpec`, `ReportSpec`, themes
and renderer plugins. It must not own orders, positions, Ledger, EventLog or
Venue connections. Interactive actions are serialized as `EngineCommand` and
sent through `qx-api`; chart rendering itself is read-only.

------------------------------------------------------------------------

# 6. Data Schema

Introduce:

    qx-protocol / qx-datastruct

V1.1 does not create a parallel `qx-schema` crate. Wire contracts are first
owned by `qx-protocol`/`qx-datastruct`, then exposed as JSON/Arrow schemas with
explicit compatibility tests. Every visualization payload must carry:

```json
{
  "schema_version": 1,
  "kind": "account_snapshot|event|backtest_report|factor_report|chart_scene",
  "run_id": "run-20260915-001",
  "account_id": "paper-main",
  "portfolio_id": "default",
  "venue_id": "paper",
  "as_of": 1726358400000,
  "event_seq": 42,
  "cursor": "42:9f4a",
  "state_hash": 123456,
  "source": "eventlog",
  "lineage": {"dataset_version": "bars-v3", "manifest_digest": "..."},
  "data": {}
}
```

`event_seq` is the committed EventLog sequence; `cursor` is the transport
resume position; `state_hash` identifies the snapshot used by the projection.
They are not interchangeable. A client must resync from a snapshot when the
cursor is too old, ahead, or belongs to a different state hash.

## MarketData

``` json
{
  "type": "bar",
  "symbol": "600519.SH",
  "open": 100,
  "close": 110
}
```

## FactorResult

``` json
{
  "type": "factor",
  "name": "momentum",
  "ic": 0.08,
  "rank_ic": 0.12
}
```

## BacktestResult

``` json
{
  "type": "backtest",
  "metrics": {
    "sharpe": 2.1,
    "drawdown": 0.12
  }
}
```

These three examples are payload bodies only. In transport they must be
wrapped by the common envelope above and include instrument identity,
timezone/calendar, units/precision, data quality, source version and lineage.
`FactorResult` is produced by finkit or a bound research artifact; qianxing
stores the binding and provenance rather than reimplementing factor math.

------------------------------------------------------------------------

# 7. Backend Service

Current protocol boundary:

    qx-api

Responsibilities:

-   REST API
-   WebSocket
-   Authentication
-   Task management
-   Runtime communication

Recommended stack:

-   Rust
-   Axum
-   Tokio
-   Serde
-   Arrow
-   DuckDB

Repository reality and required correction:

- `qx-api` is the current protocol/server boundary; `qx-server` is not a
  required new service.
- `qx-cli serve` composes the API with configured storage, control state and
  worker health. A future `qx-node` binary may improve packaging, but it must
  reuse the same composition root.
- `ApiState` must become a read-model projection. It cannot keep an
  independent EventLog as a second source of truth.
- The missing bridge is `CommittedFact -> ProjectionReducer ->
  StateReplica/AuditStream -> ApiState -> Query/WebSocket`. It must consume
  committed EventLog facts from `LiveEventPipeline` or an outbox, preserve the
  committed sequence, and support snapshot-plus-cursor recovery.
- Projection failure must mark the read model stale and trigger replay; it
  must never mutate OMS/Ledger or silently acknowledge a missing event.

------------------------------------------------------------------------

# 8. API Design

The earlier `/api/v1/*` and `/ws` examples are design placeholders, not
implemented routes. The current
API exposes `/events/live` for cursor-based HTTP streaming and `/stream` for
the WebSocket upgrade. V1.1 uses this lifecycle:

    connect/authenticate
        → receive snapshot {state_hash, event_seq, cursor}
        → receive events strictly after cursor
        → apply only contiguous events
        → on cursor gap/retention loss: resync_required
        → fetch snapshot + cursor, then resume

Current stable query/control routes are:

    GET  /health
    GET  /ready
    GET  /metrics
    GET  /account/snapshot
    GET  /account/snapshot/diff?base_hash=...
    GET  /account/orders
    GET  /account/positions
    GET  /account/balances
    GET  /account/ledger
    GET  /reconcile/reports
    GET  /scheduler/runs
    GET  /control/audit
    GET  /events?after=...
    GET  /events/live?after=...
    POST /control/commands

`/api/v1/assets`, `/api/v1/kline`, `/api/v1/factors`, `/api/v1/backtest`
and `/api/v1/portfolio` are future QueryPort capabilities. They must not be
documented as available until a versioned query model and implementation exist.

The WebSocket server also needs a production hardening pass: connection
handling must not block the listener for all other clients, each connection
needs bounded outbound buffering, heartbeat/idle timeout, authenticated
filters, and a deterministic close/resync path.

------------------------------------------------------------------------

# 9. Web Application

Technology:

-   React
-   TypeScript
-   Vite
-   Tailwind
-   ECharts
-   Lightweight Charts

Applications:

    web

    ├── dashboard
    ├── research
    ├── factor
    ├── backtest
    ├── portfolio
    └── admin

------------------------------------------------------------------------

# 10. Visualization Functions

## Dashboard

Provides:

-   Asset overview
-   Returns
-   Risk
-   Strategy status
-   Signals

------------------------------------------------------------------------

## Market Terminal

TradingView style:

-   KLine
-   Multi timeframe
-   Indicators
-   Trade markers

------------------------------------------------------------------------

## Factor Research

Similar to Alphalens:

-   IC analysis
-   Rank IC
-   Group returns
-   Turnover
-   Correlation

------------------------------------------------------------------------

## Backtest Report

Includes:

-   Equity curve
-   Drawdown
-   Sharpe
-   Volatility
-   Trade analysis

------------------------------------------------------------------------

# 11. Desktop Client

Recommended:

    Tauri + React

Architecture:

    Qianxing Desktop

    Tauri

     |

    React UI

     |

    Local qx-api + runtime composition

     |

    Runtime

Modes:

## Local Research

    Desktop

    ↓

    Local Runtime

    ↓

    Local Data

## Remote Server

    Desktop

    ↓

    Remote qx-api

    ↓

    Cloud Runtime

V1.1 correction: local mode starts the same qx-api/control/query boundary
alongside a local runtime composition; remote mode connects to `qx-cli serve`
or an equivalent composition root. There is no separate `qx-server` truth
source. Both modes must expose the same QueryPort, CommandPort, snapshot,
cursor and error semantics.

------------------------------------------------------------------------

# 12. Web Deployment

Single machine:

    Frontend

    +

    qx-api

    +

    Database

    +

    Worker

Production:

    Nginx

     |

    qx-api

     |

    Runtime Cluster

     |

    Data Storage

The production diagram is a future topology, not current capability. The
accepted baseline is single-node/single-writer with durable EventLog and
read-only projections. A runtime cluster is allowed only after cross-node
cursor ownership, idempotent command submission, fencing, projection replay,
and venue reconciliation have passed the reliability gates in the overall
Qianxing plan.

------------------------------------------------------------------------

# 13. Plugin Architecture

Introduce:

    qx-plugin

Supports:

-   Chart plugins
-   Factor plugins
-   Report plugins
-   Broker plugins

Example:

    plugins

    ├── factor-analysis

    ├── risk-dashboard

    ├── qmt-adapter

    └── trading-terminal

There are two plugin planes and they must not be conflated:

1. Rust `qx-plugin` extends startup-time engine points such as data source,
   venue adapter, matcher, fill model, risk rule and analytics sink. It is
   statically resolved with dependency and cycle checks.
2. Visualization plugins extend chart/report/renderer capabilities in the
   client boundary. They are read-only by default and are isolated from
   private credentials and Venue adapters.

Factor plugins remain a finkit concern. A qianxing visualization plugin may
render a finkit artifact, but it must not create a second factor engine.

------------------------------------------------------------------------

# 14. SDK Integration

Python example:

``` python
import qianxing

run = qianxing.backtest(
    strategy="strategy.json",
    dataset="dataset-version",
    execution_model="paper",
)

qianxing.visualize(
    run_id=run.run_id,
    source="query-api",
)
```

The SDK must return a `run_id`/`RunManifest` reference rather than an
unversioned in-memory result. Visualization then queries the immutable run
artifacts and read models, so Notebook, Web and Desktop show the same result.

Supported:

-   Python
-   Notebook
-   Third-party applications

------------------------------------------------------------------------

# 15. Architecture Review

## Review 1: Full Data Flow

The original flow was incomplete. The accepted flow is:

    Data/Research Artifact
      → Ingress + validation + lineage
      → EventEngine / EventLog
      → Reducer: OMS + Portfolio + Ledger + Risk facts
      → committed projection: StateReplica + AuditStream + RunManifest
      → qx-api QueryPort / CommandPort
      → Desktop / Web / Notebook
      → ChartScene / Report / operator action

An operator action returns through `EngineCommand`, not through a chart or
mutable read model:

    UI action → authenticate/authorize → ControlCommand
      → command queue / EventEngine → execution → committed facts
      → projection → UI update

Result: **not accepted until the EventLog-to-projection bridge is implemented
and tested for snapshot/cursor recovery**.

------------------------------------------------------------------------

## Review 2: Extensibility

Future integrations:

-   QMT
-   PTrade
-   Interactive Brokers
-   Multi asset brokers

No UI changes should be required for a new Venue, but only if the new Venue
implements the existing MarketData/Execution/Reconcile capability contract and
the projection exposes normalized facts. Venue-specific fields remain in an
extension payload with a schema version; they cannot alter the core order or
account state machine.

Result: **conditionally accepted after adapter capability and replay tests**.

------------------------------------------------------------------------

## Review 3: Frontend and Backend Connection

Verified:

    Strategy

    ↓

    Result

    ↓

    API

    ↓

    Frontend

    ↓

    Chart Rendering

Realtime:

    Committed EventLog fact
      → ProjectionReducer
      → bounded EventBus / AuditStream
      → HTTP cursor or `/stream` WebSocket
      → contiguous client reducer
      → UI update

Recovery:

    cursor gap / stale projection
      → `resync_required`
      → snapshot + state_hash + event_seq
      → events after cursor
      → resume

The original document declared this path passed without verifying the bridge,
cursor ownership, authentication, backpressure or listener concurrency.

Result: **reopened; only accepted after the end-to-end contract tests pass**.

------------------------------------------------------------------------

# 16. Development Roadmap

## Phase 0: Contract and projection bridge

-   Freeze envelope fields: `schema_version`, `run_id`, `as_of`, `event_seq`,
    `cursor`, `state_hash`, lineage and consistency.
-   Extend `qx-protocol` instead of creating an unowned `qx-schema` crate.
-   Implement EventLog/Outbox → ProjectionReducer → StateReplica/AuditStream.
-   Make `qx-api` consume the projection and remove its independent event
    truth semantics.
-   Add snapshot-plus-cursor, gap, stale projection and replay tests.

## Phase 1: Query/control surface

-   Stabilize existing account, event, reconcile, scheduler, metrics and
    control routes.
-   Add versioned QueryPort capabilities only when backed by a read model:
    assets, bars, research artifacts, runs and portfolio views.
-   Harden `/stream`: authenticated filters, bounded buffers, heartbeat,
    close/resync semantics and concurrent connection handling.
-   Provide a small dashboard that proves snapshot → live delta → resync.

## Phase 2: Research and run artifacts

-   Bind finkit outputs through `ResearchBinding`, `DatasetVersion` and
    `RunManifest`.
-   Expose backtest reports, equity, drawdown, trades and factor reports as
    immutable run artifacts, not ad-hoc API JSON.
-   Add Notebook/SDK helpers that operate on `run_id` and query contracts.

## Phase 3: Product clients and plugins

-   Build Desktop/Web clients against the same QueryPort and CommandPort.
-   Add read-only chart/report plugin isolation and signed manifests.
-   Keep Rust engine plugins separate from visualization renderer plugins.
-   Do not claim cloud/cluster deployment until cross-node cursor, fencing,
    projection replay and reconciliation gates are complete.

------------------------------------------------------------------------

# Final Architecture

                    Qianxing Clients
             Desktop / Web / Notebook / SDK
                              |
                QueryPort + CommandPort + Auth
                              |
                             qx-api
                              |
             StateReplica / AuditStream / RunManifest
                              |
                 ProjectionReducer + Snapshot/Cursor
                              |
                         EventLog / Outbox
                              |
                 EventEngine + ExecutionEngine
                              |
                 OMS / Risk / Portfolio / Ledger
                              |
                   Adapter / Venue / Reconcile

    finkit → ResearchBinding → StrategyDecision → EngineCommand/OrderIntent
    qx-plugin → startup-time Rust extension points
    UI plugins → read-only ChartScene/Report renderers

The design is considered connected only when every arrow has a concrete
contract, owner, persistence/recovery rule, and test. This document therefore
does not mark the architecture as production-complete merely because a UI can
render a response; the EventLog, projection, cursor, command, execution and
reconciliation paths must all be verifiable end to end.
