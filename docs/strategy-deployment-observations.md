# Strategy deployment observations

`GET /strategies/deployments` returns operational observations for configured,
enabled Strategy workers. `?worker_id=...` selects one exact worker; an unknown
worker returns 404. The endpoint uses the existing API authentication and rate
limits. An unconfigured provider, unreadable or invalid observation returns 503.
Unknown query parameters return 400.

The configured API host reads the same durable observations that Strategy workers
write under `storage.data_dir/strategy-observations`. Observations are stored per
worker and process owner with compact hashed path components. Each record binds
the observation to a SHA-256 digest of the complete parsed RuntimeConfig. This
lets the reader distinguish a process running an earlier configuration from the
currently requested deployment, including multi-strategy worker selection.

```python
from qianxing import AppClient

client = AppClient("http://127.0.0.1:18082")
print(client.get_strategy_deployments())
print(client.get_strategy_deployments("strategy-paper"))
# Async clients can await client.async_get_strategy_deployments(...).
```

Project CLI users can query the same endpoint without importing strategy code:

```sh
qianxing project deployments ./my-project --api-url http://127.0.0.1:18082
qianxing project deployments ./my-project --api-url https://api.example.com --worker-id strategy-paper --ca-cert ca.pem --client-cert operator.pem --client-key operator-key.pem
```

The worker defaults to the project's `worker_id`; `--worker-id` overrides it.
If neither is set, the query lists configured enabled strategy workers. TLS uses
normal certificate verification and supports the existing mTLS boundary.

The browser console also displays worker state, observation time and blocking
reasons. It renders these values as text through the existing authenticated API
connection. Pause/resume still use the audited `/control/commands` workflow and
must identify one worker.

| State | Meaning |
|---|---|
| `unobserved` | No durable process observation exists. |
| `initializing` | The worker started initialization. |
| `running` | The worker loop is running and lifecycle initialization evidence has been observed. |
| `strategy_initializing` | The worker process is running, but a custom host has not yet completed its first successful evaluation. |
| `paused` | The process observed its strategy in Paused state. |
| `stopped` | The worker completed a normal exit, including `--once`. |
| `failed` | The reporting scope exited without successful completion. |
| `stale` | The observation expired or its timestamp is in the future. |
| `configuration_changed` | The observed configuration digest differs. |
| `ownership_conflict` | Multiple process owners have fresh active observations. |

The Runtime store exposes accepted operations in deterministic acceptance order and supports atomic, idempotent accepted-to-terminal outcome persistence. The Runtime supervisor consumes this queue and restores the latest successful worker revision from the durable registry on restart, including stopped state and verified artifact binding. Deploy and rollback request EOF on the old strategy worker and require graceful exit within the configured shutdown budget; a timeout aborts the operation and never force-kills the old owner to make a replacement appear safe. write the selected frozen configuration beside the source config so relative data paths keep their meaning, bind the verified bundle path, and start the replacement. Success is recorded only after a fresh observation matches the replacement PID, Running phase, and exact effective config digest. Stop records success after process exit. Pause/resume submit authenticated, audited control commands to the durable strategy command queue, then require both a final executed audit record and a fresh target-phase observation from the same worker PID. A still-accepted command or missing observation leaves the deployment operation accepted for retry, so a delayed command cannot mutate a worker after the deployment operation was marked failed. A replacement that fails materialization or readiness is stopped, and the supervisor restarts the previous worker from its frozen active config snapshot before recording failure. If either process cannot be fenced or reaped, or restoration cannot be confirmed, the supervisor aborts instead of reporting a terminal outcome, leaving the operation accepted for operator-visible recovery. This process fence does not cover detached descendants. The supervisor also binds each strategy process to its deployment revision; the durable execution cursor records the source/target revision and migration result. A revision handover is accepted only when the existing checkpoint identity validates against the new strategy configuration; incompatible checkpoint state fails closed, restores the previous worker, and remains a failed deployment until an authorized converter is available. Running is process readiness rather than proof of trading readiness. Explicit file-backed Python strategies now snapshot bounded `.py` files beneath the entry directory (excluding VCS, virtual-environment, cache, build, and node_modules folders) into a content-addressed tree with a sorted per-file SHA-256 manifest; the supervisor verifies that inventory and rejects extra files before launch. Import-name modules and non-Python runtime resources are still not bundled. Until those gaps close, `accepted` must not be presented as an applied deployment.

Checkpoint identity uses a versioned SHA-256 over the complete strategy behavior config. This intentionally treats path changes as identity changes and keeps the config payload itself out of the durable cursor; only the digest is persisted. It prevents newly added strategy fields from being silently omitted from handover checks. A target deployment may now provide `checkpoint_state_migration` to authorize one exact state conversion: the manifest names the source and target deployment revisions, source strategy-binding SHA-256 and source checkpoint SHA-256, and supplies the converted strategy-state object. The adapter-owned state remains unchanged. The manifest is capped at 64 KiB and one-use by those source digests; the cursor stores its SHA-256 and source/target checkpoint digests before the worker starts. The manifest itself is excluded from behavior identity so later revisions do not need to carry an old migration declaration forward. Without a matching manifest, identity changes remain rejected.

Rollback follows the same monotonic revision rule as deployment. The supervisor rejects a rollback operation targeting the currently active revision or any older revision before pausing or draining the worker. To restore an earlier strategy configuration, create a new immutable revision with the next revision number and the selected earlier configuration, then submit `Rollback` for that new revision. If its checkpoint identity differs from the active checkpoint, attach a `checkpoint_state_migration` bound to the active source revision and exact checkpoint digest. This keeps cursor revision fencing monotonic and prevents a failed downgrade from interrupting the active worker.

The manifest is stored in the target strategy's runtime configuration, so it travels with the immutable deployment revision that authorizes the migration:

```json
{
  "checkpoint_state_migration": {
    "schema_version": 1,
    "source_deployment_revision": 6,
    "target_deployment_revision": 7,
    "source_strategy_binding_sha256": "<64 hex characters>",
    "source_checkpoint_sha256": "<64 hex characters>",
    "target_strategy_state": { "position_model": "net", "last_signal": null }
  }
}
```

The target strategy ID/version are taken from that same revision and written into the converted checkpoint. The source digests must be taken from the durable cursor; a revision number alone never authorizes conversion.

The authenticated `GET /strategies/deployments` projection includes the newest operation and the bounded newest-first operation history (at most 20 records), with desired revision/process state, progress, and failure details alongside observed process state and readiness blockers. It also reports the exact configured account/venue reconciliation state (`missing`, `invalid`, `future`, `stale`, `discrepant`, or `clean`), report counts, and freshest age. The console displays this evidence and the five newest historical operations. A clean reconciliation is one required live-admission input; it does not establish full trading authorization, so `trading_enabled` remains `null` until the complete readiness contract is wired. An observed Running worker is not treated as ready.

As of 2026-10-28, the Runtime strategy command path adds a fail-closed admission
check immediately before live intent persistence. New and recovered live commands
must hold a freshly claimed job lease and pass a current fencing claim, and have a
clean reconciliation for the exact account and venue within 60 seconds; every
matching report still inside that window must be clean. New
intents also recheck the current market-input digest. Prepared recovery resumes its
frozen, previously admitted intent identity after fencing and reconciliation checks;
it does not replace that input with a newer market snapshot. Missing, future-dated,
stale, malformed or discrepant reconciliation reports reject the command. This
covers the local strategy-to-command boundary. The live venue execution guard also
rejects account-scoped `PendingSubmit`, `Submitted`, `CancelPending` and `Unknown`
orders until reconciliation advances them to a known state. The lifecycle observation v2 reports construction, initialization, checkpoint-input presence, and per-instrument warmup evidence across Runtime hosts; observation v3 adds external-process restore-ack evidence. Restore acknowledgment binds each restored checkpoint to its input SHA-256, and absent or mismatched acknowledgments reject the response before cursor commit. Live intent admission and deployment readiness now consume that evidence. Binance reconciliation recovers missing fills through signed `myTrades` queries only when returned trade IDs prove the local and remote cumulative totals; it preserves the trade identity and fee currency before writing an authoritative order-status fact. Missing trade identities, incomplete history, or discrepant totals remain blocked. The required account-query scope declares `trades.read`; complete cross-venue command-to-outcome recovery remains open. Runtime
orders until reconciliation advances them to a known state. The lifecycle observation v2 reports construction, initialization, checkpoint-input presence, and per-instrument warmup evidence across Runtime hosts; observation v3 adds external-process restore-ack evidence. Input schema v10 carries the Rust-computed checkpoint digest through JSON and columnar transports; external processes echo it after restore, and Rust rejects absent or mismatched acknowledgments before accepting output or committing the cursor. Live intent admission and deployment readiness consume this evidence. Binance reconciliation recovers missing fills through signed `myTrades` queries only when returned trade IDs prove the local and remote cumulative totals; it preserves the trade identity and fee currency before writing an authoritative order-status fact. Missing trade identities, incomplete history, or discrepant totals remain blocked. The required account-query scope declares `trades.read`; complete cross-venue command-to-outcome recovery remains open. Runtime
Python class strategies additionally require the returned durable checkpoint to
list every candidate intent instrument in `warmup_completed`; evaluation emits no
live intent until this evidence is present. Construction or `on_init` errors stop
the evaluation before intent generation.

Checkpoint-capable live strategies also use the first cold-start evaluation only to
commit their initial durable checkpoint. Its candidate orders are not persisted;
the next fresh market trigger is evaluated after that checkpoint is available.
During recovery, an unsubmitted prepared batch can commit its checkpoint without
submitting its candidate orders. A prepared batch with any already-persisted
command is retained and blocked until the separate unknown-submission recovery
path can resolve it.

The Binance reconciler compares its remote order snapshot with the account
EventLog and appends a replayable `OrderStatusReconciled` fact only when cumulative
fills match exactly (using exchange precision when available). When remote cumulative fills are
ahead, Binance `myTrades` backfills only the missing identities after the complete history, already
recorded IDs, and exact quantity totals reconcile. The fact retains the Binance order ID, and the
account projection exposes it after recovery. This can resolve accepted, working, rejected,
cancelled, expired or filled outcomes after a lost response. A missing trade identity, incomplete
history, missing remote order, fill mismatch, or illegal lifecycle transition remains
`ReconcileRequired`; the reconciler does not fabricate fills from cumulative quantities. Other
venues and complete cross-host recovery remain separate work.

The separate Runtime launcher is the only caller that supplies server delivery
capabilities. A backtest's audit worker ID does not make it a Runtime worker.
Cross-process tests cover rejection of Runtime requirements from that backtest
launcher, preventing it from claiming account updates or durable Runtime delivery.

Running is process readiness rather than proof of trading readiness. File-backed Python
strategies snapshot bounded `.py` files beneath the source root (excluding VCS,
virtual-environment, cache, build, and node_modules folders) into a content-addressed
tree with a sorted per-file SHA-256 manifest; the supervisor verifies that inventory
and rejects extra files before launch. A dotted import-name module is resolved only
beneath the runtime configuration directory, which is its explicit trusted source
root; resolution rejects ambiguous module/package pairs and symlink escapes, and
never searches installed `site-packages`. The frozen tree root is added to the
strategy subprocess import path after verifying that it contains the entry module,
so package imports keep their original root-relative behavior. Additional non-Python
resources must be listed in `strategy.python_resource_paths`; each path is relative
to the trusted source root, exposed to the worker as `QX_STRATEGY_DEPENDENCY_ROOT`.
Paths must resolve to regular non-symlink files and are frozen with the same
per-file size, tree-size and count limits as Python sources. The
supervisor rejects missing, changed, or unmanifested files. Complete trading-readiness
proof and Windows/Linux process lifecycle acceptance remain open. Until those gaps
close, `accepted` must not be presented as an applied deployment.

Schemas: [deployment status](../schemas/strategy-deployment-status-v1.json),
[worker observation v1](../schemas/strategy-worker-observation-v1.json),
[worker observation v2](../schemas/strategy-worker-observation-v2.json),
[worker observation v3](../schemas/strategy-worker-observation-v3.json),
[worker lifecycle evidence v1](../schemas/strategy-worker-lifecycle-v1.json),
[worker lifecycle evidence v2](../schemas/strategy-worker-lifecycle-v2.json),
[deployment revision](../schemas/strategy-deployment-revision-v1.json), and
[deployment operation](../schemas/strategy-deployment-operation-v1.json).

The worker now writes observation schema v3, which keeps the v1 process/phase
fields and adds typed host lifecycle evidence: `host_id`, construction, first
successful initialization/evaluation, whether a committed checkpoint was supplied
to the host, whether an external-process restore ack is required and was verified,
and per-instrument warmup applicability/required/completed facts. The service accepts
stored v1/v2 observations as legacy records, while new supervisor starts wait for
v3 construction evidence. Deployment status reports
`strategy_initializing` until hosts with deferred initialization complete their first successful contract evaluation and
adds `strategy_warmup_incomplete` when applicable. These observations describe
worker lifecycle; they do not authorize live trading.

When a durable external-process checkpoint is supplied, the worker records restore
ack as pending until the response's digest-bound acknowledgment validates. Live
intent admission also checks that acknowledgment, and deployment status reports
`host_checkpoint_restore_not_acknowledged` while required evidence is pending.


## Multi-node deployment projection (in progress)

The API defines a node-scoped deployment snapshot and projection contract. Each registered node has one authenticated operator identity and a bounded freshness window; the aggregator rejects unregistered nodes, operator mismatches, duplicate identities, duplicate worker IDs, and future-dated snapshots. Deployment rows remain nested under their source node, so equal worker IDs on different nodes never become one synthetic owner. `GET /strategies/deployment-nodes` returns the registered projection. RuntimeConfig can explicitly register up to 1,024 node/operator pairs. Aggregator nodes must use mTLS; each peer maps to a uniquely registered ReadOnly operator. `POST /strategies/deployment-nodes` derives identity from the mTLS transport (the body cannot claim a node), rejects query parameters, future/invalid/replayed content, and stores the latest snapshot atomically with cross-process locking under a 64 MiB bound. The GET route is wired to that store and reports unobserved/fresh/stale per node.

For a reporting node, `api.strategy_deployment_publisher` enables the publisher in `qx-service::http_host::serve_api`. It sends the local deployment projection immediately at startup and then at `interval_ms` using a client identity that the aggregator has mapped to that node's unique ReadOnly operator. The publisher trusts only the configured `server_ca`, verifies the server name from `server_host`, presents `client_certificate_chain` and `client_private_key`, caps each JSON request below the API transport's 1 MiB request limit, and stops with the API worker. Certificate/key loading errors prevent startup; transient publish failures are logged and retried next interval.

Aggregator-side configuration has `api.transport: "mtls"`, server `api.tls`, a ReadOnly operator certificate, and a matching registry entry:

```json
{
  "api": {
    "transport": "mtls",
    "operators": {
      "deployment-east": {
        "permission": "ReadOnly",
        "certificate": "certs/east-client-chain.pem"
      }
    },
    "strategy_deployment_nodes": [
      {
        "node_id": "east",
        "operator_id": "deployment-east",
        "freshness_window_ms": 15000
      }
    ]
  }
}
```

The reporting node config uses its registered client certificate and the aggregator's CA:

```json
{
  "api": {
    "strategy_deployment_publisher": {
      "server_host": "aggregator.example.internal",
      "server_port": 19090,
      "server_ca": "certs/aggregator-ca.pem",
      "client_certificate_chain": "certs/east-client-chain.pem",
      "client_private_key": "certs/east-client-key.pem",
      "interval_ms": 5000
    }
  }
}
```

Both fragments are additive to a complete runtime configuration; certificate paths are resolved by the process working directory. Cross-node operational acceptance is still required to confirm a real mTLS handshake, registered identity mapping, restart behavior, and freshness transitions on the deployment platform.


## Strategy JobQueue 容量边界

运行时 `scheduler.max_pending_jobs` 限制该配置共享的未确认作业数，默认值为 4,096，可设为 1..=4,096；文件、SQLite、PostgreSQL 后端在各自的原子入队边界检查上限，达到上限后显式返回 Conflict 并保留已有作业。每个序列化 Job envelope 最大 16 KiB，避免单条作业绕过数量上限形成大对象。Strategy worker 每轮最多读取 256 条当前 owner 可领取的作业，并在下一轮继续排空。升级前已存在的超限队列不会被删除；新入队继续拒绝，worker 分批排空到配置上限以下。

配置上限只约束 durable JobQueue 的未确认记录，不包含 `ControlCommandQueue`、数据库之外的其他进程内队列或外部 broker；扩展部署仍需按其独立的队列策略设置容量。

`maturity/acceptance-artifacts/465422b3-b760-400c-a2d7-c7e758231d35.log` confirms the PostgreSQL storage feature builds on Windows and its database-independent storage tests pass. PostgreSQL integration tests remain ignored because this environment has no `QX_TEST_POSTGRES_DSN` or running server; this is compile/unit evidence only, not PostgreSQL capacity or worker recovery acceptance.

## Paper deployment lifecycle acceptance

`maturity/evidence/strategy-deployment-lifecycle-2026-11-04.json` records a local Windows loopback run through the authenticated console BFF and the real supervisor. It completed deploy, pause, resume, rollback, failed replacement with previous-config restoration, and stop. The failed replacement advanced the durable deployment cursor before the strategy host exited; restoration now keeps the prior config while binding the worker to the attempted monotonic revision. Stopped revisions may retain a configured Strategy worker while marking it disabled, so runtime validation can distinguish configured identity from whether the deployment should run.

This Paper run started no venue worker, sent no orders, made no external network requests, and did not assess API/live readiness. The reusable runner now supports Windows and POSIX process control plus file, SQLite, and PostgreSQL storage. Full readiness proof and native macOS evidence remain open. Linux/PostgreSQL deployment lifecycle and supervisor-restart acceptance is wired into the `strategy-storage-recovery` CI job; its evidence remains pending that job's execution and review.

## R13 SQLite lifecycle and reconciliation gate

`maturity/evidence/r13-strategy-deployment-lifecycle-sqlite-2026-11-04.json` repeats the local Paper deploy/pause/resume/rollback/failure-restoration/stop sequence with the control store and control-command queue configured for SQLite. The deployment revision registry, strategy observations, and execution cursor remain file-backed under `storage.data_dir`; this evidence therefore proves the SQLite control-plane path and its interaction with file-backed deployment state, not PostgreSQL or a database-backed checkpoint cursor.

`maturity/evidence/r13-strategy-deployment-lifecycle-windows-sqlite-restart-2026-11-05.json` repeats that sequence, stops and restarts the real supervisor, then verifies the durable desired revision remains 4 with `desired_running=false` and a stopped observation. The public projection correctly ages to `stale` because the stopped worker is no longer producing observations; it does not relaunch the strategy after restart.

PostgreSQL runtime profiles now store the deployment registry and execution cursor/checkpoint in transactional JSON state. Schema v4 adds content-addressed `BYTEA` storage for strategy bundles and Python dependency files. The supervisor verifies hashes when loading those blobs and materializes a checked local cache for worker startup; pre-v4 registry rows can import their verified files lazily. Cursor import/fencing and blob/cache contracts run in the PostgreSQL service CI job. The Linux storage-recovery job now also drives the authenticated Paper deployment lifecycle through PostgreSQL, stops and restarts the supervisor, and validates that the stopped desired revision and observation survive; the observation may correctly project as stale because no worker should be running. Lifecycle and checkpoint evidence remain pending job execution and review.

`strategy_deployment::reconciliation_gate_tests` exercises the same gate used before live resume and rollback. It rejects missing reports, wrong account identity, stale reports, and order discrepancies, and admits only a fresh clean report for the configured account and venue. The tests use local report snapshots and do not claim a live venue operation. PostgreSQL and cross-platform lifecycle evidence remain outstanding.

## R14 local two-node mTLS acceptance

`maturity/evidence/r14-two-node-mtls-2026-11-04.json` records two independent reporting API processes publishing over verified mTLS to a separately configured aggregator. Both nodes deliberately report the same worker ID; the aggregator preserves each row under its authenticated node and operator. The run also rejected a body-supplied node identity, a future timestamp, an older replay, and an unregistered client identity; it observed one node become Stale while the other stayed Fresh, recovery after publisher restart, and durable snapshot reads after aggregator restart.

The runtime run exposed and fixed an authentication ordering defect: the listener inspected the TLS peer certificate before the lazy TLS handshake had completed, leaving every mTLS connection without a mapped operator. The transport now completes the handshake before the API resolves the certificate to an operator, with a five-second whole-handshake deadline. This acceptance used isolated loopback processes and a short-lived local CA; it does not establish routing, firewall, or certificate-provisioning behavior between deployment hosts. The reusable runner and evidence checker are `tools/accept_strategy_deployment_nodes.py` and `tools/check_strategy_deployment_nodes_evidence.py`.

## R10 Venue submit crash recovery

`maturity/acceptance-artifacts/23934a5d-8099-40f2-b470-5501d60b7432.log` records two Windows file-EventLog process-exit cases. In the before-effect case, the child exits at the Venue call boundary before the simulated Venue changes remote state; in the after-effect case, the simulated Venue accepts the order and the child exits before the local `Accepted` fact is appended. Both restarts replay the durable `Submitted` intent, record `ReconcileRequired`, and block a second Venue call. The after-effect case confirms the simulated remote call count stays at one; both paths end in `Unknown` until reconciliation.

`maturity/acceptance-artifacts/e28fc9ab-997c-4961-953d-350e8fd8e8ce.log` repeats the submit-after-effect process exit with SQLite EventLog storage. SQLite reopens the same durable `Submitted` intent and prevents the recovery worker from making another Venue call. This adds a SQLite crash-boundary result alongside the file backend; PostgreSQL remains untested.

`maturity/acceptance-artifacts/eee1a461-011d-4036-a7bf-72653ca150c6.log` extends the crash matrix through authoritative reconciliation: the before-effect path becomes `Rejected`, the after-effect path becomes `Accepted`, and both states survive a second reopen. `maturity/acceptance-artifacts/368f2f7a-e957-4dc3-9bbb-6b4dceb4204a.log` covers a Venue timeout: the order is durably `Unknown`, a restarted worker cannot resubmit, and a later authoritative `Rejected` snapshot closes the order and survives replay.

`maturity/acceptance-artifacts/0e5a0b9b-d9e2-4132-9553-e11d3096de21.log` exercises the full Strategy worker's durable unknown-command recovery in file and SQLite configurations. The worker holds its execution cursor while command state is uncertain, then waits for Venue facts and a fresh clean reconciliation report before resolving the command and advancing the cursor. The subprocess fixture includes process restarts and does not contact an external venue.

`maturity/acceptance-artifacts/f053dd0e-bf2e-4972-8b72-30124a623289.log` adds assertions over the durable V3 observation produced by those restarted file/SQLite workers. The default Rust Strategy host now reports the `builtin_rust_strategy_runtime` capability catalog (`Bar` and `BarHistory`) as well as lifecycle v2 construction/initialization. The deployment readiness projection consumes that observation without `host_capabilities_not_observed` or `host_lifecycle_not_observed`; `trading_enabled` remains null. The acceptance exposed and fixed the missing default Rust host catalog. The fixture does not authorize trading or contact a Venue.

This closes the local submit-side crash-before/after-effect slice for file and SQLite EventLog storage, plus the file-backend timeout and Accepted/Rejected reconciliation paths, alongside the existing cancel-side crash test and Strategy worker file/SQLite restart acceptance. It does not complete R10-AC01: PostgreSQL storage, Linux/cross-host runtime acceptance, and authorized testnet/live operation remain open. The tests use simulated Venue facts and send no real order.

Linux target compilation was attempted from Windows for both `qx-cli` and `qx-runtime`. The installed Rust target is present, but both builds stop in `ring` because the host has no `x86_64-linux-gnu-gcc`; WSL also has no usable distribution/command interface on this machine. These failed checks are retained in the R10 evidence list and do not count as Linux build or runtime acceptance.
