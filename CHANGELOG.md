# Changelog


### V13 R9 收口（2026-10-04）· 第 12 轮三扫 · 审计链并发误报、WS 谎报路由、投影键分裂、策略超时错配

- **R9-A 修 `sync_control` 在 SQLite/Postgres 上的并发误报（真缺陷）**：三个后端的 `sync_control` 原先用 `existing.len() > records.len() || 前缀 zip 不等` 判分叉。但 **file 后端的整段 `transact_control` 都在同一把 `.control-plane.write.lock` 内**（读→改→存快照→补链一步到位），而 **SQLite/Postgres 的 `sync_control` 跑在控制面事务 `commit()` 之后**（`sqlite.rs:1084 → :1088`、`postgres.rs:1500 → :1507`）。`PRAGMA busy_timeout = 5000` 让两进程的写事务在提交时排队，于是进程 B 的事务必然读到 A 已提交的状态、它的 plane 是 A 的超集；B 补链在前、A 补链在后时，A 手里的 `existing`（含 B 追加的记录）就比 A 的 `records` 长——前缀逐条一致，旧判据却报 `Conflict`。**后果不是数据错，而是"控制面状态已经提交成功、`transact_control` 却返回 Err"**，调用方按失败重试会撞上重复请求。已把三后端改成只比公共前缀（`common = existing.len().min(records.len())`，前缀内容对不上才 `Conflict`，链更长按幂等空操作收口）。**篡改检测没被放宽**：`a_chain_ahead_of_the_snapshot_fails_the_transaction_inst_of_rewriting_history` 仍绿——它守的是同位内容分叉（`FORGED(99)` 落在 index 1，公共前缀内），不是长度。补回归用例 `audit_sync_control_treats_a_longer_chain_as_idempotent_not_conflict` 钉住三支语义（链更长＝幂等、前缀分叉＝仍拒、链更短＝正常追加）。`file/state.rs` 那段主张"链超前必须永久失败"的注释按新口径改写。**顺带核掉 R9-D2 另报的三条**：三后端 `sync_control` 前缀检查本来就逐字等价、哈希链共用同一份 `audit_entry_hash`（fnv1a）、`append` 幂等判定语义等价、空存储 `read` 三端一致；`sqlite.rs` 的 sequence 用 `max(sequence)+1` 而 file/postgres 用 `entries.len()`，稠密链等价，而 `validate_audit_chain` 读时强制 `entry.sequence == index` 直接拒掉 gap 链，实际不可达，按已验证一致登记。
- **R9-B 修 WS 查询键拒绝的谎报路由**：`crates/qx-api/src/ws.rs:77` 硬写 `"/events/live 不接受查询参数 {name}"`。但 **WS 通道不占路由表**（`deploy/README.md:299`："任何路径带 `Upgrade: websocket` 即在 HTTP 分派前转交"），客户端连 `ws://host/events?foo=bar` 收到的正文点名的是 `/events/live`——运维照那句话去查会查错地方。已取出请求真实 target（`split_target` 的 path 段）按实际路由点名，与 HTTP 侧 `lib.rs:1507` 的 `format!("{route} 不接受查询参数 {name}")` 同口径。同轮核掉 R9-B 其余四条：12 个文档登记码名与代码产出点逐一相等、`KEYLESS_READ_ROUTES`(4) 与 `PROJECTION_SCOPED_ROUTES`(7) 分派正确、OPTIONS 预检三态正确区分 `NotConfigured`/`Rejected`、`refuse_connection` 在 TLS 包装前判预算。**未修的登记面**：`/events` 的 500 分支文档没登记而 `/events/live` 登记了（两条都不可达）、`ApiProjectionKey::validate()` 的"两把都传但一个为空"分支文档没点名、`ControlError` 的 `{:?}` 复合串与 `serde_json` 原始错误串两处客户端无法精确匹配——都属契约文档补写，本轮按登记面处理。
- **R9-C 修投影键归一化不一致导致的同账户分裂**：`ApiProjectionKey::new` 原先不 trim。`market_bridges.rs:100-115` 的 `configured_account_event_logs` 返回**原始** `worker.account_id`/`venue_id`，而 `api_service.rs:343-344` 对快照 header 做 trim、`account_event_log_name` 内部也 trim——配置里写 `" main "` 这种带空白的账户，事件投影落到 `(" main ", venue)`、快照落到 `("main", venue)`，**同一账户在 `state.projections` 分裂成两格**，且两格都在表里、`missing_projection_response` 都返回 `None`，运维侧看不到告警。已在构造器统一 trim，三条路径（投影桥 / 查询串 / 快照 header）自动同一口径。**登记未修**：`/reconcile/reports` 把 `data_dir/reconcile/*.json` 全量返回（多账户下每个 worker 一份），而 `/account/ledger` 只读默认账户——同属 `KEYLESS_READ_ROUTES`，一给全部一给默认，已在 `capabilities.yaml` 登记为 `keyless_read_routes_do_not_scope_by_account`。
- **R9-D 修策略冷启动分支忽略 `python_timeout_ms`**：`strategy_host.rs:613` 的冷启动 helper 直接传写死的 `PYTHON_STRATEGY_TIMEOUT_MS = 2000`，而配置侧 `strategy_validation.rs:218-219` 允许 `python_timeout_ms ∈ [1, 60000]`。预热分支（`workers.rs:131/149`、`strategy_contract.rs:45/60`）都读配置，只有冷启动那条读死值——用户配 30s 在首轮被 2s 掐死。已把超时改为参数并接上 `config.strategy.python_timeout_ms`；那份常量只剩测试在用，改标 `#[cfg(test)]`，避免"常量从未使用"的告警（`cargo clippy -D warnings` 会红）。同轮修 `live_check.rs` 的本机回环判定：字符串前缀 `"127."` 覆盖不到 `[::1]:port`，改成 `SocketAddr::parse().is_ok_and(|b| b.ip().is_loopback())`，与 `topology_validation.rs:54` 的 `api_bind.ip().is_loopback()` 同口径，`localhost` 单列保留。
- **R9-E 类型面孤儿**：门禁三层零读者判据（`pub fn`/`pub const`/`pub(crate) fn`）**完全不索引类型**。独立脚本扫全仓 467 个 `pub struct`/`enum`/`type`：433 个有生产读者，`materialize` 家族的 5 个类型全部只被 `#[cfg(test)]` 消费（`materializer.rs` 的 `#[cfg(test)] mod tests` 在第 287 行，全部调用点在其后），与 R4-C 的 #119 断链登记一致。唯一真孤儿是 `qx-data::DataSchemaVersion`（结构体 + `impl Default`，全仓零引用；同文件 `DATA_SCHEMA_VERSION` 常量被 `provider.rs:25` 的 `BAR_FRAME_SCHEMA_VERSION` 使用，保留），已删除并同步 `lib.rs` 的 `pub use`。`InMemoryProvider`/`MemoryDataStorage`/`TcpHttpTransport` 三个测试专用类型按名字补登（`capabilities.yaml`）。`FactorEvaluator` trait 的两个实现者（`MomentumEvaluator`、`BadEvaluator`）都在 `#[cfg(test)]` 之后，属 #119 断链家族，不动。
- **R9-F 工业级差距复核（结论不变：未达工业级）**：R9-E 重扫终止性——11 处新增 `loop`/`while` 全部有界或属设计意图的常驻 supervisor（`shutdown.rs:65`、`supervisor_stop.rs:39` 等停机信号是本职）；无跨 crate static Mutex、无嵌套锁、无无超时 `Condvar::wait`；Python 三条 serve 路径都有父进程探测或 EOF 出口。14 处 `== "production"` 判定全部一致（都用 `eq_ignore_ascii_case`，且 environment 已由 `ENVIRONMENT_VOCAB` 收口），`scheduler.rs:102` 的 `dry_run` 判定是另一条独立轴不冲突。**0 项会导致进程永挂或真实资源泄漏**。硬缺口仍是 P0-1 sandbox evidence（`capabilities.yaml` 里 `sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端事务语义、P0-3 C ABI 信任根——本轮**没有**新动 P0 项，全部维持「未排期」。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 621 / [FAIL] 0**（`GATE_CHECK_FLOOR` 619 → 621，新增 `audit_sync_control_prefix_check` 与 `websocket_refused_param_route_check` 两颗）；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / **1058 passed / 0 failed / 1 ignored**（R7 基线 1057 + 本轮 1 颗回归用例）；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；`python tools/validate_core.py` 全部自校验通过。行数棘轮按 `--snapshot` 重登记。
- **发布件按终树重打**：本轮改动横跨 `qx-api` / `qx-cli` / `qx-storage` / `qx-data` / `tools` / `maturity` / 文档六个面，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` **12,557,824 字节**，`crates/**/*.rs` 比它新的 **0** 份；`tools/build_python_wheel.ps1` 得 **219,449 字节 / 17 条目**，sha256 `c36d25b700b049aa38c8de73109eb65fd6ca7c349005b28795f624f19d8650ec`，内嵌 `.pyd` md5 `55ced3bad29541d5cc1a89f5ce4805ed` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净 py3.12 venv（`--no-deps --no-index`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`。



### V13 R8 收口（2026-10-04）· 第 11 轮三扫 · 零读者判据第三层盲区、补单闸门漏档、超时误报、控制面口径

- **R8-A 补上零读者判据的第三层盲区**：`PUB_SURFACE_DEF` 只认 `pub fn` / `pub const`，全仓 **500 个 `pub(crate) fn` 整层逃过**——本轮之前没有任何一轮数过它们。新增 `zero_reference_pub_crate_surface_check`（含元判据共 3 颗，**616 → 619**）。`pub(crate)` 天生只在本 crate 可见，所以读者**按本 crate 计数是构造性正确的**，不需要像 `pub fn` 那样为跨 crate 的假活口付代价值得。那种匹配正是 `qx-storage::after` 活到本轮的原因：`AuditFileStore::after` 自己零读者（唯一调用点是 `#[cfg(test)]` 里的 `assert_eq!(store.after(0)…)`），却被 `qx-api` 事件游标里 **26 处**无关的 `after` 参数保活——`crates/qx-api/src/admission.rs:395` 的 `["account_id","venue_id","after"]`、`event_cursor.rs` 的参数与局部变量、`lib.rs` 十余处 `let after = match parse_after_cursor(query)`、`ws.rs` 与 `read_after(&self, after)`。`_production_lines` 其实**已经**剔掉注释行（`return [line for line in _strip_cfg_test_items(text) if not line.lstrip().startswith("//")]`），所以之前怀疑的「英文注释给 `after` 加读者」是错的——真正的原因跨了 crate。同轮把四个 AuditStore 镜像方法删掉：`AuditFileStore::root`（全仓零引用）、`AuditFileStore::after`（测试断言改用 `read()` + `filter(entry.sequence > 0)` 表达同一件事）、`SqliteAuditStore::path`（全仓零引用，`SqliteAuditStore` 只有 `::new` 与 `.sync_control` 两个生产入口）、`PostgresAuditStore::after`。**保留 `append` / `read`**：`read` 被 `append_unlocked`（`lib.rs:1451`）与 `sync_control`（`lib.rs:1473`、`postgres.rs:1324`、`sqlite.rs:1442`）在生产路径**内部**调用，`append` 是 `PostgresAuditStore::sync_control` 的生产入口（`postgres.rs:1338`），两者都不是孤儿——上一轮把「仅测试读者」误写成「零读者」，这一轮按源码内部调用点重判。唯一的新孤儿是 `qx-cli::for_test`：`#[cfg(all(test, feature = "nats"))]` 的复合 cfg 条件 `_strip_cfg_test_items` 剥离不掉（它只认恰好等于 `#[cfg(test)]` 的标记行），于是这个 test+nats 门控构造器看起来像一条生产定义；`EventConsumerHandler` 的三个字段是本模块私有，`tests/` 作为兄弟模块拿不到写权限，只能由定义模块开这个入口，已带理由登记。
- **R8-B 修 `submit()` 补单闸门漏了一档**：`crates/qx-execution/src/lib.rs:428` 的 `submit()` 对已存在的同 `client_id` 订单只挡两类——`is_terminal()` 或 `Accepted|Working|PartiallyFilled|Filled`（已确认，回 `ALREADY_APPLIED`）与 `Submitted|Unknown`（已出网但未确认，转 `ReconcileRequired` 并 Err）。`OrderStatus` 共 11 个变体，**`CancelPending` 掉出两个分支后直接落到 `venue.submit_order`**。而迁移表（`qx-core/src/order.rs:18-79`）里 `CancelPending` 只能走到 `Cancelled|Filled|Unknown`，`Submitted` **非法**——所以对一条撤单在途的订单补发等于用同一个 `client_order_id` 再下一单，这正是闸门本该挡住的那类事故。已并入 reconcile 闸门并改写错误信息（带状态名，原来那句「已离开本地但未有 Accepted 事实」对 `CancelPending` 并不成立）。**`PendingSubmit` 刻意放行**：已核对 `mark_reconcile`（`lib.rs:600`）只 append 一条 `ReconcileRequired` 事件、不写 status；而 `submit()` 里 `venue.submit_order` 的 Err 分支（`lib.rs:494-499`）经 `mark_reconcile` 转 `Unknown`、空回包（`lib.rs:489-492`）也 `mark_reconcile`，所以订单会停在 `PendingSubmit` 当且仅当**远端从未收到**（注册后、出网前失败，或跨重启恢复），补发是恢复动作而非重复下单。审计里那条「PendingSubmit 二次调用会重复记账」的结论不成立（`Accepted` 属 `semantic_replay`，EventLog 按 `correlation_id` 去重），但 `CancelPending` 那半是真的，已修。
- **R8-C 修策略宿主共享输入 ring 的超时误报**：`crates/qx-cli/src/strategy_host.rs:379` 的输入写入循环原来是三臂——`Ok` break、`Err(Full) if Instant::now() < deadline` 睡 1ms、catch-all 报「写入共享 ring 失败」。循环本身会终止，但**到点仍是 `Full` 时落入 catch-all**，运维读到的错误是「ring 满/写入失败」，方向被引向共享内存实现本身；实际是 worker 停止消费输入——与紧接着的输出侧 `Err(Empty)`（`strategy_host.rs:400`，报「worker 响应超时」并 `child.kill()` + `death_note`）**是同一种死法**。已改报「worker 输入写入超时 timeout_ms=…：共享输入 ring 满，worker 已停止消费」，并照输出侧补上 `child.kill()` 与 `death_note` 诊断，避免留下不再消费输入的 worker 让下一次请求再卡满整个预算。
- **R8-D 更正两处对控制面审计的失实口径**：`/metrics` 的 `qx_control_retired_*` 与 `/control/audit` 读的都是 `ApiState.control` 这份**进程内副本**——装配期从持久化控制面载入（`api_service.rs:22`），之后只有本进程 `submit_command` 走 `store.transact` 成功时才回写（`lib.rs:1800`）。worker 进程经 `control_store.transact(execute)` 落盘的命令终态与随之发生的退场**不回流**，所以在本进程下一次 `submit_command` 之前这两条计数都是**滞后上界而非实时值**。原注释写「这两条计数因此挂在每次 `/metrics` 的现读上」，`QueryPort` 那段也只说「`/control/audit` 走 `control_audit()`」——两处都已按实改写，实时化归入 P0 的 QueryService 拆分那层（登记项，本轮不动）。`/control/audit` 与它的三个兄弟端点（`/scheduler/runs`、`/account/ledger`、`/reconcile/reports`）错误口径不齐那半**未修**：`control_audit()` 直读内存不可能失败，要加 503 就必须改从 store 现读，那正是上一条的同一个改造，本轮不做半截。
- **R8-E 工业级差距复核（结论不变：未达工业级）**：硬缺口仍是 P0-1 sandbox evidence（全仓唯一一份 testnet 证据 `outcome:"skipped"`、`credentials_present:false`、`send_orders:false`；`capabilities.yaml` 里 `sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端事务语义（EventLog/Outbox/队列在文件后端非单事务，`append_batch|BEGIN|COMMIT` 在 `crates/qx-storage/src` 只命中 sqlite.rs 两处且都不是这三条链）、P0-3 C ABI 信任根（`c_api.rs:645-865` 有 sha256 + 可选 Ed25519 `load_verified`，但 `ed25519_public_key` 是部署配置字段而非锚定的信任根）。`data_binding.rs` 整模块未接线那一条经核对**已在盘上登记**（允许清单 #53 + 计划 P1-7），本轮不删不接。
- **发布件按终树重打**：本轮改动横跨 `qx-storage` / `qx-execution` / `qx-cli` / `qx-api` 四个 crate，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` **12,555,264 字节**，`crates/**/*.rs` 比它新的 **0** 份；`tools/build_python_wheel.ps1` 得 **219,447 字节 / 17 条目**，sha256 `b299ecfe…`，内嵌 `.pyd` md5 `8618a4b524df2c4d899fad0fe2ffc459` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净临时 venv（`--no-deps --no-index --find-links dist`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 619 / [FAIL] 0**；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / 87 段 / **1057 passed / 0 failed / 1 ignored**（与 R7 基线同数）；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；`python tools/validate_core.py` 全部自校验通过。行数棘轮按 `--snapshot` 重登记。R8-A 删除 4 个方法净减约 25 行生产代码，门禁判据与注释净增约 90 行。


### V13 R7 收口（2026-10-04）· 第 10 轮三扫 · R6-A 的语义漏洞、零读者判据盲区里的三处零多态消费者



- **R7-A 修掉 R6-A 自己留下的语义漏洞**：R6-A 把两条投影失败都改成 `pipelines.remove(&pipeline_key)` + `continue`，注释写「不能留着继续轮询每 250ms 重复同一行错误日志」。但外层是 `while !stop { for in sources { … } thread::sleep(250ms) }`——下一轮 `while` 到这个 key，`pipelines.entry(...)` 是 Vacant，于是 `storage.open_read_only` 重开同一本账本 → `refresh()` → 再次投给 API。`project_event_log` 的失败是内容漂移或序号缺口，**不会自愈**，所以同一行 `eprintln!` 每 250 ms 复发一次，正好是我自己注释里说不会发生的那件事。新增 `permanently_retired: BTreeSet<(String,String)>`：两条投影失败各记一次、循环顶部跳过，退场成为终局。**`refresh()` 失败那一路刻意不入名单**——读盘失败可能是瞬时 IO，下一轮重开重试才正确，而投影失败是账本内容本身冲突，重试只会无限刷屏。顺带把残余的分叉窗口说明白：按账户投影成功、全局投影失败那一支，`state.projections[key]` 已推进一格而 `state.events` 停在原地，两个读面就此冻结在该点并由那行错误日志留证，不再继续发散。锁顺序复核过：`project_event_log` 与 `project_account_event_log` 各自只做一次 `self.state.lock()`，`ApiState` 内无二级锁，无死锁风险。**门禁新增第 616 颗判据**（615 → 616）把这三点钉住：永久退场名单定义恰 1 处、入名单点恰 2 处（两条投影失败各一）、循环跳过点恰 1 处，并按源码偏移量守「按账户投影调用点先于全局投影调用点」（R6-A 的提交顺序本身）。这颗判据补的正是它自己暴露的那个问题——R6-A 的注释与代码矛盾这件事，靠读代码才看得见。
- **R7-B 清掉零读者判据盲区里的三处零多态消费者**：这一轮查的是 `PUB_SURFACE_DEF` 的盲区——它只数 `pub fn` / `pub const`，**不认 trait 定义与 trait 方法**，所以这三处恒绿，前九轮每轮都绿过：①`qx-zhenlu` 的 `VenueAdapter` trait（`capabilities()` 返回 `ConnectorCapabilities`，全仓 `\.capabilities\(\)` 命中 **0** 条，连测试都没有；`health()` 返回 `AdapterHealth`，仅 `binance.rs` 三个 `#[cfg(test)]` 调用点）——trait 与只由它读的两个类型 `ConnectorCapabilities`/`AdapterHealth` 一并删除（后者若留下，删掉方法后就成了无读者的 `pub struct`），`capabilities()` 直接删、`health()` 的三个测试调用点改成直接读同模块的私有字段 `state` / `last_event_ts`。②`qx-execution` 的 `ReconcilePort` trait——全仓无 `dyn ReconcilePort`、无 `where T: ReconcilePort` 约束，三个生产调用点（`binance_reconcile.rs`、`ccxt_execution.rs`、`ccxt_reconcile_worker.rs`）都拿具体类型 `EventLogReconcilePort` 直接调，`reconcile_port_contract.rs` 那份「契约测试」也是直接调具体类型。删 trait，`require_reconcile` 改成该类型的固有 **`pub`** 方法——原来方法本身没有 `pub`，靠 `use ... ::ReconcilePort` 把 trait 拉进作用域才可见，trait 一删必须显式补 `pub`，否则三个跨 crate 调用点直接 `E0624`。三处 `ReconcilePort` 的 doc 引用（`lib.rs`、`reconcile_port_contract.rs` 文件头、`qx-genglu` 的 `VerdictAction` 注释）同轮改口。
- **R7-B 的裁定边界**：`JobQueueBackend` 按同一把尺子**保留**。它的五个 trait 方法（`enqueue_job`/`available_jobs`/`claim_job`/`ack_job_at`/`recover_expired_leases`）在生产链零调用者——生产队列面是 `ConfiguredJobQueue` 枚举，按 `match` 臂调各后端类型的**固有**方法（`enqueue`/`available`/`claim`/`ack_at`/`recover_expired`），两套接口并行存在、连名字都不同。但它唯一的多态消费者是 `crates/qx-storage/tests/queue_backend_semantics.rs` 的 `assert_job_queue_semantics(queue: &dyn JobQueueBackend)`，靠**同一份**断言代码逐个跑 file / SQLite / Postgres 三个后端——这是后端一致性 harness，不是死面，删掉等于删证据。改为在 `storage_consistency_contract.limitations` 登记为 `job_queue_backend_trait_methods_have_no_production_caller`，写明收敛方向（先让两套接口收敛成一份，或先定 `ConfiguredJobQueue` 为什么必须持有构造器而不是裸 trait 对象）。同一轮补 `AuditFileStore::after()` 这类「同名兄弟遮蔽」的失配：门禁按名字找读者，`PostgresAuditStore::after` 有生产读者就把 `AuditFileStore::after`（仅 `lib.rs:2070` 一处用例读者）判成活面——与 `latest_quote` / `query_command` 那次是同一个盲区家族。
- **R7-C 能力矩阵与家族登记同步**：fam02「API 读模型」加续修注——它名下的 6 格里今天已有 4 格在盘（**事件按账户键**由 R5-A 接通、**快照历史有界** `MAX_SNAPSHOT_HISTORY = 1_024`、**控制面现读** `submit_command` 整体换 `state.control`、**停机令牌与连接上界** `session_shutdown` / `connection_budget`），唯一未落的是 QueryService 拆分那层「API 自己那份查询读模型 `EventLog` 无淘汰水位」，R4 那段的技术理由不变（把窗口外的旧事件剪掉会让一次合法的重叠投影查不到而被报成内容漂移）。R6-A2 的第三本台账登记、R6-B 的六处公共面删除也一并补进对应条目。
- **R7-D 口径收口**：终版文档 §1.1 门禁行与 §1.3 标题从 615/六轮改到 616/七轮，§8 一句话结论补第 10 轮段落并把「九轮」改「十轮」。
- **发布件按终树重打**：本轮改动横跨 `qx-cli` / `qx-api` / `qx-storage` / `qx-execution` / `qx-zhenlu` / `qx-adapter` / `qx-genglu` 七个 crate，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` **12,582,400 字节**，`Get-ChildItem crates -Recurse -Filter '*.rs' | Where-Object LastWriteTime -gt exe` 得 **0** 份；`tools/build_python_wheel.ps1` 得 **219,447 字节 / 17 条目**，sha256 `5ed022fe…`，内嵌 `.pyd` md5 `497dcb08bd04e84cbb6154a5b9380e85` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净临时 venv（`--offline --no-deps --no-index`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 616 / [FAIL] 0**；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0（**87 段 / 1057 passed / 0 failed / 1 ignored**）；`python -m unittest discover -s python/tests -q` **64 passed / OK**；行数棘轮按 `--snapshot` 重登记。R7-B 的三处删除净减约 40 行生产代码，R7-A 净增约 25 行（含永久退场名单与门禁判据）。**发布件因此过期**，需按同一套流程重打 `qx-cli.exe` 与 wheel。


### V13 R6 收口（2026-10-04）· 第 6 轮三扫 · R5 自身缺陷、第三本去重台账、六处零读者公共面

- **R6-A 收口 R5-A 自己的提交顺序缺陷**：`spawn_api_projection_bridge` 里 `project_account_event_log` 与 `project_event_log` 各自在内部加锁改状态、返回值只是结果，原写法是**两条都提交完才开始看错误**。于是默认账户的按账户投影失败（`project_event_log` 的内容漂移 / 序号缺口这类不会自愈的冲突）时，全局那格已经被写过——同一个账户的 `/events`（读 `state.events`）与 `/events?account_id=&venue_id=`（读 `projections[key].events`）从此各讲一份，而这恰是 R5-A 的 CHANGELOG 声称已经关掉的那个形状。改成先提交并当场判定按账户那条、成功后才写全局那格。连带修掉我自己在这轮引入的第二个问题：全局投影失败原先只 `eprintln!` 不摘管道、不 `continue`，而 `project_event_log` 的失败不会自愈，于是同一条错误每 250 ms 复刷一次（约 4 行/秒），且 `/ready` 的 `projection_readiness()` 只扫 `state.projections`、永远看不见全局那格，进程照报 200。现在两条失败都摘管道退场。
- **R6-A2 补上第三本只增不减的成交幂等台账**：R5-B 的正则 `seen_(?:fill_keys|trade_ids)\.` 不认 `seen_fills`，而且判据只读 `BINANCE_VENUE_FILE`/`CCXT_PUMP_FILE` 两个文件——`crates/qx-runtime/src/pipeline.rs` 的 `LiveEventPipeline.seen_fills: BTreeSet<(u64,u64,i128,i128,i128,String)>`（键 `(order_id, ts, qty, price, fee, venue_order_id)`，比 Binance 那本多一栏 venue 订单号）整本不在范围内。它只增：唯一读点 `.contains`、三处 `insert`（两条 ingest 路径各一、`rebuild_runtime_indexes` 重启时把整本 EventLog 的 `Filled` 全插回来一处）、全词无 `remove`/`retain`/`drain`/`pop`/`clear`/`truncate`；上界是进程一生 ingest 过的成交数，`LiveEventPipeline` 还是 `#[derive(Clone)]`，克隆一次多复制一整本。**淘汰是否安全：不安全**——它挡住 user stream / reconcile / Outbox 补投影三条路径重投同一笔 fill，去掉任何一条已见键同一笔成交就被 `oms.apply_fill` 记两次账，与 R5 注释写的同一个失败方式。所以该登记 + 扩门禁，不是加水位：正则扩成 `seen_(?:fill_keys|trade_ids|fills)\.`，pipeline.rs 纳入同一颗判据（Binance 1 处 `insert` / CCXT 1 处 `entry` / 运行期 3 处 `insert`），三处字段注释都写明「只增不减的成交幂等台账」，capabilities.yaml 的 `binance_direct.limitations` 从两本改三本。门禁条数不变（615）。
- **R6-B 清掉六处零调用者的公共面**：①`JobQueueBackend::ack_job` 及三个实现（sqlite / postgres / file-jobs）——被带 fencing token 的 `ack_job_at` 取代，全仓 `.ack_job(` 命中 0 条，留着会让读者以为无 token 的 ack 仍在契约里；`ack_at` 那条在用（`tests/queue_backend_semantics.rs` 108/115/119），且 `FileJobQueue::ack` 固有方法仍有用例读者（`lib.rs` 2061 附近的接管测试）。②`AuditStore` 整个 trait 与三份实现——四个方法（`append_record`/`read_entries`/`query_command_entries`/`entries_after`）零调用者，全仓无 `dyn AuditStore`、无 `impl Trait: AuditStore`，生产读审计走固有方法 `AuditFileStore::append` / `append_unlocked`；那份测试改用 `read()` + 内联 filter。③`MarketDataPort` 与 `LiveEventPipeline` 的实现——无 `dyn MarketDataPort`、无 `latest_quote(&dyn)` 调用点，生产用的是同名固有方法 `latest_quote_with_depth`（`spread.rs`、`venue_runtime/paper_submit.rs` 在读）。④`ControlPort` 与 `ApiService` 的实现——无 `dyn ControlPort`、也没有 `control_port()` 访问器（对比 `query_port()` 定义在用），与 `QueryPort` 那条在用路径不对称。⑤`QueryPort::events_after` 与实现——全仓 `events_after` 的其余命中全是另一个函数 `events_after_cursor`（`event_cursor.rs`，`/events`、`/events/live` 走它），trait 方法零调用者。⑥`DataStorage::load_range` 与 17 行默认实现——唯一读点是自己那个测试，用例改成只断言正序。
- **R6-B 的连带孤儿**：删完立刻被零读者判据反查出两条**新**孤儿——`qx-runtime::latest_quote`（唯一读者是刚删掉的 `MarketDataPort::latest_quote` 实现，测试 `paper_bridge_and_bundles.rs` 的那一处断言与紧随其后的 `latest_quote_with_depth` 断言重复，改成对 depth 版补 `ts`/`bid`/`ask` 三个断言）与 `qx-storage::query_command`（文件版与 Postgres 版，唯一读者是刚删掉的 `AuditStore::query_command_entries`）。门禁条数不变（615），但「删了公共面又长出零读者」这条链路被判据当场拦下来，这正是判据该做的事。
- **R6-C 更正一条不存在的节奏**：`crates/qx-api/src/snapshot_history.rs` 头注释原先写「读模型每 250 ms 重装一次账户快照」，于是 1_024 那份数被描述成「约 4 分钟的基线可回看窗口」。事实是投影桥只喂事件读模型（`project_event_log`/`project_account_event_log` 只写 `events`+`event_bus`，**不碰** `snapshot`/`snapshot_history`），快照只在 `build_configured_api_service` 装配时按域装载一次（全仓 `publish_snapshot` 的生产命中只有 `api_service.rs` 两处 boot 调用）。按真实契约改写：首次加载走快照、增量走事件、读侧自己拿 `/events` 做 reducer；1_024 从「4 分钟窗口」更正为内存天花板，窗口实际多长取决于本进程换过几次摘要。
- **R6-D 口径收口**：终版文档 §8 摘要停在 614 且把「第 4/5/6 轮」和「R4」两套编号混着数，§1.1 与 §1.3 已经是 615；统一到 615 与 R1…R6 一套编号，并补上 R6 的四颗。同轮更正 §1.3「三轮共同结论」里「ccxt/binance insert-only map 无界」仍在待办那格的旧说法（R5/R6-A 已补牙齿），以及投影桥那段注释只提 `project_event_log` 漏了 `project_account_event_log`。
- **发布件按终树重打**：本轮改动横跨 `qx-api` / `qx-storage` / `qx-runtime` / `qx-data` / `qx-execution` 五个 crate，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` 12,581,376 字节，`Get-ChildItem crates -Recurse -Filter '*.rs' | Where-Object LastWriteTime -gt exe` 得 **0** 份；`tools/build_python_wheel.ps1` 得 **219,448 字节 / 17 条目**，sha256 `79fac3b2…`，内嵌 `.pyd` md5 `1e2f3ab8611ee6289c8cbb212a435536` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净临时 venv（`--offline --no-deps --no-index`，补 `tzdata`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 615 / [FAIL] 0**（含扩到三本台账的那颗「成交幂等台账只增不减」）；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / 87 段 / **1057 passed / 0 failed / 1 ignored**；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**。行数棘轮按 `--snapshot` 重登记（`pipeline.rs` 2332→2338 注释、`snapshot_history.rs` 注释、四处 orphan 删除使 4 个文件下探）。行数变化为增的为**注释与判据正则**，删除均为零读者公共面，无一改动生产语义。尚未 `git add`、未推送。


### V13 R4/R5 收口（2026-10-04）· 第 4、5 轮三扫 · 断链与门禁盲区收口 · 发布件按终树重打

- **R4-A「无预算等待」族第三处**：`crates/qx-adapter/src/ccxt.rs` 的 `CcxtProcessClient::call` 原先直接 `self.stdin.write_all(payload)` 再等读侧——一旦 CCXT worker 存活但不再从 stdin 取字节，超出 OS 匿名管道缓冲（~64KB）的那段写入永久阻塞，`timeout_ms` 只守读侧，本进程就此卡死且连关停都读不到。修法与 #281（策略宿主）、#282（event consumer handler）同族：`stdin` 降为 `Option<ChildStdin>`，写 `write_all`+`flush` 交给 spawned 线程，主路径按 `write_done.recv_timeout(Duration::from_millis(self.timeout_ms))` 收口，超时即杀 worker 并回「CCXT Worker 输入超时」诊断。新增门禁 `ccxt_pipe_write_budget_check` 三条（写入被交给线程且有界等待、写侧预算取自 `self.timeout_ms`、超时诊断与句柄回收在位）。
- **R4-C 两处门禁盲区**：`PUB_SURFACE_DEF` 旧式 `pub (?:async )?(?:fn|const) (\w+)` 在 `pub const fn` 上把捕获组落到 `fn` 本身——全仓 23 处 `pub const fn` 因此全部映射成 `crate::fn`，而 `fn` 一词到处都是，「零读者」永远数不出来；`pub use` 重导出行曾被当成生产读者，而那行只是把名字摆到 crate 根、没有任何调用。两处修好后立刻放出 `qx-data::load_bar_batch`（批量装载逐条校验 provider 不得返回越界行，能力在盘但生产走 `ingest_bars`）与 `qx-core::default_maker_taker`（生产直接读 `DEFAULT_MAKER_BP`/`DEFAULT_TAKER_BP` 装配，只有用例在调构造器），各带理由登记进允许清单；新增一颗元判据把两处盲区钉住。同时复核 479 个 `pub struct/enum/trait/type` 与 82 条 `pub use` 重导出，除 `load_bar_batch` 外全部有生产引用，允许清单既有 27 条零读者登记无一长出读者。
- **R5-A 接通无键事件读面这条断链**：`GET /events`、`GET /events/live`、不带 `account_id`/`venue_id` 的 WebSocket 三条读面读的是那格全局兼容投影，而 `spawn_api_projection_bridge` 只喂按账户投影，于是生产上这三条读面恒为空数组。修法与快照那条判据同源：桥按 `default_account_event_log` 认出默认账户，把它的那本 EventLog 同时投影到全局兼容读模型，其余账户仍只进自己的投影，多账户部署下两个读面不会各讲一个账户。投影桥函数原有的文档注释（「调用 API 的 `project_event_log`」）此前与代码不符，本次一并落到真实调用。
- **R5-B 给成交幂等台账补牙齿**：Binance `seen_fill_keys` 与 CCXT `seen_trade_ids` 是**只增不减**的去重台账——user stream 与 `trades` 查询都会把同一笔成交重投，淘汰任何一条已见键就等于允许同一笔 fill 被 trace 两次（重复记账），这是本仓库唯一不接受的失败方式。它们刻意不设水位，所以新判据守的是「只增」而不是「有界」：入账点各恰一处、两个字段全词无 `remove`/`retain`/`drain`/`pop`/`clear`/`truncate`、两处字段注释都写明「只增不减的成交幂等台账」，并登记进 capabilities.yaml 的 `binance_direct.limitations`。同期把 `QueryPort` 的注释改诚实：它原先谎称「网络协议只依赖这些只读方法」，而 `/account/orders|positions|balances` 三个端点实际先走 `snapshot_for_query` 做身份解析再取分表，从不走 `account_orders()` 那三个**无身份的默认账户视图**——那三个方法只有用例读者，接到带身份的端点会在多账户部署下静默串账，因此注释写明它们需要身份感知变体才能上生产。
- **门禁 610 → 615（净增五颗）**：R4-A 三颗、R4-C 一颗元判据、R5-B 一颗。行数棘轮按 `--snapshot` 逃生门重登记（本轮涨的是 `qx-adapter/src/binance.rs` 2121→2127、`qx-adapter/src/ccxt.rs` 1174→1180、`crates/qx-api/src/lib.rs` 2979→2990，均为注释与写侧预算）。
- **发布件按终树重打（台账 #251，本轮落地）**：此前 `target/release/qx-cli.exe`（18:40）早于 R4-A 的 `contract.rs`（19:44）与本轮 R5 的 `market_bridges.rs`（22:08）、`binance.rs`/`ccxt.rs`（22:16），而 `dist/…whl`（18:22）里嵌的 `qianxing_bridge/strategy.py` 只有 14504 字节、仓库 15344 字节——**发布物仍是改前那份代码**。本轮：`cargo build --release` 后 `qx-cli.exe` 12,581,376 字节，`Get-ChildItem crates -Recurse -Filter '*.rs' | Where-Object LastWriteTime -gt exe` 得 **0** 份；`python\.venv` 不存在，按 `uv venv python\.venv --python 3.12 --clear` + `uv pip install pip tzdata` 补齐前置后跑 `tools/build_python_wheel.ps1`，得 **219,448 字节 / 17 条目**，`.pyd` md5 `dc8b48c61576334274ff1ba50811f92a` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；再把这份 wheel 装进**另一个**干净临时 venv（`--offline --no-deps --no-index`，补 `tzdata`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 615 / [FAIL] 0**；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets` rc 0 / 87 段 / **1057 passed / 0 failed / 1 ignored**；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0。尚未 `git add`、未推送。


### V13 R7 收口（2026-10-04）· 第 7 轮三扫 · 台账 #284 · R7-a —— 策略输出契约的六臂折叠诊断把「漏写 schema_version」念成「身份不一致」

- **这颗不是扫出来的，是 R6 那次干净 venv 冒烟撞出来的**：策略作者按 `schemas/strategy_api_v1.md` 返回一个 dict 而漏写 `schema_version`，两侧宿主都回同一句折叠诊断（Rust `StrategyContractOutput 与输入身份、标的或有效期不一致`、Python `strategy output identity or expiry does not match input`），于是他去查 `request_id`——那一格本来就没写错，永远查不出问题。跨语言分叉最常见的长相恰好是「worker 侧 Python 已放行、宿主侧 Rust 才挡下」，而那条链上第一格就是版本号；一句折叠话术把七种成因压成一条查不到的线索。
- **落地（两侧同一口径，拒绝集一字未动）**：`crates/qx-runtime/src/strategy_contract/contract.rs:273` 的 `validate_for` 拆成八句各说各话的诊断、首臂点 `schema_version`；`python/qianxing_bridge/strategy.py:290` 同样八句 `ValueError`，空标的走自己那一臂；解码入口 `crates/qx-runtime/src/strategy_contract/contract.rs:379` 的 `from_json_for` 与 `python/qianxing_bridge/strategy.py:337` 的 `from_dict` 各过一次 `validate_for`，Python 缺键读 `0` 而不采纳自家默认版本。牙齿按「折叠优先」走：Rust 侧把 `crates/qx-runtime/src/strategy_contract/tests.rs:54` 那颗既有用例扩成六臂点名表并新增一颗解码入口用例，Python 侧折进既有那颗 `python/tests/test_strategy_contract.py:41`。
- **三条诚实话**：「两侧过期极性不一致」这颗**从来没有存在过**（HEAD 两侧都是 `!= 0`），本轮差点把一句注释当缺陷——它现在被钉成不变量，是钉子不是修复；解码入口那次 `validate_for` 在 HEAD 也已在场，同样是钉子；台账里那句「+0 颗 `#[test]`」**半假**——Python 侧真折叠（`Ran 64` 保持），Rust 侧净增一颗（承担用例文件 HEAD 4 颗 / 工作树 5 颗，全仓磁盘在册 1070 → 1071），地板随之 1070 → 1071。
- **门禁 602 → 610（净增八颗运行时判据）**：轮函数 `strategy_output_arm_diagnostics_check()` 两侧都从源码现取诊断句集合，不写第二份手工清单——把某臂抄回折叠句、删一臂、首臂换人、两侧极性式子分叉、解码入口少一次校验、承担用例改名六种动作全部当场红；`tools/check_architecture.py:542` 的 `GATE_CHECK_FLOOR` 注释里那半句 `602 -> 610` 就是本轮。`_python_scope()` 取不到唯一锚点时返回空串让下游判 FAIL（方向对且不 `die`），剥 `#` 注释让「散文既不替代码定罪也不替代码作证」由 C1/C2 两颗对照枪实测。
- **变异取证 14 枪 + 4 枪重放**：主轮档位现读 `TALLY {'fired': 14, 'killed': 12, 'equivalent': 0, 'masked': 0, 'noconclusion': 0, 'control_green': 2, 'control_bad': 0, 'restore_ok': 14, 'collateral': 0}`、`IDENTITY fired=14 tiers_sum=14 equal=True restore_ok=14`，BASELINE 与 FINAL pristine 同为 `PASS=610 FAIL=0 八颗在场=8/8`，四份注入文件按 sha16 逐颗比回（`contract.rs=a55080f30dfe5b25` / `strategy.py=e1a3c0d255d0436f` / `tests.rs=b9e40c31c37fb113` / `check_architecture.py=7793ea3c13e54f43`）；形状枪 S1/S2 各读三颗 FAIL（`PASS=607 FAIL=3`）证明锚点取空的方向是红不是静默通过；因主轮开火后 `tests.rs` 又被本轮自己的 clippy 修复改过字节，G1/G2/G8/G10 四颗一律在终字节上重放击杀（4）。**最有价值的一格是 G7**：门禁红而 `Ran=64` 套件仍绿——「两侧同一个式子」这条判据眼下没有用例替它作保、它是唯一读者，按规矩登记为下一轮的活（给极性分叉补一颗常驻反例）。
- **本轮实测（九腿整跑在终树上，读数全部由日志派生，文档里没有一个手抄数字）**：树摘要九腿前后同为 `142085caa8745541`、rc 全 0、无 host-locked 腿；`fmt --check` rc 0（`Diff in` 0 行）、`clippy --workspace --all-targets -D warnings` rc 0（诊断 0 行，本轮新写的六臂表先被 `type_complexity` 抓过一次）；`cargo test --workspace` rc 0 / 107 段 / 1057 passed / 0 failed / 1 ignored（86 段 Running + 21 段 Doc-tests），`--all-targets` rc 0 / 87 段 / 1057 passed，`--doc` rc 0 / 21 段 / 0 passed；`--list` rc 0 / 1058 行 / 1056 唯一名，恒等式 1057 + 1 = 1058、1071 − 1069 = 1058 − 1056 = 13（`nats` 10 + `postgres` 3）；Python `Ran 64 / OK`（rc 0，本轮这遍 0 skip）；`validate_core.py` rc 0；门禁 rc 0、**[PASS] 610 / [FAIL] 0**。引用普查（落笔后重跑）：活文档 @CENSUS_LIVE_TOTAL@ 条 `path:NN`、@CENSUS_LIVE_DEAD@ 条落空，存档 @CENSUS_ARCHIVE_TOTAL@ 条、@CENSUS_ARCHIVE_DEAD@ 条落空（天花板 9，只降不升）。**Python 的 skip 口径本轮纠正**：此前 README 与终版计划写的 `OK (skipped=2)` 是「没构建 `_qianxing_native.pyd` / 没装 `pyarrow`」那台机器的读数，文档现在写「读数 + 口径」两句而不是把 2 换成 0 了事。
- **发布件重建义务（台账 #251，本轮未做，按依赖图与字面量普查现读定义务）**：`cargo tree -p qx-python -e normal` 不含 `qx-runtime`，而 `qx-cli` 直接依赖它——本轮改动落在 CLI 二进制与 wheel 的 Python 载荷上。wheel 侧 12 份 `.py` 里与仓库逐字节不符的恰好 1 份，正是 `qianxing_bridge/strategy.py`（wheel 内 14504 字节 vs 仓库 15344 字节，折叠句命中 1 次、逐臂句 0 次）——**wheel 内嵌的仍是改前那份代码**；`target/release/qx-cli.exe` 按字面量面已带八句（各 ≥ 1 次、旧折叠字面量 0 次）但字符串普查不担保产物字节；wheel 内 `.pyd` = `f88cd81076318c342659a35b3afb6b9d` 而 `target/release/_qianxing_native.dll` = `1b971292e74a2bbe2fd898b4a2c7d52c`，重打包时「dll → 包内 `.pyd`」那一拷贝步必须一起走。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.54。**尚未 `git add`、未推送**：Rust 一侧、Python 一侧、门禁八颗、capabilities 那一条证据行与五份文档的活面全部仍在未暂存状态。


### V13 R5/R6 收口（2026-10-04）· 第 5、6 轮三扫 · 发布条件收敛

- **R5 落地的四颗**：`/ws` 的升级分支补上浏览器准入（此前 R1-B 那套 CORS 名单 / 预检 403 / 在途预算 503 / 握手 key 只长在 HTTP 那一臂上，跨源网页可带着凭据起一根有状态 WS——CSWSH），跨源·身份·握手 key 三种判定全部排到写 `101` 之前，握手 key 只读一次、缺失回 400 `missing_websocket_key`，`101` 带的 `Sec-WebSocket-Accept` 取自准入算出的那份会话，五条出口带同一份跨源头；`backtest book` 的 `--latency-snapshots` 越过「永远等不到成熟」那条线时从退 0 落一份空成交表变成本场拒绝并点名可用上限 `帧内快照数 − 2`；三条子进程管道逐行读补 16 MiB 行长闸（`read_capped_line` / `read_capped_worker_line`，越界即中止并点名，不截断——半行 JSON 交给解码器是更难归因的失败）；`qx outbox-relay` 三个出口都把 `parked` 念进摘要行，"这一页搬完了"不再被读成"中继干净"。
- **R6 的结论按两半写**：三扫在第 5 轮终树上**没有再发现可修的运行面缺陷（0 颗）**；剩下的是"有意边界"与"文档没跟上代码"两类。四条全局出口（`/health`、`/ready`、`/metrics`、`/schema/account-snapshot-v1`）不判查询串，裁定维持——它们是探针与网关的落点，判键等于给存活探测加一条 400 路径；接口文档现在明写这一支与"12 条读面入口 + WS 那一臂"的覆盖面。独立策略进程那段此前没写行长闸，现补齐两枚 16 MiB 常量与门禁的 1/2/0 调用形状判据。能力矩阵一处漂移重钉（provider 那四格字段挪成独立条目 `provider_capability_registry` 之后，README 六格按现读改成 **23 / 21 / 386 / 357 / 120 / 697**，并把"以仓库内路径开头"的口径点名为八个前缀——漏 `.github` 会把 357 读成 349）。
- **变异取证 16 枪**：现读 `fired=16 killed=14 equivalent=0 noconclusion=0 control_green=2 control_bad=0`，锚点普查 16、`restore=OK` 16（`ws.rs` / `admission.rs` / `lib.rs` 三份逐 sha 比回）。G1~G8 是 R5 的八条判据（其中 G4 是"行为等价、形状不等价"那一类——用例全绿而门禁 `601/1` 红；G5 额外被 clippy 抓住 `field 'accept' is never read`），H1~H6 是读面名册与 WS 查询串臂，C0/C1 两颗对照枪三腿全绿。
- **本轮自己的三处工具缺陷如实记**：取证脚本的三档恒等式漏算对照枪那一档，`14 != 16` 触发 exit 5，改五档后**从同一份日志重导**（没重开枪）；九腿驱动的 `--list` 分类器把 1057 行读成 289（`split(":")[0]` 撞上测试名里的 `::`），改成按行尾 `: test` 收口并同时报行数与唯一名数；本轮第一枪红在 clippy 的 `useless_format`，是本轮自己写的用例被抓，拆成 `&str` 字面量后 8 行全绿。
- **本轮实测（九腿整跑在终树上，读数全部由日志派生）**：树摘要九腿前后同为一个哈希、无 host-locked 腿、rc 全 0；`fmt --check` 干净；`clippy --workspace --all-targets -D warnings` 诊断 0 行；`cargo test --workspace` 107 段 / 1056 passed / 0 failed / 1 ignored，`--all-targets` 87 段同数，`--doc` 21 段 / 0 passed；`--list` 编译出 1057 行 / 1055 个唯一名（唯一那对重名是两份后端契约文件各写一遍的 `user_stream_runner_reconnects_*`）；磁盘在册 `#[test]` 1070、差 13 颗（nats 10 + postgres 3）；Python 腿 Ran 64 / OK；`validate_core.py` rc 0；门禁 rc 0、**[PASS] 602 / [FAIL] 0**，`GATE_CHECK_FLOOR` 不动——两轮 `#[test]` 净增 0、判据净增 0，四颗落地全部折叠进既有用例与既有判据。行数棘轮按 `--snapshot` 逃生门重登记（涨的三格是 `qx-adapter/src/lib.rs` 681→728、`qx-adapter/src/ccxt.rs` 1139→1144、`qx-cli/src/strategy_host.rs` 846→856）。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.52 与 §9.53。尚未 `git add`、未推送。


### V13 R4 收口（2026-10-04）· 第 4 轮三扫 · API 与前后端贯通专项

- **落地的七颗**：CLI 位置参 `outbox-relay … 0` 不再"什么都不搬却退 0 报健康"（`parse_relay_limit` 挡 `1..=10000`，与配置孪生 `relay_batch_size` 同域）；缺 `Sec-WebSocket-Key` 的判定从 `serve_websocket` 提到 `admit_websocket`，握手之前就能回 400 `missing_websocket_key` 而不是掐断套接字；`schemas/strategy_api_v1.md` 那句"Python 侧不带 margin/position/leverage"按现读改口（Python 三格都带，只有 C ABI 头只带 `position_side`）；QIFI 信封在烟测链上第一次真的往返一遍（此前文档替它作保、代码零调用）；`JsonStateEnvelope` 从外销 `pub trait` 收为 `pub(crate)`；策略契约的 `uniqueItems` 折进既有用例拿到运行牙齿；两条 HTTP 读链补 16 MiB 总量界（`set_read_timeout` 只卡单个读窗口，慢速滴水上游可无限续期）。
- **登记的一颗**：API 事件读模型无淘汰（fam02）。本轮把"为什么不落"从裁定升级成技术理由——给 `Vec` 加长度上限写不出来：`project_event_log` 比较重叠前缀，尾部截断会把合法新事件判成「事件内容漂移」。需要的形状是 `retained_from_seq` 水位线，属策略决策。`ApiEventBus::publish` 那一半在 4096 处已淘汰。
- **按实测销案的两颗（假阳性，写明是为了下一轮别再捡）**：Binance `allOrders` 翻页 `loop` 有三条可点出口（页不满 / 游标未前进 / `checked_add` 溢出），人为页数上界会把"还没看完的资金事实"当"已核对"；Postgres 的 `Mutex<Client>` 全文件只有两处 `.lock()` 且都把毒锁映射成错误，锁内受 `statement_timeout=30s / lock_timeout=5s / idle_in_transaction=60s / connect_timeout` 约束。
- **变异取证 3 枪**：2 枪 KILLED、1 枪**无结论**。第一枪想把去重判据拆掉，写成 `insert(..) && false` 触发的是 `error[E0282]`（`BTreeSet` 推不出元素类型），整 crate 编译失败、`test result:` 没印出来——分类器读成"SURVIVED"是错的，那是断管道不是存活；重放成保留类型锚点后 KILLED。第二枪拆掉 HTTP 总量闸：KILLED。
- **本轮实测（10 腿电池，全部读数由日志派生）**：`cargo fmt --check` rc 0；`clippy --workspace --all-targets -D warnings` rc 0、诊断 0 行；`cargo test --workspace` rc 0、107 段 / 1056 passed / 0 failed / 1 ignored；`--all-targets` 87 段 / 1056 passed；`--doc` 21 段 / 0 passed（本仓无 doctest）；`--list` 编译出 1057 条；磁盘在册 `#[test]` 1070 颗、地板 1070，差 13 颗逐名可点；`python -m unittest discover -s python/tests` Ran 64（skipped=2）；门禁 rc 0、[PASS] 602 / [FAIL] 0，**本轮未新增判据**故 `GATE_CHECK_FLOOR` 不动。
- **两处口径随本轮改掉**：README 里「不带 `QX_PYTHON` 时那 2 条 Python 桥用例必红」按代码事实改口——解析是"`QX_PYTHON` 非空才用、否则回落 PATH `python`"，决定红不红的是回落点是不是可用解释器（本轮 `QX_PYTHON` 未设置、PATH 解析到 CPython 3.13.13，两条桥用例照跑照绿）；README 那句「`python/.venv` 本轮它在场」也已按事实改成"第 4 轮这遍它不在盘上，数字是用 PATH 上的解释器跑的"——这条风险第二次落地，正好说明 `build.bat`/`build.sh` 从 `[0/9]` 步探测解释器而不是信任 venv 是对的。
- **第三处口径纠正是本轮自己造的**（B 扫产物，记为 R4-D）：静态「门后」census 是**扫描器形状的读数**，四种形状本轮各现读一遍——三段齐 + 作用域按**花括号深度**收口 = 36 颗（sqlite 23 / nats 10 / postgres 3）；作用域只收到**列 0 的 `}`** = 44 颗（sqlite 31），**过计**（内联 `mod tests { … }` 里被门住那项的右括号落在列 4，门清不掉、漏到同块后面的用例上）；少认「**声明那个模块的行上的门**」那一段 = 30 / 38 颗，**少报**。判据取编译器真相：`36 − 23 = 13` 与「磁盘在册 − `--list` 编译出」同数，sqlite 那 23 颗逐名都在 `--list` 里、nats/postgres 那 13 颗一条都不在。本轮中途把 README 与终版计划 §1.1 写成过计那一格（44 / 31），已改回并把四种形状写进文档。
- **行数预算**：`crates/qx-cli/src/cli_args.rs` 513 行、`crates/qx-adapter/src/lib.rs` 697 行两颗越过 500 阈值，按该文件自订的逃生门 `--snapshot` 重登记，并核对预算名册全部 44 格与磁盘行数同数；`maturity/capabilities.yaml` 与 `crates/qx-storage/src/lib.rs` 的改动都是净零行数。尚未 `git add`、未推送。

### V13 R1 收口（2026-10-04）· 台账 #283 · 合流 `c07ad22` 回退面登记

- **登记（不重落）**：合流 `c07ad22` 按 `-s ours` 保留上游发布线，丢掉了本地 V11/V12/V13-R1 审计线的 135 份文件与 39 颗门禁判据。按用户裁定「以上游发布线为准（现状）」，本节把它们按 15 族逐名登记为**已回退，未排期**，口径不是「已修复」；名册写全在 V13 §9.48，取证目录不进仓库也不影响可读。
- **缺陷驱动落地的三颗**：控制面终态退场 + `RetirementSummary` 两条计数接上 `/metrics`；删掉零构造者的 `CommandStatus::Rejected`，终态词表收到 `is_final()` 一处；给 8 颗没有执行者的 `nats` 门后 `qx-storage` 用例在 CI 里补了一条非 ignore 的腿（本机实跑 8 passed / 1 ignored）。门禁新增 `command_status_vocabulary_single_source_check`，`GATE_CHECK_FLOOR` 515 → 602，`WORKSPACE_TEST_FLOOR` 866 → 1069。
- **`#152` 落地**：新增 `doc_citation_reachability_check`，活文档里每一处 `path:NN` 都要文件在盘上、行号不越界；`docs/archive/` 只降不升。
- **`#144` 关掉**：磁盘在册 `#[test]` 1069 颗 vs `--list` 编译 1056 颗，差 13 颗逐名点出（nats 10 + postgres 3）；README 里那条 `866 − 858 = 8` 的假恒等式删掉。同时纠正本轮自己的一处口径：静态 `#[cfg]` 扫描标出 36 颗「门后」，但 `qx-cli` 的 `default = ["sqlite"]` 让特性归一打开了 `qx-storage/sqlite`，那 23 颗其实照常执行。
- **本轮实测（7 腿电池，digests 稳定、无 host-locked 腿）**：`cargo fmt --check` 干净；`clippy --workspace --all-targets -D warnings` 诊断 0 行；`cargo test --workspace --all-targets` 87 段 / 1055 passed / 0 failed / 1 ignored；`cargo test --workspace` 107 段 / 同样 1055 passed；`python -m unittest discover -s python/tests` 64 条 OK；门禁 602 项全绿。
- **在册未修**：`docs/SECURITY.md` 已回退（全仓现读 35 处命中里 README 占 0 处，所以**没有**悬空指针——本条第一版写的「README 指向它的是悬空指针」是没现读就写下的假断言，已在 V13 §9.48 ② 改口）；新补的那条 NATS 腿只由 ci.yml 的文本存在性保证，门禁里没有判据核它（看守它的那颗在回退面里）；`cap_ledger_reading_check` 在回退面里，README 台账六格本轮按同一配方手工重钉（22 / 20 / 384 / 355 / 114 / 681）但没有牙齿。

## Unreleased — V13 R2 第三十五遍 #282：NATS 事件 consumer handler 的 stdin 写入接上 handler.timeout_ms 预算，handler 存活却不接收输入时从永久卡死变成可观测超时（阻塞点 / 预算 / opt-in 面）（2026-10-03）

第三十五遍沿「无预算等待」这一族继续查（#281/#263/#220 同源），扫到 #281 的同构残留：`crates/qx-cli/src/event_pipeline.rs` 的 `invoke_event_consumer_handler`（`#[cfg(feature = "nats")]`）先把 `stdin.write_all(payload)` 直接跑在主线程，之后才用 `handler.timeout_ms` 守后面的 `try_wait` 轮询——timeout_ms 管不到写侧。一旦用户配置的外部 consumer handler 进程仍存活却不再从 stdin 取字节，超出 OS 匿名管道缓冲（~64KB）的那段写入永久阻塞在 write_all 上，本进程就此卡死、连关停都读不到。这不是假设：跨进程 Outbox 事件里 AccountPositionSnapshot/AccountBalanceSnapshot 把整段 Vec 内联进单条 payload，足以越过 64KB。修法与 #281 一致：把 write_all 交给 spawned 线程并先 `drop(stdin)` 落 EOF，主路径按 `write_done.recv_timeout(Duration::from_millis(handler.timeout_ms))` 等它回传结果，超时即 kill handler（关管道读端、放行写线程）并回「handler 存活但不接收输入」诊断，把无限阻塞变成可观测超时。

回归用例 `crates/qx-cli/src/tests/event_consumer_write_budget.rs::live_but_non_draining_event_consumer_write_is_bounded_not_hanging`（随 nats 特性门控）直接跟踪一颗 `PING.EXE`（继承 stdin 读端却一字节不取，`kill()` 即关闭读端放行写线程），喂一份 200_000 字节 payload 的 `OutboxEvent` 使写入必然超出管道缓冲，断言调用在预算内以超时通道失败——本轮实测耗时 2.02s（timeout_ms=2000）而非挂死。为此给私有的 `EventConsumerHandler` 加了 `#[cfg(all(test, feature = "nats"))] for_test` 构造入口。新建门禁 `event_consumer_pipe_write_budget_check` 三条：写入被交给 spawned 线程且被 `write_done.recv_timeout` 有界等待、写侧预算取自 `handler.timeout_ms`、超时诊断在位。架构自检本轮实测 555 项全绿（logs/s838_pass35_gate_green.txt），变异反向验证把 thread::spawn 摘掉、把 handler.timeout_ms 换成 3_600_000、改掉超时措辞后恰只各咬对应的一条（baseline 3/3、三颗变异分别 2/1，真树按镜像还原逐字节一致）。event_pipeline.rs 行数预算 775→788（本轮多出的 for_test impl）。

与 #281 的边界不同：这条落在 `#[cfg(feature = "nats")]` 的 opt-in 后端，不在默认发布 exe、也不在 wheel，所以「按终树重打独立 exe 与 wheel」不是本条的收口步骤——默认 `cargo build --release` 的 qx-cli 与 wheel 不因本条改变；本轮只证明 `cargo check`/`cargo clippy -p qx-cli --features nats --all-targets -- -D warnings` 干净、nats 门控用例实跑 bounded。尚未 `git add`、未推送。capabilities.yaml 记入 limitation `event_consumer_pipe_write_bounded_by_killing_handler`：写侧超时只能靠杀掉 handler 子进程关闭管道读端、放行阻塞的写线程，故 handler 一旦 wedge 需重启而非复用；单事件最坏墙钟约 2×timeout_ms（写侧与退出轮询各等一次）；本机无 NATS/handler 沙盒，有界性只由 nats 编译+回归用例+门禁自证，`sandbox_tested` 保持 false。


## Unreleased — V13 R2 第三十四遍 #281：默认策略管道传输的 stdin 写入接上 timeout_ms 预算，worker 存活却不接收输入时从永久卡死变成可观测超时（阻塞点 / 预算 / 发布物）（2026-10-03）

第三十四遍查「无预算等待」这一族（#263/#220 同源）。`crates/qx-cli/src/strategy_host.rs` 的 `PythonStrategyClient::request` 在默认 `Jsonl`/`FramedJson` 传输上先直接 `stdin.write_all`+flush、之后才 `responses.recv_timeout(timeout_ms)`——timeout_ms 只守读侧。一旦 Python worker 进程仍存活却不再从 stdin 取字节，超出 OS 匿名管道缓冲（~64KB）的那段写入会永久阻塞在 write_all 上，主进程就此卡死、连关停令牌都读不到，而这条是编译进发布 exe 的默认路径（`crates/qx-runtime/src/runtime_config/strategy_schema.rs` 的 transport 默认 `Jsonl`）。修法是让写入与读取同受 timeout_ms 约束：把 write_all+flush 交给 spawned 线程，主路径按 `write_done.recv_timeout(self.timeout_ms)` 等它回传结果，成功即回收 stdin 句柄、超时即 kill worker 并回「worker 存活但不接收输入」诊断，把无限阻塞变成可观测超时。

回归用例 `crates/qx-cli/src/tests/worker_pipe_failure_diagnostics.rs::live_but_non_draining_worker_write_is_bounded_not_hanging` 直接跟踪一颗 `PING.EXE`（继承 stdin 读端却一字节不取，`kill()` 即关闭读端放行写线程），喂 40_000 行 Bar 使写入必然超出管道缓冲，断言 `request()` 在预算内失败——本轮实测耗时 2.06s（timeout_ms=2000）而非挂死。新建门禁 `strategy_pipe_write_budget_check` 三条：写入被交给 spawned 线程且被 `write_done.recv_timeout` 有界等待、写侧预算取自 `self.timeout_ms`、超时诊断在位且两个成功分支各回收一次句柄。架构自检本轮实测 552 项全绿（logs/s835_pass34_gate_green.txt），变异反向验证把 thread::spawn 摘掉、把 timeout_ms 换成 3600_000、改掉超时措辞后恰只咬这三条（logs/s836_pass34_gate_mutation.txt）；strategy_host.rs 行数预算 824→846。

与 #263 的诚实边界不同：这条改动落在默认编译进发布 exe 的 qx-cli，不是 opt-in feature，发布产物会随重建而变，所以「按终树重打发布 exe」是本条的收口步骤——本轮已按终树整跑九步 `build.bat` 全绿（logs/s837_pass34_ninestep_green.txt：门禁 552、全 workspace 测试含两条 Python e2e 全 ok、clippy 干净、runtime-check 通过，尾「全部完成」），且 `find crates -name '*.rs' -newer target/release/qx-cli.exe` 得 0，独立 exe 确已携带本条 #281；wheel 不受本条影响（#281 只在 qx-cli 的 `strategy_host.rs`，既不碰 wheel 原生扩展的源 crate qx-python、也不碰纯 Python 发布包）。尚未 `git add`、未推送。capabilities.yaml 记入 limitation `strategy_pipe_write_bounded_by_killing_worker`：写侧超时只能靠杀掉子进程来关闭管道读端、放行阻塞的写线程，故 worker 一旦 wedge 需重启而非复用；单请求最坏墙钟约 2×timeout_ms（写侧与读侧各等一次）；本机无 Python worker 沙盒，有界性只由编译+回归用例+门禁自证，`sandbox_tested` 保持 false。


## Unreleased — V13 R2 第三十三遍 #263：origin/main 竞争架构以 -s ours 合流收口，PostgreSQL 建池握手补上 connect_timeout（合流 / 阻塞点 / 预算）（2026-10-03）

第三十三遍收两件事。其一是分叉合流：origin/main 上另有一套互斥的 V11/V12 qx-cli 拆法，与当轮已实测到发布条件的 V13 R2 发布线不可共存，按「保留当轮发布线」裁定，用 `git merge -s ours` 把 origin/main（f5b9a09）记为祖先而工作树逐字节不变（合流提交 4e2fb6b，树 ≡ 3b14cac，已推送，HEAD..origin/main 归零），此后不再反复撞同一处分叉。其二是 #263 阻塞点：`PostgresStorage::connect_with_pool_size` 串行建池（1..=128）时每条连接走 `Client::connect(dsn, ..)`，握手（TCP 连接 + TLS 协商 + 认证）无时间预算，一台接受了 TCP 却迟迟不完成握手的库、或一个挂起的 DNS，会让同步的启动路径按存储后端逐个无限阻塞——会话级 `statement_timeout` 等三条 SET 只在连接成功之后才生效，管不到这一段。修法是让 DSN 解析与 `Client::connect` 完全同路（后者内部就是 `dsn.parse()?.connect(..)`）：改为 `postgres::Config` 一次解析、`connect_timeout(CONNECT_TIMEOUT)`（正的 5s 预算）后 `config.connect(connector)`，握手从无限收成有界。新建门禁判据 `postgres_connect_budget_check` 两条：`.connect_timeout(` 恰 1 处、无界 `Client::connect(` 残留恰 0，且 `CONNECT_TIMEOUT` 常量必须是正的 `Duration::from_secs(N)`（改成 0 或删掉都判红）。架构自检本轮实测 549 项全绿（logs/s832_pass33_gate_reconfirm.txt），本轮新增 2 条判据。

发布产物逐字节不受影响：`postgres` 是 opt-in feature，默认 `qx-cli`（features = ["sqlite"]）根本不把这段编译进 exe/wheel，本轮无需重打包。诚实边界记入 capabilities.yaml：`sandbox_tested` 保持 false——本机无 Docker/Postgres，connect_timeout 的有界性只由编译与门禁自证，真库下的握手（含慢 ack、DNS 挂起）仍无实测记录。

## Unreleased — V13 R2 第三十二遍 ③ #279：发布条件复核 —— 当轮重打的 wheel 离线装回后带的是修好的 A 股提示，未过时的独立 exe 就地兑现 A 股 / BTC 快速回测端到端（发布 / 易用性）（2026-10-02）

第三十二遍 ③ 不改代码，只把 ①（#276 可选 extras）、②（#278 A 股断链 + 编译门禁）确认落进**当轮重打的发布物**，并在最终 exe 上跑通核心链路。

### Added（发布物携带修复的端到端实测）

- **新 wheel 离线装进干净环境后携带 #278 的写法**：`uv venv` 起一颗不装任何可选依赖的解释器，`uv pip install --offline` 一次装成 `qianxing-bridge==0.1.0`（印证 #276 降 extras 后无索引也能装）；四个发布包 `import` 全 OK；在缺 AkShare 的环境跑发布包自带的 `python -m qianxing_ashare fetch --provider akshare`，抛出的 `AshareProviderError` 逐字节是 `pip install 'qianxing-bridge[a-share-akshare]'`（`HAS_NEW_FORM=True`、`HAS_OLD_EDITABLE=False`）。`logs/s815`—`logs/s818`。
- **A 股 / BTC 快速回测在最终 exe 上端到端跑通，产物不落仓库**：`qx-cli quickstart <临时>` 五步全过（init→doctor→backtest `fills=1 result_hash=26fdd6b5…`→report 读回→status，`orders_sent=false`，RC=0，`logs/s819`）；A 股 `qx-cli backtest <ashare runtime> <barframe> <spot spec>` 出 `fills=1 result_hash=6be034a4…`（RC=0，`logs/s820`）。14:27 建好的 exe 未过时：`find crates -name '*.rs' -newer` 得 0 颗、`deploy/*.json` 全部早于 exe（本轮只有 `deploy/README.md` 变过，而它不是 `build.rs` embed 的输入）。
- **#194 不变量在重打后的 wheel 上重取**：wheel 内 `.pyd` ≡ `target/release/_qianxing_native.pyd` ≡ `_qianxing_native.dll`，md5 全等 `b972756dfec29045116d1f846d326671`，17 条目、218,719 B（`logs/s821`）。
- **发布条件判定**：第三十二遍 ①②③ 闭合；门禁 547 项全绿（本轮未改码，沿用 `logs/s813`）；两份发布物按终树身份可复核。**未 git add、未推送**。

## Unreleased — V13 R2 第三十二遍 ② #278：A 股缺件提示把 wheel 用户指向执行不了的 `-e` 命令；本格修复一度把包改成 SyntaxError、被 Python 测试而非门禁抓到（核心链路三查 / 断链）（2026-10-02）

三查在 Python 分发面抓到一条断链，又逼出了门禁自身的一个盲区：`tools/check_architecture.py` 把
`python/qianxing_ashare/__init__.py` 当**文本** grep（缺件提示 / 指针判据），从不确认它还能编译。

### Fixed（缺件提示改成两类受众都执行得了的写法）

- `python/qianxing_ashare/__init__.py`：AkShare/Baostock/easy_tdx 三处 `AshareProviderError` 的修法从源码专用的
  `pip install -e '.[a-share-*]'`（wheel 用户手里没有本地工程可 `-e`）收成与 `qianxing_ccxt` 同族的受众无关写法
  `pip install 'qianxing-bridge[a-share-*]'`。同形指路在 `deploy/README.md`「A 股数据源」段与
  `docs/A股数据源接入与快速选股回测方案-V1.md` 安装块各一处，一并补上 wheel 派写法、把 editable 降为「从源码根目录」的等价项。
- **修这条提示的过程中一度把整份 `qianxing_ashare/__init__.py` 写成了 `SyntaxError`**（新的 `"` 撞进外层同为 `"` 的字符串）。
  改后的门禁仍报全绿——因为 pointer 判据只读文本；把它暴露出来的是 `python/tests` 收集期的 `import qianxing_ashare`。已改回单引号包裹并 `py_compile` 通过。

### Added（门禁判据 546→547 + 反向验证）

- `tools/check_architecture.py wheel_optional_dependency_check` 共加三条：① A 股缺件提示不得再出现 `pip install -e '.[`；
  ② 提示点名的 `qianxing-bridge[a-share-*]` 必须在 `pyproject` extras 里有定义（#157 一族，从 ccxt 扩到 ashare）；
  ③ **用内置 `compile()` 逐个语法核对四个发布包的每个 `.py`**——任一 `SyntaxError` 即点名「文件:行: 原因」，只检语法、不落 `.pyc`、无副作用，
  把「改一句用户提示把整个包改崩」从「跑到测试才红」前移到「门禁当场红」。终树门禁 547 项全绿、0 `[FAIL]`（`logs/s809_pass34_gate_278_compilecheck.txt`；
  文档批后复跑同计数 `logs/s810_pass34_gate_278_afterdocs.txt`）。
- 反向验证（镜像树、真文件全程未写入，`logs/s811_pass34_278_compilecheck_mutation.txt`）：基线全绿 → 往镜像的 ashare 注入本遍真犯过的 `SyntaxError` →
  **只有新增编译判据变红**并点名 `python\qianxing_ashare\__init__.py:1922: invalid syntax` → 还原后复绿。
- 消息改完后 `python/tests` 复跑 64 项 `OK (skipped=1)`；缺 `ccxt`/`akshare`/`baostock` 的解释器里四个发布包 `import` 全 OK（`logs/s812_pass34_278_python_evidence.txt`），
  #253/§9.42 的惰性加载未在本轮回归成 import 期硬失败。


## Unreleased — V13 R2 第三十二遍 ① #276：`pip install <wheel>` 离线装不成——`ccxt`/`tzdata` 从强制依赖降为可选 extras（分发面 / 易用性）（2026-10-02）

发布物 `python/pyproject.toml` 的 `[project].dependencies` 里钉着 `ccxt>=4.4.0` 与 `tzdata; sys_platform=='win32'` 两颗**强制**第三方依赖，
于是 `pip install dist/qianxing_bridge-*.whl` 在没有索引（离线 / `--no-index` / 内网无镜像）时会在**解析依赖**这一步直接失败，用户连包都装不进来——
可这两颗运行时都是**按需**才碰：`ccxt` 只在真跑 CCXT worker 时 import，`Asia/Shanghai` 只在真取 A 股时区时解析（#253 已把后者从 import 顶层挪到用时）。

### Fixed（基础安装零强制依赖，能力改走 extras）

- `python/pyproject.toml`：`dependencies` 收成 `[]`，两颗依赖移进 `[project.optional-dependencies]` 的七档 extras——
  `ccxt` / `ccxt-pro` / `tz` / `a-share-akshare` / `a-share-baostock` / `a-share-easy-tdx` / `a-share`（含 Windows 的档各带 `tzdata>=2024.1; sys_platform=='win32'`）。
  缺能力时调用点抛的可执行提示（`_load_ccxt()` 的 `qianxing-bridge[ccxt]`、`_shanghai()` 的 `pip install tzdata`）与新 extras 名一一对上。

### Added（判据 + 用例 + 离线实测）

- `tools/check_architecture.py wheel_optional_dependency_check`（3 条）：`dependencies` 里不得出现 `ccxt`/`tzdata`；`optional-dependencies` 必须齐七档 extras；
  再从 `python/qianxing_ccxt/__init__.py` 正则抓所有 `qianxing-bridge[<name>]` 引用，非空且逐个被 extras 覆盖——把「提示语 extras 名」与「pyproject 声明 extras」锁成同源。
  终树门禁 `logs/s805_pass34_gate_after_readme.txt` **544 项全绿**（541→544，`GATE_CHECK_FLOOR = 515` 未动，判据只增未减）。
- `python/tests/test_packaging_contract.py` 三颗：基线安装零强制第三方依赖、七档 extras 齐、缺 ccxt 时 `_load_ccxt()` 降级为 `UNSUPPORTED` + `qianxing-bridge[ccxt]` 指针。
  Python 侧 `python -m unittest discover -s python/tests` **64 项 OK（skip 1）**；整树 `cargo test --workspace` `logs/s803` **1026 passed / 0 failed**（`QX_PYTHON` 已设）。
- 离线安装实测 `logs/s804_pass34_offline_install.txt`：干净 venv 里 `pip install --no-index <wheel>`（**不加** `--no-deps`）退出码 0，四包导入 + 原生扩展加载，
  缺 ccxt / 缺 tzdata 各自在调用点抛带 extras 名的可执行提示；`#194` 三处 md5 等值同时守住（`b972756dfec29045116d1f846d326671`，353280 字节）。
  反向验证（镜像树、真树不落写）：基线 3 格全绿；ccxt 塞回强制 → 红 [0]；删 `ccxt` extras → 红 [1,2]；改 extras 引用名 → 红 [2]。
- `cargo fmt --all --check` `logs/s800`、两条 clippy（默认 `logs/s801` + `--features nats` `logs/s802`）退 0。本轮无 `.rs` 改动。

### Docs

- `README.md` 装 wheel 段：`--offline --no-deps` + 单独 `pip install tzdata` 这条 footgun 配方改成「基础安装离线可成、能力按 extras 选装」，
  并把 `导入时即解析 Asia/Shanghai`（#253 已作废）改成「用时才解析、缺了给可执行提示」。
- `deploy/README.md`：「安装 Python wheel 时会安装公共 `ccxt`」按 extras 口径改正为「跑 CCXT 链路需显式装 `qianxing-bridge[ccxt]`」。
- 在册 #212（联网装齐 wheel 依赖本机未测）由反向那一格补实：基础安装不联网即成，能力全走 extras；正向「联网装 ccxt 跑 worker」仍属外部索引/交易所环境，`sandbox_tested` 继续 false。

## Unreleased — V13 R2 第三十一遍 ② #275：`paper-check`/`paper-e2e` 同日重跑零新增仍打验收 ✓（核心功能维度）（2026-10-02）

取证方式是在同一个新临时目录里连敲两遍 `paper-check`，而不是在仓库根跑一遍。`logs/s783_pass32_paper_check_doublerun_after_fix.txt`
头注记的改前现场：第二遍 `[调度] skipped=1`、`[策略] processed=0`、`[Paper · Execution] processed=0`——端到端一手没跑，
末行却照旧 `orders=1 ledger_entries=4 ✓`。旧末行读的是账户事实流的**累计量**：上一轮的成交还在这本 EventLog 里，`orders()` 非空、
`orders()[0]` 的 Executed 审计也还在，空账本 fail-close、命令队列清空、Executed 审计三道闸门全过，于是把一次空转报成一次验收通过。
这正是 #274 §9.39 遗留点名的「同日重跑零新增仍打 ✓」。

### Fixed（末行按「本轮新增」给结论，空转仍退 0）

- `crates/qx-cli/src/venue_runtime/paper_worker.rs`：`run_paper_pipeline_once` 进场打开账户事实流时先快照 `orders_before`/`ledger_before`
  作本轮基线，末行取 `new_orders = orders_now - orders_before`：只有 `new_orders > 0` 才打带 ✓ 的成功行并并列 `(+N 本轮新增)`，
  否则如实说「本轮零新增：当日调度已跳过、复用上一轮既有事实，未端到端重跑」。两遍都**退 0**——既有契约
  `paper_e2e_entrypoint_runs_scheduler_strategy_execution_and_ledger` 对两次 `run_paper_pipeline_once(...).unwrap()` 都解包，
  幂等空转必须留 Ok，所以这一格收的是播报诚实性而非退出码，不把空转改成 fail。

### Added（判据 + 用例 + 变异）

- `tools/check_architecture.py paper_check_delta_honesty_check`：不锁 rustfmt 换行、不数 `✓` 字符（这条链的正文注释里就有一个 ✓，数符号会误咬），
  只锁取数口径四格——进场基线、增量子、增量分支、零新增分支各一条字面量在位；旧无条件累计成功句 `ledger_entries={} ✓` 不得复活；
  成功 ✓ 必须排在 `if new_orders > 0 {` 那一支、零新增那句排在 `else` 之后。终树门禁 541 项全绿（538→541，`GATE_CHECK_FLOOR = 515` 未动，判据只增未减）。
- `crates/qx-cli/tests/default_example_paths.rs` 新增 `paper_check_same_day_rerun_does_not_claim_a_fresh_success`：同目录连跑两遍，
  第一遍断 `orders=1 (+1 本轮新增)` 且带 ✓，第二遍断 `本轮零新增` 且 `+0 本轮新增` 且**不得**再出现 ✓，两遍都退 0。
- 反向验证 `logs/s789_pass32_275_gate_mutation.txt`（镜像树、单次跑、真文件全程未写入）：基线三格全绿；摘掉增量守卫 → 红 [A, C]；
  把累计 ✓ 塞回 → 只红 [B]；摘掉零新增那句 → 红 [A, C]。整跑 `logs/s792_pass32_275_workspace_tests_py.txt` 1026 passed / 0 failed
  （`QX_PYTHON` 已设）；`cargo fmt --all --check`、两条 clippy（含 `--features nats`）退 0（`logs/s790_pass32_275_clippy.txt`）。


## Unreleased — V13 R2 第三十一遍 ② #274：查找面只接了一半只读入口，`backtest`/`paper-submit-order`/`reconcile` 在无关目录里必断（核心功能维度）（2026-10-02）

取证方式是换一个无关的当前目录去敲命令，而不是在仓库根跑。`logs/s774_pass32_lookup_unmounted_before.txt` 抓到修复前现场：
已经挂了 `parse_deploy_path` 的入口（`paper-check` 等）能把 `deploy/qianxing.runtime.paper-strategy.example.json` 从别处搬来并跑出成交，
而 `backtest <runtime>`、`paper-submit-order <runtime> <command>`、`reconcile <runtime>` 这三条只读入口的位置参数**没挂解析器**——显式给的同一条
`deploy/…` 路径被当字面量拼在当前目录上，退 2 回 `系统找不到指定的路径 (os error 3)`，只有报错补话指出「别处真有这一份」。这正是 #273 §9.38 遗留里
点名的「查找面只接了一半只读入口」。

### Fixed（把只读入口的位置参数接满查找面）

- `crates/qx-cli/src/cli_args.rs`：给六个「读取示例输入」的必填/可选位置参数补上 `#[arg(value_parser = parse_deploy_path)]` —— `backtest` 的
  `runtime`/`frame`/`spec`、`paper-submit-order` 的 `path`/`command_path`、`reconcile` 的 `path`。选 clap 挂载而不是让读漏斗自动搬路径，是为了守住
  读失败侧**绝不落盘**、也不改文档里逐字抄写的默认路径措辞；写目标（`--output`、锁产物）仍不接。`binance-submit-order` 是带实盘凭据的必填精确路径，
  与 `scheduler-worker` 同侧，刻意留在没挂名单里，`deploy_lookup.rs` 的模块注释记下这条不对称。
- 改后现场 `logs/s775_pass32_lookup_mounted_after.txt`：同三条命令在无关目录里各说一句 `[查找 · Lookup] … 改用 …` 后跑到成交/走到撮合与对账通道，
  不再出现 `os error 3`；`crates/qx-cli/tests/default_example_paths.rs` 新增 `the_read_input_positionals_relocate_from_an_unrelated_directory` 钉住
  这份契约（该目标 7 条用例全过，`logs/s778_pass32_workspace_tests.txt`）。

### Added（判据 + 变异）

- `tools/check_architecture.py example_read_funnel_check` 加第 8 颗：按 `LOOKUP_MOUNTED_READ_ARGS` 逐变体逐字段核对读取位置参数是否挂着解析器，
  变体或字段找不到就判「判据失去对象」，缺挂载就点名 `{变体}.{字段}`。终树门禁 538 项全绿（`GATE_CHECK_FLOOR = 515` 未动）。
- 反向验证 `logs/s779_pass32_lookup_mount_mutation.txt`：逐颗摘掉这六处解析器，六颗全部判红且各自点对被摘掉的字段；字节还原（16623 bytes / CRLF 0）后基线与
  还原树同绿。整跑 `logs/s778_pass32_workspace_tests.txt` 1025 passed / 0 failed；`cargo fmt --all --check`、两条 clippy（含 `--features nats`）退 0
  （`logs/s776_pass32_clippy_default.txt`、`logs/s777_pass32_clippy_nats.txt`）。


## Unreleased — V13 R2 第三十一遍 ② #273：Paper 提交缺行情时命令停在 Accepted、队列不确认，重投被幂等闸门永久挡住（BTC paper 交易链，核心功能维度）（2026-10-02）

取证方式是按**待敲命令**跑 BTC paper 交易链，而不是读代码。`logs/s769_pass32_btc_paper_submit.txt` 抓到修复前现场：
`paper-submit-order` 在缺行情时以 `FAIL_CLOSED: … 等待 MarketData worker 注入后重试` 退出（退 2），但那条命令停在
`Accepted`、`control-queue/commands/2001.json` 与 `2001.lease.json` 都还留着，而照文案重投同一条命令只会撞
`DuplicateRequest` —— 文案指的路走不通。根因是 `plane.submit_as(…)`（Accepted 已落盘）与
`plane.execute(…)`（终态回写）之间有 `?` 直接退出函数：同一个缺陷类在 Binance 入口也有一处。

### Fixed（一条命令在两个消费者那里拿到同一个裁决）

- `crates/qx-cli/src/venue_runtime/paper_submit.rs`：Accepted 之后到终态回写之间不再提前退出函数。所有可预期失败
  都收敛成 `action` 的值，由控制面当场写 `Failed`，随后无条件 `ack_command_at`；缺行情的拒绝文案改成
  `… 缺少 <instrument> 的最新行情事实；本命令已记为终态失败，修好之后换新的 command_id 与 request_id 重新提交`。
- `paper_submit_match_attempt(…)` 抽成一次性入口与常驻 `paper-worker` 循环共用的唯一撮合尝试（全仓两个消费者各一处
  调用）：改前一次性入口判失败、常驻循环则被同一条命令整轮打停。
- `terminal_submit_rejection(…)` 是「这一手已记为终态失败」指路口径的唯一出口，Paper 与 Binance 两条 venue 入口都从
  这一处引用，散文里不许手抄第二份。`binance_submit_action` 里三处「还没写入任何事实」的失败也改走它。

### Added（判据 + 用例 + 一条格式盲区）

- `tools/check_architecture.py submit_terminal_state_check()` 10 项：提交顺序、Accepted 之后的 `?` 退出必须逐项等于
  在册的队列管线调用（多一处、或修好一处不撤登记都判红）、回写之前不许 `return`、Paper 的 ack 排在回写之后且
  中间不夹分支、指路口径与撮合尝试各只有一个定义点、三条用例挂载在位。
- `crates/qx-cli/src/tests/paper_submit_terminal_state.rs` 3 条用例跑真的入口与真的 worker（`tests/mod.rs` 挂载）。
- 反向验证 `logs/s772_pass32_submit_terminal_mutation.txt`：文本级 9 颗变异 8 红 1 绿，绿的那颗是**格式负对照**
  （把终态回写按 rustfmt 拆成多行并补尾逗号，判据必须仍然全绿）；把缺陷形状回填生产代码后，门禁与用例同时红
  （`running 3 tests` + `Accepted + 终态，一条都不能少` left=1 right=2），字节还原后两侧同时绿。
- 顺带关掉一格常驻盲区：本轮 `cargo fmt --all` 给多行实参补尾逗号，`lease_clock_domain_check` 的 ack 秒域判据按字面
  比对立刻读出 0 处。门禁新增 `_collapsed_code`/`_squeezed` 取数口径（压行、去标点空白、并尾逗号），判据源码里的
  needle 仍按人写的字面量保存；M8（ack 第 4 个实参退回毫秒墙钟）证明新口径仍咬得住。

### Known limitations（登记而非顺手改掉）

- `run_paper_submit_order` 在 Accepted 与终态回写之间仍有两处 `?`：`enqueue_command` 与 `claim_command`。这两格失败时
  租约还不属于本进程，把 `Failed` 写进去会覆盖常驻 worker 对同一条命令已做出的裁决，因此按缺陷登记、由门禁第 2 项
  钉成允许名单（`maturity/capabilities.yaml` paper_execution）。

### Verified（当轮数字只抄当轮日志）

- CLI 链 `logs/s770_pass32_paper_submit_terminal.txt`：首投 `accepted=Accepted final=Failed`、审计两格齐
  （`AUDIT_LEN=2`）、`QUEUE_FILES=0`、`paper-worker --once` 退 0 且 `processed=0`；换新号重投拿到同一个终态裁决，
  同 `request_id` 仍按幂等挡为 `DuplicateRequest`；两轮后 `AUDIT_LEN=4`（`Accepted 2 / Failed 2`）、`QUEUE_FILES=0`；
  注入行情的对照腿 `[Paper · E2E] … orders=1 ledger_entries=4 ✓`。
- 整跑 `logs/s771_pass32_full_test_counts.txt`：**1024 passed / 0 failed**（106 段）——`qx-cli` 单元 311、
  `qx-cli` 集成 63（15 段）、其余 crate 650（90 段），`cargo test --offline --workspace`（默认特性，`QX_PYTHON` 已设）。
- `cargo fmt --all --check` 退 0；`cargo clippy --workspace --all-targets -- -D warnings` 与 `--features nats` 两侧退 0
  （本轮修掉三处：`binance_submit.rs` 的 `match`→`?`、`format!` 套在 `eprintln!` 里、新用例对 `Copy` 状态做 `clone`）。
- 门禁 `logs/s773_pass32_gate_after_273_docs.txt`。
同一个默认路径有两套解析机制，装好的 exe 一份模板都读不到——查找面单源 + 内置模板层（#264–#267/#271/#272，易用性维度）（2026-10-02）

这一遍换维度：前三遍（§9.33–§9.35）按"补功能"走，这一遍按**新手第一次接触这个项目时会撞上的东西**走。取证方式是
把 `docs/竞品对比与易用性改进优化计划-V1.md` §4.2 的 U 序列当成待敲命令逐条敲，而不只是读。现场
`logs/s730_pass31_newcomer_paths.txt`（无参数 / `init` / `doctor` / 裸 `backtest` / `status` / `report`）、
`logs/s731_pass31_profile_nextstep.txt`（7 份 profile 的引导行逐条照抄执行）、
`logs/s732_pass31_installed_layout.txt`（工作目录里只有 exe 的布局）、`logs/s733_pass31_newcomer_tails.txt`
（`quickstart` 的收尾行与三条下一步）。撞到的是四格，其中三格本轮改掉：

- **F1/F2（改前事实，`logs/s734_pass31_default_path_probe.txt`）**：`deploy/` 里示例配置的默认路径有**两套并存**的
  解析机制。`doctor`/`status`/`report`/`config *` 经 `repository_deploy_path` 能回到仓库里的 `deploy/`，而
  `runtime-check`/`live-check`/`paper-check` 只是把 `"deploy/…"` 字面量拼在当前目录上。同一棵树、同一个无关启动
  目录，前一组退 0、后一组退 2，回 `读取运行时配置失败 deploy/…: 系统找不到指定的路径`。使用者据此以为仓库坏了，
  真因是启动目录换了一个位置。
- **F4**：报错只说"读不到"，不说"那一份在别处真的存在"，也不说找过哪些位置。
- **F3（本轮判为不改）**：`runtime-check` 的检查清单口径（把一批失败攒成一张表）与"这一份示例在别处"回答的不是
  同一个问题，硬接漏斗会把它的清单拆散。

### Added（查找面只有一条链，模板跟着 exe 走）

- `crates/qx-cli/src/deploy_lookup.rs`（277 行，新增；`main.rs:112` 挂载、`:149` 单点 re-export）：候选根按
  `QX_DEPLOY_DIR` → exe 同级 `deploy/` → 再往外一层 → 构建期源码树 → 当前目录排，先命中先用；第五层是二进制里的
  **内置模板清单**——需要时把整份清单落进当前用户临时目录、按内容签名分桶。落整份而不是只落被点名的那一份，因为
  `fast-backtest` 的 manifest 里 jobs 按**同级文件名**引用 runtime/bars/spec，只落一份会让这条链在下一格读取上断掉。
  内置层只接读取路径（`locate_deploy_file`），**报错文案那条链不写盘**（`pick_deploy_file` 只认真实目录）。
- `crates/qx-cli/build.rs`（133 行）：构建期把 `deploy/` 顶层 52 份 JSON 快照成 `DEPLOY_TEMPLATES` 与 FNV-1a
  `DEPLOY_EMBED_SIGNATURE`；`init` 与只读入口因此在"源码树已删、exe 单独存在"的机器上仍有模板可读。
- 唯一读取口 `read_example_json(path, label)`：把「读不到」与「那一份在别处」交在同一句。生产侧现在 8 个文件、
  18 格读取经它（`runtime_wiring` 1、`backtests/mod` 2、`artifacts` 2、`fast_backtest` 1、`single_strategy` 1、
  `strategy_backtest` 2、`ashare_binding` 3、`dataset_commands` 6）。改前只有 `runtime-check` 那条链接了补话，
  同一棵树里 `fast-backtest` 只留一行 os error 3（`logs/s750_pass31_standalone_fast_backtest.txt`）。
- `crates/qx-cli/src/tests/deploy_lookup.rs`（368 行 / 10 条）与 `crates/qx-cli/tests/default_example_paths.rs`
  （214 行 / 6 条真起 binary 的子进程用例）：候选根顺序、先命中先用、空 `QX_DEPLOY_DIR` 不占位、内置层落整份、
  报错面不落盘、显式路径的补话点名、查找换位置必须在 stderr 说出来。

### Changed

- `cli_args.rs`（498 行，未越 500 门槛）里 7 处以 `deploy/<文件名>` 作默认值的路径参数全部挂 `value_parser =
  parse_deploy_path`；`fast-backtest` 的 manifest 由读取点自己调 `relocate_deploy_path`。挂在哪一侧是按职责分的：
  命令表只给解析器，读取点自己重定位，两者同一条规则。
- `strategy_backtest.rs` 210 → 202 行：策略 `dataset_bundle_path` 那两处手写 `fs::read_to_string` + 自拼文案
  收进漏斗。同一份 bundle 被 `verify_dataset_bundle_binding` 再读一次这件事**登记为观察、本轮不动**（改它要动
  校验顺序，不属于这一遍的口径）。
- 内置桶是**按内容签名命名的共享目录**，并行用例会互相把对方刚删的那份补回来 → 三条要动桶的用例共用一把
  `DEPLOY_BUCKET_LOCK`，且断言前先把要判的那几份删掉（见 Verification 的假绿一条）。

### Verification（本轮实测）

- 改前→改后同一入口同一棵树：`logs/s734_pass31_default_path_probe.txt`（后一组退 2）→
  `logs/s735_pass31_default_path_after_fix.txt`（逐条退 0）。
- 安装面（exe 与仓库分离）：`logs/s744_pass31_standalone_exe_probe.txt` 在第二棵源码树上把 `deploy/` 整目录删掉
  （127 份）后 `version`/`init`/`doctor`/`status`/裸 `backtest` 逐条退 0，默认输入落点是
  `%TEMP%\qianxing-deploy-952fd03575e79c90`，`result_hash=1189853a7c12447d` 与仓库树同输入逐字符相同；
  `logs/s752_pass31_standalone_fast_backtest.txt` 同一棵无 `deploy/` 的树上 A 股 `jobs=1 completed=1`、
  BTC `jobs=2 completed=2`。
- 门禁 527 项全绿：`logs/s745_pass31_gate_after_lookup_registration.txt`（518 项，查找面登记后）→
  `logs/s759_pass31_gate_before_docs.txt`（**527 项**）。`example_read_funnel_check()` 从 5 颗扩到 7 颗，新增
  两条是"命令表里以示例形状作默认值的路径参数必须全挂解析器"和它自己的**防空转判据**（判据没有对象就报，
  不静默给绿）；`FUNNEL_CONSUMERS` 按路径登记 8 个文件的最小读取份数。`GATE_CHECK_FLOOR = 515` 一行未动。
- 变异反向验证三份：`logs/s743_pass31_builtin_layer_mutation.txt`（内置层）、
  `logs/s753_pass31_funnel_mutation.txt` + `logs/s755_pass31_funnel_mutation.txt`（六颗破坏按声明层级咬住）。
  **s753 里 M-c 当场假绿是真缺陷**：那句"内置层把 manifest 引用的兄弟文件一起落下来"的断言，因为本机临时桶已被
  上一轮落满整份，判的其实是环境残留而不是这段代码；改成先删要判的那几份、拿锁、再落，s755 同一颗才在单元层
  咬住（`点名到=['the_funnel_names_the_other_copy_and_the_builtin_layer']`）。
- `logs/s756_pass31_lookup_parser_mutation.txt`：把命令表那 7 处 `value_parser` 整片摘掉 → 门禁 rc=1 点名新判据、
  集成 rc=101 红 2 条，单元侧仍绿（默认值不挂解析器不影响仓库根里逐条敲的形状）。这一跑的"干净树复核"红是
  **还原只改字节、mtime 拨回更早**造成的：`cargo test --test` 会按变异后的源码重链 `qx-cli.exe`，复核那一跑用的
  还是那颗旧 exe。按当轮重链之后三侧全绿见 `logs/s757_pass31_clean_recheck_after_touch.txt`。
- 用例：`logs/s758_pass31_full_test_counts.txt`——`qx-cli` 单元 1 段 **308 passed / 0 failed**、集成 17 段
  **382 passed / 0 failed**、其余 crate 89 段 **619 passed / 0 failed**，三段 rc 全 0、红名各 0 条。
  clippy `logs/s747_pass31_clippy.txt` / `logs/s748_pass31_clippy_nats.txt`。

## Unreleased — V13 R2 第三十遍：三查（孤儿/无预算/前后端贯通）在快速回测的默认值上挖到两条真断链（#260–#262，发布条件实测）（2026-10-02）

维度是"链路本身"：`backtest` 一类入口的**默认值**与 `quickstart` 收尾的**计数**都在按错误的东西取数。

### Fixed

- **`backtest <runtime.json>` 的 BarFrame 默认值不看配置里声明的那一份**（`crates/qx-cli/tests/backtest_frame_default.rs`，
  214 行 / 4 条真子进程用例：`config_only_backtest_reads_the_declared_ashare_bars`、
  `missing_declared_bars_fails_closed_instead_of_swapping_dataset`、`btc_example_runtime_still_backtests_on_its_declared_frame`、
  `undeclared_top_level_bars_does_not_guess_a_strategy_bars`）。改前无条件取仓库那份 BTCUSDT 示例夹具，
  于是 `init --profile ashare` 生成的项目照 README 敲 `backtest qianxing.runtime.json`，读的是**另一个标的的行情**，
  而屏幕上回的是 A 股那枚 run_id。改前矩阵 `logs/s725_pass30_config_only_matrix_before.txt`（逐模板 rc 列表），
  三颗变异 `logs/s726_pass30_mutations_frame_default.txt` 全部按声明红集咬住（还原逐字节相同）。
- **`quickstart` 收尾的文件计数没有目录预算**（`crates/qx-cli/src/quickstart.rs`：`PROJECT_FILE_COUNT_BUDGET = 2000`、
  `project_file_count(dir, budget)` 返回（份数，是否截断），截断时那句要说"已在 N 个目录的计数预算处截断，实际可能更多"）。
  这是一条会自己往下扫的路径：符号链接环或一棵大目录树能让收尾步永不返回。两条用例在
  `crates/qx-cli/src/tests/quickstart_file_budget.rs`；变异第一份 `logs/s727_pass30_mutations_quickstart_budget.txt`
  里 M2 红集漏声明、M3 掉到 2 恰好不截断真实项目（首轮探针：init 落 9 份文件、1 个子目录）所以不咬，
  第二份 `logs/s728_pass30_mutations_quickstart_budget.txt` 把口径改对之后三颗全咬。

### Registered（本轮只立案，不在这一遍动）

- #259：`RunManifest` 的 digest 把"输入怎么声明"吃进产物身份（同输入的两种调用形状 `instrument_spec_version` 不同），
  所以 P2 那格的相等判据只能是 `result_hash`。
- #263：PostgreSQL 连接面两条无预算等待（启动串行 connect 最多 128 次、无 connect timeout）。
- #169/#225、#174、#186、#195 保持在册。
- 发布条件复跑 `logs/s724_pass30_release_criteria.txt`：release exe 的 version / BTC quickstart（14 份、退 0）/
  A 股与 BTC 两条 `fast-backtest` / `deploy/data` 跑前跑后 71 → 71；唯一残留观察是启动目录在仓库根写出
  `ash`/`btc`/`pap` 三个目录（用例各改各的落点，见 capabilities 的 `integration_backtest_cases_write_into_the_repo`）。

## Unreleased — V13 R2 第二十九遍：首跑收成一条命令 `quickstart`，引导面收回「照着敲就能跑」——P2 第一格（U7 + U4 成功侧，#255/#260/#261）（2026-10-01）

这一遍仍不做断链取证，而是把 `docs/竞品对比与易用性改进优化计划-V1.md` §4.3 里 P2 的第一格做掉：
U7（首跑要自己串五条命令）、U4 的成功侧（跑完之后不指路）、#260（引导里印的程序名在装好的机器上根本不存在）、
#261（收尾那条 `paper-check` 只对七分之一的情形成立）。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.35。

### Added（#255：一条命令走完首跑，且与逐条敲同一条实现）

- `crates/qx-cli/src/quickstart.rs`（162 行，新增）：`run(project, force)` 依次执行 建项目 → 静态检查 → 跑一轮回测 →
  读回摘要 → 看安全状态，**直调这五个入口所用的同一批函数**（`run_init_with_profile` / `run_doctor` /
  `run_unified_backtest` / `run_report` / `run_status`），不另起第二套实现——否则「一条命令与逐条敲同结果」这句话
  只能靠文档承诺。每步成功印「[完成] <步骤名>：<该步的完整命令>」；任一步失败只回显**失败那一步**的原文命令与
  重跑整条的写法，以退出码 2 结束（`require()`）。收尾印「你刚做完了 5 步」+ 三条能直接敲的下一步
  （`report --json` / `strategy list` / `init --profile ashare`），并按 #261 把 `paper-check` 写成带前提的一句话而不是待敲命令。
- 命令行文本一律由 `cli_args::PROGRAM_NAME` 拼（`command_line()`），这是 #260 的口径：印出去的东西要能在装好的机器上敲。
- `crates/qx-cli/tests/quickstart_first_run.rs`（308 行 / 3 条真起 binary 的子进程用例）：
  `one_command_lands_the_same_chain_as_the_command_by_command_chain`（同输入 `result_hash` 逐字相等 + 项目内 14 份文件 +
  每条产物路径都以给定目录开头 + 仓库 `deploy/data/` 份数不变）、
  `advertised_next_steps_run_as_printed`（收尾三条下一步各退 0，且 `paper-check` 不出现在待敲命令里）、
  `second_run_without_force_stops_at_the_first_step`（不带 `--force` 重跑必止步第 1 步并回显原文命令）。
- 命令面接入：`cli_args.rs` 的 `Quickstart { project, force }` 变体 + `cli.rs` 的派发臂 + `cli_help.rs` 的条目；
  门禁既有的「help ≡ clap 命令表 ≡ 显式派发分支」三处相等判据当下报 `命令表 43 项`，`quickstart` 已在册
  （本轮未改门禁一行，靠的是这条常驻判据而不是新加的判据）。

### Changed（#260/#261：`init` 引导面抽出单源，收尾行按 profile 分三支）

- `crates/qx-cli/src/init_guidance.rs`（117 行，新增）：`init` 印给用户抄进终端的命令行只在这一处拼装——
  `init_backtest_step()` 按「配置里绑了策略」与「项目里真复制了 BarFrame / 市场规格」决定 `backtest` 那一行的形状，
  `init_flow_tail()` 分三支（paper 印 `paper-check`、绑了策略印 `report --json`、连 BarFrame 都没复制的不印），
  `init_readme()` 生成项目内的 `README.qianxing.md`。改前这三段散在 `init_project.rs` 里、程序名逐处硬编码 `qianxing`，
  而装好的机器上可执行文件叫 `qx-cli`——首跑照抄第二句就是 command not found（#260）；收尾那句 `paper-check` 此前对
  7 个 profile 一律印，照抄必撞「Paper 主链路缺少启用的 Scheduler worker」并以 2 退出（#261）。
- `init_project.rs` 496 → 427 行（搬出引导面），`main.rs` 挂载 `mod init_guidance;`；`cli.rs` 667 行仍在 670 的冻结预算内。
- `crates/qx-cli/src/tests/init_onboarding.rs`（303 → 388 行）新增 `readme_recommended_flow_runs_line_by_line`：
  把生成项目 README 的「推荐流程」**当数据源逐行执行**——base / builtin+macd / paper / ashare 四份 profile 各生成一个项目，
  印出的每一行命令都照原样真跑并断言退 0，收尾行的形状与那一支该不该出现 `paper-check` 一并判。
  同一文件里其余用例的 needle 也改由 `cli_args::PROGRAM_NAME` 拼，不再手写程序名。

### Verification（本轮实测）

- 架构门禁：改前 `logs/s696_v13_r2_pass29_gate.txt`、变异还原后 `logs/s704_pass29_gate_after_restore.txt` 都是
  `架构不变量自检全部通过 ✓（515 项）`；`tools/check_architecture.py` 一行未改（判据本体归协调者）。
- 用例：`cargo test -p qx-cli`（带 `QX_PYTHON`）`logs/s698`＝15 段 / **360 passed / 0 failed**；不带 `QX_PYTHON` 的
  `logs/s697`＝294 passed / 2 failed，那 2 条是 Python 桥用例（本机事实，见 V12 §15.4），不是本轮回归。
  整树 `logs/s699_v13_r2_pass29_workspace_tests.txt`＝104 段 / **999 passed / 0 failed / 1 ignored**，
  与上一遍终树 `logs/s694_pass28_workspace_tests_final_tree.txt`（103 段 / 995 passed / 1 ignored）之差恰好是本轮新增的
  3 条集成 + 1 条单元用例。
- quickstart 现场 `logs/s700_pass29_quickstart_chain_measure.txt`：`qx-cli quickstart <仓库外临时目录>` 退 0，项目内 14 份文件，
  `result_hash=26fdd6b52d020700`；同一条输入逐条敲 `backtest` 得同一枚哈希（`[direct] backtest rc=0`）；
  启动目录除项目目录本身零新增，仓库 `deploy/data/` 跑前跑后 71 → 71；不带 `--force` 重跑退 2 并回显原文 `qx-cli init …`；
  收尾三条下一步逐条退 0；四份 profile 的生成 README 逐行退 0。
- 变异反向验证 7 颗，两轮：`logs/s701_pass29_mutations_round1.txt`（M1 派发臂改空打印 → 3 条 quickstart 用例红；
  M3 摘掉失败那一步的原文回显 → 1 条红；M5 收尾无条件印 `paper-check` → 1 条红）与
  `logs/s702_pass29_mutations_round2.txt`（M2 摘掉 quickstart 的回测步 → 3 条红；M4 程序名改回硬编码 → 5 条红；
  M6 `bound_strategy` 恒真 → 2 条声明的红；M7 摘掉 help 的 `quickstart` 条目 → `GATE_RC=1` 且点名
  `只在派发里 ['quickstart']`）。每颗都 `cargo build -p qx-cli` 后再跑判据，还原按字节比对；
  还原后 `logs/s703` 回到 15 段 / 360 / 0、`logs/s704` 回到 515 项全绿。
- 流程事实（记下来防止下一遍重踩）：第一轮 M2/M4 各出 14 条无关红且 `suites=1`，成因是变异脚本把还原后的 mtime
  推到未来 + `cargo test` 不重链 `target/debug/qx-cli.exe`，于是仓库内 `assert_binary_fresh` 那颗守卫先 panic、
  cargo 又是按 target fail-fast 把后面的测试目标整段吞掉；同因让 M6/M7 的构建撞上 `os error 32`。
  第二轮改成「每次先 build 再 `--no-fail-fast` 测 + 还原用当前墙钟 + 写前逐字节预检 needle 命中数」后，
  每颗变异的红集合都能对上声明。M6 现场另有一条 `dead_worker_write_failure_names_program_origin_and_exit_state`
  的红属并发竞态（干净复跑 `logs/s703` 里它是绿的），不作判据。
- 格式化与 lints `logs/s705_pass29_fmt_clippy.txt`：`cargo fmt --all --check` rc=0、
  `cargo clippy --workspace --all-targets -- -D warnings` rc=0、带 `--features nats` 同样 rc=0，warning 行数 0。
- 九步构建 `logs/s706_pass29_nine_step_build.txt`：`BUILD_RC=0`，`[1/9]` 门禁 515 项、`[4/9]` Rust 104 段 999 passed / 0 failed、
  `[6/9]` Python `Ran 61 tests … OK (skipped=1)`、`[8/9]` CLI 全链路与生态冒烟通过。
- 发布面（按 #159/#179/#194 的载荷口径，不抄整档 sha）：
  `logs/s708_pass29_release_exe_probe.txt`——`target/release/qx-cli.exe` 跑同一条 `quickstart` 同样退 0 / 14 份文件 /
  同一枚 `result_hash=26fdd6b52d020700`；两档构建的 RunManifest `digest` 不同（debug `61edf8801c0df856`、
  release `bad55f99ef068572`），因为 `digest` 覆盖构建身份而 `result_hash` 只覆盖回测结果。
  `logs/s709_pass29_wheel_repack.txt` 按终树重打包 → `logs/s710_pass29_wheel_payload.txt`：wheel 17 条目名称集合不变、
  逐条目 CRC 只 2 项变（`_qianxing_native.pyd` 与 `RECORD`，Python 侧一行没动），wheel 内 12 份 `.py` 与仓库 `python/`
  逐字节相同，内嵌 `.pyd` md5 ≡ 当轮 `target/release/_qianxing_native.dll`（`bbfdd07adb4528df6b0e9bec77c47397`，
  同一次调用内的 staging 核对）。`logs/s711_pass29_clean_venv_smoke.txt`：`uv venv` + `pip install --no-deps` 装好后
  四包导入 OK、`native.available=True`、`StrategyIntent` 线格式往返相等、5 类非法字段各抛 `ValueError` 计数 5/5、
  装机侧 `.pyd` md5 仍等于那颗 dll；`tzdata` 从仓库 venv 复制进去才有「四包导入 OK」（#212 那一格仍未在册外测）。

### 没做的事（如实在册）

- `quickstart` 只把 **builtin + macd** 这一条首跑路径收成一格：ashare / ccxt / multi-venue 的 profile 各自要带的
  规格与数据仍不同，首跑没有统一入口（写进 `maturity/capabilities.yaml` 的 `cli_scenario_init` limitation）。
- P2 其余格没动：`python/examples/` 2 → 6 份、全局 `--config` 别名、以及「从 README 逐行取数」这类判据仍缺常驻门禁。
- 本轮没有新增门禁判据（`tools/check_architecture.py` 归协调者）；quickstart 能被钉住靠的是既有三处相等判据 + 上述 4 条用例。
- `docs/` 与 README 里散文侧的逐字重复块仍无判据（#195）；本轮在 `deploy/README.md` 的推荐入口串里人工检出并删掉了
  一条逐字重复的 `cargo run --release -p qx-cli -- backtest`，这是自检发现、不是判据发现。
## Unreleased — V13 R2 第二十八遍：装好之后先答"我装的是哪一个"——`version` 出口 + 用法错误分级回显（P1：U1/U2/U4，#257/#258）（2026-10-01）

这一遍不做断链取证，而是按 `docs/竞品对比与易用性改进优化计划-V1.md` §4.3 的排序把 P1 的三格做掉：
U1（没有任何版本/构建出口）、U2（一条拼错的命令换来整篇入口摘要）、U4（用法错误不回「下一步」）。
详见 `docs/自研量化框架审计与重构方案-V13.md` §9.34。

### Added（#257：构建身份一个出口、一份来源）

- `crates/qx-cli/src/build_identity.rs`（48 行，新增）：`RUNTIME_VERSION` / `BUILD_REVISION` / `TARGET_TRIPLE` / `BUILD_PROFILE`
  四个常量 + `short_revision()`（7 位十六进制并保留 `-dirty`）+ `identity_line()` + `print_identity()` + `doctor_check()`；
  一行形状是 `qianxing <semver> (build <sha7>[-dirty], target <triple>, profile <release|debug>)`。
- `crates/qx-cli/build.rs` 注入两颗新变量：`QX_TARGET_TRIPLE`（cargo 的 `TARGET`）与 `QX_BUILD_PROFILE`（`PROFILE`），
  和既有 `QX_GIT_COMMIT` 走同一条 loop、缺失时同一条 `unknown` 回落。
- 命令面新增 `version` 入口（`cli_args.rs` 的 `#[command(name = "version")] Version` + `cli.rs` 的派发臂），
  `--version` / `-V` 在 `cli.rs` 最前面的那次别名预检里指向同一个 `print_identity()`：三条写法逐字相同、都退 0、都不带横幅。
- 单源化的另一半是收掉旧的多份抄写：改前 `env!("QX_GIT_COMMIT")` / `env!("CARGO_PKG_VERSION")` 散在四份文件里的六枚（2+2+1+1）
  （`scheduler.rs`、`selfcheck.rs`、`backtests/depth.rs`、`backtests/single_strategy.rs`），现在四份全部指向 `build_identity::*`；
  只有 `src/tests/backtest_entries.rs:412` 故意留一枚自己的 `env!("QX_GIT_COMMIT")` 作独立对照，不让判据与实现共用同一个来源。
- 同一行进 `doctor` 的第一格（`build_identity`），`status --json` 与 `report --json` 各带一枚 `runtime_version`；
  人工读面与机器读面取的是同一个常量。

### Changed（#258：用法错误从整篇摘要收成三段回显）

- `crates/qx-cli/src/usage_errors.rs`（19 行，新增）：`report(&clap::Error) -> !` 打印
  ①`未知命令或未知参数: <clap 错误正文>`（含最接近的入口名，实测 `versoin` 点名 `'version'`）、
  ②clap 自己给出的该入口 `Usage:`、③两行「下一步」与「自证构建」，退出码仍是 2。
- 改前每条用法错误先 `print_cli_help()`：实测 162 行 / 12,319 B（`--version`/`-V`/未知名）与 164 行 / 12,361–12,389 B（`version`/`doctor --config …`），五段逐字在 `logs/s691_pass28_before_fix_error_wall.txt`（改前 debug 树）；改后 `bogus-entry` 8 行 / 289 B、
  `doctor --config x.json` 10 行 / 359 B，stdout 保持为空（错误只走 stderr）。
  整篇摘要只在 `help` 与 `--help`/`-h` 出口打印，三者仍退 0（`cli.rs` 的 `fail_usage()` 对 `DisplayHelp*` 单独放行）。
- `cli_help.rs` 里 `help` 条目的文案改口：它此前声称用法错误会打印这份摘要，正是 #157 那一类「文档手抄 CLI 输出」的活体样本，本轮实测已把这一处消掉（#157 整条仍在册，缺口是常驻判据而非这一句）。

### Tests（`crates/qx-cli/tests/version_and_usage_echo.rs`，172 行 / 6 条子进程用例）

`three_version_spellings_print_one_identical_line` · `identity_line_carries_version_commit_target_and_profile` ·
`doctor_reports_the_same_identity_line_as_version` · `usage_error_echoes_graded_lines_instead_of_the_entry_summary` ·
`misspelled_and_misshaped_entries_get_their_own_hint` · `json_surfaces_carry_the_same_runtime_version_as_version_entry`

### Deviation（两条如实记录，不改判据换绿）

- 计划里 P1 的文档判据写的是「`--version` 的正确输出文本在 `deploy/README.md` 与 README 各出现一次，且与源码常量单源」，
  **没有采用**：identity 行含当轮 git sha 与 `-dirty`，写进文档就是每构建一次腐一次，与 #157/#159 同一形状。
  替代口径是「文档写形状、等值由测试钉」：文档只写 `<semver> (build <sha7>[-dirty], target <triple>, profile …)`，
  三条写法互相相等 / `doctor` 首格 ≡ `version` 行 / JSON 的 `runtime_version` ≡ 该常量三件事由上面 6 条用例在运行时判。
- **没有做** `artifact_probe`（exe 整档 sha / 字节尺寸核对）：qx-cli 没有 sha2 依赖，构建身份里烧进的 git sha 已是更强的出处凭据，
  且 #159/#179 已判定「整档 sha 与字节尺寸从来不是判据」。发布物本轮只按载荷口径复核（见 Verification 最后两条）。

### Verification（本轮实测）

- 架构门禁两次：`logs/s678_pass28_gate_after_p1.txt`、`logs/s687_pass28_cli_surface_probe_gate.txt` 各 515 `[PASS]` / 0 `[FAIL]` / `GATE_RC=0`；
  **门禁本体一行未改**（`tools/check_architecture.py` 归协调者），本轮新增入口能被钉住靠的是既有的三处相等判据。
- 整树 `logs/s679_pass28_workspace_tests.txt`：103 段 `test result` 行 / 995 passed / 0 failed / 1 ignored / `TEST_RC=0`；
  与上一遍 `logs/s660_pass27_workspace_tests.txt`（102 段 / 989 passed）的差恰好是新增的 `version_and_usage_echo`（日志里 `running 6 tests`）。
- 变异反向验证 5 颗：`logs/s680_pass28_mutations_m1_m4.txt` M1 摘掉 `--version`/`-V` 别名预检（判 `three_version_spellings_print_one_identical_line`）/
  M2 把用法错误退回整篇摘要墙（判 `usage_error_echoes_graded_lines…`）/ M3 `doctor` 不再报构建身份（判 `doctor_reports_the_same_identity_line_as_version`）/
  M4 版本号改成硬编码字面量（判 `identity_line_carries_version_commit_target_and_profile`），
  每颗 `running 6 tests` 里恰好 1 条 FAILED，均 `restored byte-identically`，还原后基线 `6 passed; 0 failed`；
  `logs/s681_pass28_mutation_m5_help.txt` M5 删 `help` 里的 `version` 入口行 → `GATE_RC=1` / 1 条 `[FAIL]` 点名 `只在派发里 ['version']`，逐字节还原 + `touch` rc=0。
- `cargo fmt --all` 后 `--check`、clippy `--workspace --all-targets -- -D warnings`（`logs/s682_pass28_fmt_clippy.txt`）
  与 `--features nats`（`logs/s683_pass28_clippy_nats.txt`）均 rc=0。
- 九步 `build.bat` 整跑 `logs/s685_pass28_nine_step_build.txt`（167,595 B）`BUILD_RC=0`，`[0/9]` 解释器为本轮 `QX_PYTHON`（3.12.13）。
- release 产物行为 `logs/s686_pass28_release_surface_probe.txt`（exe 12,040,704 B）：三条 version 写法 rc=0、单行 85 B 逐字相同、5–6 ms；
  `qianxing 0.1.0 (build ad2908b-dirty, target x86_64-pc-windows-msvc, profile release)`；`doctor` 首格 `[PASS] build_identity:` 与该行逐字相等；
  `status --json` / `report --json` 均含 `"runtime_version": "0.1.0"`；`help` 与 `--help` 仍 157 行 / 12,347 B（唯一保留的全量出口；与改前那 162 行差 5 行 = 不再打印的 8 行错误块 − 新增的 1 行 `version` 入口，两条线各自独立）。
  debug 产物同形对照 `logs/s684_pass28_cli_surface_probe.txt`（`profile debug`、83 B/行）。
- wheel 按终树重打包 `logs/s688_pass28_wheel_repack.txt`（rc 0），载荷 `logs/s689_pass28_wheel_payload.txt`：17 条目 / 218,663 B，
  `_qianxing_native.pyd` md5 ≡ 本轮 `_qianxing_native.dll` md5（#194 口径）。
- 环境事实一条：不带 `QX_PYTHON` 的整树 `logs/s690_pass28_workspace_tests_without_qx_python.txt` 在 qx-cli 段 2 failed
  （`e2e_and_python_contract` 的两条 Strategy worker，报错原文点名「QX_PYTHON 未设置，回落 PATH python」），
  即那两条是环境门而不是代码回归；本轮全部绿数都在带 `QX_PYTHON` 的前提下取得。

## Unreleased — V13 R2 第二十七遍：同一份 runtime 配置指两棵树——回测产物落点与运行态/证据闸门落点收成一条口径（#255）（2026-10-01）

这一遍的触发点还是"把上一遍写下的复跑当真跑一次"：整树 `cargo test --workspace` 在 qx-cli 段就判红，
五条失败全指向 `storage.data_dir` 被读成两棵树。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.33。

### Fixed（#255：`storage.data_dir` 只有一条落点）

- 改前分工：回测产物写侧与 `report`/`status` 读侧按"相对 runtime.json 同级目录"解析
  （`crates/qx-cli/src/backtests/strategy_backtest.rs:19`、`crates/qx-cli/src/config_commands.rs:209`/`:283`），
  其余一律按"相对进程当前目录"——可写运行态（账本/队列/outbox）、Paper 入账、HTTP 读面，
  以及实盘就绪闸门 `crates/qx-cli/src/readiness.rs:154` → `verify_event_backtest_evidence`。
- 三条后果当场可复现：文档那条 `fast-backtest deploy/*.json` 写出的 `runs/*.summary.json` 永远进不了它自己的实盘证据闸门
  （"回测证据先于实盘"这一格是假的）；同一条命令从仓库根启动把产物写进**被 git 跟踪**的 `deploy/data/**`；
  `dataset-ingest` 的注册表与回测打开的注册表落在两棵树，注册与消费看不见彼此。
- 改法是三个点全换成同一条口径（`Path::new(&config.storage.data_dir)`，相对进程当前目录）。**没有**采用"读侧两棵树都扫一遍"：
  门禁把 `verify_event_backtest_evidence(root,research)` 与每个读侧恰好一处 `validate_research_snapshot_binding(root,` 钉死
  （`tools/check_architecture.py:3352` 一带），双根扫描等于把闸门改成"任选一棵"，那正是本条病灶。
- 补第二条：`init` / `strategy init` 生成的项目把 `storage.data_dir` 钉成项目目录下的绝对路径
  （`crates/qx-cli/src/path_resolution.rs:27` 的 `anchor_init_data_dir`，`crates/qx-cli/src/init_project.rs:338`/`:463` 各调一次；
  已是绝对值的模板如 production 的 `/var/lib/qianxing` 原样保留）。理由是 README 第 99 行那句"在任意目录都能开一个自包含项目并跑通回测"
  改口径后只有这一种活法；`init_backtest_step`（`init_project.rs:198`）本就按同一原则钉输入路径。
- 判据：`crates/qx-cli/tests/backtest_artifact_root.rs` 三条子进程用例（`running 3 tests` / 3 passed / 0 failed，
  `logs/s669_pass27_judge_file_3of3.txt`）分别钉"产物落启动目录而不是配置同级"、"report 与启动目录同源且换目录不许跨树"、
  "init 项目自带绝对落点"。
- 变异反向验证两跑：去掉写侧口径 → 前两条判据同时红（`logs/s662_pass27_mut_data_root.txt`，
  报错原文点名 `configs\data/qianxing-ashare\runs` 与 `未找到回测摘要: data/qianxing-ashare\runs`）；
  去掉 `anchor_init_data_dir` 两处调用 → 1 条集成判据 + 3 条在库用例红（`logs/s663_pass27_mut_anchor_red.txt`，
  含 `init_lands_nine_files_and_the_advertised_backtest_adds_five` 的 9↔14），两跑均 `RESTORED … identical=True`。
- 回归（换落点不许换结果）：A 股 `fast-backtest` `jobs=1 completed=1 result_hash=6be034a4cb0760fa`（`logs/s665_…`）、
  BTC `fast-backtest` `jobs=2 completed=2` 两条 `result_hash=1189853a7c12447d` / `bd323a37d6186dcc`（`logs/s666_…`）
  与改口径前逐字相同；`report` 读回同一份摘要（`logs/s667_…`），`config doctor` 在旧树还在时如实报
  `[WARN] storage.data_dir.split` 并点名两棵落点（`logs/s668_…`）。
- 文档同步：`README.md` 的首屏命令注释与"产物落点"两处口径、`README.md` 末段链路表的回测未收口一格、
  `deploy/README.md` 的回测产物段落（补上"一条口径 + 换目录=换树 + doctor 会点名"）、
  `maturity/capabilities.yaml` 的 `integration_backtest_cases_write_into_the_repo` 限制改口并补两条 evidence。

### Verification（本轮实测）

- 整树复跑：改前 `logs/s655_pass27_workspace_tests.txt` 只跑到 82 段就 `TEST_RC=101`（qx-cli 段 `290 passed; 5 failed`）；
  改后 `logs/s660_pass27_workspace_tests.txt` = `TEST_RC=0` / 102 段 `test result` 行 / 989 passed / 0 failed。
- 架构门禁 `logs/s657_pass27_anchor_gate.txt` = 515 项 `[PASS]` / 0 条 `[FAIL]` / `GATE_RC=0`（与本轮改码前 `logs/s656_…` 同计数）；
  clippy `logs/s658_pass27_clippy.txt` 与 `logs/s659_pass27_clippy_nats.txt` 均 rc=0。
- 工作树：`git status --porcelain -- deploy/data` 0 行；未跟踪 40 条全部是本轮新增源码/用例与 `docs/` 计划文档，0 条命中 `data/**`。
- 发布物按 #194 的固定顺序在终树重跑：九步整跑 `logs/s672_pass27_nine_step_build.txt` `BUILD_RC=0`、`[1/9]`—`[9/9]` 全过
  （102 段 `test result`、`FAILED` 0 次、`[6/9]` Python `Ran 61 tests` / OK (skipped=1)、`[8/9]` `digest=62aa341c3cd073ac`
  与 `[9/9]` `config_fingerprint=2fc5aa6783c53892b7808ea9e166ed7357eb721abdc8a4df03cda5d84a2fd2b6` 均与上一遍逐字相同）；
  `tools/build_python_wheel.ps1` 从终树重打包得到 wheel 218,664 字节 / 17 个条目，内嵌 `.pyd` 353,280 字节、md5
  `a9eaf573125ab37332e8b0efba3538f0` ≡ `target/release/_qianxing_native.dll`（`logs/s673b_pass27_wheel_repack.txt`）；
  重打包的 wheel 装进第二个干净临时 venv（`--no-deps --no-index`）四包 import 全 OK、`normalize_instrument("sz.000001")` → `000001.SZSE`。
  `.pyd` md5 与上一遍不同而尺寸相同：链接器每轮重打 build-id，正是 #159"按载荷核对而不是按整档 sha"的第二格实证。

## Unreleased — V13 R2 第二十六遍：安装包在一台没装 tzdata 的 Windows 上 `import` 就崩——把时区取值从模块顶层挪到用时（#253）（2026-10-01）

这一遍的触发点不是新读代码，而是把发布链上一遍写下的那句"干净 venv 装 wheel 复跑"**真的执行了一次**：前二十五遍的复跑都跑在仓库自己的
venv 里（那里 `tzdata` 一直在）。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.32。

### Fixed（#253：`import qianxing_ashare` 不再要求机器先有时区数据库）

- 改前实测（`logs/s642_pass26_offline_import_before_fix.txt`）：拿上一遍定稿的 wheel（218,449 字节）在临时 venv 里
  `pip install --no-deps --no-index`，`qianxing_bridge` / `qianxing_strategy` / `qianxing_ccxt` 三包 import 成功，
  唯独 `qianxing_ashare` 当场抛 `ZoneInfoNotFoundError: No time zone found with key Asia/Shanghai`。崩点是模块顶层的
  `_SHANGHAI = ZoneInfo("Asia/Shanghai")`——Windows 的 `zoneinfo` 不自带时区库。
- 定性为缺陷而不是"依赖没装而已"的三条理由：① 与本模块开头「数据源依赖均为可选依赖，核心包和离线测试不需要安装……」的承诺相反，
  且同文件三个数据源模块本来就都是 `importlib.import_module` 惰性加载 + 抛 `AshareProviderError` 点名 `pip install …`；
  ② 用户不查 A 股、不做时间换算也会 import 失败；③ 原文不指修法。
- `python/qianxing_ashare/__init__.py`（76,800 → **77,339 字节 / 1,920 行 / CR=0**）把顶层取值换成 `:38` 的 `_shanghai()`，
  两处调用点 `:522` `_manifest_timestamp_ms`、`:794` `_timestamp` 一并改为用时取值，报错原文
  「Asia/Shanghai 时区数据不可用，请执行 pip install tzdata 后重试」。**没有**加"回落本地时区/固定 +08:00"的兜底：
  时区口径进跨语言指纹（`build_dataset_bundle_manifest` 的 `start_timestamp` / `end_timestamp` 要与 Rust 侧对齐），
  静默换算是比崩溃更坏的结果。
- 三处下游口径同步改口：`build.bat:102`、`build.sh:25` 的 tzdata 预检提示与 README 排错表那一行，原文都写「缺 tzdata 时 `[6/9]` 的 A 股用例以 `ZoneInfoNotFoundError` 失败」，本遍之后这句不再成立（失败的是 `AshareProviderError`，`import` 那一步照常通过）。
- 判据 `python/tests/test_ashare.py`（18,068 → **19,917 字节**，套件 59 → **61** 条）`:431` / `:440` 两条：
  `test_naive_dates_are_stamped_at_shanghai_midnight` 钉住裸日期 `2024-01-02` → 1,704,124,800,000 毫秒；
  `test_missing_tzdata_fails_at_first_use_and_names_the_remedy` 把 `qianxing_ashare.ZoneInfo` 换成必然抛错的实现后走真实入口
  `normalize_bar_rows`，要求抛 `AshareProviderError` 且文案含 `pip install tzdata`。
- 变异反向验证 `logs/s640_pass26_mutations.txt` 两发各点亮一条、互不重叠：① 整文件换回改前那份 →
  `FAILED (failures=1)`，红的是 `test_missing_tzdata_…`（另一条仍绿——两种写法数值本就相同）；
  ② 只把 `ZoneInfo("Asia/Shanghai")` 改成 `ZoneInfo("UTC")` → 红的是 `test_naive_dates_…`。
  还原后 `RESTORE_IDENTICAL=True size=77339`、md5 与镜像一致（`be78f761a2ad`）。
- 交付面按终树重跑（#194 顺序）：九步构建 `logs/s637_pass26_nine_step_build.txt` `BUILD_RC=0`（第 2279 行），`[6/9]` Python 侧
  从上一遍的 `Ran 59 tests`（`logs/s635_pass25_nine_step_build.txt` 第 2214 行）变成本轮第 2210 行的 `Ran 61 tests`；
  第 2241 行 `digest=62aa341c3cd073ac` 与第 2270 行 `config_fingerprint=2fc5aa6783c53892b7808ea9e166ed7357eb721abdc8a4df03cda5d84a2fd2b6`
  与上一遍逐字相同，即"本轮只改 Python"的对照证据。wheel 重打包为 218,663 字节，
  载荷核对 `PYD_EQ_DLL=True`（`.pyd` ≡ `target/release/_qianxing_native.dll`，353,280 字节、md5
  `81ad2d1af96d20f13cd96dbf26946303`），17 个条目 CRC 变化恰好 3 条（`qianxing_ashare/__init__.py`、`_qianxing_native.pyd`、`RECORD`）。
  这份 wheel 装进**另一个**不带 tzdata 的干净 venv 复跑（`logs/s638_pass26_wheel_offline_smoke.txt`）：四包 import 全 OK，
  裸日期那一格抛 `AshareProviderError` 且文案点名 `pip install tzdata`，`normalize_instrument("sz.000001")` 照常返回 `000001.SZSE`。
  改后重跑读文档那族用例（`cargo test -p qx-cli doc`，`logs/s643_pass26_doc_tests_after_docs.txt`）：`running 23 tests` / 23 passed / 0 failed / TEST_RC=0；本轮四处文档里被 Rust 解析的是 `maturity/capabilities.yaml`，`docs/` 长文档与 `CHANGELOG.md` 按 `artifact_identity_doc.rs:9` 的既有口径不进判据。
- 跨文档引用的一次追账：`docs/竞品对比与易用性改进优化计划-V1.md`（第二十四遍落盘的改进计划）里有三处引用本审计文档的规模与本仓待暂存路径数，全是第二十四遍当时的快照，本遍之后都成了假话。本轮的处理是把那两处规模引用**换成不漂移的口径**——改引 V13 的章号（§9.32 / 第二十六遍）而不是字节数，因为一份文档的字节数每被编辑一次就变，引用方永远在被引用方定稿之前不可能写对；这与 #159「发布产物整档 sha 不可复现，文档引用要换成载荷口径」是同一条纪律的两个实例。待暂存计数按本轮实测改成 **115 条 / 116 个文件**（76 条已改 + 39 条未跟踪条目，其中 2 条目录条目展开后是 40 个新文件）；§0.1/§3 的 109 条、50 条重叠读数保留原样，只声明过期，不改写历史。这一笔正是「文档散文没有常驻判据」（#195）的代价：引用方与被打方各长一次，就要人工追一次。

### Verification（本遍终树）

- 架构门禁五次：`logs/s639_pass26_gate_after_code.txt`（改码后）、`logs/s641_pass26_gate_after_docs.txt`（文档落盘后）、
  `logs/s646_pass26_gate_after_wording.txt`（三处报错口径改口后）、`logs/s649_pass26_gate_terminal.txt`（收口后）、
  `logs/s651_pass26_gate_after_doc_sync.txt`（跨文档数字改口后），五次都是 **515 项 `[PASS]` / 0 条 `[FAIL]` / GATE_RC=0**。
- 读文档那族 Rust 用例四次：`logs/s643` / `logs/s647_pass26_doc_tests_after_wording.txt` /
  `logs/s650_pass26_doc_tests_terminal.txt` / `logs/s652_pass26_doc_tests_after_sync.txt`，
  四次都是 `running 23 tests` / 23 passed / 0 failed / TEST_RC=0。
- Python 侧终树整跑 `logs/s648_pass26_python_suite_after_wording.txt`：**`Ran 61 tests` / `OK (skipped=1)` / PY_RC=0**，
  与九步构建 `[6/9]`（`logs/s637_pass26_nine_step_build.txt` 第 2210 行）同一口径、同一数字。
- Rust 源码本遍零改动：`[8/9]` 的 `digest` 与 `[9/9]` 的 `config_fingerprint` 与上一遍逐字相同（见上），
  发布物（release exe 与 wheel）都产自 `logs/s637` 那一跑；`s646` 之后的改动只落在 `docs/` 与 `CHANGELOG.md` 的散文上，
  这两类文件没有任何判据取数（门禁读的 Markdown 只有 `README.md` 与 `deploy/README.md`）。
- 补记：把上面这些计数写进文档后又各复跑一次——门禁 `logs/s653_pass26_gate_after_addendum.txt`、
  doc 用例 `logs/s654_pass26_doc_tests_after_addendum.txt`，结果与 `s651` / `s652` 逐字相同
  （**515 项 `[PASS]` / 0 条 `[FAIL]` / GATE_RC=0**；**`running 23 tests` / 23 passed / 0 failed / TEST_RC=0**）。
  这两跑不再计入上面的「五次 / 四次」：**判据计数以本轮最后一条日志号为准**——门禁
  `s639`/`s641`/`s646`/`s649`/`s651` 之后加跑 `s653`，doc 用例 `s643`/`s647`/`s650`/`s652` 之后加跑 `s654`，
  否则『记录这次运行的那句话』本身又要求一次新的运行。

### 在册（本遍只登记，不动门禁）

- **#254**：门禁缺"Python 模块顶层有副作用调用"这一族判据（形状：扫 `python/**/*.py` 顶层语句里的 `ZoneInfo(` / `socket.` /
  `open(`，白名单需能表达"常量表"这类合法顶层求值）。按本仓纪律 `tools/check_architecture.py` 归协调者。
- **#212 保持在册**：联网装齐 wheel 声明依赖那一格本机仍无网络证据；本遍补的是反向那一格（不装依赖时包的行为）。
- `deploy/README.md` 未改：在线路径 `pip install dist/*.whl` 会带上声明的 tzdata，那条没有需要修正的话。

## Unreleased — V13 R2 第二十五遍：一轮的"第几条意图"不是订单身份——把 `round_scope` 折进 `client_id`（#248，移植上游同名修复）（2026-10-01）

这一遍只落一颗，但它坐在三条链共用的一个键上：策略侧的 `intent_id` 是**轮内**局部序号，而 `Order::client_id` 同时是控制面的
`command_id` 与执行面的 `client_order_id`，两者都是全店唯一键。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.31。
上一遍（#247）只落 `docs/竞品对比与易用性改进优化计划-V1.md`，没有代码条目。

### Fixed（#248：换一轮就重新从 1 起算的意图序号不再复用上一轮的订单身份）

- 链条实测（逐段读源码，非推断）：`crates/qx-cli/src/strategy_host.rs:792` 每轮新建 `BuiltinStrategy`，
  `crates/qx-strategy/src/builtin.rs:248` 的 `next_intent_id` 从 1 起算；旧代码 `client_id: intent.intent_id` 于是让
  **第二轮的第一条意图**撞上第一轮的第一条。`ControlPlane::submit` 的顺序是先 `requests` 后 `commands` 去重，所以后果不是
  报错而是 `DuplicateCommand`——收下不执行，长跑 paper/live 作业从第二轮起悄悄不发单。
- `crates/qx-cli/src/strategy_contract.rs`（→ **860 行 / 33,703 字节**）新增 `strategy_order_identity(round_scope, intent_id)`：
  `qx_core::Fnv1a` 折当轮 `request_id` 与轮内序号，`finish().max(1)` 保证 `client_order_id` 为正。
  `build_strategy_order_from_contract_intent` 多收一个 `round_scope`，两处生产装配点各传当轮 `output.request_id`
  （`strategy_contract.rs:192` 的 ContractBarStrategy 循环、`crates/qx-cli/src/workers.rs:380` 的 worker 循环，后者 → 527 行）。
  同轮重放仍落回同一身份（幂等），换一轮得到新身份，`trace.intent_id` 保留策略侧原值。
- 没有一起改的两侧写进注释并说明理由：回测链 `crates/qx-strategy/src/lib.rs:282` 的 `to_orders` 仍是裸 `intent_id`——一个 run
  只 `BuiltinStrategy::new` 一次（`crates/qx-cli/src/backtests/mod.rs:134`、`backtests/depth.rs:106`），序号全程单调，且落进
  本地簿而非控制面（`crates/qx-xingban/src/orderbook.rs:229` 重复 `client_id` 当场报错）；目标仓位再平衡链
  （`strategy_contract.rs:649`）本就取 `run_id.max(1)`，已是轮级唯一。
- 新增 `crates/qx-cli/src/tests/strategy_order_identity_round_scope.rs`（**99 行 / 3,585 字节**，挂 `tests/mod.rs:428`）两条：
  `contract_intent_identity_is_scoped_by_round`（同轮重放相等 / 跨轮不等 / 同轮不同意图不等 / `client_id > 0` /
  `trace.intent_id` 原值）与 `consecutive_rounds_are_accepted_by_the_control_plane`（三轮真实 `ControlPlane::submit`
  收进三个不同 `command_id`）。绿侧 `logs/s628_pass25_identity_tests.txt`（`running 2 tests` / 2 passed）。
- 变异反向验证 `logs/s632_pass25_mut248_identity.txt`：`client_id` 退回裸 `intent_id` → 两条同时判红
  （`test result: FAILED. 0 passed; 2 failed`），随后按字节还原（`restored_identical=True`，33,703 字节）。第一次脚本在写入前
  `assert` 中止——编辑把整篇 `strategy_contract.rs` 翻成 CRLF 使 needle 命中 0 次；恢复 LF 后才拿到红侧证据。
- 行数棘轮第一次咬本遍：`--snapshot` 前门禁 `[FAIL]`（`logs/s629_pass25_gate_ratchet_red_before_snapshot.txt`），重算快照后
  `maturity/line_budgets.yaml` 的 diff 只有 4 行（本遍 `strategy_contract.rs` 841→860、`workers.rs` 526→527；前两遍删码留下的
  `qx-core/src/event.rs` 727→721、`qx-runtime/src/pipeline.rs` 2371→2332 为下行）。
- 上游台账：这颗与上游 `main` 的 `round_scope` 修复同形，属**移植**，#232 真合流时该 hunk 不再需要仲裁。三颗上游修复仍待移植
  （控制面 `AUDIT_WINDOW_RECORDS = 1_000`、NATS `wedged: Arc<AtomicBool>` 闩、整模块缺席的 `crates/qx-core/src/file_lock.rs`）。
  同时纠正第二十四遍台账两处误判：`OutboxRecovery::{ReadOnly,ReprojectOnOpen}` 与 serve 的按请求取时（#221）是我方独有；
  `environment` 词汇表上游另有一套（`RUNTIME_ENVIRONMENTS` / `environment_kind()`），属重复实现而非一侧缺席 → 立案 #249。

### Verification（本遍终树）

- `cargo fmt --all` 后 `--check` 干净（FMT_RC=0）；clippy 默认与 `--features nats` 均 RC=0（`logs/s633_pass25_clippy_after_248.txt`）。
- 架构门禁 `logs/s630_pass25_gate_after_248.txt`：**515 项 `[PASS]` / 0 条 `[FAIL]` / GATE_RC=0**。
- 整树测试 `logs/s631_pass25_whole_tree_after_248.txt`：**101 行 `test result:` / 986 passed / 0 failed / 1 ignored**。
  同一棵树第一次整跑（不设 `QX_PYTHON`）判红 2 条 Python worker 契约用例，报错原文即「`QX_PYTHON` 未设置，回落 PATH python……
  WindowsApps 的 python 占位桩」——红的是环境不是代码，口径见 #133/#185。

## Unreleased — V13 R2 第二十三遍：一道按措辞而不是按地址判的接口面闸门，一本门禁看不见的变体台账，与一个由拼写决定真实提交的字段（#244 / #243 / #245）（2026-09-30）

取证问句仍是「主体流程联通 + 孤儿逻辑 + 前后端贯通」三格，这一遍三处从三个方向进来：#244 是**配置面闸门与接口文档不一致**
（明文 API 可以绑可路由地址，等于把一个无鉴权的下单入口挂在网络上）；#243 把第二十二遍立案的 #241 落地（门禁的变体扫描
看不见结构体式变体，142 颗在册变体从来没有被判据看过一眼）；#245 是「孤儿逻辑」的反面——一个字段有 14 处按措辞分派的
读者，却没有任何一处限定它能写哪些值。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.30。

### Fixed（#244：明文 API 只能绑回环地址，判定式、接口文档与服务装配面三处钉在一起）

- 改前实测（`logs/s555_before_validate.txt`）：把仓库那份明文模板的 `api.bind` 换成 `10.20.30.40:8443` 之后
  `config validate` 仍回 `[PASS]`。旧判定式只把 `environment` 与字面量 `production` 相比，而仓库 17 份明文 runtime 模板
  用的是 paper/sandbox/testnet——闸门对着的那批配置根本没有一个写 `production`。
- 危害不是"少一层加密"：明文面**不装**操作员权限策略（`api.operators` 为空 ⇒ `ApiService::new` 走 `policy: None`），
  `POST /control/commands` 的档位直接取请求体里的 `permission` 字段，等于调用方自报权限。两条合起来是一个无鉴权的下单
  入口挂在可路由地址上，而接口文档当时只承诺「除三个只读端点外都要求已认证 operator」。
- 闸门改成按**地址**判：`crates/qx-runtime/src/runtime_config/topology_validation.rs`（→ **481 行 / 21,754 字节**）里
  `ApiTransport::Plaintext && !api_bind.ip().is_loopback()` 即拒绝，报错原文「明文 API 只能绑定回环地址，当前 bind=… 不是；
  请改用 transport=mtls 并配置 Operator 证书」。`config validate`、`doctor`、`runtime-check`、`serve` 共用同一个
  `RuntimeConfig::validate()`，所以四条入口一起关上。
- 新增 `crates/qx-cli/src/tests/api_transport_auth_boundary_doc.rs`（**200 行 / 8,612 字节**，挂 `tests/mod.rs:305`）三条：
  ① 文档里那句拒绝语从 `deploy/README.md` 的散文里抠 `「…」` 引用，与源码字面量对照——不在这份文件里抄第二份字面量；
  ② 17 份明文 runtime 模板逐份按 `config validate` 的读法装载，每份的 bind 都得是回环（mtls 那份单独数出来，
  只允许 `qianxing.runtime.production.example.json`）；③ 同一条自报档位的下单请求，在"没装策略"与"装了策略"两种
  `ApiService` 上分别拿到 202 与 403 `authenticated_operator_required`。
- 变异反向验证：`logs/s559_mut_m1_no_guard.log`、`logs/s560_mut_m2_untyped_guard.log`（摘掉闸门 / 把地址判定写成恒真）
  各让 `plaintext_api_is_confined_to_loopback_binds` 判红；`logs/s564_mut_n1_guard_removed.log`、
  `logs/s565_mut_n2_doc_reworded.log`（删源码那句 / 只改文档措辞）各让
  `the_doc_quotes_the_same_loopback_guard_the_config_plane_enforces` 判红（0 passed / 289 filtered）。
- 发布面侧同格复核：干净 venv 冒烟用**带出去的 exe**（release 11,942,912 字节）跑 `config validate`，`production` + 明文
  那份配置被「production 环境禁止使用明文 API」拒回（`logs/s556_pass23_clean_venv_smoke.txt` Section E），证明这道闸门
  不只活在 `cargo test` 里。

### Fixed（#243：全仓枚举变体的可见面清点，把 #241 立案的盲区变成判据）

- 改前实测（`logs/s566_enum_surface_run1.txt`）：同一份取数语料（`crates/*/src/**/*.rs`，排除 `tests`，剥
  `#[cfg(test)]` 整项与注释行）按花括号配平能解析 **390** 颗 `pub enum` 变体、**77** 本枚举，而门禁
  `enum_variant_producer_check` 的单行扫描只认到 **248** 颗——**142 颗**（含 `EventKind` 整本 14 颗）从来没有被判据
  看过一眼。`EventKind::Timer`/`MarketBar` 正是靠这个盲区藏了 21 遍（第二十二遍 #239 删的就是这两颗）。
- 新增 `crates/qx-cli/src/tests/enum_variant_surface.rs`（**351 行 / 14,512 字节**，挂 `tests/mod.rs:399`）：对
  「配平看得见、单行看不见」的那一批要求每颗都有生产限定名引用（`Enum::Variant`，或 `impl Enum` 块体内的 `Self::Variant`）；
  允许清单本轮为**空**（实测「盲且零引用」= 0 颗）；三块防空转地板（变体 390 / 枚举 77 / 盲点 142）与「四本整盲的枚举
  各自至少贡献一颗」（`EventKind`、`RuntimeExternalEvent`、`StorageError`、`QxError`）——扫描口径退化时先在这里红，
  而不是让判据对着空集合自证。本轮实测打印行：「枚举=77 变体全量=390 门禁盲点=142 盲且零引用=0」。
- 拆文件让位：新判据挂在 qx-cli 侧不动 qx-runtime 预算，但 `schema_tests.rs` 长到 **529 行**撞上单文件行数预算
  （`logs/s572_gate_pass23.txt` 唯一一条 `[FAIL]`）。按职责把 topology 那组用例拆成
  `crates/qx-runtime/src/runtime_config/topology_tests.rs`（**257 行 / 9,888 字节**，挂 `runtime_config/mod.rs:79`），
  `schema_tests.rs` 收到 **331 行 / 12,763 字节**；`logs/s573_qx_runtime_lib_after_split.txt` 实测 10 条
  `schema_tests::` + 6 条 `topology_tests::` 全绿，`logs/s574_gate_after_split.txt` 回到 **515 项 / 0 条 `[FAIL]`**。
- 变异反向验证（三种各自判红）：`logs/s568_mut_m1_orphan.log`（造一颗盲孤儿变体）、`logs/s569_mut_m2_test_only.log`
  （生产者只留在测试面）、`logs/s570_mut_m3_scan_degraded.log`（退化扫描口径）三处分别让
  `variants_the_gate_cannot_see_still_have_production_producers` FAILED；`logs/s571_surface_restored.log` 复绿 2 passed。
- 本轮**没有改动** `tools/check_architecture.py`：门禁口径归协调者，仓库侧先把盲区纳入判据，立案仍在册。

### Fixed（#245：`environment` 收成闭合名单，拼错的措辞不再能替实时作业选提交臂）

- 改前它是自由字符串，`RuntimeConfig::validate()` 只拒空值（`logs/s584_env_before_fix.log` 里那条报错正是
  「运行时 environment 不能为空」）。同一时刻全仓有 **14 处**按它的措辞分派生产加固（配置面 9 处：
  `strategy_validation.rs` 5 + `topology_validation.rs` 4；CLI 侧 5 处：`live_check.rs` 1、`readiness.rs` 2、
  `strategy_binding.rs` 1、`strategy_contract.rs` 1），而 `crates/qx-cli/src/scheduler.rs:102` 的
  `dry_run: environment.eq_ignore_ascii_case("paper")` 只认一种写法。于是 `"paper "`（尾部一个空格）会静默落进
  **真实提交**臂，`"production "` 会把 14 处加固臂全部关掉。改前全仓取值清点见 `logs/s582_environment_values.txt`
  （`logs/s583_rust_environment_values.txt` 里 `pass`/`fail` 两格是 `live_check.rs` 的播报字面量，与环境写法无关）。
- 修复是**闭合名单 + 同轮锁步**：`crates/qx-runtime/src/runtime_config/schema.rs:12` 的
  `ENVIRONMENT_VOCAB: [&str; 4] = ["paper", "sandbox", "testnet", "production"]` 是唯一来源，
  `topology_validation.rs` 在装载时按大小写不敏感、不含首尾空白逐条比对，名单外一律拒绝并在报错里回吐同一份名单。
  `sandbox`/`testnet` 的隔离性不靠措辞兜底——它们分别由 CCXT 端点 JSON 的 `"sandbox": true` 与
  `venue_id=binance-testnet` 承载。
- 新增 `crates/qx-cli/src/tests/environment_submit_arm_table.rs`（**59 行 / 2,471 字节**，挂 `tests/mod.rs:400`）：
  `SUBMIT_ARM_TABLE` 声明四种写法各自的提交臂（`paper` 模拟，其余三种按名字提交到各自 venue），用例要求它与
  `qx_runtime::ENVIRONMENT_VOCAB` **集合相等**，再逐写法驱动真实 `live_strategy_job` 核对 `dry_run`，最后钉一条
  大小写混排的 `"Paper"` 仍须是模拟。名单每加一个写法，这里必须同轮为它显式决定"模拟还是真实提交"，否则先在这里红。
- 接口文档新增 `### environment 只有四种写法` 小节（`deploy/README.md`），把这四种写法、14 处分派点与提交臂表的关系写给
  部署方；登记表同步更新 `maturity/capabilities.yaml`。
- 变异反向验证：`logs/s604_245_mut_m1_vocab_without_table.log`（扩名单却不改提交臂表）与
  `logs/s606_245_mut_m3_dry_run_flipped.log`（翻转 `paper` 那一格提交臂）分别让
  `every_admitted_environment_spelling_declares_its_submit_arm` 判红（0 passed / 292 filtered，红点分别在
  `environment_submit_arm_table.rs:34` 的集合相等与 `:49` 的逐写法核对）；
  `logs/s605_245_mut_m2_gate_removed.log`（摘掉装载闸门）让同一条锁步的装载侧判据
  `environment_outside_the_closed_vocab_is_rejected_not_silently_branched` 判红（0 passed / 53 filtered）；
  `logs/s602_245_baseline_runtime.log`、`logs/s603_245_baseline_cli.log` 是改后基线两份。

### Verified（终树发布链，全部为本轮实测）

- 格式化与静态检查：`cargo fmt --all --check` 干净（`logs/s601_fmt_after_clippyfix.log`）；
  `cargo clippy --workspace --all-targets -- -D warnings` 与 `--features nats` 均 RC=0
  （`logs/s607_clippy_default_after_dereffix.log`、`logs/s608_clippy_nats_after_dereffix.log`）。
- 门禁：`logs/s597_gate.log` —— 架构不变量自检 **515 项 `[PASS]` / 0 条 `[FAIL]`**，含「门禁自身至少执行 515 条判据」那条自计数。
- 整树测试：`logs/s598_whole_tree_test.log` —— **81 行 `test result:` / 984 passed / 0 failed / 1 ignored**。
- 九步构建（终树整跑，`logs/s609_pass23_nine_step_build_terminal.txt`，166,561 字节）：`BUILD_RC=0`；`[1/9]`
  「架构不变量自检全部通过 ✓（515 项）」；`[6/9]` `OK (skipped=1)`；`[8/9]` RunManifest `digest=62aa341c3cd073ac`、
  control `digest=f9c5193e30739f8f`；`[9/9]` `config_fingerprint=2fc5aa6783c53892b7808ea9e166ed7357eb721abdc8a4df03cda5d84a2fd2b6
  locked=false`；末行「===== 全部完成 (all gates passed) =====」。
- wheel 按终树重打包（#194 口径，`logs/s616_pass23_wheel_repack.txt`）：打包前 218,449 字节 / md5
  `8ee4ee5904bbb3664b203d867269e655` / 17 条目，其中 `.pyd` 仍是上一轮的 `18b293949801df3e7dcd45d5afe1546e`，
  与当轮 dll `5f8739a8621f0013f9f2093afefb56a1` **不相等**；重打包后 218,448 字节 / md5
  `7e4cf8abe2fa9404147021318da5999c`，12 份 `.py` 条目逐字节不变、只换 `.pyd` 与 `RECORD`，`PYD_EQ_DLL=True`
  （`.pyd` 与 `target/release/_qianxing_native.dll` 同为 md5 `e4c4ddc2ad1d7d7959d0d5b2e836b433`、353,280 字节）。
- `/metrics` 发布面（`logs/s555_pass23_release_surface.txt`）：`paper-worker --once` 回 rc=0 后写出 8 行 `.prom`，
  `GET /metrics` 抓到 12 行样本、其中 6 行 `qx_pipeline_*`，且这六行与文件内容**逐字相等**、无 stale 标记，
  指标名集合与第二十二遍**相同**。
- 干净 venv 冒烟（`logs/s556_pass23_clean_venv_smoke.txt`，离线 `uv pip install --no-deps` 那份 218,448 字节的 wheel）：
  `SMOKE_RESULT PASS` —— 四个包导入通过、`native.available()`、意图往返、三条非法字段各自 ValueError、
  安装份 12 个 `.py` 与仓库份逐字节相等、无父进程时 worker 退出 0。


## Unreleased — V13 R2 第二十二遍：日志词汇表里两颗永远造不出来的事实种类，与接口文档正文里一个从未分派过的端点（#239 / #240）（2026-09-30）

取证问句仍是「孤儿逻辑 + 前后端贯通」两格。第二十一遍交下来的 #141 缺口清单里有一条**变体生产者判据看不见结构体式
变体**，而这一遍的探针把它落到了实处；另一格是接口文档——两张端点表自第八遍起按张核对，可**表外散文句子**里承诺的
路由从来没进过任何判据。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.29。

### Fixed（#239：删 `EventKind::Timer` / `EventKind::MarketBar`，把这本日志词汇表变成机器台账）

- 改前实测（`logs/s521_pass22_probe_before.txt`）：门禁 `enum_variant_producer_check` 的变体扫描是**单行**的（一行
  `Name,` 才算变体，体内遇到第一个 `^\s*}` 即停），对 `EventKind` 数出的变体集合是**空的**；同一把尺子在全仓按花括号
  配平能解析 383 颗、单行扫描只认 248 颗，**135 颗不在判据里**。于是 `EventKind::Timer` 与 `EventKind::MarketBar`
  挂着三处下游臂（摘要、API 投影、恢复忽略）却在生产代码里没有任何构造点——`crates/*/tests` 集成用例里的引用数是 0，
  只有 `event.rs` 自己 `#[cfg(test)]` 的内联用例在写（`logs/s522_*.txt` PROBE 2c）。
- 线格式侧同口径复核（`logs/s522_*.txt` PROBE 2d）：`market_bar`、`"timer"`、`EventKind`、`event_kind` 四个 token 在
  schema/JSON 面**一处都没有**——没有任何外部契约承诺过这两颗，删掉不会让声明过的形状变成谎言。
- `crates/qx-core/src/event.rs`（→ **721 行 / 27,555 字节**，LF）删掉这两颗变体与其摘要标签位 **1、2**；词汇表剩 14 颗、
  末位是 `Settle`。三处下游臂（`crates/qx-api/src/lib.rs`、`crates/qx-runtime/src/pipeline.rs`、事件摘要）一起收到 `Settle` 为止。
- 新增 `crates/qx-cli/src/tests/event_kind_variant_ledger.rs`（**299 行 / 13,521 字节**，挂 `tests/mod.rs:399`）四条判据：
  变体集合按花括号配平枚举（`>= 14` 且末位为 `Settle`）、每颗都要有生产构造点（台账合计地板 `>= 29`）、退役的两颗在全仓
  不留残留臂、摘要标签不得重号也不得复用 `["1","2"]`。引用计数一律走**标识符边界**匹配：`QxMarketEventKind::Timer`
  （`crates/qx-strategy/src/c_api.rs:291`）是策略层 C ABI 的活变体，裸 `contains` 会把它读成已删变体的残留。
- 立案 `#241`（门禁侧缺口，本轮**没有改动** `tools/check_architecture.py`）与 `#242`（同把尺子的下一层：探针名单里那
  15 颗零限定引用变体中，`Backoff::Fixed/Exponential` 实测各有 2 处 `Self::` 构造，说明还缺一次 `Self::` 口径才能判定）。

### Fixed（#240：接口文档的幽灵端点，与「表外散文」第一次进判据）

- `deploy/README.md` 第 856 行那句正文承诺 `GET /accounts/{id}/snapshot`，而 `handle_inner` 从未分派过这条路由——
  按张核对的两条判据与门禁 `api_surface_doc_check` 都只吃以 `| ` 开头的表格行，于是它跨过了三轮文档改动没人报错
  （`logs/s523_pass22_probe_doc_routes.txt`：全文 44 条 `` `METHOD /path` `` 字面量，表外 14 条，幽灵 1 条）。
  改成仓库真形状 `GET /account/snapshot?account_id=&venue_id=`（现第 858 行），并在「### 端点表按张核对」补一段点名句
  （第 303 行）。改后同口径复测：45 条字面量 / 表外 15 条 / **幽灵 1→0**（`logs/s531_*.txt` 逐条列出这 15 条的行号与归属）。
- 新增 `crates/qx-cli/src/tests/api_doc_prose_routes.rs`（**99 行 / 5,438 字节**，挂 `tests/mod.rs:298`）：表外每一处反引号
  路由字面量都拿去与 22 对 `(方法, 路由)` 分派集合比对，另加全文禁词 `/accounts/` 与「文档↔判据/文件」双向点名；
  取数地板按实测 15 条钉。分派集合那把尺子（`dispatch_method_routes`）留在原文件共用。
- 接口文档 **102,284 → 103,477 字节 / 1,076 → 1,078 行**，整文件 CRLF、裸 CR 0；`maturity/capabilities.yaml`
  **99,293 → 102,596 字节 / 605 → 610 行**（`event_fact_metadata` 两条证据 + 新 limitation
  `enum_variant_producer_check_cannot_see_struct_style_variants`，`paper_execution` 两条散文路由证据；
  `sandbox_tested: false` 一格没动，本轮不使用任何外部服务或凭据）。
- 一次自造的回归当场修在明处：散文判据起初写在 `api_endpoint_table_routes.rs` 里，门禁 `line_budget_check` 判红
  「554 行未登记」（`logs/s530_*.txt` **514 `[PASS]` / 1 `[FAIL]`**）——按仓库口径**拆文件**而不是把超限行登记进棘轮，
  拆成 475 + 99 两文件后门恢复 **515 项 / 0 条 FAIL**（`logs/s533_*.txt`）。

### Verified（本轮实测，日志 `logs/s521_*.txt`—`logs/s551_*.txt`）

- 变异四发全部真咬，还原后复跑全绿：M1 把 `EventKind::Timer` 放回词汇表 → **四条台账判据同红**
  （`logs/s534_*.txt`：4 failed / 283 filtered，四条各说一件事——标签位 1 被占、删掉的变体回来了、残留臂出现在三个文件、
  生产构造点缺席；`logs/s535_*.txt` 还原后 4 passed / 283 filtered）；M2 把幽灵路由写回正文 → 判据红在幽灵断言
  （`logs/s536_*.txt`、`logs/s537_*.txt`）；M3 删掉文档里的点名句 → 红在双向点名（`logs/s538_*.txt`）；
  M4 删掉整节点名段 → 红在取数地板「只数出 14 条」（`logs/s540_*.txt`，还原后 `logs/s539_*.txt` 1 passed / 286 filtered）。
- 两处**假红**在变异前就被自己的实测挡下并当场改口径（`logs/s528_*.txt` 留档）：残留臂判据第一版用裸 `contains`，
  把策略层的 `QxMarketEventKind::Timer` 报成已删变体的残留；构造点地板照探针抄成 39，而判据按"构造 vs 匹配臂"分开后
  只数到 29。两者都在写下地板之前用真树读数改正，没有把 39 那种口径带进判据。
- 绿侧：`cargo fmt --all -- --check` 空输出退出 0（`logs/s542_*.txt`）；架构门禁 **515 项 `[PASS]` / 0 条 `[FAIL]`**，
  登记表落盘后再跑同数（`logs/s533_*.txt`、`logs/s541_*.txt`，末行仍「架构不变量自检全部通过 ✓（515 项）」）；
  clippy 默认与 `--features nats` 各退出 **0**（`logs/s544_*.txt`、`logs/s545_*.txt`）。
- 整树测试：第一次没带 `QX_PYTHON` 就红在 `e2e_and_python_contract` 两格（WindowsApps 占位桩），报错正文按 #185 的口径
  点名了「程序=python（QX_PYTHON 未设置，回落 PATH python）」并给出 remedy（`logs/s543_*.txt`，`exit=101`）；补上
  `QX_PYTHON` 后 `cargo test --workspace --all-targets --no-fail-fast` **81 条 `test result:` 行 / 976 passed / 0 failed /
  1 ignored / `WS_RC=0`**（`logs/s543b_*.txt`；971 → 976 就是本遍新挂的 5 条判据）。
- 九步构建整跑 `BUILD_RC=0`（`logs/s546_*.txt`）：`[1/9]` 515 项、`[4/9]` **101 条 `test result:` 行 / 976 passed / 0 failed /
  1 ignored**（与整树 passed 逐数相等）、`[6/9]` `Ran 59 tests in 1.294s` / `OK (skipped=1)`、`[8/9]` 更路
  `digest=62aa341c3cd073ac` 与控制命令 `digest=f9c5193e30739f8f` **均未漂移**、`[9/9]`
  `config_fingerprint=2fc5aa6783c53892b7808ea9e166ed7357eb721abdc8a4df03cda5d84a2fd2b6`（`locked=false`）——
  删掉两颗变体没有扰动任何被 bless 的产物。
- 发布物：`#194` 那格窗口**第九次复现**（`logs/s547_*`，`VERIFY_RESULT FAIL`）——wheel 内嵌 `.pyd`
  `92b5f21d5a4d4f3601037a00a26ab445` ≠ 终树 dll `132f4f7865d31767979eedebbf34ab11`；按终树重打包
  （`logs/s548_*.txt`，`REPACK_RC=0`，新 wheel 218,449 字节、sha256 `552f22dad4dcf5b2…`）后复测
  （`logs/s549_*`）：**218,449** 字节 / md5 `8ee4ee5904bbb3664b203d867269e655`、17 条目、内嵌 `.pyd`
  `18b293949801df3e7dcd45d5afe1546e` **≡ 当下 dll**、12 份 `.py` 与仓库逐字节一致（**`VERIFY_RESULT PASS`**）。
  exe 侧按 `#179` 走载荷口径：release **11,941,888** / debug **21,884,928** 字节（各比上一遍小 10,240 / 18,944，
  本轮删的是代码不是判据），五个字面量在两份产物里计数仍是 **1**。
- 发布面实测（`logs/s550_*.txt`）：装机形态的 release exe 走 `init` → `paper-worker … --once` → `serve`，落盘 `.prom` 八行
  （`qx_worker_up` 0、六个计数 `1/1/0/0/1/0`），`GET /metrics` 得 **12 行样本 / 其中 6 行 `qx_pipeline_*`** 且与落盘那份
  **逐字相同**（`RELEASE_SURFACE_PIPELINE_METRICS=PRESENT`）——与第二十一遍那一格逐字相等，删变体没动产品面。
- 干净 venv 冒烟 `SMOKE_RESULT PASS`（`logs/s551_*.txt`）：`uv venv`（CPython 3.12.13）+ `uv pip install --no-deps --offline`
  装那份 218,449 字节的 wheel → 四包导入、`native.available()`、意图线格式往返一致、三格非法值各抛 `ValueError`、
  12 份已安装 `.py` 与仓库逐字节、已安装 worker 在父进程消失后退出 0；`tzdata` 仍按 `#212` 手工复制；
  release/debug 两个 exe 的 `help` 各退出 0、行数 155。

## Unreleased — V13 R2 第二十一遍：14 处「打开账户 pipeline」的站点里只有 4 处该并进 worker 计数——这份名单从手工抄写改成机器清点（#178 出口台账）（2026-09-30）

取证问句是第二十遍「在册不改」里的第 ②③ 格：`absorb` 的调用点位置只有那条真链路夹具在钉，而「哪些 pipeline 不该
出现在这族计数里」还只是 `pipeline_metrics_report.rs` 模块文档里手工抄写的一段。详见
`docs/自研量化框架审计与重构方案-V13.md` §9.28。

### Fixed（#178 台账：豁免名单与站点清点钉成同进同退的判据）

- 新增 `crates/qx-cli/src/tests/pipeline_metrics_open_sites.rs`（**180 行 / 7,632 字节**，挂在 `tests/mod.rs:409`），
  两条判据：`account_pipeline_open_sites_are_either_counted_or_exempted_by_name` 按函数核对「打开站点数 == `absorb` 次数」
  ——少一次是漏计、多一次是双计，而这两种错法都不会让 `.prom` 正文报错；同时要求每个豁免函数名在出口模块文档里点名。
  `open_site_ledger_actually_sees_the_production_sources` 是反向自我证明：扫到的站点总数必须等于两份名单之和，
  并账总数必须等于 `paper_worker.rs` 这个文件里的 `.absorb(` 数。
- 站点认**两个**入口：`open_account_pipeline(` 与它下面那层 `open_runtime_pipeline(`。只数前者会留一条绕过路线——
  `build_configured_api_service` 调的正是后层。改前实测与一份独立 Python 探针同数：扫 196 份生产源码、
  10 个函数、**14 处站点**、**4 次并账**。
- 名单：并进 worker 出口的 2 个函数（`run_paper_spread_recovery_worker` 1 处、`run_paper_execution_worker` 3 处）；
  豁免 8 项（1 处统一入口向下一层的内部委托、5 处按请求或装配期打开的只读站点、`run_paper_pipeline_once` 与
  `run_paper_submit_order` 这两个一次性验收入口）。`paper-submit-order` 的写入照旧进 EventLog，但**不在这族计数里**——
  它以 `paper-execution` 的名义领取租约，把计数并进 worker 只会让那份「按进程累计」的量更假。
- `crates/qx-cli/src/pipeline_metrics_report.rs` 的模块文档随之重写排除段（100 → **107** 行、4,830 → **5,549** 字节），
  两份名单与文档由判据双向钉住，任一边退回手工状态就红。
- 接口文档与登记表同步：`deploy/README.md`「指标出口」一节加一条要点（101,267 → **102,284** 字节 / 1,076 行），
  `maturity/capabilities.yaml` 那条 limitation 补句（→ **99,293** 字节 / 605 行），两份都保持纯 CRLF、裸 CR 0。

### Verified（本轮实测，日志 `logs/s505_*.txt`—`logs/s518_*.txt`）

- 变异五发全部真咬（`logs/s506_*.txt`，`MUTATION_VERDICT=PASS`、`MUTATION_RESIDUE=none`、还原后两条判据复跑全绿）：
  删一次 `absorb`、加一处未登记的站点、删一行豁免、从出口文档删一个名字、在豁免函数里加一次 `absorb`。
  第一版矩阵 `logs/s505_*.txt` **整轮作废**：那次只删了 `[(&str, usize, &str); 8]` 里的一行而没改长度字面量，
  变异体根本不编译（`running 0`、没有 `test result:` 行）——按「没有 `test result` 行的红是坏树不是判据」重跑整矩阵。
- 绿侧：`cargo fmt --all -- --check` 退出 0；架构门禁 **515 项 `[PASS]` / `GATE_RC=0`**（改文档后复跑同数）；
  整树 `cargo test --workspace --all-targets` **81 条 `test result:` 行 / 971 passed / 0 failed / 1 ignored / `WS_RC=0`**
  （969 → 971 就是本遍新挂的两条）；clippy 默认 profile 与 `--features nats` 各退出 0。
- 九步构建整跑 `BUILD_RC=0`：`[1/9]` 515 项、`[4/9]` **101 条 `test result:` 行 / 971 passed / 0 failed / 1 ignored**
  （passed 两侧逐数相等）、`[6/9]` `Ran 59 tests in 1.570s` / `OK (skipped=1)`、`[8/9]` 更路
  `digest=62aa341c3cd073ac` 与控制命令 `digest=f9c5193e30739f8f` 均未漂移、`[9/9]`
  `config_fingerprint=2fc5aa6783c53892b7808ea9e166ed7357eb721abdc8a4df03cda5d84a2fd2b6`（`locked=false`）。
- 发布物：`#194` 那格窗口**第八次复现**——九步构建后第一次复核量到 wheel 内嵌 `.pyd`
  `d21992ec18980a6fac78a55bd3e6b426` ≠ 终树 dll `5a1d08f5d1f02fe70127246c7febc851`（`VERIFY_RESULT FAIL`）；
  按终树重打包（`REPACK_RC=0`）后 **218,450** 字节、md5 `6dcc3a162925671dfeda1097a181dcdc`、17 条目、
  内嵌 `.pyd` `92b5f21d5a4d4f3601037a00a26ab445` ≡ 当下 dll、12 份 `.py` 与仓库逐字节一致（**`VERIFY_RESULT PASS`**）。
  本轮**没有新增运行面字面量**：release 11,952,128 / debug 21,903,872 两份 exe 里那五个字面量计数都是 1，与上一遍逐字相等——
  这正是「改判据与文档不动产物」应有的对照（单向证据，`#179`）。
- 发布面实测（`logs/s517_*.txt`）：装机形态的 release exe 走 `init` → `paper-worker … --once` → `serve`，
  落盘那份 `.prom` 是八行（`qx_worker_up` 0、六个计数 1/1/0/0/1/0），`GET /metrics` 得 **12 行样本、其中 6 行是
  `qx_pipeline_*`**，且这六行与 worker 落盘那份**逐字相同**（`RELEASE_SURFACE_PIPELINE_METRICS=PRESENT`）。
- 干净 venv 冒烟 `SMOKE_RC=0` / `SMOKE_RESULT PASS`（`logs/s518_*.txt`）：`uv venv` + `uv pip install --no-deps --offline`
  装那份 218,450 字节的 wheel → 四包导入、`native.available()`、意图线格式往返、三格非法值各抛 `ValueError`、
  12 份 `.py` 与仓库逐字节、已安装 worker 在父进程消失后退出 0；两个 exe 的 `help` 各退出 0、行数 155。

## Unreleased — V13 R2 第二十遍：六个 `qx_pipeline_*` 计数每笔都在算却没人读——量纲定成「按 worker 进程累计」，一路写到装机的 `/metrics`（#178）（2026-09-29）

取证问句是第八遍立案的 `#178`：**`LiveEventPipeline::metrics()` 那六个数在生产里有谁读**。答案是没有人读——
改前实测 `logs/s481_*.txt`：真跑一轮 paper 链之后 `data\worker-metrics\paper-execution.prom` 根本不存在（`os error 3`）。
详见 `docs/自研量化框架审计与重构方案-V13.md` §9.27。

### Fixed（#178：先把量纲三选一钉死，再给计数一个只在重启时归零的出口）

- 三个候选口径（按 pipeline 对象 / 按 tick / 按 **worker 进程**）里选按进程。为什么不按对象：对象级累计直接印成 `_total`
  会给抓取端一条每次请求归零的「累计计数」，比不印更糟——当初把"按请求各开一个 pipeline"的 API 读路径挡在出口外就是这条理由。
  于是**唯一的归零边界是进程重启**，这一点在接口文档与登记表里都写明。
- 新增 `crates/qx-cli/src/pipeline_metrics_report.rs`（**100 行** / 4,830 字节，LF）：`PipelineMetricsReporter::{new, absorb, publish}`，
  正文八行 LF —— `qx_worker_up` + 心跳 + 六个 `qx_pipeline_*_total`，**每行都带 `worker`/`account` 两条标签**并按 Prometheus 口径转义（`"` → `\"`）。
  写失败只在 stderr 打一行「pipeline 指标写入失败，业务处理继续」：指标 I/O 不许变成交易链路的失败面。
- `crates/qx-cli/src/venue_runtime/paper_worker.rs`（427 行）接线：两条 worker 循环各持一份按进程累计的量（`:37`、`:135`）；
  行情按命令、恢复按 tick 各开一个 pipeline 对象，所以 `absorb` 是**按对象各并一次**（4 处：`:68`、`:139`、`:241`、`:288`）；
  `publish(true, now)` 紧贴在 `context.heartbeat(now)?` 之前（`:78`、`:290`），每条循环退出各有一次 `publish(false, runtime_timestamp_ms())`
  （`:85`、`:302`）——发布与心跳同节律，沿用 NATS 那条 worker 已有的口径。
- 聚合侧不改写值：`read_worker_metrics` 把 worker 落盘那份**原样追加**在 API 自身四条样本之后，只在心跳超过
  `messaging.worker_stale_after_ms` 时改写 `qx_worker_up` 为 0 并补一条 `qx_worker_metrics_stale{worker="…"}`；**失鲜不抹掉计数**——
  告警正靠"up=0 而计数还在"把崩溃与空闲分成两条通道。
- 删掉 `crates/qx-runtime/src/pipeline.rs` 里那份**无调用者的第二实现** `PipelineMetricsSnapshot::to_prometheus`：六个计数的渲染收口到
  worker 侧一处；`PipelineMetricsSnapshot` 仍是 `metrics()` 的返回类型。
- 三处说法**同进同退**地翻转：`crates/qx-runtime/src/lib.rs`（140 行）公开面指认 `qx-cli/src/pipeline_metrics_report.rs` 为真实读者；
  `maturity/capabilities.yaml` 的旧登记换成 `pipeline_metrics_publish_per_worker_process_only`；`deploy/README.md` 「指标出口」给出带标签的真形态。

### Tests（7 条新判据 + 1 条双向判据改名 + 3 条解 gate，变异 10 发全咬）

- `crates/qx-cli/src/tests/pipeline_metrics_surface.rs`（新，**395 行** / 7 条，挂载 `crates/qx-cli/src/tests/mod.rs:409`）逐条对应那条量纲。
  计数按夹具真实笔数**取相等**（`ingest_attempts 4 / ingested 4 / deduplicated 0 / refreshes 4 / transient_retries 0 / failures 0`，
  实测正文 `logs/s484_*.txt`）而不是取"非零"：**漏并一个对象是少计，把一个对象并两次是双计**，两种错法都不会让正文里任何一行报错。
- 判据不写成 `contains`（沿用 #177 那一课）：`parse_exposition` 按抓取端口径逐行拆 `<名>{<标签>} <整数>`，并先断言正文里没有字面 `\n`。
- `crates/qx-cli/src/tests/zero_reader_fields.rs`（161 行）改名后的双向判据 `pipeline_metrics_publication_and_docs_move_together`：
  生产读者 ⟺ 旧登记缺席 ⟺ lib.rs 指认读者 ⟺ README 给出带标签形态，四者必须一起动。
- `crates/qx-cli/src/tests/worker_observability.rs`（497 行）三条从 `#[cfg(feature = "nats")]` 摘下——未 gate 的观测口径也该在默认 profile 跑得到。
- 变异 10 发 M1–M10 **全部真红**、还原后 9 条判据**全绿**，每一行都标着 `实跑=1`（`logs/s486_*.txt`）：少计 / 双计 / 漏一拍 / 退出不收 0
  打在 paper 与节律判据上；`\n` 转义与标签改名同时打中行为判据和 `prometheus_exposition_is_line_separated_at_both_ends`；
  标签不转义打中转义判据；三处说法各改一处都由那条双向判据红。
- **点名一处流程教训**：上一版的变异命令写成 `cargo test NAME --exact`，`--exact` 落在 cargo 那一侧，测试二进制收到的其实是"没有 filter"，
  实测 `running 0 tests` 而 rc=0 —— **红、绿两侧都是空的**（探针 `logs/s488_*.txt`）。现在统一 `-- --exact`，并解析 `running N tests` 后
  要求 `ran == 1`，否则整轮变异作废。
- 绿侧（当轮）：面判据连同双向判据 **8 passed / 0 failed**（`logs/s483_*.txt`）；整树 `cargo test --workspace --all-targets`
  **81 条 `test result:` 行 / 969 passed / 0 failed / 1 ignored / RC=0**（`logs/s490_*.txt`）；架构门禁 **515 项 PASS**（`logs/s489_*.txt`）；
  默认与 `--features nats` 两侧 clippy 各自 `Finished` 且日志里没有 warning 落盘（`logs/s491_*.txt`、`logs/s492_*.txt`）。

### Docs

- `deploy/README.md`（1062→**1075** 行、100,037→**101,267** 字节，整文件 CRLF、裸 CR 0）「指标出口」把六个计数写成带 `worker`/`account`
  标签的八行样本形态，并说清两个边界：`--once` 之后 up=0 而计数仍在；API 请求内只读 pipeline 与 live venue worker 两半**仍未接线**。
- `maturity/capabilities.yaml`（605 行、99,008→**98,777** 字节，整文件 CRLF、裸 CR 0）的登记项指回 §9.15 立案、§9.27 接线。
- `docs/自研量化框架审计与重构方案-V13.md` 新增 §9.27（第 2361 行起 / 152 行：量纲决策、出口、三处说法翻转、7 条判据、10 发变异表、
  `--exact` 两处失真、九步构建与发布物、「文档落盘后的最后一次总账」与那份合并实测），另在 §9.15 那一格加前指（第 1138 行），
  整文件 2359→**2513** 行、286,193→**303,695** 字节（整文件 LF、裸 CR 0）。§9.27 末尾明写一句：`docs/` 与 `CHANGELOG.md`
  都不进门禁与用例取数（`crates/qx-cli/src/tests/artifact_identity_doc.rs:9`），所以这两个数字只取到"写下那一刻"，可复核口径是日志与 `git diff`。
- 记一次自己造成的回归并当场撤销：追加 §9.27 用 `pathlib.write_text`，在 Windows 上把整篇 V13 从 LF 换成 CRLF（2468 行全中），
  发现后按字节换回 LF 并复测 `CRLF 0`。口径与 §9.20 那条 CRLF 纪律同一处：**往仓库写文件一律 `write_bytes` 或显式 `newline`**。

### Docs & Release（收口与发布物）

- 九步构建按终树整跑 `BUILD_RC=0`（`logs/s493_*.txt`）：`[1/9]` 门禁 **515 项 PASS**、`[3/9]` release `Finished` 于 1m 56s、
  `[4/9]` **101 条 `test result:` 行 / 969 passed / 0 failed / 1 ignored**、`[6/9]` `Ran 59 tests in 1.327s` + `OK (skipped=1)`、
  `[8/9]` 更路 `digest=62aa341c3cd073ac` 与控制命令 `digest=f9c5193e30739f8f`、`[9/9]` `config_fingerprint=2fc5aa6783c53892…`（`locked=false`）。
- **#194 第七次复现**：收口后第一次载荷复核就是 `VERIFY_RESULT FAIL` —— wheel 里的 `.pyd`（md5 `4873023878119ed0…`）不等于当轮 dll
  （`b3f23bd5af28913d…`，`logs/s494_*.txt`）；按终树重打包（218,449→**218,450** 字节，`logs/s495_*.txt`）后 `.pyd ≡ dll =
  d21992ec18980a6fac78a55bd3e6b426`、12 份 `.py` 与仓库逐字节相同（`logs/s496_*.txt` `VERIFY_RESULT PASS`）。
  这条核对**只有放在收口之后做一次才算数**，本轮又一次证明"构建步骤里跑过"不等于"终树一致"。
- **结立案那句**：发布面用**装机的 release exe** 实测——`init --profile paper` → `paper-worker … --once` 落盘那份 `.prom` 8 行；
  再起 `serve` 取 `/metrics`，12 行样本里 `qx_pipeline_*` 恰 6 行、与落盘那份**逐字一致**（`logs/s497_*.txt`，
  `RELEASE_SURFACE_PIPELINE_METRICS=PRESENT`）。第八遍在同一个位置量到的是"三条样本里没有 `qx_pipeline_*`"。
- 干净 venv 冒烟（`logs/s498_*.txt`，`SMOKE_RC=0`）：`uv venv` + `uv pip install --no-deps --offline` 装那份 218,450 字节的 wheel →
  四包导入、`native.available()`、意图线格式往返一致、三格非法值各抛 `ValueError`、12 份 `.py` 与仓库逐字节、已安装 worker 在父进程消失后退出 0；
  release/debug 两个 exe 的 `help` 各退出 0、行数 155，本遍五个字面量（两个计数名、`qx_worker_metrics_stale`、`worker-metrics`、
  「pipeline 指标写入失败」）计数均 ≥1（单向证据，#179）。
- **终树复跑一次总账**（§9.20：文档落盘后只复跑这一次）：`cargo fmt --all -- --check` 退出 0、差异 0 行；架构门禁
  **515 项 `[PASS]` / `GATE_RC=0`**（`logs/s499_*.txt`）；`qx-cli` 单 binary **280 passed / 0 failed / `BIN_TEST_RC=0`**（`logs/s500_*.txt`）。
  这一条**第一次是红的**：两条 Python 契约用例报「程序=python（QX_PYTHON 未设置，回落 PATH python）…若该程序是 WindowsApps 的
  python 占位桩」，278 passed / 2 failed —— `#21`/`#185` 那条诚实报错点名了缺的旋钮，而不是把断管道读成"worker 没输出"；
  带上 `QX_PYTHON` 复跑后 278→280。九条判据在终树复跑 7/1/1/1 全绿（`logs/s501_*.txt`），其间 `--exact` 配**模块名**又复现一次
  `running 0 tests` 而 rc=0 的失真（`logs/s488_*.txt` 那次是旗标位置，这次是名字粒度）—— **`--exact` 只配完整用例名，
  模块级复跑用前缀匹配，且每段都要核对 `running N tests` 的 N**。
  **上面那三条取于文档落盘前**，本节与 §9.27 写完后按同一口径再跑一遍才是终树读数：`qx-cli` 单 binary
  **280 passed / 0 failed**（`logs/s502_*.txt`）、门禁 **515 项 `[PASS]` / `GATE_RC=0`**（`logs/s503_*.txt`），两侧 passed 与判据数逐字相等。
- **合流这一步现在起不来，如实登记**：`git fetch` 后 `origin/main` = `f5b9a09` 比本地 `HEAD` = `ad2908b` **领先 4 颗、落后 0**
  （上游那颗 `e44bfd8` 已把 `ad2908b` 收进去），改动面 **211 份 / +44,020 −6,218**，与本地 **98 份未提交路径重叠 48 份**——
  含 `paper_worker.rs`、`pipeline.rs`、`qx-runtime/src/lib.rs`、`CHANGELOG.md`、`README.md` 以及整份
  `tools/check_architecture.py`（上游那一侧就多 5,107 行，与本地 515 项判据是两套互斥规则集）。工作树脏到这个形状时
  `git merge` 无法开始，而本仓的规矩是**索引由用户整理、agent 不跑 `git add`**，所以本轮只把安全网拉好：
  `C:/temp/qx_pass21_dirty_mirror/`（99 份 / 2,646,840 字节 / 逐字节抽检一致，`_manifest.json` 记全量清单），
  并把「合流时两套门禁谁胜出」列为下一遍第一问（V13 §9.27 末尾）。
- 在册不改（如实留下两格）：① API 请求内只读 pipeline 与 live venue worker 不进这个出口——它们的对象生命周期与「按 worker 进程累计」
  不是同一个量纲，硬并会给抓取端一条归零边界说不清的 `_total`；② 每条 `absorb` 的**位置**只由那条真跑 paper 链的夹具钉住，
  节律判据取的是源码形状，不核对调用点落在循环的哪一段（V13 §9.27 末尾登记）。

## Unreleased — V13 R2 第十九遍：`--features nats` 那条链路在"连接成功"那一步当场崩溃，三条等待的界全在依赖默认值上（#220）（2026-09-29）

取证问句是第十八遍「在册不改」里点名的 `#224`：**NATS 侧的等待预算有没有一格是本仓写下的**。
量完得到三件，第一件不在计划里，而且比另外两件都严重。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.26。

### Fixed（#220：reactor 外的 `jetstream::new` 崩溃，与三条等待接上本仓写下的预算）

- 立案时写的「broker 不可达时 `connect` 永不返回」**被实测证伪**：`retry_on_initial_connect=false` 时只 `try_connect()` 一次，
  而那一次被依赖的握手默认包着，端口被拒时 publisher 2.071s / consumer 2.061s 返回 `Err`（`logs/s442_*.txt`，os error 10061）。
  **真正的断链是崩溃不是卡死**：`jetstream::new(client)` 写在 `block_on_runtime` **返回之后**，而它内部
  `ContextBuilder::build → spawn_acker → tokio::spawn`（async-nats 0.50 `jetstream/context.rs:129`）要求当前线程已进入 Tokio 反应堆；
  qx-cli 五个 NATS 入口与 `#[test]` 线程都没有 reactor，于是握手刚成功就 panic `there is no reactor running`，
  20s 看门狗内既不返回 Ok 也不返回 Err（`logs/s443_*.txt`）。现在 `Context` 在已进入的 runtime 内建好再交出。
- 崩溃修掉之后才有第三条可读：**有界是真的有界，但界全在依赖默认值上，本仓一个字没写**。哑 server（握手成功、请求有去无回）下
  `publish + ack` 5.017s 返回 Err（依赖的 ack 默认 5s），`get_stream` / `get_consumer` **10.029s** 才返回（依赖的请求默认 10s）
  （`logs/s444_*.txt`）。这里还纠一处立案时的口误：不是"三条等待全退回 5s"，而是**请求 10s 与确认 5s 两格**。
  `$JS.API.*` 的界在**连接层**（`Connection::request_timeout`），`Context::set_timeout` 只管发布 ack —— 只接后者会让一半路径没界。
- 新增 `crates/qx-storage/src/wait_budget.rs`（**80 行**）：`NatsWaitBudget { connect_timeout_ms, request_timeout_ms, pull_expires_ms }`，
  默认 5000/5000/1000，三条下界统一 **100ms**（沿用 #214 那条教训：预算被打成毫秒级时 worker 循环变成对 broker 的热循环），
  上界 60000/300000/30000。`request_timeout_ms=5000` **收紧不放宽**：一次替换原来 10s 请求默认与 5s 确认默认，把两格并成一格。
  发布者与消费者的 `connect` 各接**两个**旋钮（`ConnectOptions::connection_timeout` + `request_timeout` + `Context::set_timeout`），
  pull 走 `.expires()`；`crates/qx-storage/src/lib.rs`（2406→**2409** 行）再导出。
- 配置面补三键（`crates/qx-runtime/src/runtime_config/schema.rs`）+ 打开配置时按同一区间校验（`topology_validation.rs`，
  报错形如 `messaging.pull_expires_ms 必须在 100..=30000 内`），默认值由 `NatsWaitBudget::default()` 提供、不另写第二份常量；
  `deploy/qianxing.runtime.messaging.example.json` 与 `deploy/qianxing.runtime.consumer.example.json` 两份模板各写出三行；
  qx-cli 五个 NATS 调用点接线（`crates/qx-cli/src/event_pipeline.rs`，748→**753** 行）。
  本轮曾把两个一次性 relay 入口合并成共用泵函数，被"按路径取数"的门禁判红（`.pump_once(lease_clock(` 必须 3 处）后撤销——
  门禁脚本本轮不改（改动权在协调者），改的是自己的形状。

### Tests（七条无 broker 判据 + 一条常驻配置判据，变异四发全咬）

- `crates/qx-storage/tests/nats_wait_budget.rs`（**273 行** / 7 条，`#![cfg(feature = "nats")]`）：只用本机 TCP 桩
  （被拒端口 + 只完成握手、对请求有去无回的哑 server），覆盖三条预算各自有界、默认值与被替换的依赖默认逐项相等、
  越界在发起任何 IO 之前被拒、两个 `connect` 在无 reactor 的线程上都能返回。**要点名**：这七条要 `--features nats` 才编得进去，
  默认 profile 的 `cargo test --workspace --all-targets` 里该 target 报 `running 0 tests`（`logs/s468_*.txt`），
  即 §4「在册 ≠ 实跑」在本轮新增面上同样成立。
- `crates/qx-runtime/src/runtime_config/schema_tests.rs` 常驻一条
  `nats_wait_budgets_are_readable_writable_and_bounded`（三键可读可写、越界被拒）。
- 变异四发全部真咬（`logs/s462_*.txt`—`logs/s465_*.txt`）：**M1** 把 `jetstream::new` 搬回 runtime 外 → 2 failed / 5 passed，
  红因正是依赖自己的 panic `there is no reactor running`（不是编译错）；**M2** 摘掉发布侧 `set_timeout` →
  `publish_gives_up_within_the_request_budget` 从 5s 档退到依赖默认，6.03s 红；**M3** 摘掉两处 `ConnectOptions::request_timeout` →
  `consumer_stream_lookup_gives_up_within_the_request_budget` 11.02s 红；**M4** 摘掉 topology 的区间校验 → 那条常驻配置判据红。
  每发之后被改文件与 `%TEMP%` 镜像按字节还原（`restored=True`）。
- 绿侧（当轮）：`--features nats` 七条 **7 passed / 0 failed / 1.33s**（`logs/s467_*.txt`）；整树
  `QX_PYTHON=… cargo test --workspace --all-targets` **81 条 `test result:` 行 / 959 passed / 0 failed / 1 ignored**
  （`logs/s468_*.txt`，958→959 只多那条新挂的常驻配置判据）；架构门禁 **515 项 PASS**（`logs/s466_*.txt`、`logs/s471_*.txt`）；
  `qx-storage --features nats` 与 `qx-runtime`/`qx-cli --features nats` 的 clippy 带 `-D warnings` 均退出 0（`logs/s469_*.txt`、`logs/s470_*.txt`）。

### Docs

- `deploy/README.md`（1031→**1062** 行、97,073→**100,037** 字节，整文件 CRLF、裸 CR 0）新增「三条 NATS 等待预算：连接、请求/确认、有界拉取」一节，
  写出三键与区间、为什么请求侧要接两个旋钮、改前那次 panic，以及"这七条要 `--features nats`"那一格。
- `maturity/capabilities.yaml`（599→**605** 行、96,417→**99,008** 字节，整文件 CRLF、裸 CR 0）的 `nats` 块补 5 条证据行
  与 1 条 limitation（`nats_wait_budget_bounds_measured_on_local_tcp_stubs_only`）：三条预算的界只在本机桩上量过，
  真 broker 下的握手重试、慢 ack 与长拉取仍无记录，`sandbox_tested: false` 保持不动（本轮不使用任何外部服务或凭据）。

### Docs & Release（收口与发布物）

- `docs/自研量化框架审计与重构方案-V13.md` 新增 §9.26（上面那几段的完整版：三格等待的改前/改后对照表、变异四发的红因原文、
  以及「按路径取数的门禁这一处挡对了」那段），上一遍「在册不改」点名的 `#224` 由此结清。
- 九步构建按终树整跑 `BUILD_EXIT=0`（`logs/s472_pass19_nine_step_build.txt`）：`[1/9]` 门禁 **515 项 PASS**、`[3/9]` release
  `Finished` 于 2m 33s、`[4/9]` **101 条 `test result:` 行 / 959 passed / 0 failed / 1 ignored**、`[6/9]` `Ran 59 tests in 2.199s`
  + `OK (skipped=1)`、`[8/9]` 更路 `digest=62aa341c3cd073ac` 与控制命令 `digest=f9c5193e30739f8f`、`[9/9]`
  `config_fingerprint=2fc5aa6783c53892…`（`locked=false`）。**两处取数口径要钉住**：`[4/9]` 的 101 行比整树 `--all-targets`
  （`logs/s468_…`，81 行 / 959 passed）多 20 行，全是 0 用例的 Doc-tests 目标（`[4/9]` 里 `0 passed` 行 26 − 整树 6 = 20），
  passed 两侧逐数相等；`[1/9]` 里以 `[PASS]` 开头的行是 **516** 条，多出的一条是同一次运行的「runtime 引用文件校验通过」，
  **按 `[PASS]` 行数当判据总数会读高 1**，判据总数只认末行那句「（515 项）」。
- 发布物按 `#194` 那格窗口收口（**第六次复现**）：先拍 before 快照（`logs/s473_…`，收口前 wheel 218,448 字节、内嵌 `.pyd`
  `99443f77ad4b1f55…` ≠ 当下 dll `5adf9be00f39ec7c…`）→ 重打包（`logs/s474_…`，脚本自带 `cargo build -p qx-python --release`）
  → 复核（`logs/s476_…`）：新 wheel **218,449** 字节、md5 `1da57e8608bdcb60…`、17 条目、内嵌 `.pyd` `4873023878119ed0…`
  ≡ 终树 dll、12 份 `.py` 与仓库逐字节一致、末行 `VERIFY_RESULT PASS`。**`s473` 那句 `FAIL` 要拆成两半读**：`.pyd ≠ dll` 是真的
  （旧 wheel 落后于 dll），同一行里那 12 个 `MISSING_IN_REPO` 是复核脚本把仓库基准路径取错了；第一次修（`logs/s475_…`）改到一半
  就跑了脚本，`NameError: name 'REPO' is not defined`、退出码非 0，`s476` 才是修好的复跑——**崩了的日志不能留作末行**，
  且复核脚本末行只写 ASCII（§9.25 那条流程纪律的第二次触发）。
- exe 侧仍按 §9.19 / `#179` 走载荷口径：release `qx-cli.exe` **12,038,656** 字节里
  `connect_timeout_ms` / `request_timeout_ms` / `pull_expires_ms` / 「必须在 」计数 **2/1/1/21**，debug（21,894,144 字节）同四项 2/2/2/22。
  而 `NATS 连接失败` 与 `there is no reactor running` 两格在两个 exe 里都是 **0**——这一次**不必**套 `#179`：qx-cli 的
  `default = ["sqlite"]`、`nats` 是 opt-in feature（`crates/qx-cli/Cargo.toml:7-16`），安装包 exe 根本没链接这段代码。
  所以本轮的真实交付形状是**三键配置名与区间报错在安装包里，NATS 客户端代码本身不在**，读者不能读成"装上包就能跑 relay/consumer"。
- 干净 venv 冒烟（`logs/s480_pass19_clean_venv_smoke.txt`，`SMOKE_RC=0`）：`uv venv` + `uv pip install --no-deps dist/*.whl`，
  全部从「已安装」那一侧读——四包导入、`native.available()`、意图线格式往返归一成 `buy cross hedge 2` 且读回一致、
  三条非法取值各按契约抛 `ValueError`（`margin_mode must be` / `position_mode must be` / `leverage must be positive`）、
  `Asia/Shanghai` 可用、已安装 12 份 `.py` 与仓库逐字节相同、已安装 worker 在父进程消失时退出码 0、`help` exit 0 且 **155** 行。
  两格如实记下：① `--no-deps` 不兑现 `pyproject.toml:13` 那条 `tzdata; sys_platform == "win32"`，tzdata 是手工复制进临时 venv 的，
  `#212`（联网装齐依赖）那一格**仍留在册**；② 冒烟时 debug exe 已是 21,902,848 字节（`s476` 那次 21,894,144），四项计数一字未变——
  `#159` 那一族的字节版，**exe 大小从来不是判据**。
- 文档落盘后按 §9.20 的纪律再收一次口：架构门禁 **515 项 `[PASS]` / `[FAIL]` 0 条 / `GATE_EXIT=0`**
  （`logs/s477_pass19_gate_after_926.txt`）→ `cargo build -p qx-cli` `BUILD_EXIT=0`（`logs/s478_…`，`Finished` 于 4.11s）→
  带 `QX_PYTHON` 的 `cargo test -p qx-cli --bins` **270 passed / 0 failed / `TEST_EXIT=0`**（`logs/s479_…`，5.34s）。
  "改这两篇不必重跑判据"这句本轮先去 `tools/check_architecture.py` 里坐实：它按名读的交付件是 `deploy/README.md`、
  `maturity/capabilities.yaml`、`maturity/line_budgets.yaml` 与根 `README.md`，`docs/` 与 `CHANGELOG.md` 一个字符串都没读。
  270 与上一遍同数，因为本轮新增的判据都不在 qx-cli：7 条在 `qx-storage` 且躲在 `--features nats` 后、1 条常驻在 `qx-runtime`。


## Unreleased — V13 R2 第十八遍：换个后端就把账户读成空账本——打开入口上的 fail-closed 闸门，以及"分段不是那笔价钱的出路"（#225 量具 / #226 修复）（2026-09-29）

取证问句是上一遍「在册不改」里那句**"稳态每 tick 的整本读+整本重写有没有便宜的出路"**（#225）。这一遍先把那条链拆开量，
量到两件：① 那条"看似出路"（打开分段后端）在价钱上是**倒贴的**，所以 #225 剩下的那半只能靠"有保留/压缩/归档的触发者"，
本轮不下手；② 探后端形状时撞出一条比价钱危险得多的行为缺陷（#226）——**改一个可选字段就能让同一账户在同一目录里并存两本账**。
详见 `docs/自研量化框架审计与重构方案-V13.md` §9.25。

### Fixed（#226：换 EventLog 后端不再静默丢历史）

- 改前实测（`logs/s406_pass18_backend_switch_probe_before.txt`）：用单文件后端写下 5 条事实后，把
  `storage.event_log_segment_events` 从空改成数值再打开——运行时**读到 0 条事实、现金 0**，随后追加的第一条事实拿到 `seq 0`，
  根目录变成 `["binance-main.json", "binance-main.manifest.json", "outbox", "segments"]`，同一账户从此并存两本历史
  （单文件 5 条 / 分段 1 条）；反向同形。三条打开入口（`open` / `open_configured` / `open_read_only`）都把
  「自己那套后端的文件不在」当成首次启动（`unwrap_or_default()`），而两套形状在同一个 `storage.root` 下互不相交。
  这与 `open` 自述的「不会用空状态掩盖生产数据问题」正好相反。
- 新增 `crates/qx-runtime/src/pipeline/backend_switch.rs`（**147 行**）：`LiveEventPipeline::open_with_store` 构造完 `store`
  之后过一道闸门（`pipeline.rs:563`），**三条打开入口共用一处**，所以读面同样拒绝——换错后端时印一份"现金 0"的账户快照
  比当场报错危险得多。只查**当前配置没选中的那套文件后端**有没有**非空**历史（`filter(|log| !log.is_empty())` 保住首次启动），
  报错文案点名被留下的那本历史（后端名、路径、多少条事实）并写全两条出路（改回原后端 / 把旧文件归档到别的数据目录）。
- `crates/qx-cli/src/runtime_wiring/pipeline_storage.rs`（120→**129** 行）：选定 SQLite / PostgreSQL 之前调
  `LiveEventPipeline::assert_no_abandoned_file_log`（`:62`），只查文件一侧。**这条不对称如实写下**：反向（数据库→文件）时
  数据库的空表判断不了"曾经有过历史"，需要迁移时人工核对。非法 `log_name` 由真正的打开入口报错，闸门放行，免得两处口径。
- `crates/qx-runtime/src/runtime_config/schema.rs`：`event_log_segment_events` 的文档补上"两种形状写互不相交的文件、
  改这个字段等于换一本账、留史会当场拒绝、分段换来的是归档抓手与摘要校验而不是更省的稳态写入"。

### Tests（六条行为判据 + 一条编排链判据，本轮没有一条靠形状）

- 新增 `crates/qx-runtime/tests/event_log_backend_switch.rs`（**266 行 / 6 条**）：两个方向都拒绝且**不动磁盘**
  （拒绝后旧历史摘要逐字可读、不建出 `segments/` 目录）、读面同样拒绝、不拦正常路径（首次启动 / 同目录别的日志名 /
  同后端照常开册并补上崩溃缺口）。
- `crates/qx-cli/src/tests/account_event_log_identity.rs` 新增 `switching_event_log_backend_refuses_to_start_an_account_from_an_empty_book`：
  走真实编排链，只改 `event_log_segment_events` 一个字段 → 拒绝；再改 `storage.backend = Sqlite` → 同样拒绝，
  且报错**点名 `.json` 而不是先报 feature 未启用**（这条同时钉住了闸门与 feature 检查的先后）。
  与上一遍对照着记：那条"面"的腐坏不留可观测后果只能按形状数，这一条拒绝会留下后果，所以行为判据够用。
- 变异矩阵六发（`logs/s431_pass18_mutation_matrix_run_after_split.txt` 为当轮总账）全部咬住：M1 摘掉 `open_with_store`
  的闸门调用 → 3 红；M2 只留一个方向（分段打开不看单文件历史）→ 3 红；M3 丢掉"空占位不算历史"的过滤 → 1 红（误伤首次启动）；
  M4 摘掉编排里的数据库闸门 → 1 红；M5 非法名口径被闸门抢走 → 1 红；M6 报错不点名被留下的那份文件 → 4 红。
  每发之后三份被改文件与 `%TEMP%` 镜像 md5 逐字节一致，六发后绿侧复跑 `after-runtime exit=0`（1.7s）/
  `after-cli exit=0`（5.5s）。**这批是拆分之后重跑的**：拆分把闸门挪进子文件、脚本 needle 跟着改，
  拆前那批（`s410`—`s417`）只作"拆分没改变判据咬合"的对照。

### Changed（#225 的价数量具：把"分段是不是出路"这个问题量掉）

- 稳态单价（`logs/s403_pass18_steady_probe_flat.txt`，`test` profile，每档 5 次均值）：`N=1000/2000/4000/8000` 的
  `refresh()` 空转 0.01149/0.02184/0.04439/0.09007 s/tick、重复事实不落盘 0.01172/0.02340/0.04744/0.09505、
  一次真实追加 0.04560/0.08767/0.17664/**0.34747**，倍率全部落在 1.90–2.03（严格线性于日志长度），日志 358,393→2,878,413 字节、
  outbox 累计 1006→8006。**一次追加 ≈ 空转读的 3.9 倍**，落盘那条链上还有整本重写。
- 跨进程那一格（`logs/s405_…`）：追加+跨进程 `refresh` 0.05991/0.11746/0.22836/**0.46200** s/tick，
  比"空转读+只写"两者之和多出 0.00320s（N=1000）→ **0.02705s（N=8000）**，这一段才是每次跨进程新事实触发的整本重放净代价。
  paper 循环里行情桥与恢复桥各开一个 pipeline，所以这才是稳态形状。含义：**单价由日志总长决定，不由本 tick 新事实条数决定**。
- 分段 vs 单文件（`logs/s404_…`）：`N=8000` 真实追加 单文件 0.34309 / 分段 500 **0.41688** / 分段 5000 **0.41737** s/tick（+21%以上），
  磁盘合计三档差 0.01% 量级（7,731,404 / 7,732,281 / 7,731,636 字节）——**分段既不省时间也不省空间**。
  `seg5000` 在 N=2000/4000 时事件文件仍只有 1 份（形状等同单文件却已贵两成），不能当噪声剔掉，它量的正是闸门多付的那一份。
  分段买到的是按段归档的抓手与 manifest 摘要校验。四段量具挂在两颗临时探针文件里（`tmp_pass18_steady_probe.rs` 的三个用例 +
  `tmp_pass18_backend_switch.rs` 的改前/改后对照），出数后已从树里删除，**引用这几个日志时要记得被测对象已不在**。
- 拆分量级与行数棘轮：闸门写进 `pipeline.rs` 后它从 2462 长到 2617 行，第一次门禁当场判红
  （`logs/s409_…`：514 PASS + 1 FAIL「`pipeline.rs` 2617 行 > 预算 2462」）。按职责切出 `backend_switch.rs` **147 行**
  与 `fact_context.rs` **133 行**（后者是三个事实元数据/归因 helper 原样搬家），父文件回到 **2371** 行、
  `maturity/line_budgets.yaml` 同步 2462→2371（只降不升），两个新模块 <500 行故不登记。本轮**没有**要求改动任何按路径取数的判据
  （`tools/check_architecture.py` 不改，改动权在协调者），门禁总数 515 条拆前拆后相同，只有预算那条从红转绿。

### Docs & Release（收口与发布物）

- `docs/自研量化框架审计与重构方案-V13.md` 新增 §9.25（本章上面那五段的完整版，含两张价钱表与变异矩阵）；
  `deploy/README.md` 新增「换 EventLog 后端会被当场拒绝」一节（1014→**1031** 行、93,428→**97,073** 字节，全 CRLF 手术、裸 CR 0），
  写出报错原文、两条出路、"数据库→文件方向拦不住"这条不对称，以及"别为了省稳态代价去开分段"；
  `maturity/capabilities.yaml` 的 `storage_consistency_contract` 补 4 条证据、把两条 limitation 换成当轮实测数
  （595→**599** 行，`sandbox_tested: false` 仍 **19** 处——本轮不用任何外部服务或凭据，这一格没动）。
- 九步构建按终树整跑 `exit=0`（`logs/s435_pass18_nine_step_build.txt`）：`[1/9]` 门禁 **515 项 PASS**、`[2/9]` 格式检查通过、
  `[4/9]` **100 条 `test result:` 行 / 958 passed / 0 failed / 1 ignored**、`[5/9]` clippy 通过、`[6/9]` `Ran 59 tests … OK`、
  `[9/9]` `config_fingerprint=aada66156749d230…`（`locked=false`），与第十五~十七遍同一份拓扑示例，说明本轮没动配置口径。
  **取数口径要钉住**：`[4/9]` 跑的是 `cargo test --workspace`，比 `--all-targets` 那次多出 21 个 Doc-tests 目标
  （21 个全是 `running 0 tests`），所以 result 行 100 > 80 而 passed 同为 958——这条相等正是"本轮没有用例只挂在 doc-test 上"的旁证。
- 发布物按终树重打包。`logs/s436_pass18_wheel_repack.txt` 抓到 **#194 那格窗口的第五次复现**：收口前 `dist/` 那份 wheel
  （218,449 字节、md5 `19078eb9f65538b8…`）内嵌 `.pyd` 是 `1711ce0a46f80e41…`，而本轮九步构建产出的 dll 已是
  `d5b5e4c29af03b6d…`——不同源；重打包之后 dll 又变成 `99443f77ad4b1f55…`（打包脚本自带 `cargo build -p qx-python --release`）。
  那一跑复核逐格 True（`.pyd ≡ 当前 dll`、12 份 `.py` 与仓库逐字节一致）却 `exit=1`：脚本末行打印 `✓` 时被 GBK 控制台打成
  `UnicodeEncodeError`。**日志里那些 True/False 才是判据，脚本退出码不是**；按纪律不留 `exit=1` 的收口日志，带
  `PYTHONIOENCODING=utf-8` 重跑一次得 `exit=0`（`logs/s437_…`）：**新 wheel 218,448 字节**、17 条目、12 份 `.py` 与仓库
  逐字节一致、内嵌 `.pyd` `99443f77ad4b1f55…`（353,280 字节）≡ 当下 dll。
- 干净 venv 冒烟（`logs/s438_pass18_clean_venv_smoke.txt`，`exit=0`）：四包导入、`native.available() -> True`、线格式归一成
  `buy cross hedge 2` 且读回一致、三条非法取值各按契约抛 `ValueError`（`margin_mode must be` / `position_mode must be` /
  `leverage must be positive`）、`Asia/Shanghai` 时区可用、已安装 12 份 `.py` 与仓库逐字节相同、已安装 worker 在父进程消失时
  退出码 0、`help` exit 0 且 **155** 行。exe 侧仍按 §9.19/#179 只登记单向证据：release `qx-cli.exe` **12,035,072** 字节
  （md5 `0779649bad04f331…`）四项字面量计数 1/2/1/1，debug（21,886,464 字节）1/5/1/1——本轮两个新计数点是 #226 那句
  闸门文案与它点名的 `.manifest.json` 形状。上一遍 release exe 是 12,063,232 字节，本轮小 28,160 字节，
  **这个差不作解释**（exe 大小从来不是判据，别把它读成能力增减）。
- 文档落盘后按 §9.20 的纪律再收一次口：门禁 **515 项 PASS / 0 FAIL、`exit=0`**（`logs/s439_pass18_gate_after_final_docs.txt`，
  §9.25 那两段发布物与流程缺口的文字进树之后重跑）→ `cargo build -p qx-cli` `exit=0`（`logs/s440_…`，`Finished` 于 12.07s）→
  带 `QX_PYTHON` 的 `cargo test -p qx-cli --bins` **270 passed / 0 failed、`exit=0`**（`logs/s441_…`，6.18s）
  = 上一遍的 269 加本遍新挂的那条编排链判据。


## Unreleased — V13 R2 第十七遍：一次 GET 顺手重写整本 Outbox——读面与写面分成两种声明，投影游标前移到序列化之前，前缀核对改成按序号二分（#169 前半）（2026-09-29）

取证问句是第十一遍立案那句话里的"这笔价钱由谁付"。#169 当时写的是"每轮全量重放、无压缩/保留策略，长跑 O(n²) 且无上限"，
拆开之后它来自三条互不相同的通道：① **读打开在写 Outbox**——`LiveEventPipeline` 只有一条打开入口，日志非空就补投影，
于是每个读请求都顺手按日志长度重写一遍投递件（`logs/s377_pass17_probe_read_open.txt`：`--release` 下 `N=4000` 的一次"读"打开
写出 4000 个 outbox 文件、花 5.4913s，倍率 1.95/2.05/1.95）；② **写面补投影把整本日志逐条 serde 一遍**，游标之前的已投递事实
先序列化再丢弃；③ **API 轮询桥的前缀核对是线性 `find`**，把一份没变过的日志再投影一次的代价随长度平方
（`logs/s379_api_projection_before.txt`：`N=16000` 二次投影 0.6038s，倍率 3.87/4.18/4.32）。三条这一遍都收掉；
**稳态每 tick 的整本读+整本重写、`ingest_once` 每次追加先 `self.clone()` 整份状态、日志无任何保留/压缩/归档策略**这三格留在
`docs/自研量化框架审计与重构方案-V13.md` §9.24 的「在册不改」，本轮不是"长跑代价已解决"。

### Changed（#169a：打开语义由每个调用点显式声明，不再由注释推断）

- `crates/qx-runtime/src/pipeline.rs`：补投影那条守卫改成 `if recovery == OutboxRecovery::ReprojectOnOpen && !log.is_empty()`
  （`:667`），`open_read_only`（`:517`）与 `open_stored(.., recovery)`（`:532`）分开，文件/SQLite/PostgreSQL 三个后端各两支
  （`:562`/`:573`、`:603`/`:620`）。读面从此一个字节都不写。
- `crates/qx-cli/src/runtime_wiring/pipeline_storage.rs`（**新文件，120 行**）：分派层从 `runtime_wiring.rs` 拆出（父文件当时
  越过 500 行门槛，现 **442** 行），三个后端 × 两个面各一支分支，两个统一入口 `open_runtime_pipeline` / `open_account_pipeline`
  各把面收成形参。读面 6 处调用点（`api_service.rs:35`/`:204`/`:340`、`strategy_binding.rs:207`、`strategy_contract.rs:401`、
  `market_bridges.rs:151` 的 API 轮询桥）与写面三条链（`market_bridges.rs:273` 的 Paper 行情桥、`venue_runtime/paper_submit.rs`、
  `venue_runtime/paper_worker.rs`）全部点名自己那一面。
- 复测（`logs/s381_read_open_after.txt`，注意它跑在 `test` profile 而 `s377` 是 `--release`，**两个绝对值不能互比**）：
  读面 `open_read_only` 在 `N=500/1000/2000/4000` 为 0.0071/0.0145/0.0295/0.0594s、outbox `0 → 0`；同档写面仍 7.9600s 与 4000 个文件。
  可比的只有同档之内的倍率与写副作用计数：改前"读打开"与"写打开"是同一条路径，改后读面写 0 个。

### Changed（#169b / #169c：把白算的那笔从两条热路径上拿掉）

- `crates/qx-storage/src/lib.rs`：`project_event_log_to_outbox(log_name, log, projection_cursor: u64)`（`:425`）新增游标形参，
  `filter(|event| event.seq >= projection_cursor)` 排在 `serde_json::to_string(event)` **之前**（判据按偏移量核对顺序，
  过滤排到序列化之后就等于装饰）；日志名合法性改用那份共用的 `validate_segment_name(log_name)?`，不再在函数里抄一份字面量字符集。
- `crates/qx-api/src/lib.rs`：`projected_event`（`:804`）用 `partition_point(|current| current.seq < seq)` 按序号定位，
  账户级（`project_event_log_inner`，`:533`）与全局兼容（`project_event_log`，`:770`）两条投影链共用同一份；
  两处按序号的线性 `find` 删除。复测（`logs/s380_api_projection_after.txt`）同一份未变日志的二次投影
  `N=2000/4000/8000/16000` 为 0.0009/0.0018/0.0036/0.0090s，倍率回到 2.06/2.05/2.49。

### Tests（判据：三条通道的行为 + 一份只能按形状数的接线表）

- 新增 `crates/qx-runtime/tests/event_log_open_faces.rs`（164 行 3 条：读面不碰 outbox 但仍读到全部事实、写面仍补崩溃缺口、
  分段后端保持同一处分面）、`crates/qx-storage/tests/outbox_projection_cursor.rs`（96 行 2 条）、
  `crates/qx-api/tests/api_projection_prefix_lookup.rs`（104 行 3 条）。合计 **+3 个测试目标、+9 条用例**。
- 新增 `crates/qx-cli/src/tests/event_log_face_wiring.rs`（194 行 1 条，形状判据）。它存在的理由是：把某个读调用点改回写面构造器，
  行为用例全绿——读模型照样读得到正确结果，区别只是它顺手写了 4000 个文件。所以按文件逐处数分派（读面声明份数、
  分派层 6 支分支、拆出模块的两行挂载、游标与序列化的先后偏移）。
- 变异矩阵七颗（`logs/s398_pass17_mut_*.txt`）各红在自己那一格：M1 `:23`（读链退回写面）、M2 `:36`（轮询桥入口退回 `open`）、
  M3 `:84`（分派层两条面静默合并）、M4 `:103`（具名 re-export 退回 glob）、M5 `:118`（补投影无条件执行）、
  M6 `:143`（游标过滤被删）、M7 `:175`（二分退回线性）。七颗全部 `exit=101` 且含 `test result:` 与 `FAILED`，
  每颗之后 7 份被改文件与 `%TEMP%` 镜像逐字节相同；还原后复跑 `exit=0`、`test result: ok. 1 passed`（`logs/s399_…`）。
  两格流程记账：M1 的 needle 在终树有两份（`:204` 与 `:340` 只差行首缩进），preflight 的 `count == 1` 在**写之前**当场拒掉；
  绿侧第一次复跑红在 `LINK : fatal error LNK1104`（测试进程还占着句柄），日志里没有 `test result:` 那一行，
  按本仓口径那是坏构建而不是判据不咬。

### Docs & Release（收口与发布物）

- `deploy/README.md` 新增「事件日志的读面与写面」一节（1005→**1014** 行、90,388→93,428 字节，全 CRLF 手术、裸 CR 0，
  `logs/s395_…`），并把上面那条 profile 口径写进去；`maturity/capabilities.yaml` 的 `storage_consistency_contract`
  补 5 条证据 + 2 条 limitation（`event_log_steady_state_tick_cost_is_linear_in_log_length`、
  `event_log_has_no_retention_or_compaction_trigger`），588→**595** 行，`sandbox_tested: false` 仍 19 处。
- 九步构建按终树整跑 `exit=0`（`logs/s390_pass17_nine_step_build.txt`）：`[1/9]` 门禁 **515 项 PASS**、
  `[4/9]` **99 条 `test result:` 行 / 951 passed / 0 failed / 1 ignored**（= 上一遍的 96/942 加上本遍新挂的 3 target / 9 条用例）、
  `[5/9]` clippy 通过、`[6/9]` `Ran 59 tests … OK (skipped=1)`、`[9/9]` 指纹 `aada66156749d230…`（`locked=false`）。
  本轮没有撞行数棘轮：`qx-api/src/lib.rs` 3185 / `qx-storage/src/lib.rs` 2406 / `qx-runtime/src/pipeline.rs` 2462 与
  `maturity/line_budgets.yaml` 现值逐一相等。
- 读面零写入这轮有**运行结果**而不只有字面量（`logs/s391_pass17_serve_read_face_e2e.txt`）：发布 exe 起 `serve`，
  30 次读请求之后 outbox 3 → 3、日志 40,988 字节 md5 不变。夹具只有 56 条事实，所以它证的是形状不是 N=4000 档的耗时。
- 发布物按终树重打包（`logs/s392_pass17_wheel_repack.txt`，`exit=0`）：新 wheel **218,449 字节**、17 条目、12 份 `.py` 与仓库
  逐字节一致、内嵌 `.pyd` `1711ce0a46f80e41…`（353,280 字节）≡ 重打包后 dll。**#194 那格窗口第四次复现**：重打包前那份 wheel
  （218,451 字节）内嵌 `.pyd` `53622351501d0c06…` ≠ 当时 dll `a77530e17a10cd8a…`。干净 venv 冒烟（`logs/s393_…`，`exit=0`）：
  四包导入、`native.available() -> True`、线格式 `buy cross hedge 2` 读回一致、三条非法取值各按契约抛 `ValueError`、
  已安装 12 份 `.py` 与仓库逐字节相同、已安装 worker 在父进程消失时退出码 0、`help` exit 0 且 155 行；
  release `qx-cli.exe` **12,063,232** 字节四项字面量计数 1/1/1/2，debug（21,869,568 字节）同计数。
- 文档落盘后按 §9.20 的纪律再收一次口：门禁 **515 项 PASS**（`logs/s400_pass17_gate_after_docs.txt`）→
  `cargo build -p qx-cli`（`logs/s401_…`）→ 带 `QX_PYTHON` 的 `cargo test -p qx-cli --bins`
  **269 passed / 0 failed**（`logs/s402_…`）= 上一遍 268 + 本遍新挂的那条形状判据。


## Unreleased — V13 R2 第十六遍：一条请求的时间戳是从哪一刻来的——三条通道共用同一个冻结值，以及状态行替三个码说了同一句话（#221 / #222）（2026-09-28）

取证问句一个：`#221`——**这条 API 连接上的 `ts` 是从哪一刻取出来的？** 答案是"进程把 API worker 起来的那一刻"，
而且它不止影响日志好看：`crates/qx-api/src/lib.rs` 的两条监听循环签名是 `ts: u64`，值在进 accept 循环之前一次算好
（`crates/qx-cli/src/strategy_contract.rs` 两处传的是 `runtime_timestamp_ms()` 的**结果**），循环里每条连接复用同一个数，
而那个数是三条通道共同的唯一时间来源——限流桶的补充（`handle_inner` → `try_acquire`，桶见底之后再也没人给它回血）、
控制命令的审计时间（`ControlPlane::submit_as(.., ts)`，全部命令的落地时刻都写成开机那一刻）、命令入队的租约秒
（`lease_clock(ts)`，uptime 越久就越"一落库已过期"）。顺带第二问：状态行的原因短语只点名了六个码（`200/202/400/404/409/429`），`403`「认证没过」与
`503`「闸门后端自己坏了」一起落进 `_ => "Internal Server Error"`——客户端按状态行读，
会把"没权限"与"服务坏了"听成同一句 500 的话（与 §9.12/#172 把风控端口的"拒绝"与"端口坏了"分成两条通道同族）。
`#222` 是本遍插探针时抓到的一条门禁盲区，立案后交协调侧（见下）。日志 `logs/s347_pass16_clock_judges.log`—
`logs/s362_pass16_workspace_test_with_qxpython.log`（构建与安装面的实测记在本章「收口」一节）。

### Changed（#221：时间戳按连接现取，跨进限流桶的秒域只换算一次）

- `crates/qx-api/src/lib.rs`：`serve`（`:1928`）与 `serve_tls_mtls_with_stores`（`:1840`）的参数从 `ts: u64` 换成
  `mut now: impl FnMut() -> u64`，两条 accept 分支各自在拿到连接之后调用一次
  （`:1937` 明文 / `:1863` mTLS），`serve_once` / `serve_once_tls` / `spawn_connection` 往下仍是"一条连接一个 `ts`"，
  所以三条通道同时活过来而读面口径不变。装配侧 `crates/qx-cli/src/strategy_contract.rs` 两条分支各交出一台
  真时钟（`runtime_timestamp_ms` 这个函数本身，不是它的返回值）。
- `crates/qx-api/src/lib.rs`：新增 `rate_limit_bucket_seconds`（`:618`）作为毫秒请求戳跨进令牌桶秒域的**唯一**换算点，
  `handle_inner` 在 `try_acquire` 之前调用它（`:1423`）；`ApiRateLimiter::try_acquire` 的文档同时改口写明"`now` 的时钟域
  是 epoch 秒"，`crates/qx-storage/src/lib.rs` 的 `FileTokenBucket` 段落补同一句（`now` 域由调用方决定，
  `refill_per_second` 的"每秒"就是那个域里的一格）。此前是毫秒戳直接喂给按秒补充的桶——"每秒 100 次"实际是"每毫秒 100 次"，
  任何持续流量都读成"额度用不完"，而那份额度正是 `DEFAULT_RATE_LIMIT_*` 唯一声明的政策。
- `crates/qx-api/src/lib.rs` `write_http_response`（`:2290`）的原因短语按状态码逐个点名：
  `200/202/400/403/404/409/429/500/503` 九格，兜底那格给 `Unknown` 而不是复用 500 那句话——兜底复用会让新增的
  没登记码听起来完全正确，读者永远看不出少了一格。

### Changed（#221 的文档与判据侧：共享出口要每行都写，时钟域要说清是"每条连接一次"）

- `deploy/README.md`（纯 CRLF 手术，本轮实测 1005 行、无裸 LF）：「端点 | 语义 | 非 200 口径」那张表 **14 行逐行**
  补上共享尾部 `429 api_rate_limit_exceeded；503 api_rate_limit_backend_unavailable`（这两格是全表共有的出口，
  以前只在 `/rate-limit` 附近出现过一次，读者按行读会以为别的端点不会 429）；`/health` 那格删掉"恒 200"这种
  与限流出口直接矛盾的旧说法，`/ready` 那格改成点名"第二格那些条件"。表后新增小节
  **「请求时间戳的三个时钟域」**：毫秒（请求戳/审计）→ 秒（限流桶）→ 秒（租约 `lease_clock`），并如实写出
  "每条连接一次，不是每条请求一次"这一格与 `Connection: close` 的边界关系。
- 新判据八条用例。`crates/qx-api/tests/request_timestamp_clock.rs` 两条（注入一台每连接递增一秒的墙钟，走真 socket
  提两条控制命令，审计 `ts` 必须一个是 `BASE_MS`、一个是 `BASE_MS + 1000`；共享令牌桶在同一个毫秒时刻仍回 429、
  跨过一秒后回 200）；`crates/qx-api/tests/status_line_and_limiter_exit.rs` 三条上线实测（限流出口的状态行与正文同句、
  桶后端坏掉回 `503 Service Unavailable` 而不是 500、未认证的账户读回 `403 Forbidden` 而不是 500）；
  `crates/qx-cli/src/tests/api_shared_exits_and_status_lines.rs` 两条（14 行端点表逐行声明共享码名 + 全篇不许再出现
  "恒 200"；状态行短语覆盖读面写出的每个码——本轮取数 **8 个非 200 码 vs 9 格点名**——且两两不同、兜底格不复用已点名短语）；
  `crates/qx-cli/src/tests/runtime_api_worker_identity.rs` 一条装配判据（两条 serve 分支都必须把真时钟交给 API worker）。
- 变异矩阵八行（`logs/s351_pass16_mut_*.log`，每行 `cargo` 以 `error: test failed` 收尾即 `exit=101`，日志含 `test result:`）：
  M1 accept 循环退回入口那份常数 → `request_timestamp_clock.rs:98`；M2 桶域换算换成恒等（毫秒直接喂）→ `:151`；
  M3 装配侧交出冻结闭包 → `runtime_api_worker_identity.rs:171`；M4 把 `/health` 那行退回 `—`＋"永远返回 200" →
  `api_shared_exits_and_status_lines.rs:32`；M4b 只把"恒 200"那格退回原文 → 同文件 `:41`；M5a 删掉 `403` 那格 → `:104`；
  M5b 把 `503` 的短语改成 500 那句 → `:127`；M6（上线用例侧）把 `403` 的短语改掉 → `status_line_and_limiter_exit.rs:143`。
  八行全部咬住。**收口时复跑了一次同一套矩阵**（`logs/s367_pass16_mutation_summary.txt`）：八行逐条打印
  `exit=101 咬住`，但脚本**最后一次** `restore_all()` 在写回 `crates/qx-api/src/lib.rs` 时抛了
  `OSError [Errno 22]`（刚跑完的测试二进制还占着句柄），树因此**留在了 M6 的突变上**（`403 => "Internal Server Error"`），
  而脚本的退出码只说"汇总没打印"。发现它靠的是逐条对照 `%TEMP%` 镜像的 md5（三个被改文件里只有 `lib.rs` 不同，
  `difflib` 显示差异正是那 1 行）；修法是 `shutil.copy2` 带重试 + `os.utime`，再核对逐字节相同（`:2295` 回到
  `403 => "Forbidden"`），复跑受影响判据确认回到绿侧（`logs/s368_pass16_after_restore.log`：`cargo test -p qx-api`
  全部 target **42 passed / 0 failed**、qx-cli 那 4 条 `ok`）。含义写进 V13 §9.23：**变异脚本的"跑完"不等于"还原完"**，
  每发之后与整轮结束都要拿镜像 md5 对一遍。除 `lib.rs` 之外那两个文件当轮就已还原一致，M6 也只碰 `lib.rs`。

### 立案（#222：门禁把 `src/tests.rs` 当成生产代码，零读者判据在它面前是瞎的）

`tools/check_architecture.py:1432` 的 `TEST_PATH = r"(?:^|/)tests/|(?:^|/)test_|_tests\.rs$"` 认目录形态的
`src/tests/`，却不认**单文件**形态的 `src/tests.rs`——而 V12 R2 为了绕开行数棘轮，恰恰把八个 crate 的库内用例
搬成了后者（`crates/qx-protocol/src/lib.rs:784-785` 的 `#[cfg(test)] mod tests;` 挂进来的那份就是它）。
探针实测（`logs/s355_pass16_probe222.log`）：往 `qx-protocol` 塞一个 `pub fn`、唯一读者只写在 `src/tests.rs` 里，
架构门禁原样印 `[PASS] 每个 pub fn/pub const 都有生产读者`——这一格当场是瞎的，全轮唯一红的是探针自己的行数预算。
按同一套取数条件复算（`logs/s355_pass16_probe222_survey.log`）：8 份扁平 `src/tests.rs` 被当成生产代码，
10 个公共入口"只活在这类模块里"且不在允许清单：`qx-datastruct::{close_at, from_view, resample_with_manifest,
select_time_with_manifest}`、`qx-protocol::{from_wire_json, to_qifi, to_wire_json}`、`qx-runtime::to_json_for`、
`qx-xingban::{corporate_action_supported_by_ledger, corporate_actions_from_data}`。同一条路径过滤还罩着
`PUBLIC_ENTRY_FAMILIES`（订单提交入口）与枚举变体生产者那两条判据。门禁文件改动权在协调侧，本遍只立案；
探针已按 `%TEMP%` 镜像还原并复跑门禁（`logs/s356_pass16_gate_after_probe_restore.log`，515 项全过）。

### 收口（九步构建与发布物）

- `cargo fmt --all` 先跑（`[2/9]` 的格式检查不在门禁也不在测试里），随后 `cargo clippy --workspace --all-targets -- -D warnings`
  `exit=0`（`logs/s359_pass16_clippy.log`）。
- 架构门禁三次复跑均 **515 项全过**：本轮代码定稿（`logs/s354_pass16_gate.log`）、探针还原后
  （`logs/s356_…`）、能力矩阵补条目后（`logs/s360_…`）。本轮没有新增门禁判据，515 这个数与上一遍同值。
- 整树 `QX_PYTHON=… cargo test --workspace` **96 条 `test result:` 行（23 unittests + 52 集成测试目标 + 21 doc-test）/
  942 passed / 0 failed / 1 ignored**（`logs/s362_…`）；与上一遍九步构建 `[4/9]` 那条 94 行 / 934 相比 **+2 个目标、+8 条用例**，
  正是本遍新挂的那八条。ignored 那格仍是 `replay_cost_scales_near_linearly_with_event_count`。
- 忘带 `QX_PYTHON` 的那次整跑（`logs/s361_pass16_workspace_test.log`）红了两条 e2e，报错文本直接点名
  "程序=python（QX_PYTHON 未设置，回落 PATH python）…若该程序是 WindowsApps 的 python 占位桩"——这是 §9.18/#185
  那条"失败通道必须带解释器身份"的本轮现形记账：同样的断口在上一遍只会说"worker 无 stderr 输出"。
- `maturity/capabilities.yaml` 补六条证据行 + 一条 limitation
  （`api_clock_granularity_is_per_connection_not_per_request`：时间戳按连接读一次，而每条连接都以 `Connection: close`
  收口、只服务一个请求，所以两者目前等价），纯 CRLF 手术 581→588 行、`sandbox_tested: false` 仍 19 处、无裸 LF。
- `maturity/line_budgets.yaml` 按当轮重跑 `--snapshot`：本轮增长的两格是 `crates/qx-api/src/lib.rs` 3080→**3186**
  （#221 的闭包时钟 + 状态行九格 + 共享出口判据要的文档）与 `crates/qx-storage/src/lib.rs` 2406→**2407**
  （`FileTokenBucket` 那句时钟域），其余八格是本轮早些时候尚未登记的既有漂移（六降二升，升的两格是 `ccxt.rs` +65、
  `strategy_host.rs` +23）；`qx-storage` 那格先按同段合并过一次仍差 1 行，最终按棘轮例外登记。
- 在册不改的：`spawn_connection` 仍是无上限的 thread-per-connection；租约那一格（第三条通道）没有独立新用例，
  它跟着 `ts` 的来源一起修，秒域本身已由 `crates/qx-cli/src/tests/strategy_snapshot_staleness.rs`（#210）钉住；
  `qx-api/src/lib.rs` 已是全仓最大的文件，accept / 会话 / 路由三簇按路径拆分的在册项继续往后推。
- 九步构建按终树整跑 **`exit=0`**（`logs/s363_pass16_nine_step_build.txt`，末行 `===== 全部完成 (all gates passed) =====`）：
  `[1/9]` 515 项全过、`[2/9]` 格式检查通过、`[4/9]` 与上面同数的 **96 条 result 行 / 942 passed / 0 failed / 1 ignored**、
  `[5/9]` 全 crate clippy 通过、`[6/9]` Python 套件 `Ran 59 tests … OK (skipped=1)`、`[9/9]` `runtime-check` 指纹
  `aada66156749d230…`（`locked=false`）——与第十五遍同一份 `deploy/qianxing.runtime.example.json`，即 #221 没动拓扑配置口径。
  整跑之后 `git status --porcelain deploy/` 只有本轮自己改的两份（`README.md`、`start-qianxing.ps1`），`deploy/data` 未被写脏。
- 发布物：**§18-C/#194 那格"重打包窗口"本轮第三次复现**（`logs/s364_pass16_wheel_repack.txt` A 段）——重打包前 `dist/` 里那份
  （第十五遍交付件，218450 字节、`md5=d870f454aeb29d78…`）内嵌 `.pyd` 是 `97b4b0ce0bee559f…`，而本轮九步构建产出的
  `_qianxing_native.dll` 已是 `b0afa253e229ed54…`。按 `tools/build_python_wheel.ps1`（显式 `-Python` 指仓库 venv）重打
  `powershell exit=0`，新 wheel **218451 字节 / sha256=`6cf00e861421b6b4…`**（整档摘要按 #159 只登记不采信）；载荷复核
  （`logs/s365_pass16_wheel_payload.txt`）：17 条目里 12 份 `.py` 与 `python/` 同名文件逐字节一致 **0 处不符**，
  内嵌 `.pyd` `53622351501d0c06…` ≡ 当前 dll（353280 字节）。
  **本轮才看清的一条口径修正**：打包脚本自己会重链接扩展（它先跑 `cargo build -p qx-python --release`，pyo3 以
  `extension-module` 特性重编），所以 dll 在 A 段与 C 段之间就换了一次（`b0afa253…` → `5362235…`）——
  "`wheel` 内 `.pyd` ≡ 九步构建那颗 dll"这条**跨构建**核对只能在重打包**之前**测得，重打包之后成立的只是"同一次调用内的
  staging 步没漏"（#181 那一格），两者不能混着引用。另外 `.pyd` 在 zip 里是 deflate（353280 → 164478 字节），
  所以"仓库 Python 侧零改动、wheel 本体却涨了 1 字节"也有确定解释：变的只有那颗重链接的扩展。
- 干净 venv 冒烟（`logs/s366_pass16_clean_venv_smoke.txt`，`exit=0`；`uv venv --seed --python 3.12` + `pip install --no-deps`）：
  四包导入 OK、`native.available() -> True`、`StrategyIntent` 线格式归一成 `buy cross hedge 3` 且读回一致、
  已安装 12 份 `.py` 与仓库逐字节相同、已安装的 `qianxing_strategy.worker` 在父进程消失时以退出码 0 收摊。
  exe 侧按 §9.19/#179 只登记单向证据：`target/release/qx-cli.exe`（**12062720** 字节、md5 `e252167c8a752cae…`）里
  `api_rate_limit_backend_unavailable` **1** 次、`Service Unavailable` **1** 次、上一遍的 `{"type":"server_shutdown"}` **1** 次、
  `收到停机请求后` **2** 次；`target/debug/qx-cli.exe`（21868544 字节）四项计数相同，本轮没出现"一侧 0 一侧非 0"那种形态。
- 文档落盘后的收口复跑：门禁再跑一次仍 **515 项 PASS**（`logs/s369_…`）；`cargo build -p qx-cli`（`logs/s370_…`）后
  复跑 qx-cli 全 binary，**忘带 `QX_PYTHON` 的那次又红了同样那两条 e2e**（`logs/s371_…`，266 passed / 2 failed），
  带上之后 **268 passed / 0 failed**（`logs/s372_…`）——同一格缺口在一遍之内撞两次，正是 #185 那条判据要盯着的形状，
  也是"整跑必须显式给 `QX_PYTHON`"这条纪律的本轮报价。
- README 的安装面时间戳本轮补上（第十一遍→**第十六遍**）：那一段是读者按它核对交付件的入口，而第十一遍之后连续五遍都重打
  了包却没换戳。新段落按载荷口径写：wheel **218451 字节 / 17 条目 / 12 份 `.py` 与仓库逐字节一致**、内嵌 `.pyd`
  `53622351501d0c06…` ≡ 终树 dll、12,062,720 字节的 `target/release/qx-cli.exe` 四项字面量计数（**1 / 1 / 1 / 2**），
  并把本轮那条 #194 修正一起写进去（打包脚本自己重链扩展，跨构建核对只在重打包之前量得到；`.pyd` 走 deflate，
  所以一次重链就是 353,280 → 164,478 的压缩件变化）。改完按 `artifact_identity_doc.rs` 的口径复检：两份交付文档最长的
  连续十六进制串 **32**（<64）、无 `sha256=`、七条骨架措辞与那句 stage 承诺都还在；门禁 **515 项 PASS**（`logs/s373_…`）、
  带 `QX_PYTHON` 的 qx-cli 全 binary **268 passed / 0 failed**（`logs/s374_…`）。
- **本轮自己制造又自己抓到的一处文档损伤**：把第十六遍那节插到 CHANGELOG 顶部时，第十五遍的 `## Unreleased` 标题
  被整行吃掉（正文段落还在，章却没了名字，全篇 61 个 `## Unreleased` 少一个），当时 `[4/9]`、门禁与冒烟全绿。
  发现方式是本轮写完后按 `^## Unreleased` 数章节标题、发现"上一遍的正文第一段没有归属章"，正文与标题按
  会话开头的文件快照复原。**这条正是 #195 立的那格缺口的又一次现形**（文档/散文侧的结构与逐字重复没有门禁判据），
  本轮把它连同复现方式一并写进在册项，不再新立任务号。


## Unreleased — V13 R2 第十五遍：停机这条路的两端——宽限从哪一刻起算、已在飞行的会话怎么结束（#217 / #218）（2026-09-28）

这一遍接着上一遍的"链路连着，但出口写在人心里"。取证问句是两个：`#217`——**被监管跑了一天的 worker，按一次 Ctrl+C 之后还剩多少宽限？** 答案是零毫秒：两条停机阶梯都把 `shutdown_timeout_ms` 从"开始等待"那一刻起算，请求落下的第一次轮询就判 `StopTimedOut`，而日志里那句"收到停机请求后 Xms 仍未退出"报的是 worker 已经跑了的总时长。`#218`——**accept 循环有了停机出口，那已经握好手的 WebSocket 会话呢？** 它一个都没有：会话循环的退出条件全在客户端那一侧（close 帧 / EOF / 游标过旧），服务端停机只能等前端自己断开，等不到就在宽限预算到点后把 worker 记成 `Failed`。日志 `logs/s324_pass15_orchestrator_lib.txt`、`logs/s325_pass15_runtime_lib.txt`、`logs/s326_pass15_mutation_summary.txt`、`logs/s330_pass15_ws_session_shutdown_test.txt`、`logs/s332_pass15_mutation218_summary.txt`（构建与安装面的实测记在本章「收口」一节）。

### Changed（#217：宽限预算的计时起点，连它报出的那个数一起改）

- `crates/qx-runtime/src/supervision/shutdown.rs`：`wait_for_worker_finish` 新增 `requested_at`，`StopTimedOut` 的判定与 `StoppedWithinBudget`/`StopTimedOut` 携带的 `waited_ms` 都改从**第一次观察到停机请求**起算；从未按下请求时仍按原口径报 `Finished`。`WorkerLadder` 两条变体的文档同步改口，写明这个数读作"给它多少时间它没走完"，不读作"它总共跑了多久"。
- `crates/qx-orchestrator/src/supervisor_stop.rs`：`wait_for_children` 同一处口径（托管循环的子进程版本），`SupervisorStop` 两条变体同步改文档。
- 判据两条：`crates/qx-runtime/src/supervision/tests.rs` 的 `the_grace_budget_is_counted_from_the_request_not_from_the_start_of_waiting`（注入假时钟把终止请求推到预算 10 倍之后落下，断言 `waited_ms <= budget`）与 `crates/qx-orchestrator/src/supervisor_stop.rs` 的同名用例（请求在第 5 轮之后落下、两个子进程第 7 轮才退完，断言仍是 `stopped-within-budget` 且 `waited_ms == 250`）。
- 同文件既有两条用例的期望数字按新语义改：`shutdown_signal_turns_worker_exit_into_graceful_stop` 250→0（两个子进程恰好在观察到请求那一轮退完），`stop_waits_for_the_last_child_before_reporting_graceful_stop` 1_000→750（请求落下后再等三轮才等到最后一个子进程）。这不是改断言迁就实现——那两条钉的是"按请求优雅退出"与"等最后一个而不是第一个"，数字换成请求之后的等待正是本条要立的口径；反向由 M2/M4 两条**只改上报数字**的变异证明断言仍咬得住。
- 变异矩阵四行（`logs/s326_pass15_mutation_summary.txt`）：M1 把 runtime 阶梯的计时锚退回进门 `start` → 该用例 `FAILED`；M2 只把上报的 `waited_ms` 退回 `start`（变体仍是 `StoppedWithinBudget`）→ 数字断言 `FAILED`；M3/M4 对 orchestrator 阶梯各同样一条。四条全部 `exit=101` 且日志含 `test result:`，还原后各自复跑为 `ok`，两个目标文件还原后与镜像逐字节相同。

### Changed（#218：给已在飞行的 WebSocket 会话一条服务端出口）

- `crates/qx-api/src/lib.rs`：`ApiService` 新增 `session_shutdown: Arc<AtomicBool>`。`serve` 与 `serve_tls_mtls_with_stores` 写成"先拿结论、退出前置位"的形状——正常收摊与 accept 出错两条出口都置位（mTLS 循环里原先 `tls_stream(...)?` 那条提前返回也会绕过置位，一并收进同一个 `outcome`）。`serve_websocket` 的会话循环每轮开头读这枚令牌，命中就下发一帧 `{"type":"server_shutdown"}` 再结束会话线程。
- 前后端之间因此多了一条可区分的信号：前端读到 `server_shutdown` 是计划内停机，读到裸 EOF / `ConnectionReset` 才是故障。`deploy/README.md` 两处同步——WebSocket 段的退出条件从四类改成五类并写明这一帧的读法，「停机与故障」段写明宽限计时起点与这一帧的关系。
- 判据一条，双向：`crates/qx-api/tests/accept_loop_shutdown.rs` 的 `plaintext_accept_loop_stop_ends_the_inflight_websocket_session` 先真握手拿 `connected` 帧，断言停机请求**之前**会话不得自己结束（防"一握手就断"骗绿），置位后必须在 5 秒内读到 `server_shutdown`，随后读到 EOF。
- 变异矩阵三行（`logs/s332_pass15_mutation218_summary.txt`）：M1 收摊时不置令牌、M2 会话读令牌但永不成立 → 都红在"停机之后要读到 `server_shutdown`"那一格；M3 会话不看令牌、握手后立刻自结束 → 红在"停机之前会话必须还开着"那一格（同一用例的两个方向各有独立的判红点）。三条全部 `exit=101` 含 `test result:`，还原后 `ok`，`crates/qx-api/src/lib.rs` 与镜像逐字节相同。
- 在册不改：`spawn_connection` 仍是无上限的 thread-per-connection（#218 只补会话的停机出口，连接数上限是另一件事）；会话的读写超时本来就有（读 100ms / 写 10s），所以"慢客户端把写堵死"不是无界阻塞，本遍不动它。

### 收口（拆分量级、九步构建与发布物）

- 本遍没有拆文件，越线只有一处，且它是真账：`logs/s334_pass15_gate_after218.txt` 报 `crates/qx-api/src/lib.rs 3162 行 > 预算 3140`（+22 行全为 #218 的令牌字段、两处置位与会话读点）。按 `tools/check_architecture.py --snapshot`
  登记为新高（`logs/s335_pass15_snapshot.txt`：写出 40 个超 500 行文件），复跑门禁 **515 项全过**（`logs/s336_pass15_gate_after_snapshot.txt`），能力矩阵证据行插入后再跑一遍仍 **515 项全过**（`logs/s338_pass15_gate_after_caps.txt`）。
  **登记而不是当场拆分**是本遍的选择：`qx-api/src/lib.rs` 拆到 `src/` 子模块前必须先清点按路径取数的判据与登记表（沿 §9.21 那条口径），这件事列进在册、放下一遍。
- 终树复跑：`cargo fmt --all -- --check` 空输出通过（`logs/s333_pass15_fmt.txt`）；三个被改 crate 的 clippy `-D warnings` `exit=0`（`logs/s337_pass15_clippy_three.txt`）；整树
  `QX_PYTHON=… cargo test --workspace --all-targets` → **74 个 `Running` 目标 / 74 条 `test result:` 行 / 934 passed / 0 failed**（`logs/s339_pass15_whole_tree.txt`）。
- 九步构建按终树整跑 **`exit=0`**（`logs/s340_pass15_nine_step_build.txt`，`===== 全部完成 (all gates passed) =====`）：`[1/9]` 架构门禁 515 项全过；`[4/9] cargo test --workspace` **94 条 `test result:` 行（23 unittests + 50 集成测试目标 + 21 doc-test）/ 934 passed / 0 failed / 1 ignored**
  （ignoring 的那条是 `replay_cost_scales_near_linearly_with_event_count`，手动复跑的伸缩量具，不是本轮漏挂）；`[5/9]` clippy 全 crate 通过；`[6/9]` Python 套件 `Ran 59 tests … OK (skipped=1)`；`[9/9]` `runtime-check` 指纹 `aada6615…`（`locked=false`）。
  **这条 934 与第十四遍那条 931 是同一套取数条件**（都含 doc-test 行、都是 94 条 `test result:`），差值恰为本遍新挂的 3 条用例（#217 两条 + #218 一条）；而上一条那句"74 个目标 / 934"与本条"94 条 result 行 / 934"两个 934 相等不是巧合——这一版 21 个 doc-test 目标全报 `0 passed`，`--all-targets` 少数的只有目标行，不多也不少用例。
- 发布物按终树重打包（`logs/s341_pass15_wheel_repack.txt`，`exit=0`）：新 wheel 218450 字节、`sha256=5eb46eb1631131d0…`。**#194 说的窗口本遍实测复现了一次**——重打包之前 `dist/` 里那份 wheel（218448 字节、`md5=c64158c98569fcc7…`）内嵌 `.pyd` 是 `5b1c81a883c8c502…`，而当时 `target/release/_qianxing_native.dll` 已是 `[8/9]` 那一次重链接的 `c19d01b0df9d5219…`，两者不同源。
  重打包后复核：wheel 内 `.pyd` md5 `97b4b0ce0bee559f0f113e33…` ≡ 当前 dll（353280 字节，逐字节相同），17 个条目里 12 份 `.py` 与仓库 `python/` 下同名文件逐字节一致。
- 干净 venv 冒烟（`logs/s342_pass15_clean_venv_smoke.txt`，`exit=0`；`uv venv --seed --python 3.12` + `pip install --no-deps` 那份新 wheel）：四包导入、`native.available() -> True`、`StrategyIntent` 线格式写出归一为 `buy cross hedge 3` 且读回一致；
  已安装的 12 份 `.py` 与仓库逐字节相同；已安装的 `qianxing_strategy.worker` 在父进程已消失时以退出码 0 收摊（第十四遍 #215 确实进了发布物）。
- 发布 exe 的字面量计数（同一份日志 B 段）：`target/release/qx-cli.exe` 12062208 字节，`{"type":"server_shutdown"}` 计 **1** 次、`收到停机请求后` 计 **2** 次（两条停机阶梯各一份）、`WebSocket 分片消息超过` 计 **1** 次。
  同一段里 `--parent-pid` 在 release 计 **0** 次、在 debug 计 1 次——沿 #179 的口径，**计数 >0 只是单向证据**，0 不能反证能力缺失（release 链接器会把短字面量拆进常量池/立即数）。#218 这一串是整串命中，可以当"改动进了发布物"的证据用。
- 在册（写清不改的理由）：`spawn_connection` 无上限 thread-per-connection；慢客户端写堵由 10s 写超时兜底，不是无界阻塞；停机请求的观察粒度是"一个轮询周期"（最多多给一份宽限，绝不会少给）；`qx-api/src/lib.rs`（3162 行）的拆分与按路径取数判据的清点；长期在册的 #169（事件日志每轮全量重放、无压缩）与 #152/#174/#186/#195 四条门禁缺口（改动权在协调者）。
- 文档面：本章、`docs/自研量化框架审计与重构方案-V13.md` §9.22，以及门禁取数范围内的 `deploy/README.md` 两处（WebSocket 退出条件改成五类 + 「停机与故障」的计时起点）——改完复跑门禁 515 项全过（`logs/s338_pass15_gate_after_caps.txt`）。
  本小节只再动 `CHANGELOG.md` 与 `docs/*.md`，不在 `artifact_identity_doc.rs` 的口径内（它只核对 `README.md` 与 `deploy/README.md`）。**常驻含义照旧**：改过被 binary-mtime 判据盯住的源文件的轮次，收口复跑前要先单独 `cargo build -p qx-cli`。
- 文档改完后的终账：`cargo build -p qx-cli` `exit=0`（`logs/s343_pass15close_rebuild_qxcli.txt`）→ 门禁 **515 项全过**（`logs/s344_pass15close_gate_after_docs.txt`）→ `QX_PYTHON=… cargo test -p qx-cli --all-targets`
  **12 条 `test result:` 行 / 317 passed / 0 failed / 0 ignored / `exit=0`**（`logs/s345_pass15close_qxcli_all.txt`，子进程那批用例走的就是 binary 新鲜度那条判据）。

## Unreleased — V13 R2 第十四遍：三条"连着但没数据"的路各自的出口（#213 / #214 / #215）（2026-09-28）

这一遍三件事是同一个形状：链路本身是对的，但**"这条循环凭什么退出"写在人心里而不是代码里**。`#213` 是 WebSocket 拼帧循环：单帧长度有闸门，分片却可以一直续，一条永不置 `fin` 的序列能把拼帧缓冲吃到耗尽，而调用方的停机令牌只在 poll 返回之后才读得到；`#214` 是 CCXT 用户流的空闲回话窗口按读窗自己一份公式算，读窗下界又允许 `timeout_ms: 1`，于是"等下一次事件"打成毫秒级热循环；`#215` 是共享内存策略 worker 不知道驱动它的父进程是谁，父进程被强杀后子进程以 1 kHz 空转占着 ring 与内存。日志
`logs/s315_pass14_mut_213.stdout.txt`—`logs/s315_pass14_mut_215py.stdout.txt`（构建与安装面的实测记在本章「收口」一节）。

### Changed（#213：拼帧循环的两条上限，一条管总长、一条管"这轮到底还要不要继续"）

- `crates/qx-adapter/src/lib.rs`：新增 `MAX_WEBSOCKET_MESSAGE_BYTES = 16 MiB`（帧长各自有闸门不等于消息有闸门，分片续帧只看帧长就永远卡不住）与
  `MAX_WEBSOCKET_FRAMES_PER_POLL = 64`（对端持续塞 ping 或续帧时，这条计数是 `binance.rs` 里 `while !should_stop()` 那条循环唯一的出口），超限分别具名拒绝
  （`WebSocket 分片消息超过 {} MiB 上限` / `WebSocket 单次读取窗口内帧数超过 64 上限`）；单帧上限 `MAX_WEBSOCKET_FRAME_BYTES = 16 MiB` 一起改成命名常量，三个数不再散在字面量里。
- 判据从"文件里"搬到 `crates/qx-adapter/src/tests.rs`（9 条用例整体外迁，见「收口」）。新增三条：`websocket_message_poll_caps_reassembled_message_size`
  用两条 9 MiB 分片撞总长、`websocket_message_poll_caps_frames_consumed_per_read_window` 用 72 条小帧撞帧数、
  `websocket_message_poll_still_reassembles_legal_fragmented_message` 守反方向 —— 三条必须同时绿，"能拒收"才不等于"修好了"。
- 变异矩阵（`logs/s315_pass14_mut_213.stdout.txt`）：M1 把帧数上限抬到永不触发（`> 上限 + 1_000`）、M2 把总长比较放宽 100 倍、M3 把帧数上限收成 1
  （合法三帧消息即被拒），三格全部 `exit=101` 且日志含 `test result:`（M3 红在两条用例上：拒收的那条过不了合法拼帧这条），
  还原后 `test result: ok. 6 passed; 0 failed` 复绿；`ALL_JUDGES_BINDING` 三格为真。

### Changed（#214：读窗区间与它导出的空闲窗口共用一个推导）

- `crates/qx-adapter/src/ccxt.rs`：`CCXT_WORKER_MIN_TIMEOUT_MS = 1_000` / `CCXT_WORKER_MAX_TIMEOUT_MS = 300_000`，
  `ccxt_worker_timeout_ms`（`ccxt.rs:158`）在启动前用 `1000..=300000` 区间具名拒绝越界值（`CCXT timeout_ms 必须在 {}..={} 内`），坏配置不再落进读窗；
  新增 `pub fn ccxt_idle_window_ms(timeout_ms) -> u64 { timeout_ms.saturating_mul(4) / 5 }`（`ccxt.rs:148`），
  `crates/qx-cli/src/venue_runtime/ccxt_execution.rs:320` 的观察窗口改为调用它 —— 此前两侧各写一份 4/5，改读窗的人只看得见自己那一份。
- 两条用例（`ccxt.rs` 的 `ccxt::tests`）：`ccxt_worker_timeout_bounds_are_enforced_from_config` 断 `timeout_ms` 为 1/0/300001 时拒绝、1000 与 300000 时放行；
  `ccxt_idle_window_keeps_real_pacing_at_the_timeout_floor` 断"下界导出的窗口不小于 800ms"且区间内每个读窗的空闲窗口严格短于读窗本身（相等就会把空闲具名成链路故障）。
- 变异矩阵（`logs/s315_pass14_mut_214.stdout.txt`）：M1 删下界、M2 窗口收小、M3 下界挪离边界、M4 窗口等于读窗，四格全部 `exit=101` 带 `test result:`，
  还原后 `test result: ok. 11 passed; 0 failed`；`ALL_JUDGES_BINDING` 四格为真。下界挪位（M3）能红，说明区间是判据在读而不是报错文案在念。
- 接口文档同步：`docs/CCXT多交易所接入与策略运行方案-V1.md` 的 `timeout_ms` 段落新增合法区间与推导函数两处指向，并写明"改小读窗不要绕开这条下界"。

### Changed（#215：共享 ring 的 worker 知道是谁在驱动它）

- `crates/qx-cli/src/strategy_host.rs`：新增 `shared_ring_arguments(transport, input_path, output_path, ring_config, parent_pid)`，把
  `--parent-pid` 与 `--protocol/--input-ring/--output-ring/--ring-capacity/--ring-slot-bytes` 一起发出，调用点传 `std::process::id()`。
  独立成函数是因为"参数表"这种形状最容易在别处再手抄一份。
- `python/qianxing_strategy/worker.py`：接 `--parent-pid`，每轮等待前检查该 pid 是否存活，父进程已消失时以 0 退出（无人驱动却常驻的进程会一直占住 ring 槽位与那块 mmap）。
- 两侧旗标由同一条用例对照，不让"改了一侧、另一侧静默漂移"过关：`crates/qx-cli/src/tests/strategy_worker_entries.rs` 的
  `shared_ring_launch_arguments_hand_the_worker_its_parent_identity`（参数表形状）+
  `strategy_parent_pid_flag_is_wired_on_both_sides_of_the_language_boundary`（读 `strategy_host.rs` 调用行的 `std::process::id()`，并读
  `python/qianxing_strategy/worker.py` 的 `"--parent-pid",` / `parent_pid=args.parent_pid` / `parent_pid > 0` / `if not _parent_process_alive(parent_pid):`）。
  Python 侧另两条（`python/tests/test_strategy_contract.py`）用真 spawn+wait 的死 pid 起真 worker，断它在该退出时真的退出。
- 变异矩阵（Rust 侧 `logs/s315_pass14_mut_215.stdout.txt`）：M1 删旗标对、M2 旗标与值错位、M3 旗标改名、M4 调用点传 0、M5 Python 不再消费，五格全部
  `exit=101` 带 `test result:`，还原绿且两份被改文件逐字节回到镜像（`residue ... identical=True`）。Python 侧（`logs/s315_pass14_mut_215py.stdout.txt`）
  P1 把退出换成 pass、P2 让存活检查永不运行，两格 `FAILED (errors=1)`，还原 `exit=0`。

### 收口（拆分量级、九步构建与发布物）

- 架构门禁本轮先红后绿，红的是真账：`logs/s307_pass14_gate.txt` 报三份文件越预算（`ccxt.rs` 1139 > 1074、`qx-adapter/src/lib.rs` 911 > 820、
  `strategy_host.rs` 824 > 800）。`ccxt.rs` 与 `strategy_host.rs` 的增量是本轮新增的常量/函数/用例，按 `tools/check_architecture.py --snapshot`
  登记为新高（`ccxt.rs` 1074→1139、`strategy_host.rs` 800→824）；`qx-adapter/src/lib.rs` 不靠登记过关 —— 那条门禁按路径取数，于是把 9 条用例整体外迁到
  `crates/qx-adapter/src/tests.rs`（`use super::*;`，路径键控的判据仍认 `ccxt.rs`/`strategy_host.rs` 原路径），`lib.rs` 820→681 是**下行**。
  外迁后单跑适配器用例 `logs/s308_pass14_adapter_split_tests.txt`，并用挂载探针证明没挂上的拆分产物会红（`logs/s310_pass14_adapter_split_mount_probe.txt`：`exit=101`，1 failed / 37 filtered）。
- 终树复跑：`cargo fmt --all -- --check` 通过；clippy `-D warnings` `exit=0`（`logs/s306_pass14_clippy.txt`）；架构门禁 515 项全过（`logs/s309_pass14_gate.txt`）；
  整树测试带 `QX_PYTHON`（`cargo test --workspace --no-fail-fast`）**94 条 `test result:` 行（23 unittests + 50 集成测试目标 + 21 doc-test）/ 931 passed / 0 failed / exit=0**
  （`logs/s312_pass14_whole_tree_qxpython.txt`）；本轮新挂 7 条 Rust 用例（ws 3 / ccxt 2 / 跨语言旗标 2）+ 2 条 Python 契约用例 —— 这个 931 **不能**与上一遍记的 924 直接相减，
  那一次是 74 个 `Running` 目标、不含 doc-test 行，两次取数条件不同；同一次整跑不带 `QX_PYTHON` 时**没有跑完**（`logs/s311_pass14_whole_tree.txt`：10 个目标后停在 `qx-cli`，
  那一格 263 passed / 2 failed，两条都是 `e2e_and_python_contract.rs:455`/`:499`，报错文案自己点名"QX_PYTHON 未设置，回落 PATH python"）—— 环境格，但引用它必须说它跑到哪一格；
  Python 套件 `Ran 59 tests … OK (skipped=1)`（`logs/s313_pass14_python_suite.txt`）；
  九步构建 `[0/9]`—`[9/9]` `===== 全部完成 (all gates passed) =====`、`exit=0`（`logs/s314_pass14_nine_step_build.txt`）。
- wheel 按终树重打包（`#194` 的口径：最后一次发布构建之后 `cp -p` 抓收口前快照，再重打包）：217415 → 218448 字节，17 条目、名称集合相同，
  CRC 变化恰好是 `{_qianxing_native.pyd, qianxing_strategy/worker.py, RECORD}`（`crc_changed_as_expected=True`，`logs/s317_pass14_wheel_payload_check.txt`）；
  wheel 内 `.pyd` md5 `5b1c81a883c8c502a85526b13d20de3f` ≡ `target/release/_qianxing_native.dll` md5；12 份 `.py` 与仓库逐字节无差异。
  整档 sha256 只登记（`2b3fcba88376cff4…`），采信的是上面这些载荷口径（`#159`）。
- 干净 venv 冒烟（`logs/s318_pass14_venv_smoke.txt`，`SMOKE_EXIT=0`）：四包可导入、`native.available() -> True`、意图契约的读写回环与四条非法字段按契约抛 `ValueError`；
  B 段是 #215 的**安装面**证据 —— 已安装的 `qianxing_strategy/worker.py` 与仓库逐字节相同，且它在父进程已消失时退出码 0。
- `#159`/`#179` 的新形状（记进在册盲区）：release exe 里 `--parent-pid`/`--input-ring` 的**整串**字面量计数为 0，而 debug exe 计数为 1。
  原因不是链接器常量池化，是发布编译器把短字面量拆成 8 字节立即数内联 —— 实测上下文
  `b'H\xba--parentH\x89\x10\xc7@\x08-pid…'`，拆出的两段各自计数为 1。结论沿用：exe 字面量计数 >0 只是单向证据，计数 0 **不能**反证能力缺失；
  本轮用 debug exe 计数 + 干净 venv 里已安装 wheel 的行为实测两头夹住。`#213`/`#214` 的长文案在 release exe 各计数 1（其中 #214 的上下界是格式化进去的，整串不证明下界，下界靠那条变异 M3 挪位才成立）。
- 已知缺口保持在册、不在本轮收口：`#174`（门禁缺字段级零读者判据）、`#152`（证据行 `path:NN` 锚点不校验）、`#195`（文档散文侧逐字重复块）、
  `#186`/`#157`（文档手抄 CLI 计数）、`#212`（联网装齐 wheel 声明依赖本机未测）。


- 收口文档与登记表（`maturity/capabilities.yaml` 纯 CRLF +3 证据行：`binance_direct` 的拼帧两条出口、`ccxt_rest` 的读窗区间与推导单源、`python_bridge` 的
  `--parent-pid` 存活闸门；574→577 行，`sandbox_tested: false` 仍 19 处）落树后按终树复跑最后一次：`cargo fmt --all -- --check` 输出为空、`FMT_EXIT=0`
  （`logs/s319_pass14_fmt_after_docs.txt`）；架构门禁 **515 项 `[PASS]` / `exit 0`、`[FAIL]` 0 条**（`logs/s320_pass14_gate_after_close_docs.txt`）；
  `QX_PYTHON=… cargo test -p qx-cli --bin qx-cli` **265 passed / 0 failed / exit=0**（`logs/s322_pass14_qxcli_after_close_docs.txt`）。
  中间那格留个教训：文档改完直接跑是 252 passed / 13 failed（`logs/s321_pass14_qxcli_after_close_docs.txt`），13 条全是 `crates/qx-cli/src/tests/mod.rs:346`
  那句"被测 binary 比 `crates/qx-cli/src/strategy_host.rs` 旧"—— 变异 harness 还原时打的 `os.utime` 让源码 mtime 永远新于那次 binary；单独 `cargo build -p qx-cli`
  （`logs/s321b_pass14_rebuild_qxcli.txt`）即回到全绿。**改过被 binary-mtime 判据盯住的源文件的轮次，收口复跑前先单独 build 一次。**


## Unreleased — V13 R2 第十三遍：修好的节律漏在手写名单外，一条共享预算替死链路销账，而那行再导出把自己算成了读者（#201 / #202 / #203 / #204 / #205 / #206 / #209 / #210）（2026-09-28）

这一遍主体六件事，三件是**前面几遍自己的修法留下的缝**，两件是**判据一路绿着、放过了它本该看住的东西**，一件是**文档写了一个实现从来没有过的承诺**
（收口时构建与安装面又另立两件：`#209` 三处 clippy 判红、`#210` 那道快照闸门零覆盖，见本章「收口（#209 / #210）」一节）。
共同形状很统一：一条链修好了，但"修好了"这句话是按名单说的，而名单是手写的。`#201` 是 `#168` 立的共享节律漏在两条执行 worker 的内联扫描上（当时的判据把
"哪些文件里有恢复扫描"写成两三个名字）；`#202` 是 `#167` 的重连预算被健康 symbol 每轮复位的次数清零，死 symbol 因此触不到上限、无上限重生子进程；
`#203` 是 stderr 读线程把"读不下去"和"一个字没写"记成同一件事，于是诊断会替 worker 宣称它没写；`#204` 是 `run` 统一入口里五条单配置文件入口各写一遍
"挑第一个非旗标、剩下的丢掉"，而同一个 `match` 的 `backtest` 臂早就把"收下却不处理"当成撒谎（V12 R4-e）；`#205` 是上一遍 `#191` 只补齐了带键读面的 404，
另一侧四条整体现读端点仍写着 `[?…]`，而 `snapshot_diff` 里留着一张表都没写、这条路上也永不返回的 404；`#206` 是 `tools/check_architecture.py` 的零读者判据
把门面那行 `pub use` 算成读者，于是 11 个公共名可以只剩"自己的定义行 + 那行再导出"而永远绿。日志
`logs/s256_pass13_mut_201_202.txt`—`logs/s291_pass13_qxcli_after_close_docs.txt`（构建与安装面的实测记在本章「收口（#209 / #210）」一节）。

### Changed（#201：恢复节律的名单改成从源码派生，两条执行 worker 的内联扫描真的接上）

- `crates/qx-cli/src/tests/spread_recovery_cadence.rs`（122 行 / 2 条用例）：`every_recovery_loop_polls_through_the_shared_cadence` 的文件名单
  从"写死两三个名字"换成 `collect_recovery_scanners` 递归扫 `crates/qx-cli/src` —— 只认调用形状 `pending_spread_recovery_groups(&`（所以
  `spread.rs` 里那处函数定义的 `root: &Path` 不算扫描点），跳过 `src/tests`（用例文件里那串名字是判据自己的字面量）。名单当前取到**四个文件、五处扫描**：
  `worker_entry.rs`（Binance 与 CCXT 两条专职恢复循环）、`venue_runtime/paper_worker.rs`、`venue_runtime/binance_submit.rs:223`、
  `venue_runtime/ccxt_execution.rs:64`（后两处是本轮接上的内联扫描）。同时加地板 `assert!(scanners.len() >= 4, …)`：名单比这个下界还少就说明**取数方式本身坏了**，
  那正是"手写名单"最贵的失效模式 —— 它不会红，只会安静地少扫一个文件。
- `crates/qx-cli/src/venue_runtime/binance_submit.rs` / `ccxt_execution.rs`：两处内联扫描真的按 `spread_recovery_poll_delay` 自节流。执行 worker 的循环尾仍按
  固定 100ms 轮询命令队列，所以退避**不能**做成循环尾的 sleep（那会把下单时延一起拖到 8 秒），而是换算成两次扫描之间的墙钟闸门
  `if !dedicated_spread_recovery && now >= recovery_next_at { … }`（`binance_submit.rs:222`、`ccxt_execution.rs:63`），扫描后按
  `pending_after >= pending_before` 累加 `recovery_stalls`、有推进则清零，再据 `spread_recovery_poll_delay(recovery_stalls)` 排下一次闸门。
- 逐文件断言（同一判据）：名单里每个文件都必须同时有"这一轮有没有推进"的记账（`if pending_after >= pending_before {` +
  `recovery_stalls.saturating_add(1)`）、走共享函数、并且真的据此节流（循环尾 sleep 或 `now >= recovery_next_at` 二选一）；用墙钟闸门的那一类还必须留
  `thread::sleep(Duration::from_millis(100))` 的队列轮询。两条禁令：`worker_entry.rs` 里不许再出现固定 100ms 忙轮询（那份节律只剩一处定义），
  以及 `has_pending_spread_recovery(` 不许回来 —— 那个布尔门数不出"这一轮有没有推进"，本轮已不在仓里（全仓 `grep` 只命中判据那句禁令）。
- **这条链不放弃**：`spread_recovery_poll_delay` 用 `RetryPolicy::new(u32::MAX, Backoff::exponential(500ms, 8s))`，退避有界、循环无界。
  停在 `HedgeRequired` 的分组是一条腿已成交、另一条还裸着，照行情链那套"连续失败到上限就具名报错"处理，等于让几次网络抖动之后把敞口永久晾着。

### Changed（#202：CCXT 行情重连预算按通道分账，健康标的不能再替死标的销账）

- `crates/qx-cli/src/venue_runtime/ccxt_stream_retry.rs`：新增 `CcxtChannelBudgets`（`BTreeMap<String, CcxtReconnectBudget>`，`market_rpc()` 构造）——
  `note_success(channel)` 只销这一条通道的账（条目不存在时不复位别人），`note_failure(channel)` 取该通道自己的预算算退避、触顶时把通道名写进放弃理由
  （`format!("{error}（通道 {channel}）"`），`consecutive_failures(channel)` 供日志点名。共享的 `CcxtReconnectBudget` 本体与退避公式不动，仍一律走
  `qx-core::retry`。
- `crates/qx-cli/src/venue_runtime/ccxt_market_worker.rs`：两条失败分支都按通道过预算，通道键是**调用类型 + 标的**
  （`fetch_ticker:{instrument}`、`fetch_ohlcv:{spec.instrument}`，`:89`、`:167`），成功应答同样按通道复位。缺陷形状不是"没有预算"，而是预算的**粒度**：
  一条共享计数会被任何一次成功应答复位，而这条 worker 每轮对每个 instrument 各跑两类调用，所以"A 应答正常、B 在柜台已下架"下 `MAX_RECONNECTS` 永远触不到顶，
  B 每轮重生一个 Python 子进程且无上限 —— 正是 `#167` 要收掉的形状，只是共享计数把它换个方向留了下来。
- 用户流（`watch_orders`）侧仍是单条预算：它本来就只有一个通道，本轮没把它拆成按标的（拆了反而会让"整条流断了"这种真故障按标的摊薄）。

### Changed（#203：stderr 尾窗把"读到的每一行"与"读管道这件事失败了"记成同一本账）

- 新文件 `crates/qx-cli/src/strategy_stderr_tail.rs`（39 行）：`STDERR_TAIL_MAX_CHARS = 512`、`STDERR_TAIL_LINES = 16` 是 stderr 诊断的唯一口径来源；
  `stderr_tail_note(Ok(line))` 仍按"空白行不记"处理，`stderr_tail_note(Err(error))` 记 `<stderr 读取中断: {error}>`；`record_stderr_tail` 按容量淘汰最旧行。
- `crates/qx-cli/src/strategy_host.rs:230`—`:250`：读线程从"只取 `Ok`、静默丢掉 `Err`"改成对 `Result` 记账。这一处不是排版：`diagnostics()` 与 `death_note()`
  两条诊断通道只看这个窗口，窗口空着就被说成「worker 无 stderr 输出」（`:315`—`:317` 那句，带上"若该程序是 WindowsApps 的 python 占位桩，请把 `QX_PYTHON`
  指向可用解释器"）。管道被切断或行编码坏了的时候，那句话是在替 worker 宣称它没写，而失败归因该指向宿主与子进程之间的管道；同一条读链上的响应通道（stdout）
  本来就把读取错误发回调用方，这里补齐的是不对称的另一半。
- 新用例 `crates/qx-cli/src/tests/worker_pipe_failure_diagnostics.rs`（161 行 / 4 条）：破管道记成事实而不是静默、单行截断口径、窗口容量、宿主读线程把
  broken read 也记进窗口（末条沿 `#185` 的死亡写侧身份）。

### Added（#204：`run` 的五条单配置文件入口共用一份参数读法，多余的旗标与位置参数一律报用法）

- 新文件 `crates/qx-cli/src/run_entry_arguments.rs`（42 行）：`run_entry_arguments(arguments, entry, json_output, default)` 返回
  `(配置文件路径, 是否要机器可读输出)`，遇到任何以 `-` 开头的未知旗标、第二个位置参数就返回 `run_usage` 报用法；`--json` 按**逐入口能力位**认账 ——
  `json_output` 不是调用方的偏好，外层（`cli_args.rs` 的 `RunCommand::machine_output`）从不为 `paper` / `paper-check` 生成机器可读输出，所以那里必须把
  `--json` 顶回去，否则旗标又一次"收下却不处理"。
- `crates/qx-cli/src/config_commands.rs`（559 → 539 行）：`paper`/`paper-check`、`doctor`、`live-check`、`runtime-check`、`report` 五处调用点
  （`:150`、`:156`、`:160`、`:166`、`:170`）换成这一份读法。改前的形状是各写一遍"挑第一个非旗标参数、剩下的丢掉"，于是
  `run doctor a.json b.json` 与 `run doctor --verbose` 都被收下并以 0 退出。
- 新用例 `crates/qx-cli/src/tests/run_entry_argument_honesty.rs`（125 行 / 3 条）：只接受它能兑现的形状、每种被拒的参数都在错误文案里点名，
  以及**五条入口确实共用同一份读法**的接线判据（那条是防"改了一处、其余四处照旧"的）。

### Changed（#205：四条整体现读端点对任何查询串回 400，`snapshot_diff` 那条永不返回的 404 删掉）

- `crates/qx-api/src/lib.rs`：新增 `const KEYLESS_READ_ROUTES: [&str; 4]`（`:2309`，`/scheduler/runs`、`/account/ledger`、`/reconcile/reports`、
  `/control/audit`），在路由分派前（`:1446`）对**非空查询串**回 `400 {route} 不接受查询参数`。这四条读的是整份现读模型（默认账户那一份），没有收窄键；
  `?account_id=shadow` 落在它们身上只会把默认账户的流水念成 shadow 的流水。第一张端点表原先在 `/account/ledger` 那一格写着 `[?…]`，而那条臂从头到尾没读过
  `query` —— 收窄承诺是文档单方面给的。空查询串照常 200，这条边界由用例的第三发单独钉住（见实测 M205c）。
- 同文件 `snapshot_diff`（`:1657` 起的注释）：删掉一张表都没写、这条路上也取不到的 `404 snapshot_not_found`，两格（基准缺失 / 那条投影不存在）都归
  409 `snapshot_base_not_found`。理由写在注释里：`publish_snapshot`（全局与按账户那两处）把 `snapshot_history` 与 `snapshot` 同批写入，
  没有任何入口能把前者写上不写后者，所以"基准查得到但没有当前快照"是不存在的分支 —— 留着它等于让文档去解释一条永不返回的码。
  上一遍 `#191` 保留的两处 `snapshot_not_found`（`:1491`、`:1497`）不动，它们服务的是另一格（投影在、快照还没算出来，文档承诺 200 空数组/`null`）。
- `deploy/README.md`（纯 CRLF，按字节手术）四处：第一张表那格补"这四条没有收窄键，带任何查询串一律 400"；正文那段"`/account/snapshot/diff` 不在这七条里"
  后面接上两侧新口径（含 diff 认 `account_id`/`venue_id` 这一对、只给一半是 400、以及它不产出 404 的理由）；第二张表 diff 那一格把两个非 200 码写全；
  第二张表 `/account/ledger` 那一格去掉 `[?…]`、补 `400 带任何查询串`。脚本 `logs/s258_pass13_doc_205.py`。
- 新用例 `crates/qx-cli/src/tests/api_read_route_query_contract.rs`（246 行 / 5 条）：无键读面拒掉它兑现不了的查询串、带键七条各自的键契约不被挪走、
  diff 只返回文档写过的非 200 码、两张表只在路由真的读 `query` 的那一格承诺查询键、以及**两份名单同源且不相交**（把无键路由塞进带键名单即红）。

### Removed（#206：11 个只剩"自己的定义行 + 门面那行 `pub use`"的公共名）

- 探针 `logs/s260_pass13_probe_reexport_reader.py` 数出的这一类候选，本轮全部删除；随后新常驻判据
  `crates/qx-cli/src/tests/reexport_zero_reader_surface.rs`（313 行 / 2 条）接管：
  `reexported_public_names_have_a_reader_beyond_their_own_reexport` 在**排除门面再导出行与自身定义行**之后要求每个再导出的公共名仍有读者
  （地板 `assert!(reexported.len() > 80, …)`，防"取数本身坏了的假绿"），`names_deleted_for_zero_readers_stay_off_the_surface`
  钉住这 11 个名字既没有定义行、也不在任何 `pub use` 名单里。
- 删除的 11 个名字（逐块行号与前后行数在 `logs/s261_pass13_del_206.txt`）：`build_rebalance`、`RebalanceDelta`、`Allocator`、`EqualWeight`、
  `pipeline_path`、`ShareSubscription`、`run_binance_user_stream_live`、`run_binance_user_stream_testnet`、`run_binance_user_stream_with_config`、
  `DEFAULT_WS_HOST`、`TESTNET_WS_HOST`。跨 8 个文件（第二轮补删另计 2 份镜像）：`crates/qx-zhenlu/src/portfolio/rebalance.rs` 199→165 行（含 `Allocator` trait、`EqualWeight` 实现、
  `RebalanceDelta`、`build_rebalance`）+ `crates/qx-zhenlu/src/portfolio/mod.rs` 42→10 行（门面只留 `rebalance`/`PortfolioState`/`RebalancePlan`）+
  `crates/qx-runtime/src/pipeline.rs` 2368→2362 行与 `lib.rs` 门面 + `crates/qx-core/src/ledger/mod.rs` 376→367 行与 `lib.rs` 门面 +
  `crates/qx-adapter/src/binance.rs` 2187→2142→2121 行（两轮：先删两个端点预设包装与其常量，再删中间层 `run_binance_user_stream_with_config`）+
  `crates/qx-adapter/src/lib.rs` 门面。手删 1 行 import：`crates/qx-runtime/src/pipeline.rs:27` 的 `use std::path::Path;`（`pipeline_path` 是它唯一使用点，
  删函数后由 `cargo check` 报成 `unused_imports`）。同时删掉只测 `EqualWeight` 的那个用例块（`crates/qx-zhenlu/src/portfolio/` 11 行）。
- **`s261` 主日志末尾那条 `RESIDUE build_rebalance 仍在 …` 是子串误报**（活着的 `build_rebalance_plan` 被当成残留），残留自检已改成整词匹配，
  改后重扫结果在 `logs/s261_pass13_residue_206.txt`：11 个名字在 310 个源码文件里都不再出现；`cargo check --workspace --all-targets` 干净。
- **两处判据自噬，都在变异矩阵的第一次跑时以"假绿"暴露，如实记**：① 取数语料必须排除判据自己那份文件 —— 那条复活名单是纯字面量，
  把自己的行算成读者就等于对"名单里的名字重新回到门面"失明（M206a 第一次因此绿）；② 读者循环里那句 `stripped.starts_with("pub use ")` 是永远走不到的分支
  （`pub_use_statements` 已把一条语句占的每一行记进 `covered`），删掉之后 M206c 才按期望咬住。**变异矩阵能暴露判据里够不着的分支**，这件事登记为
  `#206` 的副产物。
- 修完判据后复跑探针：候选 0（`logs/s262_pass13_probe_reexport_after.txt`）。

### 门禁自身缺口（在册，归协调者）

`tools/check_architecture.py` 的零读者判据把门面 `pub use` 行算成读者，所以"只剩定义行 + 那行再导出"的公共面在它眼里永远是绿的：本轮的修法是**新加一条常驻
用例**（`reexport_zero_reader_surface.rs`）而不是改门禁，门禁本体的口径要动仍归协调者（`#107`/`#135`/`#174` 同族）。登记时同时记下这条判据的两处脆弱性：
它按字面量取数，所以必须排除自己那份文件；它的豁免只应有一处（`covered` 行集），任何第二处 `starts_with("pub use ")` 都是够不着的分支。
沿上一遍的口径，本轮没动的还有 `#174`（字段级零读者）、`#195`（散文逐字重复块）、`#141`、`#152`、`#144`、`#169`、`#53`、`#157`/`#186`。

### 登记表滞后两处（本轮 `--snapshot` 才对齐）

- `maturity/line_budgets.yaml` 里 `crates/qx-cli/src/workers.rs` 仍钉 **584**，而上一遍 CHANGELOG 已写它降到 526 —— 那一遍没重跑快照，文档说的数和登记表钉的数
  分叉了一轮，本轮快照落成 526。同类：`crates/qx-api/src/lib.rs` 从 **3080** 跳到 **3140**，这一格的上升里包含上一遍 `#189`/`#191` 的新增（`PROJECTION_SCOPED_ROUTES`
  + `missing_projection_response` + 第四颗指标）与本轮 `#205`（`KEYLESS_READ_ROUTES` + 那条 400 臂），都住在同一个文件；本轮没有把它拆文件，所以下面那份 diff
  里这是**唯一一处上升**，其余全为下降。
- **常驻含义**：改完代码的当轮就得跑一次 `--snapshot` 再看 diff，否则登记表描述的是"上一轮之前"的世界，而棘轮"只许下降"的约定会被一次迟到的快照解释掉。

### 行数与预算

本轮 `--snapshot` 后的 diff（`maturity/line_budgets.yaml`）：`binance.rs` 2187→2121、`qx-adapter/src/lib.rs` 822→820、`config_commands.rs` 559→539、
`spread.rs` 518→514、`strategy_host.rs` 801→800、`workers.rs` 584→526（滞后一轮的快照，见上）、`pipeline.rs` 2369→2361、`qx-api/src/lib.rs` 3080→3140（唯一上升，见上）。
新挂载的判据文件都不在 500 行档内（最大 313 行）。

### 本轮实测

| 量具 | 当轮实测 | 日志 |
| --- | --- | --- |
| #201/#202 变异（6 发） | M202a（通道键摘成固定键）红 `ccxt_stream_retry_budget.rs:192`「两个失败分支不是都按通道过预算」；M202b（成功应答跨通道销账）、M202c（失败计数不分通道）红 `:143`「第 2 次坏通道的退避必须与独占该预算时同源」；M201a（binance 内联扫描丢掉节律门）红 `spread_recovery_cadence.rs:74`、M201b（ccxt 原地不动不再计数）红 `:64`、M201c（新加一处不在名单里的第四扫描点）红 `:60`（名单/地板那条）；绿侧 `green-202 = 7 passed`、`green-201 = 4 passed`，末尾 `ALL_JUDGES_BINDING 红绿成对` | `s256_pass13_mut_201_202.txt` |
| #203/#204 变异（8 发） | M203a（读侧退回吞错误）红 `worker_pipe_failure_diagnostics.rs:80`、M203b（读取中断不记账）红 `:29`、M203c（单行截断口径漂走）红 `:35`、M203d（窗口容量失效）红 `:49`；M204a（doctor 臂退回自己挑参数）红 `run_entry_argument_honesty.rs:108`「五条单配置文件入口不是都共用同一份参数读法」、M204b/c/d（未知旗标 / 多余位置参数 / `--json` 无差别收下）各红 `:88`；绿侧 `3 passed`＋`1 passed` 各两对，`ALL_JUDGES_BINDING 红绿成对` | `s257_pass13_mut_203_204.txt` |
| #205 变异（10 发） | M205a/b/c 红 `api_read_route_query_contract.rs:59`/`:59`/`:70`（400 通道整条关掉、只挡 `account_id`、空查询串也挡），M205d 红 `:84`（带键通道挪走、404 漂回 200 空数组），M205e/f 红 `:232`/`:241`（两份名单不同源 / 相交），M205g 红 `:118`（diff 又产出一张表都没写的码），M205h/i/j 红在文档判据 `:186`/`:198`/`:192`（`[?…]` 抄回、404 码抄回、审计那格 400 被抹平）；绿侧 `5 passed` + `3 passed`，`ALL_JUDGES_BINDING 红绿成对` | `s259_pass13_mut_205.txt` |
| #206 删除与残留 | 逐块行号与前后行数 16 条 DELETE/EDIT（8 份镜像）；残留整词自检「11 个名字在 310 个源码文件里都不再出现」；第二轮补删中间层包装（`binance.rs` 2142→2121） | `s261_pass13_del_206.txt`、`s261b_pass13_del_206_round2.txt`、`s261_pass13_residue_206.txt` |
| #206 变异（5 发 × 2 条判据） | M206a（放回定义 + 门面）→ 两条判据都红；M206b（只放回定义）→ 通用判据绿、名单判据红；M206c（放回并抽掉再导出行的读者豁免）→ 通用判据绿、名单判据红；M206d（`reexported_names` 直接返回空）→ 通用判据红、名单判据绿；M206e（种一个名单外的死结构体并搬上门面）→ 通用判据红、名单判据绿；`ALL_JUDGES_BINDING 红绿成对`，终树三份文件 sha 与镜像逐字节一致 | `s262_pass13_mut_206.txt` |
| 零读者探针 | 删除前数出这一类候选并给出名单，删除+判据挂载后复跑 **候选（再导出行掩盖的零读者公共面）: 0** | `s260_pass13_probe_reexport_reader.py`、`s262_pass13_probe_reexport_after.txt` |
| 架构门禁 | 代码、判据与接口文档定稿后、`maturity/capabilities.yaml` 注入前一次，注入后一次，两次都是 **515 项 `[PASS]` / `exit 0`**（`grep -c "^\[PASS\]"` 数得出 515） | `s263_pass13_gate_predoc.txt`、`s264_pass13_gate_after_caps.txt` |
| 整树测试（设 `QX_PYTHON`） | `cargo build -p qx-cli && QX_PYTHON=… cargo test --workspace --all-targets --no-fail-fast` → **74 个测试目标、918 passed / 0 failed、`TEST_EXIT=0`**（`grep -c "^test result:"` = 74） | `s265_pass13_full_test_python.txt` |
| 整树测试（不设 `QX_PYTHON`） | 同一命令不设解释器 → **916 passed / 2 failed、`TEST_EXIT=101`**，红的两条都在 `crates/qx-cli/src/tests/e2e_and_python_contract.rs`（`rust_invokes_python_strategy_jsonl_worker_through_versioned_contract`、`rust_invokes_python_multi_intent_strategy_contract`），要解释器，属环境不属缺陷 | `s266_pass13_full_test_nopython.txt` |
| 编译与格式 | `cargo check --workspace --all-targets` 干净（含删面后的 unused 检查）；`cargo fmt --all -- --check` 输出为空、`FMT_EXIT=0` | `s267_pass13_fmt_check.txt` |
| 文档拼接后的复跑 | 本章与 V13 §9.20 落进树之后：架构门禁仍 **515 项 `[PASS]` / `exit 0`**，`QX_PYTHON=… cargo test -p qx-cli --bin qx-cli` **257 passed / 0 failed**（本轮新挂的六个判据模块都在这一包里） | `s268_pass13_gate_after_docs.txt`、`s269_pass13_qxcli_after_docs.txt` |
| `maturity/capabilities.yaml` | 纯 CRLF 锚点插入 **+11 证据行 / 1 行改写**（`spread_recovery_cadence` 那条从"三条循环"改成"名单从源码派生"的口径），第 562→573 行，`sandbox_tested: false` 仍 19 处、`\r\r` 不存在、cr==crlf==lf | `s264_pass13_caps_201_206.py` |

- 两条 harness 的约定，本轮补上并如实记：变异脚本必须有 `--restore` 这条 argv 分支，否则中途异常退出会把树留在改脏的状态（本轮**发生过两次**，
  靠 %TEMP% 镜像逐字节还原 + `os.utime` 找回）；红侧只认「`exit == 101` **且**日志里有 `test result:` 行」，没有那一行的是坏树不是判据。
- 两处自我指涉：`s263` 跑的是本章与 V13 §9.20 拼接**前**的树，`s264` 是 `capabilities.yaml` 注入**后**的那次；两份文件都不在任何判据的取数范围内
  （沿上一遍 `artifact_identity_doc.rs:9` 的口径：判据只读两份交付文档；本轮核对：全目录 `grep` 只在 `init_onboarding.rs:262` 与 `reexport_zero_reader_surface.rs:243` 命中"CHANGELOG"三个字面量，那是散文不是取数）。不过文档落进树之后仍复跑了一次门禁与整包 `qx-cli` 用例（`s268`/`s269`，数字见上表），这样这张表里的每一次实测都出自终树。

### 收口（#209 / #210：九步构建第一次整跑就挡下来的三处 clippy，与那道从没被用例走过一次的闸门）

上面那张表里的实测都只在 dev profile 下成立：`build.bat` 的 `[5/9] Clippy` 一整跑就判红退出（`logs/s270_pass13_nine_step_build.txt`），
而 clippy 既不在 515 项门禁里、也不在 `cargo test` 里（rustfmt 同理，住在 `[2/9]`）—— 这是第十二遍记下的那一族盲区（"构建步骤独有的检查看不到"）的又一次落地。

- **`#209` 三处判红**（`logs/s271_pass13_clippy_after_fix.txt`，口径 `cargo clippy --workspace --all-targets -- -D warnings`）：
  ① `crates/qx-cli/src/scheduler.rs:326` `too_many_arguments (10/7)` —— 就是第十二遍 `#190` 那道实时快照指纹闸门 `live_strategy_job_is_stale`；
  ② `crates/qx-cli/src/tests/api_read_route_query_contract.rs:169` `unnecessary_map_or`；③ `crates/qx-cli/src/tests/run_entry_argument_honesty.rs:39`
  `bool_assert_comparison`。后两处是**本轮自己新挂的判据文件**被咬，不是既有债 —— 新用例只过 `cargo test` 与门禁、不过构建步骤，就会以这种形状留到收口。
- **①的修法走结构而不是 `#[allow]`**：`scheduler.rs:321`—`:332` 新增 `pub(crate) struct StrategyJobLease<'a>`
  （`queue`/`queued`/`worker_id`/`fencing_token`/`lease_now`），把"这一条队列条目 + 我这次认领"这份现场收成一个入参，签名从 10 降到 6。
  仓库对 `too_many_arguments` 的既有约定确实是 `#[allow]` + 一行"为什么拆结构体更差"，这里反过来走是因为那五个入参本来就是一件事
  （执行前与执行中两处调用各把同一份现场拆成五个变量传），而 `#210` 补的用例需要能整体构造它。②③ 按 clippy 建议直接改，本轮没有新增任何 `#[allow]`。
- **`#210`：这道闸门从写下那天起没有一条用例经过它。** 取证不是读代码，是变异：`logs/s274_pass13_mut_209.py` 想复核"`#209` 改完闸门仍咬得住"，
  摘掉两处（N1 让 `current_digest == Some(_)` 恒真、N2 让确认条目拿错时钟域）**两侧同绿**（`NOT_BOUND`，`logs/s274_pass13_mut_209.txt`）。
  红不了的原因不是判据松，而是 `live_strategy_job_is_stale` 的整条函数体**零覆盖**：`tests::strategy_job_terminal_state` 那四条钉的是终态回写与
  "重投不二次执行"，走的是回写侧；快照指纹门只在 `live_enabled=true` 且 `bars_snapshot_path` 有值时才被调用，而那批用例一份快照文件都没有。
  上一遍写"重投用例钉住了快照指纹门"是按**函数归属**说的，不是按用例取数说的 —— 记为文档口径缺陷。
- 新文件 `crates/qx-cli/src/tests/strategy_snapshot_staleness.rs`（290 行 / 6 条，挂在 `tests/mod.rs` 的 `strategy_job_terminal_state` 与
  `strategy_worker_entries` 之间）：快照指纹变了 → 跳过执行**并当场确认掉队列条目**（`done/` 有、`queue/` 无，且租约过期后不再可见 —— 这一发同时钉住
  ack 走的是秒域）；快照未变 → 放行执行；`expected_digest = None` → 连快照文件都不读（夹具故意写成坏 JSON 仍 `Ok(false)`）；快照缺失 → 按过期处理；
  快照 instrument 与配置不一致 → `Err` 且**不** ack（条目要能重试）；ack 失败 → 错误文案带上调用点给的那句话与 `Unauthorized`（租约被 `strategy-2` 接管后触发）。
- **变异矩阵同时是零覆盖探测器**（本轮第二次用到这条，第一次是 `#206` 的判据自噬）：`logs/s275_pass13_mut_210.py` 三发全部红 ——
  N1（闸门永不判过期）→ 3 条红、N2（确认条目拿 `digest_now` 这个毫秒墙钟）→ 同样 3 条红（秒/毫秒两个时钟域写错必红）、
  N3（`expected_digest` 为 `None` 时被当过期）→ 3 条红，其中两条落在 `strategy_job_terminal_state`（说明补的这批用例把重投链也接回了这道门）；
  `BASELINE 10 passed` / `GREEN exit=0 10 passed` / `ALL_JUDGES_BINDING 红绿成对`。
- **harness 的坑本轮又踩一次，形状不同、根相同**：`s275` 首跑三发全红而 `GREEN` 侧报 3 条失败。原因与上一遍记的 `cp -p` 还原同源 ——
  `restore()` 里 `os.utime(TARGET, (mirror_mtime, mirror_mtime))` 把源码 mtime 拍到**已编译的变异 binary 之前**，cargo 据此判定"不需要重编"，
  于是"还原后的绿"跑的还是变异产物。修法：还原后打**当前时间**（`os.utime(TARGET, None)`）且每次取数前先 `cargo build -p qx-cli`；改后 `GREEN exit=0 10 passed`。
- 一条弱信号按形状留在档、不当结论：一次性探针 `logs/s281_probe_zero_coverage_qxcli.txt` 按名字数出 `qx-cli` 里有上百个函数在测试语料中不出现，
  这个数字**没有**被本轮采信为零覆盖清单 —— 行为型用例经过真实入口调用被测函数时不会提到函数名（`#210` 之所以能定案，靠的是变异而不是 grep）。
  它的用途只有一个：grep 绿而变异红同时出现时，说明两者的口径不同。

**发布面按终树重跑**（`#194` 那条"收口后要重打包"的约定，本轮照做）：

| 量具 | 当轮实测 | 日志 |
| --- | --- | --- |
| clippy（改动前） | `[5/9]` 三处 `-D warnings` 判红、`could not compile qx-cli` | `s270_pass13_nine_step_build.txt`、`s271_pass13_clippy_after_fix.txt` |
| clippy（改动后） | `cargo clippy --workspace --all-targets -- -D warnings` 到 `Finished`，`warning`/`error` 行为 0（`#210` 用例落进树后复跑一次同口径，仍 0） | `s272_pass13_clippy_round2.txt`、`s276_clippy_r3.txt` |
| #209/#210 变异 | 上节：`s274` 未咬（定案零覆盖）→ `s275` 三发全红、`GREEN exit=0 10 passed`、`ALL_JUDGES_BINDING 红绿成对` | `s274_pass13_mut_209.txt`、`s275_pass13_mut_210.txt` |
| 架构门禁 | **515 项 `[PASS]` / `exit 0`**（`#210` 用例挂载后） | `s277_gate_after_210.txt` |
| 整树测试（设 `QX_PYTHON`） | `QX_PYTHON=… cargo test --workspace --all-targets --no-fail-fast` → **74 个测试目标、924 passed / 0 failed**（`grep -c "^test result:"` = 74；比上一遍的 918 多出的 6 条就是 `strategy_snapshot_staleness`） | `s279_whole_tree_python_alltargets.txt` |
| 九步构建 | `build.bat` 整跑 `[0/9]`—`[9/9]` 全过，末行 `===== 全部完成 (all gates passed) =====`、退出码 0 | `s280_nine_step_build.txt` |
| wheel 重打包 | `tools/build_python_wheel.ps1`（显式 `-Python` 指向仓库 venv）退出 0；`dist/…whl` **217,415 字节**、17 条目名称集合与收口前那份逐字节一致，CRC 变化只有 `_qianxing_native.pyd` 与 `RECORD`（其余三项只有 zip 时间戳），且 **`.pyd` md5 ≡ `target/release/_qianxing_native.dll` md5** | `s283_pass13_wheel_rebuild.txt`、`s285_pass13_wheel_install_smoke.txt` |
| 干净 venv 安装冒烟 | 新建临时 venv 装本轮 wheel：四包导入 OK、`native.available() -> True`、`StrategyIntent` 线格式写出/读回一致、四发非法值各按契约抛 `ValueError`；已安装 `worker.py` 与仓库那份**逐字节相同** | `s284_pass13_venv.txt`、`s285_pass13_wheel_install_smoke.txt` |
| release exe 播报计数 | 12,060,672 字节里本轮三条新播报各 1 次：`不接受查询参数`（#205）、`<stderr 读取中断: `（#203）、`（通道 `（#202）。计数 >0 才是单向证据（`#159`），本轮不据任何 0 计数下结论 | `s285_pass13_wheel_install_smoke.txt` |
| `maturity/capabilities.yaml` | 纯 CRLF 锚点插入 **+1 证据行 / 1 处改写**（`#203` 那条把"四条"写成"三条"），第 573→574 行，`sandbox_tested: false` 仍 19 处、`\r\r` 不存在、cr==crlf==lf | `s288_pass13_caps_209_210.py` |
| 收口文档与登记表落地后的复跑 | 本小节与 V13 §9.20 收口段、`capabilities.yaml` 证据行都写进树之后：`cargo fmt --all -- --check` 输出为空、`FMT_EXIT=0`；架构门禁仍 **515 项 `[PASS]` / `GATE_EXIT=0`**（`[FAIL]` 0 条）；`QX_PYTHON=… cargo test -p qx-cli --bin qx-cli` **263 passed / 0 failed、`TEST_EXIT=0`**（上一遍的 257 + `strategy_snapshot_staleness` 的 6 条）。最后两行日志名与 §9.20 标题的范围改写后又跑一次门禁，仍 **515 项 `[PASS]` / `exit 0`**（`logs/s292_pass13_gate_final.txt`） | `s289_pass13_fmt_after_docs.txt`、`s290_pass13_gate_after_close_docs.txt`、`s291_pass13_qxcli_after_close_docs.txt`、`s292_pass13_gate_final.txt` |

- **安装面的口径要说全**：装 wheel 时用 `--no-deps`（本轮约定不使用任何外部服务，联网取依赖不在此列），所以包元数据里那句
  `tzdata; sys_platform == "win32"` 并没有被 pip 兑现 —— 首跑 `import qianxing_ashare` 就在 `ZoneInfo("Asia/Shanghai")` 抛
  `ZoneInfoNotFoundError`。本轮把仓库 venv 里那份 `tzdata` 复制进临时 venv 后冒烟才通过。结论限定为：**声明了这条依赖**（`python/pyproject.toml:13`
  与 wheel 的 `METADATA` 都在），**而离线 `--no-deps` 安装不会自动满足它**；"联网装齐依赖"这一格本机没测。
- **两处判据行数与本章正文对不上，就地改而不是另立一节**：`#209` 的 ②③ 两处修复正好改在本轮上一节引用的两份判据文件里
  （`api_read_route_query_contract.rs` 250→246、`run_entry_argument_honesty.rs` 126→125），上一节写的是修前的数；
  `maturity/capabilities.yaml` 里 `#203` 那条证据行写"三条"而磁盘上那个文件是 4 条。三处都在本轮改齐，登记在此是因为它属于同一族失效：
  **文档引用的行数是在别的步骤之后测的，就会漂**。

### 在册未做

- 没改门禁本体的"再导出算读者"口径（归协调者，理由见上节）；`#174` 字段级零读者、`#195` 散文逐字重复块、`#141`、`#152`、`#144`、`#169`、`#53`、
  `#157`/`#186` 照旧在册。
- `#202` 只把行情 RPC 侧按通道分账；`watch_orders` 用户流仍是单条预算（本来就一个通道），`CcxtChannelBudgets` 的条目也没有回收上限——
  通道键由配置里的 instrument 集合决定，不是无界输入，本轮不加淘汰。
- 安装面只测了"离线 `--no-deps` 装本轮 wheel + 手工补 `tzdata`"这一格：**联网装齐元数据里声明的依赖**（`ccxt>=4.4.0`、
  `tzdata; sys_platform == "win32"`）本机没跑过，本轮也不使用外部服务。要把它当发布条件，需要在有网环境重跑一次同一份冒烟脚本。


## Unreleased — V13 R2 第十二遍：一次作业失败只剩"跑太久"这一种说法，拼错的账户 id 能读成"干净的空账户"（#191 / #189 / #190 / #192）（2026-09-28）

这一遍做的正是第十一遍结尾"本轮没做"名单上的那四条（当轮立案为 `#189`—`#192`，见 §9.18 末段）。四件事形态各异，
但属同一类：**通的那一段把不通的那一段藏起来了**。`crates/qx-cli/src/workers.rs` 的 Strategy worker 只有
`success=true` 一条收口，作业体报错在这条链上没有出口 —— 那条运行只剩"被下一轮调度 tick 升级成
`error_code="TIMEOUT"`"一条路，于是"策略自己报错了"读起来是"策略跑太久"，而 `JobStatus::Failed` 在生产里一个生产者都没有；
`crates/qx-api/src/lib.rs` 的 `let _ = enqueuer(...)` 把"已受理的控制命令写不进执行队列"整个吞掉，回执照样 202；
七个带键读面对"这份部署里没有这个账户"回 200 空数组，而兄弟端点 `/account/snapshot` 在同一条件下回 404；
`deploy/start-qianxing.ps1` 自带一份 PowerShell 的角色映射，与 `crates/qx-orchestrator` 的 `plan_workers` 已经漂成两回事，
而门禁的取数范围里没有 `*.ps1`，所以没有任何东西会红。日志 `logs/s243_*.txt`—`logs/s254_final_gate_pass12_after_refs.txt`。

### Changed（#191：七个带键读面对"没有这份投影"合成同一个 404）

- `crates/qx-api/src/lib.rs`：新增 `const PROJECTION_SCOPED_ROUTES: [&str; 7]` 与
  `ApiService::missing_projection_response`（`:1294`），把判定放在**路由分派之前**做一次：键的形状非法仍是 400
  （沿用同一个 `projection_key_from_query`，400 在 404 之前判），形状合法但仓内没有这份投影则
  `404 {"error":"account_projection_not_found"}`，不带键或投影存在返回 `None` 交给各读面自己处理。
  - 缺陷形态不是"回错了码"，而是**这一格根本不存在**：`/account/snapshot` 在投影不存在时回 404 `snapshot_not_found`，
    同一条件下的 `/account/snapshot/envelope`、`/account/orders`、`/account/positions`、`/account/balances`、
    `/events`、`/events/live` 回的是 200 + 空数组/`{}`/`null`。操作员把 `account_id` 少打一个字符，读到的不是
    "这个账户不存在"，而是"这个账户干净得一张单都没有"——在交易平台上这是最贵的一种误读。
  - `/account/snapshot/diff` **刻意不在名单里**：它的定位符是 `base_hash`，基准缺失由 `409 snapshot_base_not_found`
    说话，那条码已经在说"这份基线不在这条链上"，再挂一个账户不存在就是两个原因共用一个出口。
  - 有意破坏的兼容面（一处）：`/account/snapshot` 与 `/account/snapshot/envelope` 的"投影不存在"这一格从
    `snapshot_not_found` 换成 `account_projection_not_found`。仓内读者核对结果是**只有用例与文档**：
    `snapshot_not_found` 的两处服务端产出点（`crates/qx-api/src/lib.rs:1573`、`:1579`）保持原样，
    它们服务的是"投影在、快照还没算出来"那一格；`grep -rn snapshot_not_found crates/` 的另一侧读者是
    `crates/qx-api/tests/account_projection_identity.rs`（本轮改成两条通道分别核对，见下）与 `deploy/README.md`。
  - "投影在、快照还没算"**没有**被并进 404：那一格文档承诺的是 200 空数组/`null`，与"这个账户根本不在这份部署里"
    是两件事，合并成一个码就再也读不回来 —— 这条边界由用例 `projection_without_a_snapshot_keeps_the_empty_200_contract`
    单独钉住，而不是靠这段散文。
- `deploy/README.md` 两张端点表逐行补上 `404 account_projection_not_found`：第一张（返回/说明）改 2 行
  （`/account/snapshot` 与 `/account/orders`+`/account/positions` 那条合并行），并在表下那段「七条读投影的入口」
  里补出 404 口径、`/account/snapshot/diff` 的豁免理由，以及"投影在但快照还没算出来仍是 200 空数组/`null`"；
  第二张（语义/非 200 列）6 行逐行补码名。`GET /metrics` 那一行与「指标出口」小节同时补上
  `qx_api_command_enqueue_failures_total`（#189 的第 4 条样本），`POST /control/commands` 那一行写明
  "先持久化再入队，入队失败不回滚受理、计入这条计数"。
  文档手抄的码名沿用 #182/#180/#193 的口径由常驻判据核对：
  `crates/qx-cli/src/tests/api_endpoint_table_routes.rs` 的 `non_200_column_names_match_the_read_face_implementation`
  本轮实测被 M191b 变异打红过一次（把两张表里那个新码名整体改掉即红，见"本轮实测"）。

### Added（#189：已受理的控制命令写不进执行队列，有了唯一一条能被看见的通道）

- `crates/qx-api/src/lib.rs`：`ApiMetrics` 多一颗 `command_enqueue_failures_total`，`ApiMetricsSnapshot` 多一个同名字段，
  `/metrics` 的 exposition 因此多第 4 条样本（`HELP`/`TYPE` 齐，仍是第七遍定下的逐行 LF 形状）；
  `let _ = enqueuer(queued_command, ts);` 换成 `if let Err(error)`：计数 +1 并
  `eprintln!("[qx-api] 控制命令入队失败，等 worker 补入: {error}")`。
  - **为什么不推翻 202、也不改成 503**：命令此刻已经落进控制面（审计与待办都有它），worker 每轮按 `pending()`
    会把同一条命令补进队列，所以"受理成立"是真的；第十一遍 §9.18 写的"没有证据说哪一侧是运维想要的"针对的是
    状态码，而不是"这件事要不要留痕"。本轮选的是后者：把"补入了"从一句假设降成一条可被抓取的计数 + 一条进程日志，
    状态码与两张端点表都不动。
  - 用例 `failed_command_enqueue_is_counted_without_retracting_the_acceptance`
    （`crates/qx-api/tests/prometheus_exposition.rs`，第七遍为 #177 立的按行解析文件，本轮加这条）：注入必然失败的
    enqueuer，断言回执仍是 202、`qx_api_command_enqueue_failures_total` 从 0 变 1；并带反向对照 —— 入队成功时这条计数
    不动，否则它就不是失败计数而是提交计数。取值走 `sample_value`，同一个名字在一份 exposition 里出现两次即判红。
  - 单元测试侧同步收口：`crates/qx-api/src/lib.rs` 内联用例原来那句 `metrics.body.contains("qx_api_requests_total")`
    换成**数非注释样本行 == 4**，因为 `contains` 分不清"有这个名字"与"这个名字是一条样本"（第七遍的同一条理由）。

### Added（#190：Strategy worker 的失败当场收口，重投不再二次执行）

- `crates/qx-cli/src/scheduler.rs`（270 → **356 行**）新增三件，都是 worker 侧要用的口径而不是新的能力：
  - `strategy_run_is_final`：认 `Succeeded`/`Failed`/`NeedsIntervention` 三种收口结局。**读不到运行按"未终态"处理**
    （实时策略作业的运行从来不入 Scheduler 状态，把它当终态会让这类作业永远跑不了），`Paused` 也不是终态（恢复后仍要能执行）。
  - `fail_strategy_job_run`：把作业体的错当场写成 `JobStatus::Failed` + 固定错误码 `STRATEGY_JOB_FAILED`
    （`finish_run_with_code(run_id, false, Some("STRATEGY_JOB_FAILED"), ts)`）。`live-strategy:` 前缀与成功收口共用同一条豁免；
    非实时作业在 Scheduler 里缺运行则**报错**（`UnknownRun`）而不是静默当成已收口。
  - `live_strategy_job_is_stale`：执行前与执行中两处快照指纹判定共用的一段收成一份（#190 顺带收口，纯搬运）。
- `crates/qx-cli/src/workers.rs`：`for queued in pending` 的循环体收成 `let outcome = (|| -> Result<(), String> { … })()`，
  `Err` 先过 `fail_strategy_job_run` 再原样上抛（`workers.rs:301`、`:502`）；领取租约之后先问 `strategy_run_is_final`，
  已终态就走"确认并跳过"（`:290`）；`let pending = if matches!(strategy.state, Running) { … } else { Vec::new() }`（`:252`）
  把原来嵌两层的循环去掉一层。成功收口那一处 `.finish_run_with_code(queued.run.run_id, true, None, lease_now)` 的形状**没动**
  —— 门禁 `scheduler_retry_honesty_check` 钉的是剥空白后恰好 1 处（`tools/check_architecture.py:3969` 起）。
  - **没有接自动重试**：`next_retry_ts` 在默认 `max_attempts=1` 下仍是 `null`（由用例第一条当场断言），
    `retry_run`/`retry_run_at` 在 `qx-scheduler` 之外的生产调用点仍是 **0**（同一门禁的清点，实测见"本轮实测"）。
    `maturity/capabilities.yaml` 的 `scheduler_run_retry_has_no_production_path` 因此是**改写**而不是撤销 —— 撤销它
    要连带给出"这条作业失败后可安全重放"的判定依据，那是独立决策，不在发布前的顺手范围。
- `crates/qx-cli/src/tests/strategy_job_terminal_state.rs`（新用例文件 **266 行**，挂载进 `tests/mod.rs`）四条：
  - `strategy_job_failure_finalizes_its_run_instead_of_waiting_for_a_timeout`：作业体报错上抛身份不被改写
    （仍是"读取 Strategy target snapshot 失败"），运行落 `Failed` + `STRATEGY_JOB_FAILED` + `next_retry_ts == None` +
    `attempt == 1`，且**条目不消失、不进 `done`**（失败只负责"不回自己"，把条目留在队列里等租约过期后人工或重投）。
  - `a_still_running_strategy_job_is_executed_and_succeeds`：同一夹具只把 instrument 换成合法值，运行仍 `Running`
    就必须照常执行并回写成功 —— 证明上一条红的是终态判据，不是夹具本身。
  - `a_redelivered_final_run_is_acked_without_a_second_execution`：三种终态各造一次"回写之后、`ack` 之前掉电"的形态
    （条目带的是入队那一刻的 `Running` 副本），重投一律"确认并跳过"，`error_code`/`attempt` 原样保留，条目走 `done` 离开队列。
  - `unknown_and_live_strategy_runs_stay_out_of_the_final_gate`：两条豁免边界 + 一条"缺运行必须报错"。
- `crates/qx-cli/src/strategy_binding.rs`（308 → **355 行**）：五家策略后端的选择梯抽成
  `evaluate_strategy_contract`（`builtin_strategy` > `python_module`（常驻优先，没有就冷启动一次）> 外部可执行 > C ABI > 配置直给目标仓位），
  判定顺序逐字搬运，注释写明"worker 侧新增一档只能加在这里，不允许在调用点再写一份 if-else 链"。
  抽出它的直接原因是把 `outcome` 闭包的捕获面压回行数棘轮以内，不是设计上的偏好；`#[allow(clippy::too_many_arguments)]`
  携带的是那三份可变的客户端句柄，也不是新抽象。

### Changed（#192：启动器删掉 PowerShell 副本，只留 runtime-check 前置闸门）

- `deploy/start-qianxing.ps1`（**31 行**，本轮实测 `wc -l`；`git diff --stat` 记的是 95 行改动）：角色到进程入口的映射、
  按 worker 分离的 `process-logs/<worker>.out.log`/`err.log`、以及"任一受管 worker 退出就停掉其余 worker"的 fail-fast
  生命周期整体交给 `qx-cli supervise`（`crates/qx-orchestrator` 的 `plan_workers`），脚本只保留 `runtime-check` 闸门与
  `-AllowUnmanagedRoles` → `--allow-unmanaged-roles` 的旗标翻译。
  - 那份副本漂过四处，本轮逐条点名（不是"可能有漂移"）：CCXT 的 `endpoint` 只对 `spread_recovery` 一个角色生效；
    paper 判定用精确字符串而不是 `VenueFamily` 归一 —— R1-A3 把 venue 识别收成单源之后，Rust 侧对 `paper-proxy` 是
    **拒绝起进程**，脚本副本却会静默把它派给币安那条线（带着凭据）；`plan_workers` 会拒绝的拓扑在这里被静默托管；
    有内建入口的 `outbox_relay` 与 `event_consumer` 在这里被当成不可托管。副本另有一条"没有任何可托管 worker 时
    进入不退出循环"。
  - 实跑（`logs/s243_pass12_ps1_delegates_to_supervise.txt`）：桩 binary 记录 argv 两跑，各 `PS_EXIT=0`，转发序列是
    `runtime-check <config>` 然后 `supervise <config>`，第二条多 `--allow-unmanaged-roles`。
  - **这条修法本身是缺口登记的一部分**：门禁的取数范围里没有 `*.ps1`（`tools/check_architecture.py` 扫的是
    `crates/**/*.rs`、`deploy/*.md`、`tools/`、`maturity/`），所以本轮不可能"顺手加一条判据"。删副本让那四处漂移
    **无法再与 Rust 侧并存**，是把"两份实现会漂"换成"只有一份"，不是把它测住了。补真正的判据要动门禁本体，
    按本轮约定归协调者（#195 家族）。

### 行数与预算（本轮的三处口径改动，如实点名）

- `crates/qx-api/src/lib.rs`：**3080 → 3143**，`maturity/line_budgets.yaml` 的 pin 本轮**上抬一次**。这是 #191 的
  分派前闸门 + #189 的计数出口（含两处文档注释）落在同一个读面上的结果；上抬而不是压行，是因为这两条判据要钉的正是
  "读面自己那一格"，压成 helper 会让判据退化成对 helper 的测试。第十一遍那次（`3082 > 3080` 红）用的是折回两行的办法，
  本轮不适用：新增的不是断言而是生产分支。
- `crates/qx-cli/src/workers.rs`：**584 → 526**，pin 按当轮实测下修到 **526**。下行来自循环去嵌套、
  五档梯子抽出、以及成功/失败两条收口并成一个 `outcome` 闭包；`#190` 加的是判据而不是行数。
- `crates/qx-cli/src/scheduler.rs`（270 → 356）与 `strategy_binding.rs`（307 → 355）不在行数棘轮的登记表里，
  本轮没有为它们新增登记项。

### 一处 formatter↔门禁的形状耦合（本轮实测抓到，值得立案）

门禁的 `LEASE_CALL_SITES` 按**剥掉空白之后的字面量**认租约调用，而
`queue.ack_at(queued.run.run_id, context.id(), lease.fencing_token, lease_now)` 的四个参数合计 63 列，超出 rustfmt 的
`fn_call_width`（60），rustfmt 一律竖排并补一个尾逗号 —— 那个字面量在剥空白文本里就不存在了，于是门禁红。
本轮的两次处理各不相同：① 终态闸门那一处写 `#[rustfmt::skip]` 并在上一行留一句理由（语句级 skip 被 `cargo fmt` 尊重，
实测 `cargo fmt -p qx-cli -- --check` 仍 `exit 0` 且形状保持）；② `claim_command` 那处**不靠 skip**，改用 rustfmt 自己给出的
`let claim =\n    command_queue.claim_command(command.command_id, context.id(), lease_now, 30)` 形状，字面量因此保住。
- 排查代价如实记：孤立的 `rustfmt --check` / `--emit stdout` 探针在**不可解析或缩进错位**的副本上会返回 `exit 0`/空输出
  （本轮用一处故意写坏的行做哨兵，`logs/s248_sanity*.rs` 那批探针没把它报出来），所以形状测试只能打在真文件上、
  用 %TEMP% 镜像还原。**HEAD 的 workers.rs 确实是 rustfmt 的不动点**（故意破一行会被报、`--emit stdout` 与文件逐字节一致），
  所以这不是"仓库里本来就有 formatter 与门禁打架"，而是本轮的嵌套层级一变就让 rustfmt 换了选择 —— 试过的六组内联形状
  与三组缩进变体在新上下文里都不成立。
- **常驻含义**：一条按形状取数的判据会被任何一次缩进层级变化打红，而目前唯一找到的逃生口是 `#[rustfmt::skip]`。
  补一条"判据字面量必须能被 formatter 保住"的检查要动 `tools/check_architecture.py`，归协调者。

### 门禁自身的一处口径腐坏（在册，归协调者）

`scheduler_retry_honesty_check` 的 docstring（`tools/check_architecture.py:3927` 起）仍写
"`finish_run_with_code` 在 CLI 侧唯一的调用点以 success=true 收口" —— #190 之后这句不再成立（失败侧多了一个调用点，
在 `crates/qx-cli/src/scheduler.rs`）。判据本身没红，因为它钉的是 `SCHEDULER_WORKER_FILE` 里那句成功收口的剥空白计数 == 1，
新的失败调用点不在被扫文件内；但那条 limitation 的说明文字已经指向一个不存在的"唯一调用点"，
`maturity/capabilities.yaml` 本轮已改写，门禁注释要同步就得动门禁本体，按约定归协调者（#195 家族）。

### 本轮实测

| 量具 | 当轮实测 | 日志 |
| --- | --- | --- |
| 架构门禁 | 文档拼接**前** `logs/s251_*.txt`、拼接**后** `logs/s252_final_gate_pass12.txt`、以及把日志引用改成具体编号之后的**最后一次** `logs/s254_final_gate_pass12_after_refs.txt`，三次都是 **515 项 PASS / `exit 0`**（`grep -c "^\[PASS\]"` 数得 515）| `s251_v13_r2_pass12_gate_before_docs.txt`、`s252_final_gate_pass12.txt`、`s254_final_gate_pass12_after_refs.txt` |
| `qx-api` 定向（`cargo test -p qx-api`） | 7 个测试目标 **33 passed / 0 failed**：`23 + 3 + 1 + 2 + 2 + 2 + 0` | `s244_pass12_qx_api_tests.txt` |
| #189/#191 变异（3 发） | `M191a_disable_projection_404_guard`、`M189_swallow_enqueue_failure`、`M191b_unname_the_404_code_in_the_doc` 各 `red_exit=101 / green_exit=0`，红的都是具名用例（`unknown_projection_key_is_404_on_every_scoped_read_face`、`failed_command_enqueue_is_counted_without_retracting_the_acceptance`、`non_200_column_names_match_the_read_face_implementation`），`MUTATION_PAIRS_OK=True` | `s245_pass12_mutations_189_191.txt` |
| #190 变异（4 发，终树复跑） | 基线 `4 passed`；M1 闸门失效 → 红 1 条、M2 失败一律豁免 → 红 2 条、M3 未知运行当终态 → 红 1 条、M4 跳过不确认 → 红 1 条；还原后 `RESTORED_GREEN=True`，`MUTATION_PAIRS_OK=True`，每份红里都有 `test result` 行（不是坏树） | `s250_pass12_mut_190_rerun.txt` |
| #190 变异的**首跑不作数** | `s246` 那台机器打的是 `#[rustfmt::skip]` 与 `let claim =` 两处形状修正**之前**的树（workers.rs sha `5fdd581bbb62`，终树是 `0f16093de481`），M4 的锚点文本在终树里已不存在；因此换全新镜像目录按终树复跑 `s250`，本轮采信 `s250` | `s246_pass12_mut_190.txt`、`s250_*.txt` |
| `qx-cli` 整包（`QX_PYTHON=… cargo test -p qx-cli --bin qx-cli`） | **243 passed / 0 failed**。不设 `QX_PYTHON` 时同一目标 **241 passed / 2 failed**（两条 e2e Python 契约用例在 `crates/qx-cli/src/tests/e2e_and_python_contract.rs:455`、`:499` 抛错）—— 这两条要解释器，缺它是环境不是缺陷，故本行给的是设了 `QX_PYTHON` 的那一次 | `s253_v13_r2_pass12_qxcli_all.txt` |
| 文档向用例复跑（六个模块过滤器：`artifact_identity_doc` / `api_doc_cross_references` / `api_endpoint_table_routes` / `api_response_field_doc` / `control_command_executor_coverage` / `strategy_job_terminal_state`） | **17 passed / 0 failed**（`QX_PYTHON` 已设）—— 这一组里既有 #191 的两张端点表码名核对，也有 #190 的四条终态用例，全部在文档改完之后复跑 | `s255_doc_cases_after_pass12_docs.txt` |
| 启动器委派桩跑 | 两跑各 `PS_EXIT=0`，argv 序列 `runtime-check <cfg>` → `supervise <cfg>`（第二条带 `--allow-unmanaged-roles`） | `s243_pass12_ps1_delegates_to_supervise.txt` |
| 格式 | `cargo fmt -p qx-cli -- --check` **exit 0**（含 `#[rustfmt::skip]` 那两处形状选择，输出为空） | `s252a_fmt_qxcli.txt` |

- **`assert_binary_fresh` 的一次自我干扰（不是代码缺陷，如实记）**：镜像还原会把 `crates/qx-cli/src/**` 的 mtime 拍到
  已编译 binary 之后，于是 13 条子进程用例在 `crates/qx-cli/src/tests/mod.rs:345` 判红（"被测 binary … 比 … 旧"）。
  复跑前补一次 `cargo build -p qx-cli` 即回到 243/243。**常驻含义**：任何用 `cp`/`shutil.copy2` 还原源码树的轮次，
  紧接着跑 `cargo test --bin` 都必须先重链 binary，否则测的是"binary 比源码旧"这件事本身。
- **两处自我指涉**：`s251` 跑的是"代码、判据与 `deploy/README.md`/`capabilities.yaml` 均已定稿，但本章与 V13 §9.19
  还没拼进正式文档"的那棵树；`s252_final_gate_pass12.txt` 跑的是**把本章与那一节写进去之后**的那棵树（515 项 PASS / `exit 0`）；
  `s253` 那次整包测试与 `s252a` 那次格式检查跑在 `s251` 之后、`s252` 之前，因为它们之后落进树的就只有文档。
  下面这三行引用改成具体编号之后又复跑了一次门禁，记在 `s254_final_gate_pass12_after_refs.txt` —— 那一次覆盖本条以外的
  全部改动，被它覆盖的只有散文与日志编号；`s255` 是那之后按六个文档向模块过滤器的定向复跑（17 条全绿）。
  **这两份文件本身不在任何判据的取数范围内**（本轮核对：`grep -rn "CHANGELOG" tools/ crates/` 只命中
  `crates/qx-cli/src/tests/artifact_identity_doc.rs:9` 那句"取数只看两份交付文档，`docs/` 与 `CHANGELOG.md` 的执行记录
  不参与"，以及 `init_onboarding.rs:262` 的一句历史说明），所以 `s254` 之后落进树的本章与 §9.19 那两段散文不需要第三次门禁；
  代码与判据自 `s250` 起未再变过。

### 在册未做

- **本轮新登记两条门禁缺口**（都归协调者，因为补判据要改 `tools/check_architecture.py`）：
  ① `LEASE_CALL_SITES` 这类**按形状取数**的判据与 rustfmt 的宽度预算冲突，本轮以 `#[rustfmt::skip]` 绕过并留了理由，
  但没有任何东西阻止下一次缩进层级变化再把它打红，也没有东西检查"`skip` 用了几处、为什么"；
  ② `*.ps1` / `*.psm1` 不在任何判据的取数范围内，#192 的"删副本"因此是**唯一的**可用修法。
  ③（沿第十一遍）文档/散文侧的逐字重复块没有判据 —— 即 **#195**。
- `#174`（字段级零读者判据）、`#169`（事件日志无压缩/保留）、`#141`/`#152`/`#157`/`#186`、`#144`、`#53`、`#106`/`#107` 照旧在册。
- `JobStatus::Failed` 现在有了生产者，但**到期重试链仍没接**（`next_retry_ts` 恒 `null`、`retry_run`/`retry_run_at` 零生产调用），
  这是刻意的：交易作业失败那一刻无法判定订单是否已经出网，自动重跑等于二次提交。


## Unreleased — V13 R2 第十一遍：内核热路径去掉一次 O(n²)，控制面把"受理了却永远没人执行"的五类命令改成提交即拒绝（#187 / #188 / 补段 #193）（2026-09-27）

这一遍的两件事都来自读代码，不来自一次红案 —— 它们的共同点是**今天还能跑，跑久了或接久了就说假话**。
`crates/qx-core/src/sourcing.rs` 的 `EventLog::append_checked` 每收一条事件就把整份 `events` 扫一遍找重号，
而 `ReplayVerifier::replay` 每次投影都要把整条日志重新过一遍这个入口，于是"重放一场 N 条的事实流"是
O(N²) 而不是 O(N log N)；这正是 #169（长跑无压缩/保留）账上的一笔，只是 #169 问的是留多少条，这一格问的是
每留一条付多少钱。`crates/qx-control/src/lib.rs` 的 `CommandKind` 有八颗变体，全仓生产源码里提到命令类型的
派发者只有 5 份文件（`crates/qx-cli/src/api_service.rs`、`workers.rs`、`venue_runtime/` 的 paper/binance/ccxt
三份），覆盖 `SubmitOrder` / `PauseStrategy` / `ResumeStrategy` 三颗；另外五颗（`ChangeRiskLimit`、
`CancelOrder`、`ReconcileAccount`、`RetryJob`、`SwitchVenue`）没有任何执行者，提交入口照样回 `Accepted`
并把命令写进审计与队列，运维读到"已受理"的那一侧什么都不会发生；这一格的**文档侧**（接口文档只说"没有派发者"
却不说是哪五颗）在同一遍里补成名单＋常驻判据（#193）。详记 V13 §9.18，日志
`logs/s205_probe_speech_quotes.py`—`logs/s242_final_gate_pass11.txt`。

### Changed（#187：重复序号检测与日志长度脱钩）

- `crates/qx-core/src/sourcing.rs`：`EventLog` 多一个 `seqs: BTreeSet<u64>` 字段，`append` 与
  `append_checked` 在 push 之前/之后各插一次，重号判定从 `events.iter().any(...)` 换成 `seqs.contains(...)`。
  - 语义等价性的根据是 `events` 全仓只有两处 push（`append` / `append_checked`），`from_json` 走前者，
    所以 `seqs` 与 `events` 的 seq 集合恒等；这条由新用例的 `validate_still_catches_duplicates_that_append_allowed`
    与 `increasing_logs_replay_and_digest_as_before` 钉住，而不是靠这段话说服读者。
  - 文件定稿 **567 行 = 登记的预算上界**，**没有抬预算**；腾出来的行数是靠把两处说明压成单行（`seqs` 字段的
    文档注释 2→1、`Fnv1a::new()` 的豁免理由 2→1）—— rustfmt 会把长 `matches!` 与多字段结构体字面量重新展开，
    所以想省行数只能省散文，这一点本轮再次实测（`Self { h: … }` 压成一行后被 `struct_lit_width` 打回三行）。

### Removed（零读者的公共面与"受理即谎言"）

- `impl Default for Fnv1a`（`crates/qx-core/src/sourcing.rs`）：全仓 60 处 `Fnv1a` 站点一律走 `new()`，
  `Default` 形态只有定义、没有读者。
  - **删它被构建挡回来一次**（当轮实测，日志 `logs/s215_*.txt` 的 `[5/9]`）：`cargo clippy -D warnings` 的
    `new_without_default` 只认"`pub fn new()` 存在"这个形状，不看有没有读者，于是九步整跑在 Clippy 步红；
    而架构门禁的零读者判据**覆盖不到 trait 实现**（#135 登记的已知缺口），所以"门禁 515 项 PASS"并不等于
    "这个公共面删得掉"。收口方式是在 `new()` 上写 `#[allow(clippy::new_without_default)]` 并留一行理由
    （"零读者公共面已删，这条 lint 只认构造函数形状"），而不是为一条 lint 补回一颗没人调的 `Default`。
    原稿里"`Default` 是唯一能绕开初始化契约的口子"这句**作废**：`Default::default()` 与 `new()` 是同一个初值，
    删它省的是公共面，不是正确性。
- 五类命令由"提交即受理"改为**提交即拒绝**（`crates/qx-control/src/lib.rs` 的 `submit_validated` 新增
  `CommandKind::executed()` 闸门，报 `ControlError::Invalid("… 在当前构建里没有派发者，控制面不接受")`）。
  - 有意破坏的兼容面：这三颗以外的命令今天在生产里没有任何执行者，所以仓内读者只有用例；
    已 grep 的旧写法读者收口清单是 `crates/qx-storage/tests/control_audit_chain_wired.rs`（`CancelOrder` →
    `PauseStrategy`，该文件钉的是审计链而不是受理本身）与 `crates/qx-api/src/lib.rs` 的一条 202 用例
    （同改），另有两处 `CancelOrder` 用例期望 403（权限先判）保持原样。
  - 规则**只放在提交入口，不放进 `ControlCommand::validate`**：`validate` 同时被 `qx-control` 自身的
    恢复路径与 `crates/qx-storage/src/lib.rs` 的读取路径调用，放进那里会让历史队列里已存在的旧命令在重启时
    读不回来（把"这份构建不受理"写成"这份存档坏了"）。

### Added（两条常驻判据 + 一把代价量具）

- `crates/qx-core/tests/event_log_seq_index.rs`（新集成测试文件，5 条用例，4 跑 1 挂 `#[ignore]`）
  - 四条功能用例：跨位置重号必拒（且拒时 `len()` 与 `next_seq()` 不变）、`append` 载入的 seq 同样算"见过"、
    `validate` 仍抓 `append` 放行的重号、递增日志的重放摘要与改前一致。
  - `replay_cost_scales_near_linearly_with_event_count`（`#[ignore]`，复跑命令
    `cargo test -p qx-core --release --test event_log_seq_index -- --ignored --nocapture`）：20,000 条与
    40,000 条各整场重放一次，断言倍率 < 3.5。**第十一遍实测 0.0166s → 0.0325s，倍率 1.96**（`logs/s207_*.txt`）；
    这条量具是 #187 唯一的复杂度判据，四条功能用例在换回整档扫描后照样全绿。
- `crates/qx-cli/src/tests/control_command_executor_coverage.rs`（新用例文件 196 行，挂载进 `tests/mod.rs`）
  - 行为侧 `control_plane_accepts_only_the_kinds_that_have_an_executor`：八颗逐个过真实提交入口，断言受理名单
    恰好三颗、拒绝名单恰好五颗，且**拒绝时审计为空、待办为 0**（"拒绝"不能是"受理后标失败"）。
  - 源码侧 `control_command_kinds_match_executors`：`executed()` 为真的名字必须出现在生产派发者文件里，
    为假的名字在任何生产源码里都不许以 `CommandKind::<名字>` 出现；名单里 5 份文件各须真的提到命令类型，
    外加"至少 3 份"的地板，防止名单腐坏成空目录后判据自己骗绿。
  - 清单侧 `command_kind_all_enumerates_every_variant_in_the_source`：`ALL` 与源码 `enum` 体的变体数量与顺序相等。

### Added（补：受理面的文档侧也要有人核对，#193）

- `deploy/README.md` 的 `POST /control/commands` 一节补出两行名单：有派发者、能被受理的
  `SubmitOrder`/`PauseStrategy`/`ResumeStrategy`，与没有派发者、提交即以 400 拒绝的
  `ChangeRiskLimit`/`CancelOrder`/`ReconcileAccount`/`RetryJob`/`SwitchVenue`；并写明这条闸门
  只在提交入口判、不进存档校验。改动是**字节手术**（该文件纯 CRLF，见 `logs/s225_deploy_readme_acceptance_face.py`）。
- 用例 `control_plane_acceptance_kinds_are_listed_in_the_ops_doc`（`crates/qx-cli/src/tests/control_command_executor_coverage.rs`，
  文件 196 → **244 行**）：按**行前缀**取那两行名单，要求被点到的变体恰好等于 `executed()` 的真/假两侧，
  外加"同一颗不许进两行"。
  - 为什么按行不按全文：八颗的名字在部署文档里到处出现（`SubmitOrder` 更贯穿全篇），数子串等于不判。
  - 三发变异（`logs/s226_v13_r2_pass11_docface_mutation_report.txt`）：d1 把 `PauseStrategy` 从受理行挪进
    拒绝行 → `3 passed; 1 failed`，红在那颗名字上；d2 把 `CancelOrder` 同时写进两行 → 同样只红这一条；
    d3 只改判定式（`executed()` 去掉 `PauseStrategy`）而文档不动 → 三条一起红（行为侧、源码侧、文档侧）。
    每发还原后复绿 `4 passed`，三处 `RESTORE_IDENTICAL=True`，`ERROR_LINES=0`（红是有用例判红，不是坏树）。
  - 这一项补的是 #157/#182 那条老账的又一面：**文档里手抄的名单没有判据就会腐坏**。本轮之前
    那一格只写"该命令类型在当前构建里没有派发者"，读者无法知道哪三颗能用；现在名单在文档里，
    且改档位不改文档会红。

### 本轮实测

| 量具 | 当轮实测 | 日志 |
| --- | --- | --- |
| 重复序号与伸缩量具 | 定向 `4 passed / 1 ignored`；`--ignored` 那条 **倍率 1.96**（阈值 3.5） | `s207_*.txt` |
| #187/#188 变异矩阵（8 发 / 10 项判定） | 终树复跑 **10/10 合预期 / `MISMATCH=0` / `RED(BROKEN_TREE)=0` / `RESTORE_IDENTICAL=True` 8 处**；把检测换回整档扫描后量具测得 **倍率 4.35**（修复后 1.96） | `s219_*.py`、`s220_*/`、`s219_*_report_b.txt` |
| 架构门禁 | 除首跑外每次都是 **515 项 PASS / `exit 0`**：实跑清单是 `s214`（首跑红在 `qx-api/src/lib.rs 3082 > 预算 3080`）、`s216`（把 `/metrics` 断言折回两行后转绿）、`s217` 里的 `[1/9]`、两份文档按 §9.18 拼进正式文档之后的 `s224`、#193 的文档名单与用例定稿之后的 `s227`、以及本轮最后一段文档（#193 补段）拼进本章与 V13 之后的 `s230`、收口段自身定稿之后的 `s232`、按 #194 重打包并最终收口之后的 `s239`、把这条口径写进 README 安装面之后收尾复跑的 `s240`、以及把 `s241` 那次复跑数字写进本章之后的再一次复跑 `s242` | `s214_*.txt`、`s216_*.txt`、`s224_gate_after_docs.txt`、`s227_gate_after_docface.txt`、`s230_gate_after_docs_pass11.txt`、`s232_final_gate_pass11.txt`、`s239_final_gate_pass11.txt`、`s240_final_gate_pass11.txt`、`s242_final_gate_pass11.txt` |
| 接口文档受理面（#193） | 三发变异 `3 passed; 1 failed`、`3 passed; 1 failed`、`1 passed; 3 failed`，每发还原后 `4 passed`；三处 `RESTORE_IDENTICAL=True`、`ERROR_LINES=0`（红全部有用例名，不是坏树）。文档改动前后字节核对：79,644 → 80,396，`crlf_only=True`，两行名单前缀各命中 1 次。文档全部拼完之后定向复跑文档向用例：**13 passed / 0 failed**（`s231`）。README 安装面补段之后，再按五个模块名过滤器（`artifact_identity_doc` / `api_doc_cross_references` / `api_endpoint_table_routes` / `control_command_executor_coverage` / `worker_pipe_failure_diagnostics`）复跑一次：**11 passed / 0 failed**（`s241`）—— 两次过滤器集合不同，故条数不同，共同点是 0 failed | `s225_*.py`、`s226_*.txt`、`s231_doc_cases_after_final_docs.txt`、`s241_doc_cases_after_readme.txt` |
| 九步整跑 | 首跑 **`BUILD_EXIT=1` 红在 `[5/9]`**（`new_without_default`，见 Removed 段）；修后重跑 **`BUILD_EXIT=0`**：`[1/9]` 516 行 `[PASS]` / 0 `[FAIL]`、`[4/9]` **94 个测试目标 / 895 passed / 0 failed**、`[5/9]` Clippy 无新增 warning、`[6/9]` `Ran 57 tests … OK (skipped=1)`、`[7/9]` 成交=22、`[9/9]` `config_fingerprint=aada66156749d230…`；#193 补段之后再整跑一次同样 **`BUILD_EXIT=0`**（`===== 全部完成 (all gates passed) =====`）：`[1/9]` 516 行 `[PASS]` / 0 `[FAIL]`、`[4/9]` **94 个测试目标 / 896 passed / 0 failed**（比上一跑多的那 1 条正是 `control_plane_acceptance_kinds_are_listed_in_the_ops_doc`）、`[6/9]` `Ran 57 tests … OK (skipped=1)`、`[7/9]` 成交=22 手续费=11.32297 终值=100031.63703、`[9/9]` 指纹 `aada66156749d230…` 与上一跑一致 | `s215_*.txt`（失败）、`s217_*.txt`、`s228_build_bat_full.txt`（含 #193 的终态） |
| 安装包重建与载荷复核 | 本轮 wheel **217,416 字节 / sha256=`18b1d54fac7d5924…`**；干净 venv（`uv venv --python 3.12` + `uv pip install --offline --no-deps`）内四包导入 OK、`native.available() -> True`、线格式写出/读回一致、4 条 `StrategyIntent` 契约按文案抛 `ValueError`；已安装 `qianxing_ccxt/worker.py` 与仓库**逐字节相同**；对上一留档 wheel（217,415 字节）比较：**17 条目、名称集合相同，CRC 真变的载荷只有 2 条**（`_qianxing_native.pyd`、`dist-info/RECORD`），另 3 条仅 zip 时间戳变化；wheel 内 `.pyd`（353,280 字节）md5 ≡ 本轮 `_qianxing_native.dll` md5；wheel 内每份 `.py` 对仓库 `python/` 逐字节不一致 **0 份**；`qx-cli.exe`（11,999,232 字节 / md5=`89c9f6cb9d070fd3…`）里 #188 的理由字面量 **1 次**、#172 两条各 **1 次**、#177 样本名 **3 次**。该行测的是本轮第一次打包的那一件，它随后被下行 #194 的终树重打包件取代；对**最终交付件**的四包导入复验在下行（`s238`）。 | `s221_*.txt`、`s222_*.txt` |
| 安装包按**终树**重打包（#194） | 收口后复跑载荷核对，`.pyd ≡ dll` 那条**翻成 False**（wheel 内 `6e84adeea3af87ae…` vs 终树 dll `6af430b25a03ca93…`，同为 353,280 字节）—— 变异复跑与九步整跑各自重链接过 dll，源码没动。按 `tools/build_python_wheel.sh` 从终树重打：`WHEEL_EXIT=0`、**217,416 字节 / sha256=`34b21d36a3a2ffea…`**（重建前先 `cp -p` 旧件进 `%TEMP%\qx_pass11_final_wheel_before`，md5 `7eff4c2488749e75…`）；对新旧两份逐条目比 CRC：**17 条目、名称集合相同，真变只有 2 条**（`.pyd` 353,280→353,280 重链接、`RECORD` 1,885→1,885），3 条仅 zip 时间戳；新 wheel 内 `.pyd` md5 `cd5739906f519441…` **≡ 终树 dll**（True）；**12 份 `.py` 对仓库 `python/` 逐字节不一致 0 份**；exe 字面量计数与上一轮一致（#188 的理由 **1** 次、#172 两条各 **1** 次、#177 样本名 **3** 次）；再把**这一份**终树 wheel 装进干净 venv（`uv venv --python 3.12` + `uv pip install --offline --no-deps`，另补 `tzdata`），从**已安装**侧复跑同一套判据：四包导入 OK、`native.available() -> True`、线格式归一为 `buy cross hedge 3` 且读回一致、**4/4** 条非法值各按契约文案抛 `ValueError`、已安装 `worker.py` 与仓库逐字节相同 `True`；对照件换成**本轮重建前拍的 before 快照**（217,416 字节 / sha256=`18b1d54fac7d5924…`）后，CRC 真变的仍是 `.pyd` + `RECORD` **2 条**、仅时间戳 3 条、17 条目名称集合相同、`.py` 不一致 **0 份** | `s233_*.py`、`s235_final_wheel_rebuild.txt`、`s236_release_surface_final_tree.txt`、`s238_release_surface_final_venv.py`、`s238_v13_r2_pass11_release_surface_final.txt` |

- **本轮发布面取证的一处流程缺口（如实登记）**：重建 wheel 前没有先给 `dist/` 里那份旧 wheel 拍 before 快照，
  所以 C 段的对照件只能用 `%TEMP%\qx_pass6_wheel_before`（2026-09-27 12:17 / 217,415 字节）这份更早的留档 ——
  它与本轮之间还夹着一次构建，因此"CRC 只变 2 条"证明的是**本轮载荷与最近留档一致**，不是"与上一遍定稿一致"。
  下一轮的重建步骤要先 `cp -p dist/*.whl` 进当轮镜像目录再做。**本轮已按这条补齐**：#194 那次重打包先做了
  before 快照（`%TEMP%\qx_pass11_final_wheel_before`），所以"真变只有 2 条"证的就是"终树载荷与上一份定稿件一致"。

- **README 安装面（#194 的读者侧）**：新增一段「最近一次重建安装包是 V13 R2 第十一遍（2026-09-27）」的现场（217,416 字节 / 17 条目、真改载荷 2 条、`.pyd` md5 `cd5739906f519441…` ≡ 终树 dll、12 份 `.py` 与仓库逐字节相同、干净 venv 四包导入 + `native.available() -> True` + 线格式往返 + 4/4 契约抛错、exe 11,999,232 字节里 #188 的播报 **1** 次），并把「安装包必须排在本轮最后一次构建之后」写成读者看得见的纪律。数字全部取自 `s235`/`s236`/`s238` 三份日志；`artifact_identity_doc.rs` 那条常驻判据继续核对 README 不许出现 64 位整档摘要、且载荷口径骨架七句齐在（改完后 `s240` 复跑仍 515 项 PASS）。
- **#194：`wheel 内 .pyd ≡ 当轮 dll` 是一条有窗口的核对，窗口外会假红**（本轮实测到，见 `s233_*.py` 首跑）：
  s221/s222 那次打包后，`s226` 的变异复跑与 `s228` 的九步整跑都重链接过 `target/release/_qianxing_native.dll`
  （源码未变，字节变了），于是收口后的复核把这条核对读成 **False** —— 它测的是"产物与**当轮构建**同源"，
  不是"产物与**终树源码**同源"。修法不是改口径，是**按终树再打一次包**（`s235`/`s236`，全部载荷项转绿）。
  常驻含义：安装包必须是"最后一次构建之后"的那一份；任何在其后跑过 `cargo build/test --release` 的轮次，
  发布面前必须重打一次 wheel，而不是引用早先那次比对的 True。

- **三处自我指涉如实说明**（沿用既有口径，不把"最后一次门禁"写成覆盖本行本身）：`s224` 跑的是"章节已拼进
  CHANGELOG 与 V13、代码与产物均已定稿"的那棵树，`s227` 跑的是"#193 的文档名单与第四条用例已定稿"的那棵树，
  `s230` 跑的是"#193 补段已拼进本章与 V13 §9.18"的那棵树，`s232` 跑的是"把收口段自己的日志编号指回 `s232`"之后
  （那一次拼接顺手贴出一个逐字重复的四行块，当场删掉；**文档/散文侧的逐字重复块至今没有门禁判据**，已立案为 #195 —— 补这条判据要改 `tools/check_architecture.py`，按本轮约定归协调者），`s239` 跑的是 **#194 的重打包、装好后在干净 venv
  里对交付件复验（`s238`）与本章最后一段说明定稿之后**的那棵树；`s240` 再跑一次，覆盖的是**把 #194 这条口径写进
  README 安装面（新增一段带日期的最新现场 + 一句"安装包必须排在最后一次构建之后"）并改掉上面这几处引用之后**的树 ——
  `s240` 覆盖的是那一次 README 改动，`s242` 则是把 `s241` 的复跑条数写进本章之后**最后一次**门禁 —— 它覆盖本行自身以外的全部改动，且改动的只是本章与 V13 的散文与日志编号。这几次的结果都记在下面那行"架构门禁"的清单里。代码与判据自 `s227` 起未再变过；`s227` 之后落进树里的只有文档散文、
  发布产物那一次重链接，与在干净 venv 里对交付件重跑的那套载荷复验（`s238`）—— **本轮没有新增任何门禁判据**，
  #195 仍是在册未做的那一条。
  首跑那份变异矩阵（`s209`/`s210`/`s211`）里有 2 发 `MISMATCH`，证据行是一行 rustc 编译错误，成因是脚本的
  %TEMP% 镜像目录被复用、每次还原都写回定稿前的形状，因此那份矩阵**不作数**，定稿树换全新镜像目录复跑的
  `s219`/`s220` 才是本轮用的红绿对（详记 V13 §9.18）。


## Unreleased — V13 R2 第十遍：worker 的同一次死亡在两条通道上说不同的话，断管道从"整跑里随机红"收成必然复现的常驻用例（#185）（2026-09-27）

第九遍收口后整跑九步，`[4/9]` 红在一条定向跑从未红过的用例上。取证结果不是"用例坏了"，而是它**只在
整跑负载下才走到另一半分支**：跨语言 worker 的 stdin 管道读端由子进程持有，子进程一退出，父进程写侧
只能拿到断管道（Windows `os error 232/109`、POSIX `EPIPE`）；`crates/qx-cli/src/strategy_host.rs` 的
三条写路径（Jsonl 写、分帧写、flush）把这种失败只拼上 `diagnostics()`（`; stderr=…`），而超时、响应通道
断开、读线程送上协议错误这三条通道用的是 `death_note()` 的 `（程序=…，退出码…；stderr=…）`。
同一个失败在两条通道上说不同的话，集成用例对 `程序=` 与来源的断言于是变成掷硬币：写侧恰好成功就绿，
恰好失败就红。**生产逻辑（协议、超时、kill、序号校验）没改**，改的是失败信息的通道归属与一条常驻用例。
详记 V13 §9.17，日志 `logs/s195_*.txt`—`logs/s203_*.txt`。

### Added（必然复现断管道的常驻用例）

- `crates/qx-cli/src/tests/worker_pipe_failure_diagnostics.rs`（新用例文件 79 行，挂载进 `tests/mod.rs`）
  - `dead_worker_write_failure_names_program_origin_and_exit_state`：用测试二进制自己当假 worker（它不认
    `-m`，打印一行错误就退出，且从不读 stdin），再把 `StrategyContractInput` 的 Bar 列填到 20,000 行 ——
    序列化后的 JSON 远大于 OS 匿名管道缓冲，于是"写侧必然失败"由缓冲溢出保证，不再依赖子进程死在
    `write_all` 之前这个调度巧合。断言四条：失败必须走写侧通道（含 `输入失败`）、必须含 `程序=`、
    必须含解释器来源、必须含子进程状态（`退出码` 或 `进程未退出`）与 `stderr=` 尾部。
  - 集成用例 `crates/qx-cli/tests/worker_launch_diagnostics.rs` 保持原样：它的 payload 只有一千多字节，
    管道缓冲吃得下，写侧成不成全看子进程什么时候死，所以它**只能**是读侧通道的判据 —— 这条事实由新用例
    的 `输入失败` 断言与 `r4_payload_shrunk_to_one_row` 变异（缩到 1 行即红）钉住。

### Changed（写侧并回共享措辞，行数不抬预算）

- `crates/qx-cli/src/strategy_host.rs`：`Jsonl` / `FramedJson` 的 write 与 flush 合成
  `write_result.and_then(|()| stdin.flush())`，失败统一 `format!("{failure}{}", self.death_note())`。
  - 报错文案两处口径变化，全仓旧文案读者为 0（grep `刷新 .* 输入失败` 命中 0 处，`分帧输入失败` 只剩
    源码里"编码"那一处；用例、`README.md`、`deploy/`、`docs/` 均未引用）：
    `写入 X 分帧输入失败` → `写入 X 输入失败`（是否分帧由 `编码 X 分帧输入失败` 那条说），
    `刷新 X 输入失败` 并入前者 —— write 与 flush 失败对运维是同一件事：管道已经没人读了。
  - 行数棘轮按 `strategy_host.rs` 已登记的 **801 行**执行，**没有抬预算**：改成 `and_then` 链并把说明压成
    两行之后定稿 **800 行**（比上一轮少 1 行）。预算被踩出来这件事本身写在这里，是因为它的解法只能是
    "改写"或"拆文件"，把 `max_lines` 调高等于把棘轮作废。
- 本轮**没做**的两件事，理由都是"没有当轮证据"：
  - 没给 `death_note()` 加"等 stderr 读线程收尾"的有界等待。s195 现场那条消息里 `stderr=` 是在的，
    缺的只有身份字段；读线程与写侧的收尾竞态本轮没有任何一份日志证明它咬过，加一个 200ms 的等待只是
    给不可复现的场景写兜底。
  - `decode_python_strategy_response` 那条通道仍只带 `diagnostics()`：它意味着 worker **活着**并给了答案、
    只是答案不合契约，身份与退出码在这里不是缺失信息。

### 本轮实测

| 量具 | 当轮实测 | 日志 |
| --- | --- | --- |
| 新用例（修复后） | `1 passed`；把 `QX_PYTHON` 指到可用解释器后与 Python 契约用例同跑 **2 passed / 0 failed** | `s196_*.txt`、`s198_*.txt` |
| 修复前那条消息长什么样 | 变异 `r1`（写侧还原成只报 stderr 尾部）复现出与 s195 完全同形的失败文本 `写入 Python Strategy 输入失败: 管道正在被关闭。 (os error 232); stderr=error: Unrecognized option: 'm'`，并被 `程序=` 断言当场判红 | `s201_*.txt`、`s197_mutation_details/r1_write_channel_back_to_diagnostics_only.txt` |
| #185 变异（8 发，红绿成对） | **8/8 全合预期**：四红（写侧还原成旧措辞、`death_note` 摘掉 `程序=`〔同时把集成用例判红〕、写侧失败被吞掉、payload 缩到 1 行）+ 四绿控制组（标点变化、消息合并后多一个斜杠、payload 涨到 40,000 行、stderr 尾部容量 16→8）。每发跑完与 %TEMP% 镜像**逐字节一致**（`RESTORE_IDENTICAL=True`） | `s201_v13_r2_pass10_mutations_final.txt`，逐发输出与 panic 摘录在 `logs/s197_mutation_details/*.txt` |
| 变异首跑的一次误判 | 首轮 `c1` 对照被记成红且拿不到 `test result` 行（cargo 瞬时失败，非判据问题）；手工复现同发变异为 `1 passed`，脚本补上"末 40 行原样落盘"后复跑 **8/8** | `s200_v13_r2_pass10_mutations_final.txt`（首跑）、`s201_*.txt`（定稿） |
| `qx-cli` 全量 | 12 个测试目标 **287 passed / 0 failed**（单元 235 条，含本轮新增那条） | `s202_v13_r2_pass10_qxcli_all.txt` |
| 架构门禁 | **515 项 PASS / `exit 0`**，含"单文件行数预算只降不升"一格转绿 | `s199_v13_r2_pass10_gate_after_185.txt` |
| 九步整跑（本遍收口） | **`BUILD_EXIT=0` / 全部通过**：`[1/9]` 门禁 **516 行 `[PASS]`、0 `[FAIL]`**（515 项判据 + `[9/9]` 的 runtime 校验那一行）、`[4/9]` **93 个测试目标 / 888 passed / 0 failed**、`[5/9]` Clippy 无 warning、`[6/9]` `Ran 57 tests … OK (skipped=1)`、`[7/9]` 成交=22 与重放①②③、`[8/9]` `ecosystem` 八段 ✓、`[9/9]` `config_fingerprint=aada66156749d230…` | `s203_v13_r2_pass10_build_bat_full.txt` |

上一遍红在 `[4/9]` 的那条用例（`silent_worker_reports_program_origin_and_stderr`）本轮整跑 **3 passed / 0 failed**，
而它的定向跑在修复前后都是绿的 —— 也就是说"这一条整跑里红过"这件事本身没有留下常驻判据，
留下的是新用例对写侧通道的断言。


## Unreleased — V13 R2 第九遍：端点表搬进了它该住的章节，搬完才发现"指代方向"是这条链的下一层（#183 / #184）（2026-09-27）

第八遍给第二张端点表补判据时，只核了"这张表列了哪些路由"，没核**这张表住在文档的哪一章**：两张表与
「### 端点表按张核对」小节当时整段挂在 `## Outbox 与 NATS JetStream` 下，运维在 API 那一章读不到
「非 200 口径」那一格。本轮把这一整段搬回 `## Paper API`（#183），并给"住在哪"补一条常驻判据；
搬家落地后逐条复查文档，**立刻撞出第二层**（#184）：表里 `/metrics` 那一格原先写的是朝上的指代，
而被点名的「指标出口是逐行的」此时已经落在那张表下面 —— 章内一次搬家就把方向词翻了个面，
而三条既有判据（路由集合、字段清单、错误码名）全部按内容取数，看不见这句话。

**本轮没有改生产代码逻辑**：落点是一份新用例文件、一处既有取数口径、`deploy/README.md` 的段落位置
与三处文字。详记 V13 §9.16，日志 `logs/s179_*.txt`—`logs/s192_*.txt`。

### Added（#183 位置判据、#184 方向判据）

- `crates/qx-cli/src/tests/api_endpoint_table_routes.rs` 新增 `endpoint_tables_and_their_check_section_live_under_paper_api`
  - 四个锚点（两张表头、核对小节标题、"未列出的路径一律 404"那句引言）**逐个**要求：整行锚点在全文只出现
    一次，且落在 `## Paper API` 与下一个一级标题之间；再核三样东西的相对次序（表一 → 表二 → 核对小节），
    因为核对那段用「上面那张」与「本节这张表」指代两张表，次序一翻指代就落到别的表上。最后双向点名：
    文档要说出这条判据存在，判据也要认自己的定义点（`fn 名字(` 由 `format!` 拼出来，避免自指计数）。
- `crates/qx-cli/src/tests/api_doc_cross_references.rs`（新用例文件，148 行，挂载进 `tests/mod.rs`）
  - `directional_cross_references_point_the_right_side`：扫 `deploy/README.md` 里每一处 `见上/见下（文/面/中）「X」`
    点名的指代，按行首标题前缀找 `X`，要求目标**真的在方向词声明的那一侧**；点名点到不存在的小节红，
    同名标题在指代上下各一份（方向词无法告诉读者翻哪边）也红。
  - 循环之前一条**条数地板**（本轮实测 6 处）：扫描口径失灵时判据会一路绿下去，所以先让它在失灵时红 ——
    这一条是本轮变异真咬出来的，见下面"本轮实测"第二行。

### Changed（#183 的搬家与一处被搬家逼出来的取数口径）

- `deploy/README.md`：「### 端点表按张核对」连同第二张端点表整体从 `## Outbox 与 NATS JetStream` 移进
  `## Paper API`（`logs/s179_move_endpoint_tables.py` 逐字节手术，搬运前后 **78,109 字节不变**，纯 CRLF 性质不变）。
- `crates/qx-cli/src/tests/api_response_field_doc.rs` 的 `endpoint_return_cells()`：**从"按二级标题截一段"改成
  "按表头取那一张表"**。第二张表搬进同一章之后，旧的按小节截断会把两张表一起算进来，`/health` 立刻被数成
  "列了两遍" —— 这是搬家带来的真实后果，不是夹具脏。
- `deploy/README.md` 的三处文字：表二 `/metrics` 格的方向词按 #184 改成朝下；「按张核对」小节末尾补一段
  点名两条新判据（并如实写出"章内搬家会翻方向"这件事）；小节里停在第八遍的那句"共 5 条用例 / `running 5 tests`"
  改成第九遍的三文件 7 条与当轮实跑数字。

### 本轮实测

| 量具 | 当轮实测 | 日志 |
| --- | --- | --- |
| 搬家后的文档类判据 | `api_` 前缀整组 **29 条全绿**（此时新判据尚未挂载），门禁 **515 项 PASS / `exit 0`** | `s184_v13_r2_pass9_api_judges.txt`、`s183_v13_r2_pass9_gate_after_move.txt` |
| #184 首版条数地板 | 地板按 5 写 → `m2_ref_unnamed`（把一处点名改成"见下这一节"）**没有咬，判据照样绿**：删掉一条指代后仍剩 5 处 ≥ 地板 | `s187_v13_r2_pass9_mutations.txt`（第一次跑，2/13 后中断） |
| 地板抬到当轮实测 6 处后 | 同发变异改判为红，且点名"只扫到 5 处" | `s189_v13_r2_pass9_mutations_13_final.txt`、`s187_mutation_details/m2_ref_unnamed.txt` |
| #183 + #184 变异（13 发，红绿成对） | **13/13 全咬**：#184 五红（方向词翻面、指代不再点名、点名点到不存在的小节、同名标题上下各一份、判据定义点改名）+ 三绿控制组（新增一条方向正确的指代、方向词与「」之间折行、被点名标题之前再插一个小节）；#183 四红（整段划出 `## Paper API`、别处再抄一份表二、文档摘掉判据点名、判据定义点改名）+ 一绿控制组（章内加一个三级标题）。每发跑完与 %TEMP% 镜像 **逐字节一致**（`RESTORE_IDENTICAL=True`），基线与终态均 `exit=0` | `s188_v13_r2_pass9_mutations_13.txt`、`s189_v13_r2_pass9_mutations_13_final.txt`，逐发 panic 摘录在 `logs/s187_mutation_details/*.txt` |
| 文档定稿后的定向复核 | 三个用例文件 = **`running 7 tests` / `7 passed`**，门禁 **515 项 PASS / `exit 0`** | `s190_*.txt`（改文档前）、`s192_*.txt`（改文档后） |
| 文档体量 | `deploy/README.md` 定稿 **937 CRLF 行 / 79,569 字节**、纯 CRLF 性质不变；`api_endpoint_table_routes.rs` 451 行、`api_response_field_doc.rs` 430 行、新文件 148 行，都在 500 行门槛内 | `s191_doc_fix_stale_case_count.py` 的输出与当轮 `wc -l` |
| 定稿后的九步整跑 | **`BUILD_EXIT=1`**，红在 `[4/9]` 整树测试：`worker_launch_diagnostics::silent_worker_reports_program_origin_and_stderr` 收到的是 `写入 Python Strategy 输入失败: 管道正在被关闭。 (os error 232); stderr=…`，这句话里没有 `程序=`；同一目标单跑 **3 passed / 0 failed** —— 断管道分支只在整跑负载下才被走到，按"每遍修复全部问题"立案为 #185，本遍的九步数字因此由第十遍给出 | `s195_v13_r2_pass9_build_bat_full.txt`、当轮单跑输出 |

**这条链条的形状值得记一句**：#183 修的是"位置没人核"，#184 修的是"位置一变，靠位置说话的句子就坏"。
后者不是推测，是前者落地当轮实测到的 —— 如果没有把 #183 真搬一次，这条判据根本不会出现在册上。
同一轮里条数地板写低一格就让一发变异假绿，也是同一件事的两面：**判据的强度只能由当轮变异证明**，
不能由"我新加了一条判据"证明。而本轮的九步整跑红在一条从未红过的用例上（见上表最后一行），
说明"定向跑绿"与"整跑绿"覆盖的不是同一批分支 —— 这一条被立案成第十遍的 #185。

## Unreleased — V13 R2 第八遍：两张端点表按张各自钉住，`qx_pipeline_*` 的"没有出口"从口头改成常驻，发布链与接口文档的两处断链收口（#180 / #178 / #176 / #181 / #182 / #179 收口）（2026-09-27）

第七遍收的是"字段与样本对不对得上实现"，这一遍收的是**核对口径本身与它漏掉的两条链**：第六遍立案的 #176
（门禁按整篇 README 的路由**并集**比对，所以整行删掉一张表的一条路由不会红）、第七遍顺带发现的 #178
（`qx_pipeline_*` 这族指标在生产里被算出来、被丢掉），以及本轮自己踩出来的 **#181**（绕开打包脚本重打 wheel 会
静默 ship 上一轮的原生扩展）和 **#182**（读面 8 个错误码名里 2 个在接口文档没有名字，`POST /control/commands`
那一格还漏了 409）。四处都补了常驻判据并跑完变异，**生产代码逻辑一个字没改**（落点是用例、文档、
`capabilities.yaml` 与两处源码说明）。详记 V13 §9.15，日志 `logs/s154_*.txt`—`logs/s180_*.txt`。

### Added（#180：逐张表的核对口径）

- `crates/qx-cli/src/tests/api_endpoint_table_routes.rs`（新用例文件，挂载进 `tests/mod.rs`）
  - `each_endpoint_table_lists_exactly_the_dispatch_routes`：把「返回」表与「语义/非 200」表**各自**与
    `handle_inner` 的分派集合比相等（双向：只在这张表的、只在这张表没有的实现路由，各点名一条）。
    刻意不复用门禁的并集口径 —— 门禁那一侧留着的盲区正是这条用例存在的理由。取数区间按
    `fn handle_inner(` 到 `_ => ApiResponse::text(404, "not found")` 切，不按"第一次 `#[cfg(test)]`"截
    （那是 V12 §16 元判据明确禁掉的口径）。
- `crates/qx-cli/src/tests/zero_reader_fields.rs` 第三条判据
  - `pipeline_metrics_stay_documented_as_unpublished`（#178）：先由源码数出"`LiveEventPipeline::metrics()`
    的生产读者清单"（按 #171 的"提及即算读者"宽松口径，且要求同文件提到 `LiveEventPipeline` 与 `.metrics()`
    两处，否则 qx-api 自己那份 `ApiMetricsSnapshot` 会被误数），再要 `maturity/capabilities.yaml` 的登记项、
    `crates/qx-runtime/src/lib.rs` re-export 上的说明、`deploy/README.md` 那句「没有生产出口」三处与它
    **同进同退**。接上出口后这三处说法都成了假话，删登记也是漏登记，两侧都会红。

### Changed（#178 与 #176 的如实登记；#180 的取数搬家）

- **`qx_pipeline_*` 定性为"已登记缺口"而不是"待接线的小疏忽"**：六个计数在 `ingest`/`refresh` 每笔都加，
  `to_prometheus` 也渲染得出，但全仓对 `metrics()` 的调用只有 `pipeline.rs` 自己的用例。不顺手接进 `/metrics`
  的理由写进了三处说明：计数按 pipeline **对象**自打开起累计，而对象存活期不一致 ——
  `crates/qx-cli/src/api_service.rs` 的读路径按请求各开一个，`crates/qx-cli/src/venue_runtime/paper_worker.rs`
  的循环主体持有一个、行情与多腿恢复又按命令各开一个；直接印成 `_total` 是给抓取端一条会归零的"累计计数"。
  真要接，接的是按 worker 归属的 `.prom`（`read_worker_metrics` 原样透传正文，只改写 `qx_worker_up` 那行的值）。
- `crates/qx-cli/src/tests/api_response_field_doc.rs` **506 → 429 行**：`dispatch_routes` 与第一张表的路由
  断言搬进新文件，于是它退出超 500 行名单，`maturity/line_budgets.yaml` 少一条登记（行数棘轮按"拆分"下行，
  而不是把预算抬上去）。
- `deploy/README.md` 加「### 端点表按张核对」小节，两张表的引言各点名新用例；指标出口一节补 #178 那条 bullet。
- `README.md` 的发布物核对段补 **(#179)** 口径：exe 里的字面量计数只在 `N>0` 时是证据（常量池化与死分支
  剔除会让"能力在"的计数合法地为 0），反向结论一律回源码与 `--features` 组合判。

### 门禁本体没动，但盲区测清了（#176 结案口径）

`api_surface_doc_check` 的并集口径按约束留给协调侧（改动权不在本轮）。本轮把它的**形状**测出来并写进文档：
盲区不对称 —— 门禁只数 `METHOD` 前缀形态的反引号路径，所以第一张表整行删掉 `/events/live` 时门禁仍
`exit 0` 并原样印 `[PASS] …端点表与 qx-api 路由集合完全一致`（另一张表还留着这条路径）；而第二张表删掉
`/control/audit` 时门禁 `exit 1`（`只在代码 [('GET', '/control/audit')]`），因为这条路由只以 METHOD 形态
存在于那张表。也就是说"并集"并不对称地盖住两张表，用例侧必须按张比 —— 现在按张比了。


### 补：#181 发布链断链（绕开打包脚本就静默 ship 上一轮的原生扩展）

立案来自本轮自己走的一条快捷路径：`cargo build --release -p qx-python` 之后直接 `pip wheel ./python --no-deps -w dist`，
打出的 wheel 是 **217,414 字节**，内嵌 `_qianxing_native.pyd` 的 md5 `dfef616ed9537aae…` 对不上当轮 dll 的
`f0437740060ac42e…` —— 尺寸、条目数、其余 15 个条目的 CRC 全都"看着正常"，只有载荷里那一半是旧的。根因是
`pip wheel` 打的是**包目录里已经就位的那一份**，而"删掉旧扩展 + 把 cargo 产物按 Python 的导入名 stage 进包目录"
这两步只写在 `tools/build_python_wheel.ps1` / `.sh` 里。按脚本重打得到 **217,415 字节**、`.pyd` md5 == dll md5
（错误路径留在 `logs/s163_*.txt`，正确路径与 `PS_WHEEL_EXIT=0` 在 `logs/s165_*.txt`）。

- 常驻判据：`artifact_identity_doc.rs::wheel_packaging_entry_stages_the_fresh_native_extension` 对两个脚本各自按
  **位置**核对「构建 → 删旧 → stage → 打包」四个锚点严格递增，并要求 README 那句「打的是包目录里已经就位的那一份」
  与判据**双向点名**（换成一份没人核对的文档、把拷贝挪到 `pip wheel` 之后，都会红）。
- 变异 4/4 全咬（`logs/s166_*.txt`）：删 ps1 的 stage 拷贝 → 红且点名脚本路径；删 sh 的 `rm -f` → 红且报"出现 0 次"；
  把 sh 的拷贝挪到 `pip wheel` 之后 → 红在「不再排在上一锚点」；摘掉 README 那句 → 红。四发还原后与 %TEMP% 镜像逐字节
  一致，复跑判据绿、门禁 **515 项**。
- README 的 wheel 安装段因此多了**第四条前置**：不许绕开这两个脚本。

### 补：#182 接口文档断链（读面 8 个错误码名里 2 个在全仓文档没有名字）

第七遍把「返回」那一格的键集钉住之后，本轮按同一口径去核对第二张表的**「非 200 口径」那一格**。实测
`crates/qx-api/src/lib.rs` 写进 `{"error": …}` 的码名共 **8 个**，文档只点名 5 个：`forbidden`（已认证但策略给不出
权限的那条 403）与 `control_state_unavailable`（控制队列不可用的 503）在任何文档里都没有名字；同一条
`POST /control/commands` 那一格原先还漏了 409。后果落在读者侧 —— 这条路上其实有**两个不同码名的 403**，
客户端按 `error` 分支写代码就会把"没登录"与"没权限"合成一件事。

- 文档已补：那一格写全 400 / 403（两个码名）/ 409 / 503，并把 `ControlError` 的四个 409 变体名
  `DuplicateRequest`、`DuplicateCommand`、`UnknownCommand`、`AlreadyFinal` 逐名列在表后。Debug 形态是类型面而不是
  稳定契约，所以判据只核对小写下划线形态的码名 —— 这一区分写在判据的注释里，不是漏掉。
- 常驻判据：`api_endpoint_table_routes.rs::non_200_column_names_match_the_read_face_implementation`，双向 ——
  正向要格子里承诺的码名与三位状态码实现真产得出，反向要实现产出的码名与非 200 状态码在文档里有名字；取数下限
  写死"从 qx-api 至少数出 8 个码名"，防止判据空转。变异 4/4 全咬（`logs/s172_*.txt`）：文档塞一个假码名、实现改掉
  `forbidden` 的名字、文档把 `/events/live` 的 500 写成 418、摘掉点名判据那一行 —— 各红一次。
- #179 的那句「字面量计数是单向证据」本轮从"写在文档里"升级为判据锚点（`PAYLOAD_IDIOM` **6 → 7 条**），变异 2/2
  全咬（`logs/s173_*.txt`：删 README 那句、把锚里的"单向"改成"双向"，各红一次）。

**本轮实测**：

| 量具 | 当轮实测 | 日志 |
| --- | --- | --- |
| 新用例首跑（拆分 + 挂载后） | 定向 5 条（`api_endpoint_table_routes` + `api_response_field_doc`）**全绿**，门禁 **515 项 PASS / `exit 0`** | `s154_move_route_pinning.py`、`s158_v13_r2_pass8_gate_after_178.txt` |
| #180 变异（两张表各整行删一条路由） | 表一删 `/events/live`：用例 **2 FAILED / 2 passed**、红在 `api_endpoint_table_routes.rs:67` 与 `api_response_field_doc.rs:193`，**门禁 `exit 0` 仍印 `[PASS]`（#176 盲区本身）**；表二删 `/control/audit`：用例 **1 FAILED / 3 passed**，**门禁 `exit 1`** `只在代码 [('GET', '/control/audit')]`；每发跑完与 %TEMP% 镜像逐字节一致，最后 **4 passed / 0 failed + 门禁 515 PASS** | `s155_v13_r2_pass8_endpoint_table_mutation.txt` |
| #178 变异（双向五发） | **5/5 全咬**：`M1` 删 capabilities 登记→红、`M2` 删接口文档那句→红、`M3` 删 crate 公开面那句→红、`M4` 只插一条生产读者探针（不改口）→红且点名 `api_service.rs`、`M5` 探针保留 + 三处说法同时摘掉→**必须绿**（证明不是单向恒真断言）。四份文件还原后逐字节一致，复跑判据绿、门禁 **515 项** | `s160_v13_r2_pass8_pipeline_metrics_mutation.txt` |
| 文档落地后复核 | 门禁 **515 项 PASS / `exit 0`**（含 `能力矩阵证据路径全部存在`：本轮把登记项里 `xxx.rs::用例名` 的写法改成路径 + 空格 + 用例名，并补全一处相对简写路径 —— 该门禁按整行逐 token 核路径，#87 的口径） | `s158_v13_r2_pass8_gate_after_178.txt`、`s159_fix_evidence_path_tokens.py` |
| 九步构建（#181/#182 之前的整树跑） | `exit 0`：门禁 **515 项**、Release 构建、`[4/9]` **93 条** `test result: ok` / **0 failed**、Clippy 通过、python **57** 用例 OK（skipped=1）、核心语义全过、生态 OK、`[PASS] runtime 引用文件校验通过` | `s162_v13_r2_pass8_build_bat_full.txt` |
| wheel 两条路径对照 | 快捷路径 217,414 字节且 `.pyd` md5 对不上当轮 dll（缺陷证据）；按脚本重打 **217,415 字节**、`PS_WHEEL_EXIT=0`、md5 相等 | `s163_*.txt`、`s165_*.txt` |
| 发布物逐条目对证（本轮 vs 第五遍） | 两侧各 17 条目、名称集合相同；12 条 CRC + 时间戳全同、3 条仅 zip 时间戳、**2 条 CRC 变化**（`.pyd` 同尺寸 353,280 字节换 md5、`RECORD` 随之变）；12 份 `.py` 与仓库 `python/` 逐字节相同；exe **11,921,920** 字节 | `s164_release_payload.py` → `s164_*.txt` |
| #181 变异（构建 / stage / 打包的先后顺序） | **4/4 全咬**，四发还原后逐字节一致，复跑判据绿、门禁 **515 项** | `s166_*.txt` |
| `/metrics` 抓取端实测（装机产物 exe） | `200`、6 行注释 + 3 行样本、**0 条解析失败**、`qx_api_requests_total = 5`、正文无残留字面反斜杠 + n | `s167_*.txt` |
| 发布面读面真抓（`qx-cli.exe serve` 逐条端点） | 键集与状态码**全部按文档取数**：16 行全 ✓、`/ready` 503 键集与文档相等；两条 `/events` 行原先在这条探针里写死 200（pass-7 起实现按 409 回），本轮改成读「非 200 口径」那一格 —— 实测 `409 event_cursor_requires_snapshot` 与文档同现兑现 = True | `s168_*.txt` |
| 干净 venv 安装面实测 | 装本轮 wheel → 四包导入 OK、`native.available() -> True`、线格式往返一致、四种非法取值各抛 `ValueError`；已安装 `worker.py` 与仓库逐字节相同；wheel 内 `.pyd` md5 == `target/release/_qianxing_native.dll` md5 | `s169_*.txt` |
| #182 / #179 变异 | **4/4** 与 **2/2** 全咬，还原后判据绿、门禁 **515 项** | `s172_*.txt`、`s173_*.txt` |
| 文档落地后的定向复核 | `api_endpoint_table_routes` + `api_response_field_doc` = **`running 5 tests` / `5 passed`**、`artifact_identity_doc` = **2 passed**、门禁 **515 项 PASS / `exit 0`** | `s174_*.txt` |
| 终态九步整跑（第一次，fmt 未跑） | **`BUILD_EXIT=1`**，红在 `[2/9] 格式检查`：#181/#182 新写的两处断言（多行折开的 `assert!(cells.len() >= 3, …)` 与 `artifact_identity_doc.rs` 里新增的长字符串数组元素）没先过 `cargo fmt`。**门禁的 515 项与 `cargo test` 都不看 rustfmt 形状**，所以这条只能在构建脚本里红 | `s176_*.txt` |
| 终态九步整跑（`cargo fmt --all` 后重跑） | **`BUILD_EXIT=0`**：`[1/9]` 门禁 **516 行 `[PASS]` / 0 `[FAIL]`**（门禁本体 **515 项**，另 1 行是 `[9/9]` 的 `runtime 引用文件校验通过`；直接跑门禁同一棵树 **515 项 PASS / `exit 0`**）、`[2/9]` 格式检查过、Release 构建过、`[4/9]` **93 个测试目标 / 885 passed / 0 failed**、`[5/9]` Clippy 过、`[6/9]` python **Ran 57 / OK (skipped=1)**、`[7/9]` 核心语义全过（成交 22、三条重放哈希判据全 True）、`[8/9]` `ecosystem` 六段 ✓、`[9/9]` 拓扑配置 `[PASS]` | `s178_*.txt`、`s180_*.txt` |

表里那次九步整跑在 #181/#182/#179 之前；文档与判据定稿后本轮又整跑了一次九步，终态数字见上表最后两行
（`s176`/`s178`）。文档改动不进 exe，但 `[1/9]` 门禁与 `[4/9]` 里那几条按文档取数的用例会在文档与实现不平的时候红，
所以整跑必须在文档定稿后做（这也是 `#139` 那轮的口径）。**`[2/9]` 是这条链上唯一看排版的步骤**：格式检查只在
`build.bat` 里跑，515 项门禁与 `cargo test` 都不判 rustfmt 形状，所以新增/改写 Rust 文件的收口轮在整跑前必须先
`cargo fmt --all`（本轮第一发就红在那里，`s176`）。

**本轮没做（明确留在册上）**：门禁 `api_surface_doc_check` 的并集口径本体（#176 的"修门禁"那一半，改动权在
协调侧）；#178 的实际接线（按上面的量纲理由登记为缺口，不是漏掉）；`#174` 字段级零读者门禁、`#169` 事件日志
压缩/保留、`#141`/`#152`/`#144`/`#157` 照旧在册。

## Unreleased — V13 R2 第七遍：`/metrics` 的正文此前是一条抓不到的行（#177，两处转义换行）（2026-09-27）

第六遍把"文档承诺的键集"钉住之后，这一遍去真抓了一次发布面。结果是这条入口在**修好之前从来没有被抓到过**：
`qx-cli.exe serve` 的 `/metrics` 返回 `200`、462 字节，而正文的行分隔被写成转义文本（渲染出字面反斜杠 + n），
整份正文是一行，**0 条样本可解析**。Prometheus 抓取端在这种正文上解析失败不报错，所以后果不是"看到错误"，
而是所有以这些指标为条件的告警永不触发。缺陷形态出现在两份独立实现里，且各自的用例都用 `contains` 断言
（名字读得出、样本读不出），因此全绿。详记 V13 §9.14，日志 `logs/s143_*.txt`—`logs/s153_*.txt`。

### Fixed（生产代码，2 处渲染模板）

- `crates/qx-api/src/lib.rs` 的 `ApiMetricsSnapshot::to_prometheus`（模板区 **9** 处行分隔）与
  `crates/qx-runtime/src/pipeline.rs` 的 `PipelineMetricsSnapshot::to_prometheus`（**18** 处）：把转义文本
  改回真换行。修完发布面实测 `200`、**453 字节 / 9 行**、注释 6 行 + 样本 3 行、**解析失败 0 条**，
  `Content-Type: text/plain; version=0.0.4; charset=utf-8`。

### Added / Changed（判据改成按行取）

- `crates/qx-api/tests/prometheus_exposition.rs` 两条用例按抓取端口径逐行解析（原 `contains` 形态改掉）。
- `crates/qx-runtime/src/pipeline.rs` 内联用例改为按 `"\nqx_"` 计数并断言 **6** 条样本各由一个换行起头。
- `crates/qx-cli/src/tests/api_response_field_doc.rs` 新增判据
  `prometheus_exposition_is_line_separated_at_both_ends`：一头在同一进程里真驱动 `/metrics` 按行解析，
  另一头把生产源码里"看起来是样本模板"的行扫一遍（源码里 2 字符 `\n` 才是分隔，3 字符 `\\n` 是缺陷形态）。
- `deploy/README.md` 补两节：「### 指标出口是逐行的」（含抓取端静默失败的后果与 worker 标签形态）与
  「### 事件游标的冷启动口径」（把本轮实测的七条答复原样抄进去，`?after=0` 与越界游标都是 `409
  event_cursor_requires_snapshot` 而不是空数组）。

**本轮实测**：

| 量具 | 当轮实测 | 日志 |
| --- | --- | --- |
| 修前的发布面（缺陷形态取证） | `/metrics` `200`、**462 字节**、正文含字面反斜杠 + n、**按行解析 0 条样本**；同一 probe 顺手记下两条事件链的冷启动答复（`/events` `200 []`、`?after=0` `409`） | `s143_v13_r2_pass6_metrics_and_cold_cursor.txt` |
| 修后同一 probe | **453 字节 / 3 条样本 / 0 条失败**，三条计数与请求次数对得上（`qx_api_requests_total 5`） | `s153_v13_r2_pass7_release_metrics_scrapeable.txt` |
| 变异反向验证（把两处模板改回转义文本） | `qx-api` 那处：qx-api 用例 FAILED + qx-cli 判据 FAILED（红在 `api_response_field_doc.rs:446`）；`qx-runtime` 那处：qx-runtime 内联用例 FAILED（`pipeline.rs:1831`）+ 同一条 qx-cli 判据 FAILED（`:485`）；**两发都是红/绿成对**，跑完与 %TEMP% 镜像逐字节一致，复跑 qx-api **25 passed**、qx-runtime **50 passed**、qx-cli 三判据 **3 passed** | `s146_*.txt`、`s147_*.txt`、`s151_v13_r2_pass7_prometheus_mutation_final.txt` |
| 九步整跑（第七遍收口） | `[1/9]` 门禁 **515 项全通过**；`[4/9]` 整树 **93 段 `test result: ok`（70 段非空）、881 passed / 0 failed**；`[9/9]` `[PASS] runtime 引用文件校验通过` | `s152_v13_r2_pass7_build_bat_full.txt` |

**本轮没做（明确留在册上）**：这一遍只把两处出口修好、把"按行取"变成常驻口径；`qx_pipeline_*` 那一族虽然
修好了渲染，但**没有任何生产调用者**（`/metrics` 不服务它）—— 第七遍把它当"两处同时坏"的对照面记过一笔，
真正的定性留给第八遍（#178）。其余 `#174`/`#169`/`#141`/`#152`/`#144`/`#157` 照旧在册。

## Unreleased — V13 R2 第六遍：接口文档「返回」那一格开始按字段承诺（四处响应体腐坏 + 一处指标形状，#159 收口）（2026-09-27）

第五遍数"字段有没有读者"，这一遍数"文档承诺的字段对不对得上实现"。`serve` 的 HTTP 读面是这套框架对外唯一的
读面，而它此前的对齐口径只到**路由名**（门禁 `api_surface_doc_check` 比 `(METHOD, path)` 集合，一个字段都不看），
所以"路由对得上、响应体说错话"整类腐坏不在任何判据视野里。本轮人工读出四处响应体 + 一处指标形状并全部改掉，
新增 2 个用例文件 3 条判据把这条口径钉住，顺手把 #159（发布产物身份）从"立案"改成"有常驻判据"。
详记 V13 §9.13，日志 `logs/s124_*.txt`—`logs/s134_*.txt`。**生产代码一个字没改**：落点全在接口文档、README 与用例。

### Fixed（`deploy/README.md` 的端点表，五处响应体/样本形状承诺）

- **`GET /ready`**：「返回」那一格原先写 Rust 类型名 `ApiReadiness`，那是实现内部的名字而不是线格式 → 改成
  `{"ready":<bool>,"detail":<string>}`。
- **`GET /account/snapshot/diff`**：「返回」原先只写「差异」两个字。实际十条键，且**八个汇总钱标量只由
  `replacement` 整格搬运**（`SnapshotDiff::replacement` 是私有字段：serde 序列化私有字段，所以线格式里有它、
  crate 外的 Rust 代码却点名不了它）→ 补齐十条键名、五条 `Change` 数组的变体形态，并在表下另起一段说明
  "漏读 `replacement` 的客户端会拿基线权益去核对目标状态哈希、`apply` 末尾那道比对正是为这种客户端准备的、
  `qx-cli ecosystem` 的协议段跑的就是这条回路"。
- **`/events` 与 `/events/live`**：两格都写「事件数组」，实际一条是裸 `Event`（9 键）、另一条是
  `ProjectionEnvelope`（14 键，事件本体在它的 `data` 里），**不同形** → 两格各列出自己的键名并明写不同形。
- **`GET /account/balances`**：`cash_raw` 与三个标量并列却看不出它按币种聚合 → 补「`cash_raw` 是按币种聚合的
  map（没有快照时是 `{}`），另三格是标量」。
- **worker 健康段的指标形状**：文档写 `qx_worker_up=1`，线上形态是 `qx_worker_up{worker="<worker_id>"} 0|1`，
  而读侧 `crates/qx-cli/src/runtime_wiring.rs` 按 `starts_with("qx_worker_up{")` 认样本 → 两处（`_up` 与
  `_heartbeat_timestamp_seconds`）都改成带标签形态。照原文写抓取脚本的人此前拿到的是空样本。

### Added（判据：2 个用例文件、3 条判据）

- `crates/qx-cli/src/tests/api_response_field_doc.rs`
  - `the_endpoint_table_promises_exactly_the_fields_qx_api_serializes`：文档侧只解析「### HTTP 读面与控制面路由」
    这一节的「返回」那一格（刻意不扫全文 —— 扫全文就是在这里重犯门禁那个并集口径），实现侧在同一进程驱动
    `ApiService::handle` 取真序列化出来的键集，逐条比相等；四向对齐（代码有↔表有、表承诺键集↔用例驱动键集），
    所以"文档删承诺"与"用例不再驱动"两侧都会红而不是安静少比一条。
  - `documented_prometheus_metrics_are_the_names_the_runtime_emits`：文档与 `deploy/prometheus/qianxing-alerts.yml`
    点名的每个 `qx_*` 样本名必须有生产印点，带标签的还要求文档给出 `{worker=` 形态；围栏代码块先剥掉，
    免得数据库用户名 `qx_user` 被当指标。
- `crates/qx-cli/src/tests/artifact_identity_doc.rs`（**#159 收口**）：`README.md` 与 `deploy/README.md` 不许出现
  64 位连续十六进制（整档 sha 的线上形态）或等号形态的打包器摘要，同时要求 README 保住"产物身份按载荷报"
  那句口径骨架并与判据文件互相点名 —— 文档与判据用的是同一份文档名单，换一份没人核对的文件就红在读侧。
- `maturity/capabilities.yaml` 无新增条目：本轮没有新增能力面，也没有改契约。

### Changed（`README.md` 的产物身份段）

- 「所以产物身份只按载荷报（尺寸 / 条目数 / 条目 CRC / 内嵌扩展的 md5），已立案 #159」改为点名上面那条判据，
  即这条口径从"人工维持"变成"常驻核对"。

### 在册未做

- **新立案 #176**：`api_surface_doc_check` 按整篇 `deploy/README.md` 的路由**并集**比对，而文档有两张端点表，
  所以从任一张表删掉一行只要另一张还留着那条路径就照绿；本轮删行实测门禁仍 `[PASS]`。门禁改动权不在本轮，
  所以字段级与单表级由用例侧兜住。
- **#157 半收口**：HTTP 响应体字面量已钉住；仍欠 CLI stdout 字面量（需要子进程 harness）。
- `#174` 字段级零读者门禁、`#169` 事件日志压缩/保留、`#141`/`#152`/`#144` 照旧在册。


## Unreleased — V13 R2 第五遍：字段级的"没人读"与"没人写"（三处修复，一处是真断链）（2026-09-27）

第四遍数调用点，这一遍数字段：一个 `pub` 字段有人写没人读，编译器和门禁都不会抱怨（门禁只到函数与枚举变体
级，类型面那条是 #135 立的案）。三份一次性探针清出字段账目后逐条人工判定，落成三处修复：**两格零读者字段删掉**
（#170）、**三格"只有用例读者"的投影字段保留并如实登记**（#171）、**风控端口的"拒绝"与"端口坏了"原本共用一条
通道**（#172，本轮唯一真断链：正常风控拒单在播报上变成「RiskPort 执行失败」，运维据此去查一条好链路）。
详记 V13 §9.12，日志 `logs/s108_*.txt`—`logs/s122_*.txt`。

### Changed（一处端口契约，不外溢到线格式）

- **#172 风控端口的裁决与故障分通道**（`crates/qx-execution/src/application.rs:68-88`、
  `crates/qx-execution/src/lib.rs:252-283,535-557`）：`RiskPort::evaluate_order` 的返回从
  `Result<RiskDecision, String>` 换成 `Result<RiskVerdict, String>`，`RiskVerdict::{Allow, Reject { reason }}`；
  `Ok(Reject)` 表示"端口给出了裁决、这单不该出门"，`Err` 只表示"端口给不出裁决"。改前两生产实现
  （`RiskContextPort` / `CanonicalRiskPort`）**只在通过时**写 `accepted: true`，拒绝一律 `Err`，于是
  `submit_with_risk` 里「账户级 RiskPort 拒绝订单」那条播报在生产不可达、只有测试假件能触发。改后拒单播报为
  「账户级 RiskPort 拒绝订单: <规则集版本>: <违规清单>」，端口故障才是「账户级 RiskPort 执行失败」。
  该类型不带 `Serialize`，不是线格式；同时消掉与 `qx_risk::RiskDecision` 同名异物一项（§5 R2-1 预登记）。

### Removed（两处公共面，取证口径：全仓含用例零读者）

- `qx_runtime::RuntimeIngestReceipt::primary_seq` 与 `::log_digest`（#170）：`ingest` 回执的五格里这两格
  有三个生产构造点、**全仓（含用例）零读者** —— 要按序号或日志摘要读事实流本来就该走 `log()`。附带少算一笔：
  `log_digest` 要为一次没人读的播报把整条事实流重哈希，而 `ingest` 是每笔行情/成交都走的路径。
  判据把名册钉成三格（`derived_seqs` / `engine_ts` / `deduplicated`），并把"只有用例读者"与"零读者"这两层
  区分写进结构体文档 —— 前者按仓库口径保留（用例是这条归约链的回归证明面，先例 #118/#119）。
- `qx_execution::RiskDecision`（`struct { accepted: bool, reason_code: &'static str }`，#172）：改后无构造者；
  `accepted: bool` 与 `reason_code` 两个名字被常驻判据列为生产实现文件里的禁用语。

### Added（判据：新增 2 个用例文件、3 条判据、1 条 capabilities 登记）

- `crates/qx-cli/src/tests/zero_reader_fields.rs`：#170 回执名册逐字钉三格 + `primary_seq` 禁回 + 全文件
  `.digest()` 恰好 1 处（只在 `snapshot()`）；#171 **双向对齐** —— `assert_eq!(登记在否, 跨 crate 生产读者为空)`，
  内核文档那句「没有任何生产读者」参与同一等式。
- `crates/qx-cli/src/tests/risk_port_channel.rs`：#172 接线判据按调用点逐处数（生产实现 2 处，于是
  `Ok(RiskVerdict::Allow)`、`Ok(RiskVerdict::Reject {`、`decision.violations.join` 各 2 处，两条播报各 1 处）。
- `crates/qx-execution/src/tests/gateway_port.rs::canonical_risk_port_separates_verdict_from_port_failure`：
  行为判据，钉"缺产品规格的限额订单给出的是**拒绝裁决**而不是端口故障"，且拒单不落订单、不落事件、不动 `source_seq`。
- `maturity/capabilities.yaml` → `canonical_order_risk_decision.limitations` 新增
  `order_risk_projection_fields_have_no_production_reader`：投影三格每笔都算，但生产端口只读
  `allowed` / `violations` / `rule_set_version`，所以"这一单成交后仓位与保证金会变成多少"今天读不到产物。
- `crates/qx-cli/src/tests/mod.rs`：共享夹具 `workspace_source()` / `all_crate_production_sources()` /
  `path_under_crate()` 上移（此前这类"数源码字面量"的判据各自拼一次路径、有人漏掉 `unwrap`，文件改名会把
  "读不到"读成"通过"）。

### Validation（数字全部抄自当轮日志）

| 项 | 实测 | 日志 |
| --- | --- | --- |
| 一次性字段探针 | `s108` 186 份生产源 / 89 个含 `Option` 字段的结构 / **11** 格"构造点都没显式写出"；`s109` 配置侧无人读 **4** 格，其中在模板/python/schema 出现过键名的 **0** 格；`s110` 181 份生产源 / 恒写字面量且被条件读的 bool-str 字段 **23** 格 → 人工判定**真缺陷 1 组**（即 #172） | `s108`、`s109`、`s110_v13_r2_pass5_constant_discriminator.txt` |
| 变异反向验证 | **6 发**：`M1`–`M4` 首跑即咬；`M5` 首版**未咬**（判据按子串认 `capabilities` 登记，把登记项降级成注释就骗过它 —— 与 #136 同类盲区），加固为"只认该能力 `limitations:` 名单下的列表项"后 `M5`、`M6` 均咬。每发跑完即还原，还原后三份被改文件与 %TEMP% 镜像 `cmp` **逐字节相同** | `s111_v13_r2_pass5_mutation.txt` |
| 架构门禁 | 三处修复落地后首轮整跑只红在单文件行数棘轮一项（#172 那段注释多一行），压回后 **515 项全绿 / `GATE_EXIT=0`**；两处文档注释改完再整跑仍 **515 / 0** | `s112_v13_r2_gate_after_pass5.txt`、`s114_v13_r2_gate_after_pass5_judges.txt` |
| 整树测试 | `QX_PYTHON=<venv 绝对路径> cargo test --workspace` → **92 段全 ok、876 passed / 0 failed、`WORKSPACE_EXIT=0`**，`warning` 0 行（较 §9.11 记的 873 多 **3** 条，正是本轮三判据）；文档与 `rustfmt` 之后再定向复跑三判据与 `gateway_port` 全绿、`cargo fmt --all -- --check` 全仓 0 差异、门禁再整跑仍 515/0 | `s113_v13_r2_pass5_workspace_raw.txt`、`s115_v13_r2_pass5_workspace_final.txt`、`s116_v13_r2_gate_after_pass5_docs.txt` |
| `build.bat` 九步端到端（未提交的当轮代码直接整跑） | `[0/9]`…`[9/9]` 全过、`BUILD_BAT_EXIT=0`：门禁 **515 项**全绿、`[4/9]` **92 段 / 876 passed / 0 failed**、`[5/9]` Clippy 只剩 linker 提示 2 行、`[6/9]` `Ran 57 tests … OK (skipped=1)`、`[7/9]` 重放①②③全 True、`[8/9]` release 1m55s 后 `all`/`ecosystem`/「针路 · PaperVenue」全过、`[9/9]` `config_fingerprint=aada66156749d230…`。**跑完复核**：`git status --porcelain` 与跑前逐行相同（35 行），未跟踪新产物 0 份 | `s117_v13_r2_pass5_build_bat_full.txt` |
| 安装包（wheel）按当轮代码重建 + 载荷复核 | `WHEEL_BUILD_EXIT=0`、217,415 字节（整档 `sha256=b6b8b3519076880e…` 只登记不采信，#159）。载荷：17 条目名称集合相同、12 条目 CRC+时间戳全同、3 条目仅 zip 时间戳变，**真改的只有 2 条** —— `_qianxing_native.pyd`（353,280 字节重链接，md5 等于 `target/release/_qianxing_native.dll`）+ `RECORD`；**12 份 `.py` 与仓库逐字节对照不符 0 处**（本轮 `python/` 零改动，wheel 里唯一的实物变化就是那次重链接）。本轮契约改在 Rust 侧，落点是 CLI 二进制：11,921,920 字节的 `qx-cli.exe` 内「账户级 RiskPort 拒绝订单」/「账户级 RiskPort 执行失败」各 **1** 次、`RiskDecision` 与 `accepted: bool` 各 **0** 次（`reason_code` 那 1 次来自有真读者的对账裁决，禁用语只圈在 `qx-execution/src/lib.rs`） | `s118_v13_r2_pass5_wheel_build.txt`、`s119_v13_r2_pass5_release_payload.txt` |
| 新 wheel 装进干净环境真用 | `uv venv`（3.12.13）+ `uv pip install` 退出码 0；四包从 **site-packages** 导入、`native.available() -> True`、`StrategyIntent` 线格式往返一致、四类非法衍生品字段照契约抛 `ValueError`；已安装的 `worker.py` 与仓库逐字节相同 | `s120_v13_r2_pass5_wheel_install_smoke.txt` |
| 补记发布面之后的门禁复跑 | 上面三行写进文档后再整跑：`[PASS]` **515** 条 / `GATE_EXIT=0`；随后把 README 安装面段落里那处「本轮」口径改成点名轮次并补最新现场（#157 类的一处活文档腐坏），再整跑仍 **515 / 0** | `s121_v13_r2_pass5_gate_after_release_docs.txt`、`s122_v13_r2_pass5_gate_after_readme.txt` |

**本轮不做的**：字段级"零读者即报"的门禁判据（门禁改动权本轮在协调者 agent 手上，按 #174 立案在册）；
#171 的投影三格只登记不接线；`#169` 的全量重放与压缩/保留策略。`sandbox_tested` 与 `production_approved`
照旧全为 `false`，本轮不使用任何外部服务或凭据。

## Unreleased — V13 R2 第四遍：停机、空闲与无预算重生（六种"什么都没发生"被当成同一种事）（2026-09-27）

第四遍只问一条：**每条常驻循环在"什么都没发生"的时候做什么**。清点出六个落点（#163–#168），共同形状是把
`空闲`（这一窗确实没有）、`失败`（链路断了）、`放弃`（不再试了）三件事当成一件。混错的两个方向本轮都抓到了：
把空闲读成失败 → 一个当天没有成交的账户在几个窗口后被具名放弃（#165/#166）；把空闲读成成功 → 坏链路每窗复位
预算、永不放弃。反过来，**对冲恢复那条链刻意不许放弃**：停在 `HedgeRequired` 的分组是一条腿已成交、另一条还没
对冲的裸腿，几次抖动之后把它永久晾着比每秒十次扫描更危险。详记 V13 §9.11，日志 `logs/s84_*.txt`—`logs/s106_*.txt`。

### Changed（六处语义，全部只改"这一轮该做什么"，不改交易口径）

| 落点 | 动作 | 改前会怎样 |
| --- | --- | --- |
| #164 `serve` 停机 | `run_runtime_api` 明文与 mTLS 两条出口从裸 `worker.join()` 换成 `join_worker_handle(&supervisor, …)`（`crates/qx-cli/src/strategy_contract.rs:752,824`） | 屏幕上「按 Ctrl+C 停止」没有生产者，只能靠默认信号强杀；V12 R4-a 接好的停机阶梯在 API 入口上没人调用 |
| #165/#168a Binance 两条流 | `recv_user_event`/`recv_quote` 返回从 `Option` 换成 `BinanceStreamPoll::{Event,Idle,Closed}` / `BinanceQuotePoll::{Quote,Idle,Closed}`；`Idle` 只允许出现在**帧边界**（`qx-adapter/src/lib.rs`，半条消息中间超时仍是故障，否则两帧会被拼成一帧）；`binance_stream_worker.rs:39` 把 `Idle` 记成 `Degraded` 健康并累加 `idle_windows`，只有 `Closed` 收摊 | 原口径 `recv_quote(...)?` 把"十分钟没跳动"上抛，supervisor 记 `WorkerExited` 并**停掉其它 worker** —— 薄成交对的 symbol 会拖垮整个 runtime |
| #166 CCXT Pro 空闲心跳 | Rust 发 `wait_ms`（自己读窗的 4/5），Python Worker 无事件时回 `event["idle"] = True`；读侧 `ccxt_watch_reply_is_idle` **只认显式 `idle: true`**（缺键 / `false` / 字符串 `"true"` / 任何真事件都不算空闲，否则一笔成交会被静默丢掉而不进 EventLog） | 空闲窗按失败计，约 5 分钟放弃一个没有成交的账户 |
| #167 一条预算、两类通道 | `CcxtStreamReconnectBudget` → `CcxtReconnectBudget`（带 `subject`，`new()`=用户流 / `market_rpc()`=行情子进程，阶梯同源：500ms 起、8s 封顶、连续 10 次具名放弃）；`ccxt_market_worker.rs` 四处出口全过预算，删掉固定 500ms sleep | 柜台彻底不可用时"重生子进程 + sleep 500ms + continue"无限循环，且 `cycle_failures` 每轮清零（那是配置规模不是尝试次数） |
| #168c/#168d 恢复节律 | `pending_spread_recovery_groups` 从"有没有"改成"几组"，三条恢复循环（Binance / CCXT / Paper）扫前扫后各数一次、`pending_after >= pending_before` 记一轮原地不动，节律走 `spread_recovery_poll_delay(stalls)`（0 轮 100ms，其后退到 8s 封顶，**永不放弃**）；Paper 那半边把闸门挪到扫描之前，没有积压时连账户 EventLog 都不开 | 只要还有分组待对冲，每 100ms 起灭一个 Python 子进程；Paper 循环每 100ms 全量重放一次事实流 |
| #163 文档口径 | `init` 落盘份数从"14"更正为"**9**"，14 是"init + 首屏 `backtest` 之后"的目录总数（多出 5 份 = `data/qianxing/datasets.manifest.json` + 4 份同前缀 runs 产物）；本文第 105 行、V13 §9.9 那行与 `docs/工业化易用性收口指南-V1.md` 同步改掉 | 三处文档把两步之和归给一步，且 V13 的逐项列举只加到 13（漏了数据集清单）—— 谁按文档核对都会以为少了一份文件 |

### Added（判据：新增 1 个用例文件 + 6 处判据扩写）

- `crates/qx-cli/src/tests/spread_recovery_cadence.rs`（新）：一条纯函数判据钉"退避有界、循环无界"
  （0→100ms、1→500ms、1..=200 单调不减、末档恰为 8s 且仍是正间隔）；一条接线判据按**调用点逐个数**，
  覆盖 `worker_entry.rs` 两条与 `venue_runtime/paper_worker.rs` 一条恢复循环。
- `crates/qx-cli/src/tests/init_onboarding.rs::init_lands_nine_files_and_the_advertised_backtest_adds_five`：
  把 9 / 5 / 14 三个数钉成可执行来源 —— 这是 #157（文档引用 CLI 输出无人核对）的一个收口形态。
- `ccxt_stream_retry_budget.rs` +2 颗（行情通道的放弃文案必须点名「CCXT 行情子进程」而不是「用户流」；
  worker 每个重生点都在预算后：`spawn(` 3、`note_failure()?` 2、`note_success()` 2、固定 500ms sleep 0）；
  `binance_stream_retry.rs`（1 次重连预算喂 40 个静默窗仍 `Ok`、`reconnects=0`，同时"只有空闲、末尾确实断了"
  仍要被放弃）；`runtime_api_worker_identity.rs`（用"循环之后还写得动的尾巴"证明阶梯真接上）；
  `python/tests/test_ccxt_worker.py` +2 颗（给了 `wait_ms` 就答 `idle`、窗内真有事件时照样交付），
  Rust 侧再加一颗**跨语言字面量两侧对照**（`"wait_ms": watch_idle_ms` 与 `event["idle"] = True` 必须两侧都还在写，
  键名漂移只会表现为"空闲账户永远不出事件回话"）。`user_stream_retry.rs` 与
  `ccxt_stream_retry_budget.rs` 里的假流夹具随 `recv_event` 的新返回类型改写，判据口径未动。

### Validation（数字全部抄自当轮日志）

| 项 | 实测 | 日志 |
| --- | --- | --- |
| 变异反向验证（真树 + %TEMP% 原始字节镜像，逐发复核还原后逐字节相同） | **15 发全咬**（`bit=True` 15/15）：M164A、M165A/B、M166A/B、M168A、M167A/B/C、M168CA/CB/CC/CD、M168DA/DB | `s85`、`s89`、`s92`、`s93`、`s98_v13_r2_mut168d_paper.txt` |
| 架构门禁 | 落地 #167/#168c 后先只红行数棘轮一项（`spread.rs(518)`、`worker_entry.rs(521)` 未登记），`--snapshot` 复核后 **515 项全绿 / `GATE_EXIT=0`**；#168d + #163 判据之后再跑仍 515/0 | `s94`（红因）、`s95`、`s99_v13_r2_gate_after_168d.txt` |
| 行数棘轮登记 | 超 500 行文件 **38 → 40** 份（新增 `spread.rs` 518、`worker_entry.rs` 521；`paper_worker.rs` 改完 409 行，未入册） | `s82`、`s86`、`s95` |
| 整树测试 | `QX_PYTHON=<venv 绝对路径> cargo test --workspace` → **92 段全 ok、872 passed / 0 failed / `WORKSPACE_EXIT=0`**；#163 的钉数用例之后复跑 92 段 / **873 passed / 0 failed** | `s97_v13_r2_pass4_workspace_final.txt`、`s101_v13_r2_pass4_workspace_closeout.txt` |
| Python Worker 套件 | `Ran 57 tests ... OK (skipped=1)`（跳过的仍是需要真网络的 sandbox 契约，与 `sandbox_tested=false` 同口径） | `s96_v13_r2_python_suite.txt` |
| `init` 落盘份数复测 | 仓库外空目录：`INIT_EXIT=0` 后 9 份、`BACKTEST_EXIT=0` 后 14 份、`result_hash=26fdd6b52d020700` | `s100_v13_r2_init_file_count.txt` |
| `build.bat` 九步端到端（本轮代码，未提交状态直接整跑） | 解释器 3.12.13 且 `QX_PYTHON` 在 `[1/9]` 之前外传 → `[0/9]`…`[9/9]` 全过、`BUILD_BAT_EXIT=0`。分步读数：`[1/9]` **架构不变量 515 项全绿**、`[4/9]` **92 段全 ok / 873 passed / 0 failed**、`[5/9]` Clippy 仅 1 条 linker 提示、`[6/9]` **`Ran 57 tests … OK (skipped=1)`**、`[7/9]` 核心语义全过（重放①②③全 True）、`[8/9]` release 构建 1m51s 后 `qx-cli all` 与 `ecosystem` 与「针路 · PaperVenue 订单接受/报价成交/断线转对账/恢复」全过、`[9/9]` 拓扑 `config_fingerprint=aada6615…` + `runtime 引用文件校验通过`。**跑完复核**：`git status --porcelain` 与跑前逐行相同（26 改 + 1 未跟踪），未跟踪新产物 **0** 份 | `s103_v13_r2_build_bat_full.txt` |
| 安装包（wheel）按当轮代码重建 | 第一次按脚本默认解释器跑即 `WHEEL_BUILD_EXIT=1`，报的是 §19 那条探测判据本身（`interpreter python has no pip …`），**没有**默默产出旧产物；换成 venv 绝对路径后 `WHEEL_BUILD_EXIT=0`，217,415 字节。载荷口径（#159）：与 A5 那份 17 条目名称集合相同，11 条目 CRC 全同，3 条目（`METADATA`/`WHEEL`/`top_level.txt`）CRC 同而 zip 时间戳变，**真改的只有 3 条** —— `qianxing_ccxt/worker.py` 12,093 → **12,878** 字节（本轮空闲心跳那 +24/−9）、`_qianxing_native.pyd` 同尺寸不同 CRC（重链接）、`RECORD` 跟着换哈希；wheel 内 `.pyd` md5 `4777c85f34e2…` = `target/release/_qianxing_native.dll`；**12 份 `.py` 与仓库逐字节相等 0 处不符** | `s104_v13_r2_wheel_build.txt`、`s105_v13_r2_release_payload.txt` |
| 发布物内本轮诊断字面量 | 契约在 CLI 二进制不在 wheel：11,922,944 字节的 `target/release/qx-cli.exe` 内 `market stream idle consecutive_windows=` / `market stream stopped quotes=` / `ccxt pro user stream idle consecutive_windows=` / `ccxt pro user stream stopped idle_windows=` 各 **1** 次，具名放弃的「次重连仍失败，超过上限」1 次，两条通道主语「CCXT Pro 用户流」3 次 / 「CCXT 行情子进程」1 次 | `s105` 第 4 节 |
| 新 wheel 真装真用 | `uv venv`（3.12）+ `uv pip install dist/…whl` → 退出码 0；四包导入 OK、`native.available() -> True`、`StrategyIntent` 线格式往返一致、四类非法衍生品字段照契约抛 `ValueError`；从**已安装**的 `qianxing_ccxt/worker.py` 读出 `"idle"` 1 处 / `wait_ms` 4 处 —— 与仓库同一份字节，所以 `test_ccxt_worker.py` 本轮那 2 颗用例打的键名就是发布物 | `s106_v13_r2_wheel_install_smoke.txt` |
| 代码改动面 | 22 份跟踪文件 `+922 / −139`，另新增 1 份判据文件 | `git diff --numstat` |

**抓到的两条量具教训**：接线判据要按**调用点逐个数**，只测纯函数不会发现某条循环被改回固定 sleep；"不许再出现
固定 100ms"这类**负面清单必须按函数边界圈范围**，不能按文件 —— 第一次跑就是整文件断言把 `paper_worker.rs` 的
执行循环（本来就按固定节拍消费队列）误判成红。

**本轮没做**：`#169`（事件日志每轮全量重放、无压缩/保留策略）仍在册，#168d 只关掉 Paper 恢复循环那一个实例的
无效重放，没有引入压缩或保留策略；三平台 wheel 与 feature 矩阵仍只由 CI 产；`sandbox_tested` 与
`production_approved` 照旧全为 `false`（本轮不使用任何外部服务或凭据）。发布面本轮**不在"没做"里**：
`build.bat` 九步端到端（`logs/s103`）、wheel 按当轮代码重建并逐条目对载荷（`logs/s104`、`logs/s105`）、
新 wheel 装进干净 venv 真用（`logs/s106`）都已实测；只有 CI 的三平台 wheel 矩阵与 feature 矩阵没有本机等价物。

## Unreleased — V13 R1-A5：账户「这一层没算」的名单从四处散文收成一份源码派生的事实（2026-09-26）

本轮**不动一行 Rust、不动结构、不给任何一格接生产者**（§5 给 A5 的口径就是「只做上报口径的可见性」）。
要修的是一件更基础的事：账户快照有八个汇总钱字段，其中五格（`margin_raw`、`frozen_raw`、`realized_pnl_raw`、
`unrealized_pnl_raw`、`funding_raw`）在本构建**没有生产者**，读侧恒发 `null`。这件事此前写在四处文本里，
而四处**没有任何一处能被机器核对**——契约只标类型不提 null 的含义，能力矩阵把五格挤进一条合并命名
（`account_level_unrealized_pnl_margin_frozen_realized_pnl_and_funding_raw_still_have_no_producer`），
`deploy/README.md` 里五格有**四格从未点名**（本轮按字节复核：HEAD 版点名次数 `margin_raw` 1、其余四格各 0）。
于是给某一格接上生产者、或把 `null` 当 `0` 读，四处口径一处都不会红。本轮把这五个名字做成**由源码派生的
一份名单**，并要求契约、能力矩阵、接口文档、读侧用例逐条等于它（V13 §9.10，日志 `logs/s65_*.txt`—`logs/s75_*.txt`）。

### Added（门禁：6 颗同源判据 + 不变量第 26 条）

- `account_money_field_registry_check()`：名单从 `crates/qx-protocol/src/lib.rs` 的 `pub struct AccountSnapshot`
  现取八个 `Option<i128>` 钱字段，减去生产文本（门禁同一个 `production_text()`，按项剥测试项与注释行）里真有
  `<expr>.<field> =` 赋值点的那些，得到未算名单；再逐条核对四处：契约逐字段 `description` 的两套措辞
  （「本构建有生产者」/「本构建这一层没有生产者」互斥且不漏格）、能力矩阵的逐字段 limitation 行、
  `deploy/README.md` 的点名句与「`null` 不是 0」这句口径、读侧 null 用例的点名集合。
- 第 2 颗把「构造账户快照只能走 `AccountSnapshot::new()`（八格默认 None）」钉住：生产文本出现结构体字面量
  构造点即红，于是「凭空造数」只能留在赋值点上、被派生清单抓到。口径写进注释：`production_text()` 只剥**整行**
  注释，行尾注释里的字面量照样算构造点（MA10 实测），这是 fail-closed 的选择而不是疏漏。
- `GATE_CHECK_FLOOR` 509 → **515**（本轮实测整跑 515 项全绿）。

### Changed（三处口径文本，全部按派生名单重写）

| 文件 | 动作 | 实测 |
| --- | --- | --- |
| `schemas/account-snapshot-v1.json` | 八个钱字段各加一条 `description`，写明本构建这一格是「有生产者」还是「没有生产者」，以及 `null` ≠ `0` | 1,481 → **2,741** 字节（30 行，纯 LF）；这份文本是 `include_str!` 内嵌、由 `GET /schema/account-snapshot-v1` 原样公布，不是第二份手抄 |
| `maturity/capabilities.yaml` | 那条合并命名 blob 拆成 **5 条**逐字段 limitation（`account_<field>_has_no_producer`），每条写清 null 含义与缺的那把尺子 | 533 → 537 行、limitation 行 30 → 35、证据行 255 未动、`sandbox_tested` 为真仍 **0** 条 |
| `deploy/README.md` | 加一句机器可定位的点名句「**账户级无生产者字段**：…五格…」＋「`null` 的含义是这一层没有算它，不是 0」，并指明判据名 | 815 → **823** 行；两份 CRLF 文件改完逐字节复核**孤立 LF 为 0** |

### Validation（数字全部抄自当轮日志）

| 项 | 实测 | 日志 |
| --- | --- | --- |
| 新判据单独跑 | 6 颗全绿，六处名单逐条打印且同为那五格 | `s68_a5_registry_check_green_side_after_hardening.txt` |
| 架构门禁整跑 | **515 项全绿 / `GATE_EXIT=0`**；变异电池收尾再跑仍 `exit=0 红 0 条` | `s69_…txt`、`s70_…txt` 末行 |
| 变异反向验证（真树 + 进程内原始字节还原，逐颗「还原: 逐字节一致」） | **13 发**：MA1（给 `margin_raw` 凭空接上算点）红到新判据第 1/3/4/5/6 颗、连同既有「凭空造数」那颗共 6 条；MA1b（拆掉 `fees_raw` 算点）同样五颗 + 既有费用算点那颗；MA2（结构体字面量）只红第 2 颗；MA3 契约改口 / MA4 limitation 改名 / MA5 文档漏点一格 / MA6b 用例两处点名都注释 / MA7 合并 blob 复活 / MA8 给有算点的立「未算」 / MA9 删「不是 0」各**只红自己那一颗**（整跑红条数 1） | `s70_a5_mutation_battery.txt` |
| 电池抓到的两条更正 | MA10「行尾注释里的例子不该算构造点」这句预期**不成立**（实测红在第 2 颗）；MA6 只删用例里**一处**点名**不咬**——那一格还有第二处在断言同一件事，判据的真实范围是「字段在用例里还有没有名字」。两条都按实测改写进代码注释与本条记录 | 同上 |
| 安装包按当轮代码重建 | 两个对象分开测。wheel：`build_python_wheel.ps1` `WHEEL_BUILD_EXIT=0`，217,025 字节 / 17 条目，与上一轮那份**逐条目 CRC 全同**（载荷没变，变的只有 4 个条目的 zip 时间戳）；CLI 二进制才是契约所在：`cargo build --release -p qx-cli` 2m11s，11,999,232 字节的 exe 内数到"没有生产者" **5** 次 / "有生产者" **3** 次、契约前 200 字节整段在内，新 exe 实跑 `ecosystem` `ECO_EXIT=0`（含 `ecosystem_smoke.rs:297` 那发 `GET /schema/account-snapshot-v1`） | `s74_a5_package_rebuild.txt` |
| `build.bat` 九步端到端 | `QX_PYTHON=<venv 绝对路径> build.bat` → `[0/9]`…`[9/9]` 全过、`BUILD_BAT_EXIT=0`；`[1/9]` 515 项全绿、`[4/9]` 92 段全 ok / 858 passed / 0 failed、`[6/9]` `Ran 55 tests`；跑完 `git status --porcelain` 逐行与跑前相等、未跟踪新产物 **0** 份 —— 文档轮那条"整跑会落 72 份产物"的旧理由就此撤销 | `s75_a5_build_bat_full.txt` |
| 顺带抓到的一条新盲区（立案 #159） | 两份发布产物的**整档 sha256 都不可复现**：wheel 载荷 17/17 CRC 相同而整档 sha 变了；同一条 `cargo build --release` 跑两次的 exe 也是同尺寸不同 sha。README 里那处"sha256 前缀 = 产物身份证"的活口径本轮换成载荷口径，往轮的历史记录不改 | `s74` 第 4 节 |
| 整树测试 | `QX_PYTHON=<venv 绝对路径> cargo test --workspace --all-targets --no-fail-fast` → 72 段全 ok、**858 passed / 0 failed / 0 ignored**、`CARGO_EXIT=0` | `s71_a5_cargo_workspace.txt` |
| 取证探针的一处自纠 | 探针**第一版**按「文件里第一个 `#[cfg(test)]` 整行截断」剥测试，把 `api_service.rs` 的三个生产算点连着剥掉、报出「八格零生产者」；第二版改用门禁同源读法，读回三格有算点（`:354` 权益走 `ledger().equity_for`、`:361` 可用走结算账簿现金、`:387` 费用由逐笔成交 `checked_add` 加出）与五格无算点。两版并存留在日志里，含口径声明 | `s65_a5_null_field_probe.txt` |

**这一条判据将来怎么被「正确地改掉」**：给 `margin_raw` 接上真正的生产者时，第 1/3/4/5/6 颗同时红（MA1 实测），
要做的动作是撤契约那一格的措辞、删能力矩阵那一行、从文档句子里删名字、用例不再点它——四处一起改完才绿。
拦的不是接生产者，而是「接上了却有一处口径没跟着改」。

**本轮没做**：不动 `AccountSnapshot` 结构与线格式，不给任何一格接生产者（要先决定用哪把尺子，仍挂在
`three_equity_rulers_still_live`）；`/account/balances` 仍只公布 `margin_raw` 一格，其余四格只出现在
`/account/snapshot` 与 envelope；`maturity/capabilities.yaml` 的 `sandbox_tested` 与 `production_approved` 照旧
全为 `false`。发布面本轮**没留在"没做"里**：安装包按当轮代码重建（`logs/s74_a5_package_rebuild.txt`，wheel 与
CLI 二进制两件都重跑并逐字节复核）与 `build.bat` 九步端到端（`logs/s75_a5_build_bat_full.txt`）都已实测，
CI 的三平台 wheel 矩阵与 feature 矩阵仍只有 CI 跑。V13 §5 R1 的 A1/A2/A2b/A3/A4/A5/A6 到此全部落地。

## Unreleased — V13 文档轮：V11/V12 归档进 `docs/archive/`、README 改成产品说明、安装面每条数字重跑后再写（2026-09-26）

本轮**不动一行代码、不加一条判据**，只做三件事：把已被取代的两代审计移进 `docs/archive/` 并修好每一条
入站引用；把 README 从"逐轮编年 + 数字堆叠"改成能读的产品说明；把安装与使用文档里每条"实测过"的句子
当场重跑一遍，跑不起来的措辞改掉。过程抓到**三处文档与事实不符**（`### Fixed`），并新登记一条量具盲区
（文档引用 CLI 输出字面量这件事无人核对，#157）。

### Changed（归档动作本身：移动 + 横幅 + 路径同步，正文结论一字未改）

| 动作 | 实测取证 |
| --- | --- |
| `docs/自研量化框架重构方案-V11.md` → `docs/archive/` 同名；`docs/自研量化框架审计与重构方案-V12.md` 同步 | 移动后 3,734 行 / 384,944 字节、1,904 行 / 195,766 字节，两份都是 LF（`CRLF=0`）；标题下各加一节「归档状态」，写明被谁取代、数字是当轮快照、归档只改了哪几处 |
| 新建 `docs/archive/README.md` 索引 | 在册两份的行数/字节/覆盖轮次/归档日期/被谁取代/今天仍有用的部分，加三条读法纪律与"为什么留而不删"（V1–V10 那批是直接删除的，这里改用移动，理由写在索引最后一节） |
| 入站引用逐条同步 | `CHANGELOG.md`：V11 的路径 token 56 处（28 行）、V12 的 14 处（7 行）全部改指 `docs/archive/`，替换后旧路径残留 **0** 处；`docs/archive/…V12.md` 内部对 V11 的 3 处路径提及同步；`README.md` 的「文档地图」与「实现状态」两处链接改指 V13 + archive。两份归档文档里**没有**任何 markdown 链接指向别的文档（`](` 计数 0），所以不需要相对路径改写 |
| 门禁复跑确认归档没打断任何按路径取数的判据 | 能力矩阵证据路径判据（`docs/` 前缀在核对字符类里）仍绿 —— `maturity/capabilities.yaml` 唯一一条 `docs/` 证据行是 `工业化易用性收口指南-V1.md:193`，那份文档不移动 |

### Fixed（三处文档与事实相反，全部实测抓到）

1. **`docs/工业化易用性收口指南-V1.md` §1 称 `qx-cli help` 的末行指向本文** —— 实跑末行是
   「使用 `qx-cli help` 查看入口摘要；…完整说明见 README.md 与 deploy/README.md」，指向的是那两份，
   本文只在 README 的「文档地图」里（`logs/s51_docs_round_guide_claims.txt`）。同一轮顺带复核实测
   `qx-cli --version` 确实以退出码 2 fail closed。文档改为按事实写。
2. **README「安装」开头把日志位置写成 `%TEMP%/qx_v12p2/logs/`，而正文各处引用的都是 `logs/…`** ——
   `/logs/` 在 `.gitignore` 第 23 行，两者不是同一个地方。统一成 `logs/` 并当场写明：`logs/` 是本机目录、
   **不随仓库分发**，判据本身在 `tools/check_architecture.py` 与 `crates/*/tests` 里可重跑。
3. **README 称"本轮实测把全部 9 步端到端跑通了一次"** —— 那是 V12 §22 那一轮的事实（460 条 `PASS` /
   92 段 843 passed），本轮没有重跑 `build.bat`。改成如实标注，并把本轮真正重跑的两步单列：
   `[1/9]` 的架构门禁现在 509 条全绿、`[4/9]` 的整树测试现在 92 段 / 858 passed / 0 failed。

### Added（文档结构，不是代码）

- README 新增「产品说明」一节：一句话定位 + 三条主链路表（入口 / 产出 / 凭什么算可复核）+
  「它不是什么」四条边界（真账号、热插拔、统一撮合机、跨节点 HA 与柜台协议），把原来散在文末的
  三条"容易读过头的边界"提到最前面；「为什么用它」补两条今天才立得住的：钱字段的 `null`/`0` 分别、
  52 份示例模板逐份真读。
- README「文档地图」从"按文档列"改成"按你要做的事列"，并加一行"想知道某一轮**当时**修了什么" → archive。
- `deploy/README.md` 标题下加安装前置：本文只讲运维面、命令里的 `cargo run --release -p qx-cli --` 是
  源码树写法、装过的人换成 `qx-cli `，并指向收口指南。

### Validation（本轮日志 `logs/s46_*.txt`—`logs/s64_*.txt`；口径记录同步写进 V13 §9.9）

| 项 | 命令 | 实测 | 日志 |
| --- | --- | --- | --- |
| 整树测试（带解释器） | `QX_PYTHON=<venv 解释器> cargo test --workspace` | 92 个 `test result:` 段、**858 passed / 0 failed**、`TEST_EXIT=0` | `s46_docs_round_cargo_test_workspace.txt` |
| 安装路径 A | `cargo install --path crates/qx-cli --locked` | `Finished in 2m 14s`、`INSTALL_EXIT=0` | `s47_…_cargo_install.txt` |
| 仓库外六条入口 | 临时目录里 `help`/`init --strategy macd`/`doctor`/`backtest`/`report`/`status` | 六条退出码全 0；`init` 落 **9** 份自包含文件（那一行原本写 14，实为"init + backtest 之后"的目录总数，见 V13 §9.11 的更正）；`backtest` 再写 5 份：`data/qianxing/datasets.manifest.json` + 4 份同前缀 runs 产物，`result_hash=26fdd6b52d020700`（与 V12 §22 那一轮同一条命令逐字符相同） | `s48_…_installed_cli_smoke.txt`、`s49_…_install_measure.txt`、`s100_v13_r2_init_file_count.txt` |
| 命令面计数 | 已安装的 `~/.cargo/bin/qx-cli help`（在仓库外目录跑） | **52 行用法、41 个去重入口名**、help 输出 155 行、`HELP_EXIT=0`；入口名逐名列印进日志，README 那句"41 个入口"因此不再只由 `s49` 里一条正则失败的计数撑着 | `s54_docs_round_help_measure.txt` |
| 架构门禁（归档与 README 改写之后） | `python tools/check_architecture.py` | **509 项全绿**、`GATE_EXIT=0`，其中"能力矩阵证据路径全部存在"与三条 README 字面量判据都在绿侧。归档之后跑一次（`s50`），四份文档全部改完之后再跑（`s55`），V13 记完 §9.9 之后重跑到收尾（`s57`—`s62`），**六次全是 509/0**；README 定稿后又对最终字节的 README 跑了一次，仍是 509/0（`s63_docs_round_gate_after_readme_final.txt`） | `s50`、`s55`、`s57`—`s63` |
| 文档口径复核 | `qx-cli --version` 与 help 末行 | 退出码 2；末行指向 README 与 deploy/README（见 `### Fixed` 第 1 条） | `s51_docs_round_guide_claims.txt` |
| 安装包按当轮代码重建 | `powershell -NoProfile -ExecutionPolicy Bypass -File tools/build_python_wheel.ps1 -Python python\.venv\Scripts\python.exe` | `WHEEL_EXIT=0`，wheel 217025 字节 / 17 条目 / sha256 前缀 `2603b5c2be46926a`（上一份是 216439 字节、05:42 那轮产物，比当轮原生扩展旧 6 小时） | `s52_docs_round_wheel_build.txt` |
| 干净 venv 复核 wheel | `uv venv` + `uv pip install --offline --no-deps` + `tzdata` | 内嵌 `_qianxing_native.pyd` 与 `target/release/_qianxing_native.dll` md5 逐字节相同（`a9764db6131fb9cf…`）；`qianxing_ashare`/`_bridge`/`_ccxt`/`_strategy` 四包全部导入；`native.available() → True`；衍生品三字段归一成 `cross/hedge/3` 且读回一致；`margin_mode="weird"`/`position_mode="both"`/`leverage=0`/`position_side="Weird"` 各抛 `ValueError`；`VERIFY_EXIT=0` | `s53_docs_round_wheel_verify.txt` |
| 行尾与字节 | 逐文件按字节比对（`raw.count(b"\r\n")` vs `raw.count(b"\n")`） | `README.md`（521 行）/ `deploy/README.md`（815）/ `CHANGELOG.md`（4,393）/ 收口指南（412）改后**孤立 LF 为 0**（CRLF 数 = LF 数）；V13（783 行）、`docs/archive/README.md`（33）、两份归档文档（3,734 / 1,904）保持纯 LF（`CRLF=0`） | `s64_docs_round_line_endings_final.txt`（快照之后只有本条与 V13 §9.9 的引用行各改一次，README / deploy/README / 指南 / 归档字节未再动） |

**本轮没做**：`build.bat` 九步端到端（#82 未修 —— 它会把未跟踪产物写进仓库 `deploy/data/**`，本轮
为避免污染工作树而没有整体重跑）；任何代码、判据、夹具改动；`maturity/capabilities.yaml` 的
`sandbox_tested` 照旧全为 `false`。

### 量具盲区（登记，不在本轮修）

- **#157 文档引用 CLI 输出字面量无人核对**：`### Fixed` 第 1 条那处腐坏是靠人肉跑 `qx-cli help` 抓到的。
  门禁钉住的 README 字面量只有三条（PowerShell 入口带 Bypass、「不用系统时间」那句指向数据侧闸门、
  `deploy/README.md` 端点表 ≡ qx-api 路由集合），其余"某命令会印出这句话"的表述都是自由文本。
  候选判据与口径已登记为 V13 §4 L4 第 8 条，待立项。


## Unreleased — V13 R1-A6：52 份 deploy 模板逐份过一遍生产读法，三份示例被证明"照抄即坏"（2026-09-26）

§1.4 的那笔账是本轮的起点：`deploy/` 顶层 **52 份** JSON 模板里，**13 份没有任何代码或 CI 引用**，
其中 2 份（`qianxing.runtime.sqlite.example.json`、`qianxing.scheduler.jobs.smoke.json`）连文档都不提。
V13 §5 原本给的动作是"删掉那 2 份"，**实测不成立**：两份都能被它们那一类的生产读法读通
（`logs/s32_template_coverage_green_a6.txt` 第 8 行 `workers=3 storage=Sqlite`、第 52 行 `jobs=1`），
删它们等于把"没人读"这笔账用删除来销账。本轮改成给全部 52 份建立**按执行计的覆盖**：
每份模板在登记表里点名一类读取器，用例真的调用那个读点，并把它读出来的身份（worker 数、BarFrame
指纹、Bundle 摘要、作业条数……）印进日志。取证与口径见 `docs/自研量化框架审计与重构方案-V13.md` §9.8。

第一轮探针（`logs/s29_template_coverage_probe_a6.txt`）就报出 **9 处读取失败 + 1 处坏内容探针失败**，
逐条追下去是三类不同的问题，其中两类落在**已发布的示例文件本身**：

| 失败 | 根因 | 本轮处置 |
| --- | --- | --- |
| `qianxing.binance.spot.spec.json`、`qianxing.ccxt.okx.perpetual.spec.json`：`是 CCXT 形状却没有标的配对来源` | 判形状的那一步只看"有没有 `base_currency`"，产品规格形状自己带着 `instrument` 那一格，无需配对帧 | 登记表允许 `MarketSpec(None)`，标的由 `TradingInstrumentSpec` 的反序列化从文件自己那一格读出 |
| `qianxing.bar-frame.example.json`、`.okx`、`.ashare`：`invalid JSON BarFrame provider range` | 覆盖用例照抄 Provider 的入参形状，把 `start` 传成 `0`，而 `JsonBarFrameProvider::bars()` 第一步就拒 `start == 0` | 换成生产链真正用的那一对照点：`read_bar_frame_for_backtest` + `barframe_dataset_identity`（首末根时间戳即区间） |
| `qianxing.bar-frame.pairs-primary/reference`：`strict mode: unknown field \`frequency\`` | 见下面的 `### Removed` | 两份夹具各删两格 |
| `qianxing.submit-order.ccxt-derivatives`：`unknown variant \`Net\`, expected one of \`net\`, \`long\`, \`short\` at line 1 column 235` | 见下面的 `### Fixed` | 三格改成 snake_case |
| `qianxing.runtime.production`：`config validate 失败: strategy.research_snapshot_path 文件不存在 /var/lib/qianxing/…` | 不是缺陷：这份模板按设计带占位路径，`live-check` 文档明写"模板中的占位路径会按预期失败" | 不删预期，改成 `Expected::Refuses` 点名 3 项关键字，用例把拒绝理由原样印进日志（`logs/s32` 第 7 行） |
| 探针：`qianxing.costs.example.json 的读取器接受了坏内容` | `ExecutionCostRules` 是 `#[serde(default)]`，`{"coverage":"broken"}` 被静默补全成合法费率 | 坏内容探针改为**按读取器类别**给载荷，费率那一类给越界的 `taker_bp: 100001` |

### Removed（两份手写夹具里的两格，仓内无读者）

`deploy/qianxing.bar-frame.pairs-primary.example.json` 与 `...pairs-reference.example.json` 各删掉
`"frequency": "1h"` 与 `"quality": "recorded"`。这两格**只存在于这两份手写示例**（本轮 `grep` 全仓
`.rs`/`.py`：`frequency` 在 Rust 侧只有 `crates/qx-provider/src/lib.rs:28` 的 `DataQuery.frequency`
—— 那是**取数请求**里的一格，由调用方构造，BarFrame 文档里没有它；`quality` 作为字段名在 Rust 与
Python 两侧**零出现**（同文件 `:32` 那格叫 `quality_policy`，也是请求侧的）。这两格看起来就是把
`DataQuery` 的字段名抄进了行情文档。写侧同样不产它们：
`python/qianxing_bridge/__init__.py:37` 的 `BAR_FRAME_STRICT_FIELDS` 是这份文档的字段全集。
保留的后果是这两份示例**在 `schema_version >= 1` 的严格读点下必坏** ——
`crates/qx-data/src/provider.rs:231` 的 `parse_bar_frame` 先按旧格式宽解，`schema_version >= 1`
时改用 `#[serde(deny_unknown_fields)]` 的 `StrictBarFrameJson` 重解（`provider.rs:121`、`:254`）。

### Fixed（一份已发布示例照抄即失败）

`deploy/qianxing.submit-order.ccxt-derivatives.example.json` 的 `policy` 三格：`position_side`
由 `"Net"` → `"net"`、`margin_mode` 由 `"Cross"` → `"cross"`、`position_mode` 由 `"OneWay"` →
`"one_way"`。`crates/qx-core/src/trading.rs:29-46` 的三个枚举都带 `#[serde(rename_all = "snake_case")]`，
所以 PascalCase 字面量没有任何读者；这份示例被 `docs/外部链路验收执行方案-V1.md:69` 点名"只是载荷样例，
没有执行它的入口"，正因为无人执行，它带着读不懂的枚举躺在仓库里。现在它由覆盖用例的 SubmitOrder 读点执行。

### Added（1 个用例文件 + 9 颗判据）

- `crates/qx-cli/src/tests/deploy_template_coverage.rs`（779 行，登记进行数棘轮）三条用例：
  `every_deploy_template_is_registered_with_a_reader`（登记表与磁盘清单逐名相等，且没有重复登记）、
  `every_deploy_template_loads_through_its_registered_reader`（52 份逐份真读，15 类读取器各自调生产读点）、
  `every_reader_class_rejects_a_broken_template`（每一类各喂一份坏内容必须被拒，且探针覆盖数与变体数相等）。
- `tools/check_architecture.py` 的 `deploy_template_coverage_check()` 9 颗：登记表三列逐条对齐 /
  登记表与磁盘逐名相等 / `Reader` 变体清点 / 变体⊆已被使用 / 变体⊆坏内容探针 / 每类读取都落在生产读点 /
  三条用例与模块挂载在位 / 预期结果两档且 `Refuses` 必须点名 / 登记表点名的配对来源仍在册。
- 地板按本轮磁盘实测取值：`GATE_CHECK_FLOOR` 500 → **509**（+9）、`WORKSPACE_TEST_FLOOR` 863 → **866**（+3）、
  `CLI_TEST_FLOOR` 262 → **265**（src/tests 213 + `crates/qx-cli/tests` 递归 52）；
  `EXECUTION_TEST_FLOOR` 实测仍是 25（15 + 10），未动。

### Validation（日志在 `logs/s29_*.txt`—`logs/s43_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| 首轮探针（建表即跑） | `9 处读取失败` + `坏内容探针失败 1 处`，逐条见上表 | `s29_template_coverage_probe_a6.txt` |
| 覆盖用例收口后 | `test result: ok. 3 passed; 0 failed`，52 行 `=>` 身份打印（51 `Ok` + 1 按点名关键字被拒） | `s31_…_rerun_a6.txt`、`s32_template_coverage_green_a6.txt` |
| 行为变异 7 发（真树 + `%TEMP%` 逐字节镜像还原） | MA（漏登记一份）红 2 条、MB（读取器配错类别）红 2 条、MC（CostRules 走过场）红 1 条、MD（坏探针退回通用形状）红 1 条、ME（`Refuses` 关键字放宽）红 1 条、MF（示例退回 PascalCase）红 1 条、MG（夹具退回 `frequency`）红 1 条；末行「还原核对: 全部逐字节相同」 | `s33_a6_mutation_summary.txt`、`s33_mutation_0{1..7}_*_a6.txt` |
| 门禁判据变异 6 发（只改文本，跑门禁） | GA（登记表少一份）红 2 条、GB（探针少一个变体）红 1 条、GC（生产读点换成测试内自造校验）红 1 条、GD（用例改名）红 1 条、GE（配对指向不存在的文件，只改 2 处中的 1 处）红 1 条、GF（在生产侧另起一处规格标签落点）红 1 条；末行同样「全部逐字节相同」 | `s37_a6_gate_mutation_summary.txt`、`s37_gate_mutation_G{A..F}_a6.txt` |
| 门禁整跑 | 抬地板前 `509 项` 全绿；六发变异还原后再跑仍 `509`；地板抬到 509 之后第三次整跑 `[PASS]` 计数 **509** / `[FAIL]` 0 / 退出码 0 | `s35_a6_gate_second_run.txt`、`s36_a6_gate_after_mutation.txt`、`s40_a6_gate_floors_raised.txt` |
| 整树测试（`cargo test --workspace`，`QX_PYTHON` 指向 venv） | 92 个 `test result:` 段全 `ok`、**858 passed / 0 failed / 0 ignored** | `s38_cargo_test_workspace_a6.txt` |
| 在册 / 实跑双口径 | 磁盘 `#[test]` **866** 条 vs 实跑 **858** 条，差 8 条与 A4 轮同族（feature 门后），已由 #144 单独点名 | `s39_a6_floor_measure.txt` 对 `s38` |
| fmt / clippy | `cargo fmt --all -- --check` 退出 0；`cargo clippy --workspace --all-targets -- -D warnings` 退出 0（新用例一次通过，无 `redundant_closure` 复发） | `s43_a6_cargo_fmt_check.txt`、`s41_a6_clippy.txt` |
| 被跟踪产物零改写 | `git status --porcelain deploy` 4 条：3 份本轮修的模板 + `deploy/README.md`；`deploy/data` 下 **0 条** → blessed 产物未被重 bless | 本轮命令输出 |
| 能力矩阵计数 | 改动前 21 块 / 283 证据行（251 以仓库内路径开头）/ 75 limitation → 改动后 21 / **287**（**255**）/ **76**；`sandbox_tested` 与 `production_approved` 为真的仍然 **0** 条 | `s42_a6_caps_before.txt`、`s42_a6_caps_after.txt` |

### Documented

- `deploy/README.md` 新增"模板契约面：每份模板都有人真读"一节：52 份的读法登记表在哪、15 类读取器是哪些、
  为什么 production 模板按预期被拒；并把两条会咬人的口径写进接口文档 —— **`schema_version >= 1` 的
  BarFrame 文档只能带严格字段集**（宽/严两个读点同源，多一格就在 Provider 侧炸），
  以及 SubmitOrder 载荷里 `policy` 的三格是 snake_case 枚举名。
- `maturity/capabilities.yaml`：`cli_scenario_init` +3 条证据行与 1 条 limitation
  （`production_runtime_template_refuses_validation_by_design_…`），`ccxt_rest` +1 条（那份衍生品示例的
  三格枚举字面量）。
- README「当前状态」段的数字整体换成上面这轮实测值（500 → 509、863 → 866、855 → 858、283/251 → 287/255）。

### 本轮抓到的一条量具盲区（进 V13 §9.8 与 §4 L4）

**同一份 BarFrame 有两个生产读点，字段集不一样**：回测直读链先走宽口径的 `read_bar_frame_for_backtest`
（`BarFrame::from_json`，未知字段忽略），紧接着 `barframe_dataset_identity` 走 Provider 的严格口径
（`deny_unknown_fields`）—— 于是 `frequency` 这一格能在仓库里活很久：**只跑前者的用例全绿，跑后者的链路必红**，
而 §18-B 那批 BarFrame 用例恰好全是前者。同类形状还有 `JsonBarFrameProvider::bars()` 拒 `start == 0`
（`crates/qx-data/src/provider.rs`），这是"合法区间"的最低门禁，任何按"整份文件"取数的新读点都得先满足它。
本轮没有把两个读点合成一个（那会牵动 Provider 的入参形状，属 §6 之外的动作），而是把口径钉成判据：
登记表里 BarFrame 那一类必须点名**生产的那一对**读点，第 6 颗（"每类读取都落在生产读点上"）盯的就是这个。


## Unreleased — V13 R1-A4：记账币种的缺省值收成一个常量，并证明它会动发布产物（2026-09-26）

"没人声明时按 USDT 记账"这句话，本轮实测在 Rust 生产代码里写着**四份**
（`crates/qx-cli/src/backtests/mod.rs:31`、`venue_runtime/worker_runtime.rs:23`、同文件 `:76`、
`ecosystem_smoke.rs:346`；取证见 `logs/s28_currency_sites_a4.txt`，口径与门禁一致：只认引号里整串
等于 usdt 的出现，`BTCUSDT` 这类标的名不算）。四份抄本的危害与 A3 那族一样：改一处就把另外三处留在
原地，产物里的账簿币种与代码里的判词分叉而无人报错。同一轮还要回答另一半问题 —— **记账币种真的进了
发布产物吗**？它决定现金腿落在哪本账，如果两条只差币种的回答同一个 `result_hash`，产物就无法证明
"这份收益是哪种币的收益"。取证与口径见 `docs/自研量化框架审计与重构方案-V13.md` §9.7。

### Changed（4 处字面量 → 1 处常量；对外的取值一格未变）

- 新增 `crates/qx-core/src/identity.rs` 的 `pub const DEFAULT_SETTLEMENT_CURRENCY: &str = "USDT"`，
  注释写明它只是**缺省值**、不是合法币种白名单。四条回落链逐个接回：回测装配
  （`.unwrap_or_else(|| DEFAULT_SETTLEMENT_CURRENCY.into())`）、worker 自身声明
  （`.unwrap_or(DEFAULT_SETTLEMENT_CURRENCY)` 后仍 `to_ascii_uppercase()`）、账户日志的写入方全体缺席
  （`.unwrap_or_else(|| DEFAULT_SETTLEMENT_CURRENCY.to_string())`）、生态烟测的现金簿键。
- 改造后 Rust 生产代码里的整串 `"USDT"` 只剩定义点那一格加它自己的文档注释
  （`logs/s28_currency_sites_after_a4.txt`：`.rs` 4 → 2，两处都在 `identity.rs`）。全仓整串计数
  45 → 49，多出的 6 处全部是本轮门禁脚本里判据自己的字面量（`SETTLEMENT_CURRENCY_LITERAL` 与其注释）。
- **无破坏性变更**：`settlement_currency` 缺席时四处的答案仍是 `USDT`，声明了任何币种时读声明值，
  因此不触发 `### Removed`；配置字段本身（`Option<String>`）与 `skip_serializing_if` 形状未动。

### Added（3 条用例 + 11 颗判据）

- `crates/qx-cli/src/tests/settlement_currency_single_source.rs` 三条：三条回落链同值（含把 paper
  模板里所有 worker 的声明抹掉后走真实账户日志判定）、声明缺省币种与不声明等价、
  **只换 `settlement_currency` 必须换掉发布产物的 `result_hash`**（走真实回测入口跑两遍再比对）。
- `tools/check_architecture.py` 的 `settlement_currency_check()` 11 颗：定义点唯一且值仍是 `USDT` /
  全仓只有一颗 `*SETTLEMENT_CURRENCY*` 公共常量 / 生产代码除定义点外没有第二处写死的 `"USDT"` /
  定义处带着"缺省值≠白名单"的口径说明 / 消费者登记表与实盘逐一对应 / 四条回落链各自仍读常量 /
  三条用例在册 / 指纹用例钉的是 `result_hash` 而不是随墙钟动的 digest。
- 地板按本轮实测取值：`GATE_CHECK_FLOOR` 489 → **500**（+11）、`WORKSPACE_TEST_FLOOR` 860 → **863**（+3）、
  `CLI_TEST_FLOOR` 244 → **262**（本轮 +3，其余 15 条是历轮未回填的欠账，按当轮实测一次补齐）。

### Validation（日志在 `logs/s28_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| 门禁整跑 | `exit=0`，500 条 `PASS` / 0 条 `FAIL`（本轮 +11 颗） | `s28_gate_final_a4.txt` |
| 文档回合之后复跑门禁（本轮改了 README / deploy/README / capabilities / V13 / CHANGELOG） | 两份日志各自 `gate_exit=0`、500 条 `PASS` / 0 条 `FAIL`，末行打印「架构不变量自检全部通过 ✓（500 项）」；新增的 4 条证据行逐条被"证据路径存在性"判据认下（第二份是 §9.7 定稿之后再跑一次） | `s28_gate_docs_a4.txt`、`s28_gate_docs_final_a4.txt`、`s28_capability_counts_a4.txt` |
| 静态判据变异 5 次运行（真树 + `%TEMP%` 逐字节镜像还原） | MG1（回测兜底退回字面量）红 3 条、MG3（缺省值改 USDC）红 1 条、MG4（未登记的第五处消费者）红 1 条；MG2 首发只红 1 条，收紧签名匹配后复跑红 2 条；每次还原后回到 500 全绿 | `s28_gate_mut_a4.txt` |
| 行为变异（把 `crates/qx-xingban/src/backtest.rs` 的初始入金币种写死为 `"USDT"`） | 币种指纹用例 `changing_only_the_settlement_currency_moves_the_published_fingerprint` 变红（`2 passed; 1 failed`），另两条仍绿；按镜像逐字节还原后 3 条全绿（`3 passed; 0 failed`） | `s28_cargo_mut_a4.txt`、`s28_cargo_green_a4.txt` |
| 整树测试（`cargo test --workspace --all-targets --no-fail-fast`，`QX_PYTHON` 指向 venv） | 72 个测试二进制段、**855 passed / 0 failed**、退出码 0（较 A3 轮的 852 恰 +3 = 本轮 3 条新用例） | `s28_cargo_test_workspace_a4.txt` |
| Doc-tests（`cargo test --workspace --doc`） | 21 段全 ok、0 passed（仓库无 doctest）、0 failed；与上一行合起来 93 段 | `s28_cargo_test_workspace_doc_a4.txt` |
| `cargo test -p qx-storage --features sqlite`（feature 门后的 8 条不在整树里） | 10 段全 ok / 56 passed / 0 failed / 退出码 0 | `s28_cargo_test_storage_sqlite_a4.txt` |
| `cargo fmt --all -- --check` | 退出 0 | `s28_cargo_fmt_check_a4.txt` |
| `cargo clippy --workspace --all-targets -- -D warnings` | 首跑退出码 101，唯一一条是本轮新用例里的 `redundant_closure`（`settlement_currency_single_source.rs:49` 的 `.any(\|worker\| owns_account_event_log(worker))`）；改成 `.any(owns_account_event_log)` 后复跑 2 行输出、0 warning、`clippy_exit=0`（首跑那份日志被复跑覆盖） | `s28_cargo_clippy_a4.txt` |
| 被跟踪产物零改写（跑完整套测试之后 `git status --porcelain deploy`） | 1 条：` M deploy/README.md`（本轮接口文档自己改的），`deploy/data` 下 0 条 → blessed 产物未被重 bless | 本轮命令输出 |
| 能力矩阵计数（沿用 A3 那份脚本，口径是"缩进两格的条目"） | 改动前 21 条 / 279 证据行（其中以仓库内路径开头 247）/ 75 limitation；改动后 21 条 / **283**（**251** 以路径开头）/ 75。其中带 `implementation` 键的能力块是 19 个（另两条 `single_node`/`distributed` 是部署形态），16 个同时满足 `implementation` 与 `code_tested`，`sandbox_tested`/`production_approved` 无一为真 | `s28_capability_counts_before_a4.txt`、`s28_capability_counts_a4.txt` |

### Documented（口径进接口文档）

- `deploy/README.md` 在"账户事实两条口径检查"那段之后新增 `settlement_currency` 缺省值口径：缺省值只有
  一处定义、"没写这一格"与"写了 USDT"同解、换缺省币种改哪一行，以及"只换币种必须换掉 `result_hash`"
  这条产物级承诺及其用例现场。
- `maturity/capabilities.yaml`：`local_backtest` +2 条证据（装配回落读常量、指纹用例走真实入口）、
  `paper_execution` +2 条（worker 与账户日志两条链同缺省、paper 模板的三条用例）。
- `README.md` 的"当前状态"段按本轮日志重抄；`docs/自研量化框架审计与重构方案-V13.md` 新增 §9.7，
  并把 §4 L2-6 与 §5 A4 两行的状态改成已落地。

### 本轮抓到的一条量具盲区（登记为待办 #152 的补充）

- MG2 那发变异（给币种指纹用例改名）第一次**只红一条**判据：`_fn_body(text, sig)` 是按前缀匹配的，
  改名后的函数体仍能被旧名定位到，于是"在册"判据与"断言现场"判据重合成了同一条。把 needle 收成带
  左括号的完整签名后复跑，同一发变异红 2 条。这与 §9.6 记的"登记表类判据会被注释骗绿"是同一类失效：
  **定位用的字符串要收到能被改名的那一格，不能停在名字前缀**。


## Unreleased — V13 R1-A3：venue 识别收成一个函数，19 处 worker 级判定式全部作废（2026-09-26）

运行拓扑里有六处要问"这个 worker 属于哪个 Venue 家族"，而 `crates/qx-core` 早就有
`WorkerConfig.venue_id: Option<String>`。本轮实测：**同一个问题在生产代码里有 19 处写法**
—— 9 处问"是不是 Binance"（`qx-orchestrator` **五份逐字相同**的
`.map(|venue| venue.to_ascii_lowercase().contains("binance")) == Some(true)`、`qx-cli` 三份
`is_some_and` 变体、`qx-runtime` 一份私有 `fn is_binance(&Option<String>)`），10 处问"是不是
Paper"（`is_some_and` 与两种 `map(...).unwrap_or(...)` 抄法，None 的走向一份是"报错"一份是"放行"），
另有标的级整名比较 `instrument.venue.as_str().eq_ignore_ascii_case("BINANCE")` **六处**。危害方式不是
"当下算错"，而是下一次改口径只改其中一份：把子串收成整名，`binance-testnet` 会静默脱离 Binance 家族，
编排照样返回 `Ok` 而那条 worker 没人拉起；把 Paper 的整名放宽成前缀，`paper-proxy` 会被当成本地虚拟
撮合域。本轮把三条口径收进一个定义点，并给"只有一处定义"装上点名式判据（取证与口径见下表与
`docs/自研量化框架审计与重构方案-V13.md` §9.6）。

### Changed（19 + 6 处判定式换成一次读点；语义逐格保持）

- 新增 `crates/qx-core/src/venue.rs`：`pub enum VenueFamily { Paper, Binance, Other }` 与
  `parse(&str)` / `parse_option(Option<&str>)`。三条口径写在函数体里并各有一条用例：Binance 是
  **trim + 小写后的子串**、Paper 是 **trim + 小写后的整名**、缺席**不等于** `Other`（仍是 `None`，
  内核底线第 9 条）。
- 19 处 worker 级写法 → `VenueFamily::parse_option(worker.venue_id.as_deref()) == / !=
  Some(VenueFamily::Paper | Binance)`。`!=` 的两处（`paper_worker.rs` 的原
  `map(|venue| !…).unwrap_or(true)`、`topology_validation.rs` 的规格豁免）保留 None⇒报错/None⇒不豁免
  的方向；`paper_submit.rs` 那份 `unwrap_or(false)` 保留 None⇒不匹配。11 个消费者文件逐个登记进
  门禁的 `VENUE_FAMILY_CONSUMERS`。
- 6 处标的级整名比较 → `VenueId::is_binance()`（`crates/qx-core/src/identity.rs:30`）。那里必须是
  **整名**：`BINANCE` 是产品 venue 名、不是一种账户域，定义处注释写明"不能改成子串"。
- `scheduler.rs` 的 `environment.eq_ignore_ascii_case("paper")` 判的是部署环境不是 Venue，未并进本轮，
  四条判定式指纹都带 `venue` 接收者，所以它不会被误伤。
- 为满足 500 行棘轮（本轮把新增用例留在 `lib.rs` 时门禁报过
  `crates/qx-orchestrator/src/lib.rs(537 行) 未登记`，见 `logs/s27_gate_a3_draft.txt`），把
  `crates/qx-orchestrator` 的内联用例模块拆到 `src/tests.rs`：`lib.rs` 313 行 + `tests.rs` 225 行，
  `maturity/line_budgets.yaml` 无需新增登记项。

### Removed（私有面，附"仓内无读者"取证）

- `crates/qx-runtime` 的私有 `fn is_binance(venue_id: &Option<String>) -> bool`（3 个调用点：
  两条 `&& is_binance(&worker.venue_id)`、一条 `Reconciler && is_binance(...)`）。取证：它不是
  `pub` 项、只在该文件内被引用，改后 `grep -rn "fn is_binance" crates/` 只剩两处命中 ——
  `VenueId::is_binance`（新单源）与 `binance_venue.rs:3` 的 `is_binance_testnet`（判 testnet 端点，
  与家族无关，保留）。因此不外溢到任何公共面，随仓库发布的产物与命令面口径不变。

### Added（5 条用例 + 8 颗判据）

- `venue.rs` 三条家族用例（子串与大小写 / 整名且前缀不算 / 缺席不成 `Other`），加上编排侧两条
  **消费者**用例：`binance-testnet` 必须走私有 Binance worker；`" Paper "`（带空格）仍走
  `paper-worker`，而把同一格换成 `paper-proxy` 后 `plan_workers` 直接 `Err`。
- `tools/check_architecture.py` 的 `venue_identity_check()` 8 颗：定义点形状唯一（`pub fn parse(` /
  `pub fn parse_option(` / `pub enum VenueFamily` / `pub fn is_binance(` 各一处）、四条已作废判定式
  不得在生产代码复活、消费者登记表与实盘逐一对应、三条家族用例在册且 `#[test]` 数不减、编排两处
  断言现场各自钉住（testnet 分派 / `paper-proxy` 反例）、账户命名用例钉住同一家族口径、
  两份口径的差异必须写在各自定义处注释里。
- 地板按本轮实测取值：`GATE_CHECK_FLOOR` 481 → **489**（+8）、`WORKSPACE_TEST_FLOOR` 855 → **860**（+5）。

### Not done as planned（一处对方案的偏离，如实记录）

- V13 §5 A3 原本设想装一条通用的"同形 lambda 出现 ≥3 次即报"判据。本轮实测**不成立**：按与
  `merge_duplicate_block_check` 相同的取数口径扫 172 个生产代码文件（`crates/*/src/**/*.rs`，剔除
  tests 目录与 `*_tests.rs`、截断到首个 `#[cfg(test)]`），W=10 的逐字重复窗口有 **480 组 / 涉及
  1,095 个位置**，去掉捕获名后同形闭包体 ≥3 次的有 **101 组 / 492 个**（后者是粗归一化的上界，
  方法连取数口径一起落在 `logs/s27_shape_scan_a3.txt`）。绝大多数是合法并列（编排五个角色分支各自的
  启动参数、三家 Venue 的提交链、几家存储后端）。装上它要么第一天上百条允许清单，要么逼人把合法并列
  改绕。故改成"每条收拢口径一张指纹表 + 一份消费者登记表"的点名式判据，通用形状判据留在 R3-3 未装。
- 最直观的一条证据就在本轮产物里：收拢**之后**，`crates/qx-orchestrator/src/lib.rs:56,78,…` 那五个角色
  分支在 W=10 窗口下仍然逐字相同（`s27_shape_scan_a3.txt` 的第一组就是它，5 份；那里的行号按门禁口径
  是"去空行后的序号"，故与 56/78 不等）。相同的是"都调用同一个函数"，而这正是想要的形状 ——
  所以判据钉的是**那个函数只有一处定义**，不是"这段调用出现了几次"。
- 顺带说明 V12 §20 那条重复块判据（`MERGE_DUP_WINDOW`）为什么抓不到上面那 5 份**逐字相同**的复制：
  它的零容忍窗口只对 `tools/*.py` 生效，而同一把尺子在 Rust 侧刚量出 480 组合法并列 —— 这也是本轮把
  "只有一处定义"做成登记表而不是做成重复扫描的原因。

### Validation（日志在 `logs/s27_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| 门禁整跑 | `gate_exit=0`，489 条 `PASS` / 0 条 `FAIL`（本轮 +8 颗） | `s27_gate_final_a3.txt` |
| 文档回合之后复跑门禁（本轮改了 README / deploy/README / capabilities / V13 / CHANGELOG） | 仍 489 条 `PASS` / 0 条 `FAIL` / `gate_exit=0`；能力矩阵计数同场重测 | `s27_gate_docs_a3.txt`、`s27_capability_counts_a3.txt` |
| 静态判据变异 12 次运行（`.gate_mut/` 副本树，真树未动） | BASELINE 0 红；M1–M11 **每发恰好点名 1 条红**，`合计 12 次运行，未咬住 0 条：[]` | `s27_gate_mut_a3.txt` |
| 行为变异 5 项检查（真树 + `%TEMP%` 逐字节镜像还原） | B1（子串→整名）同红两条编排/账户用例、B2（整名→前缀）红 `exact_paper_venue` 用例；复跑两次全绿；`合计 5 项检查，未咬住 0 项：[]` | `s27_behaviour_mut_a3.txt` |
| 整树测试（`cargo test --workspace --all-targets --no-fail-fast`，`QX_PYTHON` 指向 venv） | 72 个测试二进制段、**852 passed / 0 failed**、`workspace_exit=0`（较上轮 847 恰 +5 = 本轮 5 条新用例） | `s27_cargo_test_workspace_a3.txt` |
| Doc-tests（`cargo test --workspace --doc`，`--all-targets` 不含它，所以单跑补全口径） | 21 段全 ok、0 passed（仓库无 doctest）、0 failed、`doc_exit=0`；与上一行合起来 93 段 | `s27_cargo_test_workspace_doc_a3.txt` |
| `cargo test -p qx-storage --features sqlite`（feature 门后的 8 条不在整树里） | 10 段全 ok / 56 passed / 0 failed / 退出码 0 | `s27_cargo_test_storage_sqlite_a3.txt` |
| Python 全量 / 内核自校验 | `Ran 55 tests OK (skipped=1)`（与 A2 轮同数，本轮未动 Python）/「全部自校验通过 ✓」 | `s27_python_unittest_a3.txt`、`s27_validate_core_a3.txt` |
| fmt / clippy | `cargo fmt --all -- --check` 退出 0；`cargo clippy --workspace --all-targets -- -D warnings` 退出 0 | `s27_cargo_fmt_check_a3.txt`、`s27_cargo_clippy_a3.txt` |
| `venue_id` 取值分布实测（本轮新量具，三条口径各自的真实支撑面） | 全仓 103 处赋值：`paper` 36 / `okx` 23 / `binance-testnet` 19 / `binance` 15 / `" Paper "` 2 / `BINANCE` 2 / `Paper` 1 / `paper-proxy` 1 / `other`\|`unmanaged-venue`\|`unknown-venue`\|`OKX` 各 1。子串口径要保住 `binance`+`binance-testnet`+`BINANCE` = **36 处**，trim 口径要保住 `" Paper "` 那 **2 处**，整名口径要排除 `paper-proxy` 那 **1 处** | `s27_venue_values_a3.txt` |
| 通用"同形写法"判据的可行性实测（决定 A3 用点名式还是形状式） | 172 个生产代码文件：W=10 逐字重复窗口 **480 组 / 1,095 个位置**、同形闭包体 ≥3 次 **101 组 / 492 个**；收拢后的编排五分支仍在第一组里 | `s27_shape_scan_a3.txt` |

### Documented（口径进接口文档）

- `deploy/README.md` 的 `paper_initial_cash_raw` 段之后新增"`venue_id` 怎么被读"三条口径（子串 / 整名 /
  缺席），点名 `paper-proxy` 那条反例的用例现场；此前这三条只散在各处注释里。
- `maturity/capabilities.yaml` 的 `paper_execution` 块 +2 证据行（家族单源与其消费者用例现场），
  实测计数随之为 19 块 / 279 条证据行 / 其中 247 条以仓库内路径开头 / 75 条 limitation
  （`logs/s27_capability_counts_a3.txt`）。
- `README.md`：构建段明确标出 `logs/s22_build_bat_full.txt` 那串 460/843 是历史快照、与今天的门禁条数
  不是一回事（§4 L3-1 抓的正是"两个历史值并存"）；"当前状态"段按本轮日志重抄（481 → 489 条、
  地板 855 → 860、847 → 852 passed、证据行 277 → 279 / 245 → 247），并把 A1 那节的引用从 §9.3 改对到 §9.1。
- **本轮自己制造、也当场抓到一处文档漂移**：A3 在 `paper_worker.rs` 里删掉的几行使
  `reject_split_account_principal` 的调用点从 103 移到 **93** 行，而 `maturity/capabilities.yaml` 的
  证据行写的是 `paper_worker.rs:103`。门禁的"证据路径全部存在"那颗把 `:NN` 剥掉再查文件
  （`tools/check_architecture.py` 里 `re.sub(r":\d+$", "", token)`），所以锚点失效不会让它红。
  本轮手改这一处，并把它作为量具盲区登记进 V13 §9.6 L4-3。

### Corrected（本轮量具自己的三处设计缺陷，当场改红）

- 第一版判据把"已收拢的判定式不得复活"跑成 8 个文件命中 —— 因为 Paper 那一侧 10 处当时**还没收拢**。
  改成先收完再钉指纹，并把指纹全部加上 `venue` 接收者锚。
- "消费者登记表逐一对应"那颗一开始被 `identity.rs` 的文档链接骗绿：判据扫的是原文，注释里的
  `VenueFamily` 也算消费者。改成扫 `without_line_comments(non_test_source(..))`，文档注释单独取数
  喂给那颗"差异写在注释里"的判据。
- M4 第一次不咬（把登记项换掉后行里仍留着 `VenueFamily::Paper`）、M6 的 needle 因 `cargo fmt` 重排
  缩进而 0 命中、M11 原设计（改名 `parse_lenient`）根本不会撞到 `pub fn parse(` 的计数 —— 三条分别
  改成整表达式替换、按 fmt 后文本取 needle、复制出第二个 `pub fn parse_option(` 签名。


## Unreleased — V13 R1-A2：A 股线格式两侧对照钉住，写侧顺带修掉三处口径（2026-09-26）

`python/qianxing_ashare` 与 Rust 读侧（`crates/qx-xingban/src/ashare*`）共用一套公司行为线格式，此前
**没有任何判据比过两侧**：字段名册、动作名册、单位口径各抄一份，抄歪了全树用例照样绿。这一轮把对照
钉上，钉的过程本身就是收获 —— 三条都是实测抓出来的，不是读代码读出来的（取证见下表与
`docs/自研量化框架审计与重构方案-V13.md` §9.4）。

### Fixed（Python 写侧，三处真实口径错）

- **写侧认不回自己写出的动作名**：`_canonical_action_type` 是中文别名的子串匹配表，`suspension` 不含
  `suspend`、`new_share_issue` 不含 `增发`/`新股`，所以已经规范化的行读第二遍会掉成 `unknown` ——
  停牌事实丢、增发按现金红利参与折算。补上"线格式名精确认回"的短路（16 个名字逐个用例覆盖）。
- **`*_raw` 输入列被再乘一次 SCALE**：`cash_dividend_raw: 500000000`（每股 0.5 元的已定点值）读进来
  变成 5e17，即 5 亿元每股。按 `git show HEAD` 的那份基线数出来：**四列**走的是这条双重放大
  （`cash_dividend`、`interest_per_bond`、`settlement_qty`、`settlement_price`），另有**九列**的
  `_raw` 名根本不在输入别名里，规范化过的行读回时那格丢成 0（`rights_issue_price`、`issue_price`、
  `conversion_price`、`subscription_qty`、`rights_expiry_qty`、`repurchase_qty`、`repurchase_price`、
  `conversion_qty`、`conversion_target_qty`），只有 `issuer_total_shares` / `issuer_free_float_shares`
  两列当时是对的。新增 `_amount_pick`：**单位规则收敛到一处**，`<field>_raw` 视为已定点整数，其余别名
  按元/股乘 SCALE，15 个金额/数量字段全走它；比例另有 `_ratio_pair_pick` 认
  `<field>_num` / `<field>_den`。
- **带时区的 ISO 公告日期回落成除权日**：`_date_text` 只切空格，`2024-05-28T00:00:00+08:00` 解析失败
  返回 `None`，调用方把它当"没有公告日期"，于是 `published_at` 取除权日 —— PIT 可见时间被悄悄改晚，
  恰好是复现"公告日之后才可见"这类研究结论时最不能错的一格。改成空格与 `T` 两种分隔都截断。

### Changed（行为口径，会改结果）

- 上述第二条会改变**任何用 `_raw` 列名喂进 Python 规范化器的行**的结果：先前它按元/股解释（放大 1e9 倍）
  或丢成 0，现在按已定点整数收下，与 Rust 读侧、`deploy/README.md` 的线格式段同一口径。仓库内没有这种
  喂法：`normalize_corporate_action_rows` 的调用点在 Python 模块内部三处 provider 读行路径与用例里，
  而 `deploy/qianxing.ashare.actions.example.json` 与 `complex-actions.example.json` 那两份带 `_raw` 键的
  样例是**信封**、由 Rust 读侧装载，不经这条 Python 路径。因此不外溢到任何随仓库发布的产物；
  自定义 provider 若曾依赖那个错口径，需要改回真值。
- 第三条让 `published_at` 在带时间的公告日期下从"除权日"变成"公告日 00:00 上海"。仓库内样例走的是
  日期粒度，结果不变。

### Added（一条夹具链 + 两侧用例 + 14 颗静态判据）

- `python/tests/fixtures/ashare_actions_cross_check.{rows,payload,expectations}.json`：数据源原始行 →
  Python 写出的 v1 信封 → 由信封派生的期望值。两侧共读，锚定那格
  `(10 + 5×0.2 − 0.5) ÷ (1 + 0.3 + 0.2) = 7.00` 由 Python 与 Rust **各自独立复算**（延续 V11 R17
  日历指纹夹具那条"谁都不抄它"的设计）。
- Python 六条用例（payload 逐字节重算、期望值派生、锚独立复算、16 个动作名逐个认回、`_raw` 与别名
  两种单位口径不混、ISO `T` 日期不回落）、Rust 两条用例（信封读回逐字段比 + 喂进 A1 那道折算锚）。
- `tools/check_architecture.py` 的 `ashare_cross_language_contract_check()`：14 颗判据覆盖版本号、
  三份字段名册（逐项、含顺序、且等于 Rust 声明的数组长度）、16 个动作名、写侧认回自己的名字、
  定点单位单源、两种 ISO 分隔、夹具齐备、两侧词根逐字相同、以及**锚定期望值不得抄成数字字面量**。
- `deploy/README.md` 新增"公司行为 v1 线格式"段：5 个信封键、35 个动作键、16 个动作名、单位与日期
  两条口径、以及三条离线自证命令。

### Validation（日志在 `logs/s26_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| 门禁整跑 | `exit=0`，481 条 `PASS` / 0 条 `FAIL`（`GATE_CHECK_FLOOR` 467 → **481**，本轮 +14） | `s26_gate_a2.txt` |
| 静态判据变异 15 例（`.gate_mut/` 副本树，真树未动） | baseline 14 项 0 红；M1–M15 **每发恰好点名 1 红**，`VOID-JUDGES: none` | `s26_mutate_a2_gate.txt` |
| 行为用例变异 6 例（真树 + `%TEMP%` 逐字节镜像还原） | B1–B6 全 HIT；B4（payload 改一字节）与 B5（锚期望值改一格）**两侧同红** | `s26_mutate_a2_behaviour.txt` |
| Python 全量 | `Ran 55 tests OK (skipped=1)`，本轮 +6 条（49 → 55） | `s26_pytest_full.txt` |
| 整树测试（`cargo test --workspace --all-targets --no-fail-fast`，`QX_PYTHON` 指向 venv） | 72 个测试二进制段、**847 passed / 0 failed**、退出码 0（较上轮 845 恰 +2 = 两条新 Rust 用例）；跑完 `git status -- deploy/data` 0 项 | `s26_cargo_test_workspace.txt` |
| Doc-tests（`cargo test --workspace --doc`，`--all-targets` 不含它，所以单跑一遍补全口径） | 21 段全 ok、0 failed、退出码 0；与上一行合起来就是 93 段 / 847 passed | `s26_cargo_test_workspace_doc.txt` |
| `cargo test -p qx-storage --features sqlite`（feature 门后的 8 条不在上树里） | 10 段全 ok / 56 passed / 0 failed / 退出码 0，与上一轮同形 | `s26_cargo_test_storage_sqlite.txt` |
| fmt / clippy / 行数棘轮 | `cargo fmt --all --check` 退出 0；`cargo clippy -p qx-xingban --all-targets -- -D warnings` 干净（新用例里一处 `u64 → u64` 冗余转换被它当场逮到）；`line_budgets.yaml` 重快照新增 `ashare/tests.rs: 764` | `s26_fmt_clippy_a2.txt` |

### Corrected（本轮量具自己的三处空转，全部当场补红）

- **第一轮 14 颗里有 3 颗打不红**：`VOID-JUDGES: [M8, M10, M13]`。M8 只查 `text.split("T", 1)[0]`
  在不在，把 `elif "T"` 改成 `elif "Z"` 那半行还原样在；M10 用子串查词根，`STEM =
  "..._cross_check_forked"` 照样算引用了同一份；M13 在整个 `tests.rs` 里查
  `expected_reference_raw`，把断言换成字面量 `7_000_000_000` 后它在别处还出现一次。三条分别改成
  "条件与截断都要在"、"两侧按整名引用夹具"、"断言现场不得是裸数字"，重跑 14/14 命中；再为下面那条
  补上第 15 发（M15），最终 `VOID-JUDGES: none`。
  → 又一条 V13 §4 L4 证据：**判据的取证范围要收到被断言那一格，不能是"文件里出现过"**。
- **锚那一格原本只有 Rust 有立场**：B5（改共读的 `expected_reference_raw`）第一次跑，Python 侧全绿 ——
  `test_expectations_are_derived_from_the_blessed_payload` 只派生计数与逐事件行，从不复算锚。补一条
  Python 侧独立复算公式的用例后，B5 才做到"两侧同红"。
- 磁盘 `#[test]` 地板 853 → **855**（两条新 Rust 用例）；在册/实跑差仍是 8 条 feature 门后用例，
  与 V12 同形，挂在 `#144` 未动。


## Unreleased — V13 R1-A1：除权除息日的锚改为从已装载的公司行为折算（FN3 收口）（2026-09-26）

V11 Q60 把涨跌停的锚从"上一根 Bar"改成"上一交易日收价"，同时在 `maturity/capabilities.yaml` 留了一条
limitations：`ashare_previous_close_raw_has_no_in_repo_producer_so_ex_rights_anchors_stay_unadjusted`。
当时的事实是：覆盖表是除权日唯一的正确锚出口，而仓库里没人往那张表里写东西。这一遍不去补写表的人，
而是把**已经装载好的红利/送转/配股**接进锚的计算 —— 数据侧那条路（`ashare_binding.rs` 与 `runtime_check.rs`
各自调用 `apply_corporate_actions_json`）本来就在，只是锚从不读它。

### Changed（行为口径，非破坏性但会改结果）

- **除权除息日的昨收现在会折算**（`ashare/trading.rs` 新增私有 `ex_rights_reference`）：
  `(昨收 + 配股款 − 每股现金红利) ÷ (1 + 送转比例 + 配股比例)`，结果按 `price_tick` 对齐。
  动作识别复用内核账本那一份 `is_cash_dividend_action`（由私有改 `pub(crate)`），**没有第二份动作清单**。
- **`previous_close_raw` 降级为覆盖出口**：它仍是锚的第一个读点（手工指定优先于折算），但不再是唯一出口。
  字段与 `deploy/qianxing.ashare.rules.json` 的样例键都保留 —— 本轮明确不改产物形状（Q1b 的重 bless 债另账）。
- 折算只在跨日推导**之后**发生：无公司行为的普通交易日，锚与 Q60 落地后逐字节相同。

### Added（`ashare_limit_anchor_check` 内四条，门禁 463 → 467）

- **折算读的是账本**：`ex_rights_reference` 必须命中 `.corporate_actions`、`is_cash_dividend_action`、
  `cash_dividend_raw`、`rights_issue_price_raw` 与 `self.price_tick`。
- **顺序**：`bars[..index]` 的跨日扫描必须在 `self.ex_rights_reference(...)` 之前，覆盖表仍是第一出口。
- **行为用例在册**：两条用例名与两个期望锚值（`Some(yuan(950))` / `Some(yuan(688))`）逐字核对。
- **折算的输入必须有非测试写点**：扫 `crates/*/src/**/*.rs`，只数实例方法调用点，注释行、文件尾
  `#[cfg(test)]` 模块、`src/**/tests.rs` 这类整文件即测试的路径、以及定义处向 `_with_report` 的
  `self.` 内部委托一律不算；qx-cli 侧期望 ≥2 处。

### Validation（日志在 `logs/s25_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| `ashare::tests::` 单库 | 18 passed / 0 failed（本轮新增 2 条） | `s25_mutate_a1_behaviour.txt` 首行 |
| 行为变异 3 例（真树 + `base_a1/` 镜像还原） | B1/B2/B3 每发各让 1~2 条用例变红，还原后回到 `('18','0')` | `s25_mutate_a1_behaviour.txt` |
| 文本判据变异 6 例（`.gate_mut/` 副本树，真树未动） | baseline 8 项 0 红；M1–M6 **每发恰好 1 红** | `s25_mutate_a1_gate.txt` |
| 门禁整跑 | `GATE_EXIT=0`，467 条 `PASS`、0 条 `FAIL` | `s25_gate_a1.txt` |
| `cargo test --workspace --no-fail-fast`（`QX_PYTHON` 指向 `python/.venv`） | 71 个测试二进制段 845 passed / 0 failed，另 21 段 Doc-tests 0 failed，退出码 0；相对上一轮 843 恰好 +2 | `s25_cargo_test_workspace.txt` |
| fmt / 行数棘轮 | `cargo fmt --all -- --check` 退出 0；`line_budgets.yaml` 重快照：新增 `ashare/tests.rs: 561`，`qx-guanxing/lib.rs 590 → 577` 一并收紧 | 同上配套 |

跑完整套测试后 `git status -- deploy/data` 为 0 项 —— 本轮没有把任何被跟踪产物改写。

### Corrected（本轮自己踩到的三个假绿/假绿边缘，全部立案）

- **M5/M6 第一发全绿**：新加的"非测试写点"判据被 `crates/qx-xingban/src/ashare/tests.rs` 里 13 处测试调用
  喂饱了 —— `is_test_scoped` 只认文件内尾部的 `#[cfg(test)]`，认不出"整份文件就是一个测试模块"这种挂载方式；
  另外定义处 `self.apply_corporate_actions_json_with_report(...)` 那一跳也被数成写点。两类都补进排除条件后
  重跑，M5/M6 才各自变红。这是 V13 §4 L4「判据的取数方式本身要能被证伪」的又一次实测。
- **字面量判据被 rustfmt 拆行打瞎**：`self.corporate_actions` 被格式化成 `self\n    .corporate_actions`，
   needle `self.corporate_actions` 当场红。判据改用跨行仍成立的 `.corporate_actions`，属于 V13 L4 的取证。
- **镜像必须在定义基线之后采集**：行为变异脚本的还原源指向了 A1 编辑**之前**的 `%TEMP%/mirror_s25/`，
  B1 一发就把整份实现还原掉，B2/B3 因此报"needle absent"，而结尾那行 `RESTORED-GREEN` 其实是
  `16 passed; 2 failed`。重放实现 + 改用 `base_a1/` 之后复跑才是上表那三行。附带一处 Windows 事故：
  `shutil.copy2` 还原时撞上 `WinError 1224`（文件被用户映射区域占用），脚本改成"写字节 + 读回比对 + 重试"，
  并且**只在内容确实等于基线时**才打 RESTORED 标记。


## Unreleased — V12 §23：变体级生产者判据落地，六颗零生产者变体删除，#137 收口（2026-09-26）

§19.6 列出四颗候选、§21.6 给出处置结论，两份都没动手。这一遍不写四个特例，而是把"每个 `pub enum`
变体都要有人在生产代码里把它造出来"接成常驻判据（门禁第 25 项）。同一把尺子在删除之前的快照上报出
**12 颗零限定名生产者**（52 个枚举 / 242 个变体），其中 3 颗是前两轮点名没点到的。

### Added（`enum_variant_producer_check`，门禁 460 → 463）

- **覆盖面地板**：解析出的枚举 ≥ 50、变体 ≥ 200（本轮实测 52/236）。挡"正则退化成一个都不匹配 → 第二条因为没有变体可判而全绿"。
- **变体级生产者**：每个 `pub enum` 变体必须有生产代码里的 `Enum::Variant` 限定名命中，或在允许清单里说明它的
  生产者在线格式上。`impl Enum` 花括号配平块内的 `Self::Variant` 一并计入（否则 `AshareBoard::Etf`、
  `CommandKind::CancelOrder` 会被误判成孤儿）；读者剔掉 `#[cfg(test)]` 与 `crates/*/src/tests/**`。
- **允许清单逐条可复核**：6 条 `CorporateActionType` 各自要同时满足"变体仍在册、所在枚举确实 derive 了
  `Deserialize`、且确实零生产者"；`Deserialize` 的证据只认 `pub enum` 上方连续的 `#` 属性行。

### Removed（破坏性，六颗变体）

- `qx-risk::RiskDecision::Rebalance`、`qx-zhenlu::ConnectorState::{Connecting, Disconnected, Degraded}`、
  `qx-zhenlu::StrategyState::Stopping`、`qx-provider::ProviderErrorClass::Retryable`。
  逐颗判定见 V12 §23.2；连接器侧"断开/降级"分别由 `ReconcileRequired` 义务与监管侧 `ServiceStatus::Degraded` 承担，
  组合再平衡的表示从来是 `qx-zhenlu::portfolio::RebalancePlan`，删除不使任何在做的事失去表示。
- **对外可见面**：`RiskDecision` 与 `ProviderErrorClass` 带 `Deserialize`，故 JSON 里的 `"Rebalance"` / `"Retryable"`
  从"能解出来但落进无人认的档位"变成**直接报错**。仓内 `schemas/`、`deploy/*.json`、`python/` 全搜无生产者；
  这两颗档位从未出现在 README 或接口文档里。

### Corrected

- **V12 §21.6 那条推测作废**：原文写"缺的映射点在 `crates/qx-adapter/src/ccxt.rs:178-188`"。复核后该处
  `CcxtRpc::call` 返回 `Result<Value, String>`（`ccxt.rs:26`），worker 的 `error.class` 只被拼进消息（`ccxt.rs:216`），
  与 `ProviderErrorClass` 之间**没有类型通路**——不存在漏接的映射点。`Retryable` 的处置结论仍是删，但理由换成"干净孤儿"。

### Validation（日志在 `logs/s23_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| 门禁整跑（真实文件） | `GATE_EXIT=0`，463 条 `PASS`、0 条 `FAIL` | `s23_gate_final_green.txt` |
| 删除前/后同一把尺子对照 | before 52 枚举 / 242 变体 / 12 颗零生产者；after 52 / 236 / 0 颗 | `s23_before_after_scan.txt` |
| 裸名 vs 限定名词形实验 | 12 颗里 **7 颗**按裸名数会被数成"有生产者"（`Degraded` 裸名 12 次全属 `ServiceStatus`/`OverallHealth`；`Stopping` 3 次全属 `ServiceStatus`；`Disconnected` 2 次全属 `mpsc::RecvTimeoutError`） | 同上 |
| 变异 7 例（%TEMP% 镜像，真树未动） | `ALL_CASES_MATCH=yes`：M0 0 红；M_A/M_B/M_C 各 1 红命中第 2 条；M_D 2 红（覆盖面 + 清单）；M_E/M_F 各 1 红命中第 3 条 | `s23_mut_variant_harness.py`、`s23_mutation_summary.txt` |
| `cargo test --workspace --all-targets` | `TEST_WS_EXIT=0`，72 段 `test result: ok`、843 passed、0 failed，warning/error 行 0 | `s23_cargo_test.txt` |
| Clippy（本轮动过的 6 个 crate，`-D warnings`）/ fmt / check | 逐个退出 0；`cargo fmt --all --check` 退出 0；`cargo check --workspace --all-targets` 无警告 | 同上配套 |

843 与 §19/§20/§21/§22 逐项一致：本轮动的是类型面与门禁，被测行为没有漂移。
一处自伤值得留档：第一次跑整树测试时我把 `QX_PYTHON` 落到 WindowsApps 占位桩，两条 Python 契约用例当轮就红，
而报错文本自己点名了解释器来源（§19 那条诚实化第二次省下排查时间）。

## Unreleased — V12 §22：本地构建路径第一次自己跑完架构门禁（八步改九步，#139 收口）（2026-09-26）

§21 收工时量出的那颗：`build.bat` 的八步里没有一步执行 `tools/check_architecture.py`，只有 CI 跑它
（`.github/workflows/ci.yml:34`）。也就是说，照 README 安装档 B 在自己机器上看到"全部完成 (all gates
passed)"，并不证明这四百多条不变量成立 —— 而 §21 刚刚还给这把尺加了"判据不许被抄成两份"的自我约束。
这一遍把接线本身做成判据，并当场跑出九步。

### Added（`build_script_parity_check` 内三条，门禁 457 → 460）

- **调用存在**：`build.bat` 与 `build.sh` 各恰好有一行**命令**调用 `tools/check_architecture.py`；注释行与
  `echo` 提示语不计（§19 #136 的口径）。
- **失败会中止**：bat 调用行之后紧跟 `if errorlevel 1 goto :err`，sh 调用行之前必须已有 `set -euo pipefail`。
- **排在 cargo 之前**：调用行早于该脚本第一条非注释 `cargo` 命令 —— 挡"接在最后一步"那种慢失败接法。
- **`build.bat` / `build.sh` 新增第 `[1/9]` 步**：`"%QX_PY%" tools/check_architecture.py`，原 `[1/8]..[8/8]`
  顺移为 `[2/9]..[9/9]`，`[0/9]` 解释器探测与 `QX_PYTHON` 交接不变。排第一是因为它只读源码、本机实测
  38.9 秒，红得早；`build.bat` 全程按字节改以保持 CRLF，新增注释写成纯 ASCII（门禁要求每行以 ASCII 结尾）。

### Changed

- `build_script_parity_check` 的写死 pins 跟着走：步骤数 8 → 9，`first_gate` 正则由 `\[1/8\]` 改为
  `\[1/\d+\]`；`cargo` 命令多重集仍是 7 条（新步是 Python 调用）。
- `GATE_CHECK_FLOOR` 457 → 460（本轮实测总条数）。
- README 安装档 B 与"构建与发布""装不上时的四个坑"改成九道门禁/新步骤号，并说明第 `[1/9]` 是什么、
  为什么排第一；`docs/外部链路验收执行方案-V1.md` 的步骤号同步。历史章节里的 `[n/8]` 是当时实测事实，不追改。

### Validation（日志在 `logs/s22_*.txt`）

| 运行 | 结果 | 日志 |
| --- | --- | --- |
| 门禁整跑（真实文件） | `GATE_EXIT=0`，460 条 `PASS`、0 条 `FAIL` | `s22_gate_final_green.txt` |
| 变异 `M0_control` | 0 条 FAIL（基线） | `s22_M0_control.log` |
| 变异 `M_A` 删掉整步 / `M_B` 只在注释里提脚本名 | 各 6 条 FAIL，含三条新判据 | `s22_M_A_no_call.txt`、`s22_M_B_comment_only.txt` |
| 变异 `M_C` bat 去掉 `goto :err` / `M_D` sh 去掉 errexit | 各 1 条 FAIL，只红"失败会中止" | `s22_M_C_*.txt`、`s22_M_D_*.txt` |
| 变异 `M_E` 把门禁挪到最后 | 1 条 FAIL，只红"排在 cargo 之前"（调用行 bat 81 / sh 74 vs 首个 cargo 37 / 49） | `s22_M_E_gate_moved_last.txt` |
| `build.bat` 全 9 步 | `BUILD_BAT_EXIT=0`；**`[1/9]` 由本地构建路径自己印出 460 PASS / 0 FAIL**；`[4/9]` 92 段 `test result: ok` / 843 passed / 0 failed；`[6/9]` `Ran 49 tests` | `s22_build_bat_full.txt` |

92/843/49 与 §19、§20、§21 逐项一致：本轮只动构建脚本与门禁，被测行为没有漂移。

### Known（未收口）

`build.bat` 九步全过不等于 CI 全集（`--ignored` 的 Postgres/NATS 契约、feature 矩阵、三平台 wheel 矩阵仍只在
CI）；"每遍都要重跑九步"依旧靠人工实跑，门禁执行不了 `cargo`；`#137` 的四颗只完成判定、代码未动；
`sandbox_tested` 仍全为 `false`；`qx-cli --version` 仍未实现。详见 V12 §22.7。


## Unreleased — V12 §21：把"合流接出来的第二份判据"变成常驻门禁，并量出本地构建路径根本不跑门禁（2026-09-26）

§20 留下一张欠条：那颗"双方各加同一段、git 不产生冲突标记"的缺陷只靠一个住在 `%TEMP%` 的一次性扫描器
证明过"全树无第二例"。这一遍把它变成仓库里常驻咬得住的判据，顺手补上 §20.5 第 5 条欠的 `build.bat`
全 8 步重跑 —— 并在核对那份日志时量出新的一颗（#139）。

### Added（门禁第 23 项：`merge_duplicate_block_check`，挂在 `main()` 最前）

- **窗口条**：`tools/*.py` 加上**正在运行的那一份自己**不得出现逐字重复的连续 10 行，命中时点名
  `文件:首处==次处`。门槛不是拍出来的：同一把尺在 318 份受版本控制的 `.rs`/`.py` 上
  W=8 命中 1,180 处 / 70 个文件、W=10 仍有 781 处 / 43 个（`crates/qx-storage/src/{nats,postgres,sqlite}.rs`
  那类合法并列实现），全树零容忍只会立一条假判据；而 `tools/*.py` 在 W=10 是 **5 份脚本 / 0 命中**。
- **描述条**：用 `ast` 静态收全部字面 `check(...)` 描述，同一条出现两次即红，并要求收到的条数 ≥ 300
  （跌破意味着取数方式本身失效）。本轮实测字面 353 条、另有 35 条 f-string 拼接不在集合内。
- `.gitignore` 收下 `/logs/`（每轮原始日志，数字才进文档）与 `/.gate_mut/`（变异取证用的门禁副本挂载点）。

### Changed

- 门禁条数地板 `455 → 457`（就是新加的两条），`GATE_EXIT=0`、打印 457 项。
- 描述条上线时当场抓出一处**既有**的共用描述（`日历夹具与摘要成对存在…` 在 `[4653, 4673]` 各印一次）：
  那条的分支支与聚合支断的是两件事，拆成两条各自描述，`DUP_LABELS` 归零。

### Validation（本轮实测，日志在 `logs/s21_*.txt`）

| 项 | 实测 | 日志 |
| --- | --- | --- |
| 红绿对（5 次跑真实文件的临时副本，被检文件 `GATE_FILE_UNTOUCHED=True`） | 基线 457 项 / 1 项已知伪红；M-A 整段重复 → 只有窗口条红；M-B 单行改描述 → 只有描述条红 | `s21_mutation_pairs.txt`、`s21_mut_*.txt` |
| §20 缺陷重建（取上游 `dbfc429` 的 19 行 R10 段落插回原处） | 摘掉新判据：退 1、**0 条 FAIL**、打印 315 条后 `NameError`；带上新判据：崩前先把重复窗口与重复描述各点名（317 条） | `s21_mut_control_defect_{with,without}_rule.txt` |
| 门禁整跑 | `GATE_EXIT=0`、457 项、0 条 FAIL | `s21_gate_final_green.txt` |
| `build.bat` 全 8 步（收口 §20.5 第 5 条） | `BUILD_BAT_EXIT=0`，`[3/8]` 92 段 `test result: ok` / 843 passed / 0 failed，与 §19、§20 逐项一致；解释器 3.12.13 且 `QX_PYTHON` 已外传 | `s21_build_bat_full.txt` |
| 本地构建是否跑架构门禁 | `grep -n check_architecture build.bat build.sh` = **0 / 0**，整份构建日志里没有"架构不变量自检"这一行；CI 跑（`.github/workflows/ci.yml:34`）→ 立案 #139 | 同上 |

### Known（本轮新登记）

- **#139**：照 README 安装档 B 从源码构建的用户，一次全绿的 `build.bat` **不执行**这 457 条不变量。
- **#137 只完成判定**：`ProviderErrorClass::Retryable` 全仓只出现在定义那一行而读者把它算作非终态
  （真生产者缺口，映射点在 `crates/qx-adapter/src/ccxt.rs:178-188`）；`RiskDecision::Rebalance` 与
  `ConnectorState::Connecting` 是干净孤儿应删；`CorporateActionType::{Split, Merge}` 带
  `serde(rename_all = "snake_case")`，属扫描正当盲区、进允许清单。代码未动。
- `sandbox_tested` 依旧全为 `false`；`qx-cli --version` 仍未实现。


## Unreleased — V12 §20：安装面三条路径各有退出码，并把上游 4 个提交真正合回来（2026-09-26）

§19 那一遍的产物全部留在未提交的工作树里，且 `origin/main` 领先 4 个提交。这一遍做两件事：把
"别人拿到仓库怎么装"写成实测过而不是期望中的路径，以及用一个**可复算的**合流判据替代"我看过了"。

### Added（安装面，README 新增「安装」章）

- **A 档 `cargo install --path crates/qx-cli --locked`**：2m10s 装出单个 `qx-cli.exe`，**在仓库外**
  跑 `help` / `init` / `doctor` / `backtest` / `report` / `status` 六条命令退出码全 0，回测落
  `result_hash=26fdd6b52d020700`；这一档不需要 Python（日志 `s22_cargo_install.txt`、`s22_installed_*.txt`）。
- **B 档 `build.bat` / `bash build.sh`** 与 **C 档 wheel** 各写清前置：wheel 216439 字节，内嵌
  `_qianxing_native.pyd` 的 md5 与当轮 `target/release/_qianxing_native.dll` 同为
  `3e3c793385cf155e92c2cdc4f3694eba`，干净 venv 里 `available()` 为真（`s21_*.txt`）。
- **「装不上时的四个坑」表**：WindowsApps 的裸 `python` 占位桩、PowerShell 未签名脚本要
  `-ExecutionPolicy Bypass`、缺 `tzdata` 时 A 股用例的失败形状、`sed -i` 会抹掉 `.bat` 的 CRLF。
- **如实记录一处缺口**：`qx-cli --version` 没有实现，会按未知参数 fail closed 退 2；确认安装用
  `qx-cli help` 首行。（挂在 §20.5 未收口。）

### Changed（合流 origin/main 的 4 个提交：`b4921ea` / `5cd11f8` / `2bf8ad6` / `dbfc429`）

- **合并提交 `dbaa9d9`** 把远端 `main` 接进本地历史；15 个冲突文件全部取我方，理由是两条判据：
  合流后 `git diff --cached HEAD` **为空**（206 个文件与提交后的树逐字节相同），且逐行审计 61 个
  上游路径后只有 65 行不在我们树里、逐行确认全是同一事实的另一种写法（`production_text()` 取代
  `.split("#[cfg(test)]")[0]`、`report.consecutive_failures` 取代局部 `consecutive`、文案与
  `line_budgets` 数字的先后版本、12 行已拆进 `snapshot_single_source/` 目录的旧单文件正文）。
- **推送走显式 `git@github.com:coeasy/qianxing.git`**（HTTPS 在本机被重置），不改 `git config`；
  推送后 `git ls-remote` 与 `HEAD` 同为 `dbaa9d9c2a4bd94e7fdfa65486df79b1793120e1`。

### Known（本轮新增的一类盲区，挂 §20.3 / #138）

- **自动合并会把"双方各加同一段"首尾相接，且不产生任何冲突标记**：`snapshot_row_wire_check()` 里
  上游那份 R10 对账判据落在我方那份之后，第一次运行以 `NameError: reader is not defined` 崩在中途 ——
  而门禁此时 `GATE_EXIT=1`、**0 条 FAIL**，看起来像环境问题而不是内容问题；两段都跑还会让同一条检查
  印两行 PASS，靠"判据条数"量的量具会被抬高。本轮用一个一次性扫描器（对合流改动的文件找出现两次以上
  的 8 行窗口）证明全树无第二例，常驻判据待 §20.5。
- **本轮没有重跑 `build.bat` 全 8 步**（§18.7 第 2 条）：推送的树与 §19 那一遍逐字节相同，八步在 §19
  已实跑通过（`s19_build_bat_fix.txt`）。

### Verified（本回合实测，逐条可 grep）

| 门槛 | 结果 | 日志 |
|---|---|---|
| 架构门禁（文档回写前） | `GATE_EXIT=0`，455 项全绿 | `s23_gate_after_install_docs.txt` |
| 架构门禁（合流后修复前） | `GATE_EXIT=1`、0 条 FAIL、`NameError: reader` | `s24_gate_merge1.txt` |
| 架构门禁（合流完成） | `GATE_EXIT=0`，455 项全绿 | `s24_gate_merge2.txt` |
| 整树测试（合流后的树） | `TEST_EXIT=0`，92 段 / 843 passed / 0 failed / 0 ignored | `s24_test_workspace_after_merge.txt` |
| 安装 A 档 | 仓库外六条命令退出码全 0 | `s22_installed_help/init/doctor/backtest/report/status.txt` |
| 推送 | `dbfc429..dbaa9d9 → main`，`PUSH_EXIT=0` | `s24_push.txt` |


## Unreleased — V12 §19：第一次把 `build.bat` 全 8 步端到端跑通，当场抓到一条从未生效过的解释器交接（2026-09-26）

§18.7 第 2 条欠的账：「每遍都要重跑 `build.bat` 全 8 步」这条口径没有判据能执行 `cargo`。这一遍不写判据，
直接实跑 —— 第一次跑就抓到 **#133**：`[0/8]` 探测出的解释器只留在脚本自己的变量里，Rust 侧读的是
`QX_PYTHON`，所以 §17 加进来的那一步**从来没生效过**，`[3/8]` 照旧回落 PATH 占位桩。顺带清掉 §18-B 删面的
残留孤儿（#134）与零读者判据根本不看的那半边类型面（#135）。更有价值的是变异反向验证当场抓出**两颗永远不会红**
的新判据（#136 / #138）：本轮新增的 6 条里有 2 条第一眼是绿的、其实是假的。全过程见
[docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §19，日志在 `%TEMP%/qx_v12p2/logs/`。

### Fixed（§19 #133：构建脚本的解释器交接）

- **`build.bat` / `build.sh` 把探测选中的解释器导出成 `QX_PYTHON`**：`[3/8]` 的两条 Python 桥契约用例与
  `[7/8]` 的策略 worker 由 Rust 去起 Python，而 Rust 只读 `QX_PYTHON` 一个变量
  （`crates/qx-cli/src/main.rs` 的 `python_interpreter_origin()`）。修前 `BUILD_BAT_EXIT=1` 且
  `[3/8]` 是 `202 passed; 2 failed`；修后 `[0/8]` 多印一行交接、八步全过、`[3/8]` 92 段全 ok / 843 passed。
  只改"仓库 venv"那一档：用户已设 `QX_PYTHON` 时原样保留，PATH 上的裸 `python` 不改写成不存在的路径，
  留给它自己的占位桩诊断。
- **编辑面硬约束**：`sed -i` 改 `.bat` 会把 CRLF 抹成 LF（实测 98 个孤立 LF，门禁立刻红），归一化只能按字节做。

### Removed（§19 #134 / #135：类型面孤儿）

- **`QualityIssue::CrossedBook` 连同 `verdict()` 的致命臂删除**：§18-B 把交叉报价判定收到
  `crates/qx-adapter/src/binance.rs` 的报价入口时只删了构造点，留下一个"永远构造不出来却声称致命"的变体。
- **`crates/qx-data/src/calendar.rs` 整文件删除 + 摘挂载**：16 行 `TradingCalendar` trait 全仓零实现
  （V11 §5 早已写明），还与 `crates/qx-scheduler` 真在驱动调度的同名 struct 撞名。
- **`qx-guanxing` 的 `NumericExt` / `to_display` 删除**：定义之外零出现的定点展示辅助，躲在"零读者判据只扫
  `pub fn` / `pub const`"的覆盖面之外。

### Gates（本轮 +6 条，地板 449 → 455）

- `build_script_parity_check` 两条：每个构建脚本都必须有把解释器导出成 `QX_PYTHON` 的**命令**，且早于 `[1/8]`。
- `quality_issue_producer_check` 两条：`QualityIssue` 每个变体都有构造它的生产者（判定臂不算）；
  `CrossedBook` 这个名字不得在任何 `crates/*/src` 生产文本复活，且 adapter 必须留着 `.is_crossed()` 那位真读者。
- `dead_type_surface_check` 两条：`NumericExt` / `to_display` 这类"定义之外零出现"的类型面删除后不得复活；
  同名日历类型只能有一个定义，且就在真的驱动调度的 crate 里（按**裸名字**数读者的判据会被同名类型误判，这条是自纠）。
- **两颗假绿判据的修正**：`interpreter_handoff()` 原先把 `build.sh` 报错提示里那句
  `echo " 或: export QX_PYTHON=…"` 当成脚本自己的导出行为（M2 因此整颗不咬，#136）；
  `CROSSED_QUOTE_NAME in _production_lines(text)` 对**行列表**用 `in`，判的是"存在一整行正好等于该名字"，
  于是 `revived` 恒空、判据恒绿（M3 因此只红一条，#138）。收口口径：**新增判据必须当场配一颗让它红的变异**。
- **变异协议补 `try/finally`**：第一次 M2 未咬时脚本 `AssertionError` 退出，把变异**留在了 `build.sh` 里**，
  靠 `%TEMP%` 镜像才还原。

### Docs

- `README.md`：`[0/8]` 那段补"挑中之后必须外传成 `QX_PYTHON`"的口径与被否证的旧表述；当前状态数字
  449 → 455；`build.bat` 全 8 步一次跑通写进实测台账。
- **README 新增「安装」章，把"快速装好并用起来"变成一条可复制路径**：三档安装（A `cargo install
  --path crates/qx-cli --locked` 只装 CLI、不需要 Python；B `build.bat`/`build.sh` 八道门禁；
  C 两条 wheel 入口 + 干净 venv 复核），每档末尾带本轮实测值。实测口径：`cargo install` 2m10s 产出
  `qx-cli.exe`，在**仓库外**的临时目录里 `help`/`init`/`doctor`/`backtest`/`report`/`status` 六条全部
  退出码 0，`init --strategy macd` 生成的项目自包含，回测写出四份产物、`result_hash=26fdd6b52d020700`
  （`s22_cargo_install.txt`、`s22_installed_*.txt`）。另加「装不上时的四个坑」表（Store 占位桩 /
  执行策略 / 缺 tzdata / 用 `sed` 改 `.bat` 抹掉 CRLF），以及一条如实记录：安装后的 binary **没有**
  `--version` 旗标，`--version` 按未知参数 fail closed 退出 2，确认安装用 `qx-cli help`。
- `docs/工业化易用性收口指南-V1.md` §1 写明"源码前缀 `cargo run -p qx-cli --` ↔ 安装后 `qx-cli`"是同一条
  clap 命令表，两条路径不必分别维护；`docs/外部链路验收执行方案-V1.md` §6 补上解释器外传的口径。

### Verified（本回合实测，逐条可 grep）

| 门槛 | 结果 | 日志 |
|---|---|---|
| `build.bat` 全 8 步 | 修前 `BUILD_BAT_EXIT=1`（`[3/8]` `202 passed; 2 failed`）→ 修后 `BUILD_BAT_EXIT=0`，843 passed / 0 failed | `s20r_build_bat_full.txt`、`s19_build_bat_fix.txt` |
| 架构门禁 | `GATE_EXIT=0`，**455 项全绿 / 0 条 FAIL**；爬升 449 → 453 → 455 | `s19_gate_judgefix2.txt`（两颗假绿判据修完复跑） |
| 本轮变异 | 6 颗：M1/M4/M5/M6 一次咬；M2 第一遍不咬 → 抓出 #136，修后红在唯一目标判据；M3 第一遍只红 1/2 → 抓出 #138，修后一次红两条。颗颗 `RESTORE EXACT` + 还原后 455 全绿 | `s19_mut_round19b.txt`、`s19_mut_m3_recheck.txt` |
| 安装包重建 | `SH_EXIT=0` + `PS_EXIT=0`，两条腿都 `Created wheel … size=216439` | `s21_wheel_rebuild.txt` |
| 安装包与当轮代码一致 | wheel md5 `fe94d8e5…`；内嵌 `_qianxing_native.pyd` 353280 字节 md5 `3e3c793385cf155e92c2cdc4f3694eba` **等于**本轮 `target/release/_qianxing_native.dll`（`MATCH`），12 份 `*.py` `mismatch: none` | `s21_wheel_verify.txt` |
| 干净 venv 复核 | `PROBE_EXIT=0`：`native.available() → True`、四个包全部导入成功、衍生品三字段 `cross/hedge/3` 往返 `True`、`margin_mode="weird"` 仍 `ValueError` | `s21_install_probe.txt` |

## Unreleased — V12 §18：第五遍逐条判定 §16 留下的 13 颗断链，4 颗接进生产、8 颗拆掉假装接上的桥（2026-09-26）

§16.6 第 3 条欠的账：13 颗"零消费者 / 无生产装配"的断链发现。这一遍不新增发现，逐条判**该接还是该写明没接**：
钱与账的 4 颗接进生产（§18-A），数据与血缘的 8 颗**删掉那条假装接上的桥**并写进对外矩阵（§18-B），
第 13 颗（#129）判为"接了更危险"，只改语义。轮末验证链又抓到两颗：一颗 clippy lint（本轮新用例自己留的），
一颗 README 承诺的 PowerShell 入口在本机默认策略下**一行都不执行**就退出（§18-C #132）。
全过程与 70 颗变异见 [docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §18，
日志在 `%TEMP%/qx_v12p2/logs/`。

### Fixed（§18-A：接进生产的四条）

- **柜台未报的币种不再折成 0（#109，交易 TX6）**：`crates/qx-runtime/src/pipeline.rs` 的
  `venue_raw` 改成 `Option<i128>`，只有 `Some(ledger_raw)` 才可能判一致；Binance 对账播报里缺席的币种印「未报」。
  新集成用例 `crates/qx-runtime/tests/settlement_balance_absence.rs` 钉住三条读法（报了 0 不算差异、
  没报落 `None` 且与"只报别的币种"同形、报过数仍是那个数）。
- **对账裁决的动作归类真的有读者（#108）**：`status_action` / `filled_action` 在
  `crates/qx-genglu/src/reconcile/order.rs` 各定义一次，裁决只调用不另起比较，`crates/qx-adapter/src/reconcile.rs`
  的 `action()` 委托同一对判据，报告多印一栏 `action`，待对账事实的 `reason` 从维度码改成动作口径。
- **超时作业会升级、并发键会释放（#110）**：`dispatch_scheduled_jobs` 每轮遍历运行做
  `is_timed_out` → `mark_timed_out`（升级 `NeedsIntervention`、释放并发键），`NotReady` 只跳过本轮，
  播报加 `timed_out=` 一栏；顺带把租约时钟统一到秒域（`lease_clock` 是唯一换算入口，八处命令队列入口 +
  派发写入的 `JobRun` 全按秒 —— 30 秒租约此前会被写成 30 毫秒）。
- **一次运行只交一个终态（#129）**：`finish_run_with_code` 的 `error_code` 只在失败侧保留，成功一律 `None`；
  Strategy worker 的收口改传 `None`（结果码只走 stdout），于是 `/scheduler/runs` 不会再显示一条带着
  `3 orders: SUBMITTED` 的"成功"作业。**自动重跑刻意不接** —— 交易作业失败时无法判定订单是否已出网，
  重跑等于二次提交；`JobStatus::Failed` / `next_retry_ts` / `retry_run_at` 这条链改为立成 limitation
  `scheduler_run_retry_has_no_production_path`，并由判据去数生产调用点。

### Fixed（§18-C：安装面的一颗）

- **README 的 PowerShell wheel 入口带执行策略**：`powershell -File tools/build_python_wheel.ps1` 与
  `./tools/build_python_wheel.ps1` 在 Windows 客户端默认策略（本机 `Get-ExecutionPolicy -List` 五个作用域
  全 `Undefined`，即 Restricted）下以 `UnauthorizedAccess` 退出、一行脚本都不执行。入口写成
  `powershell -NoProfile -ExecutionPolicy Bypass -File …`，`.ps1` 文件头同步写明；§17.8 记的 `PS_EXIT=0`
  因此被否证为"当时那个进程的策略恰好放行"。

### Removed（§18-B：拆掉假装接上的桥）

- **静态帧到 `ProviderRegistry` 的注册桥整条删除（#112/#114）**：`with_received_at`、`with_registry_capability`、
  `registry_received_at`、`registry_capability`、`default_registry_capability`、`RegistryDataProvider` 六个名字
  全仓清零；帧读侧不再有 `as_of` / `receive_time` / `received_at` / `unwrap_or(query.end)`（桥把
  `query.end` 当"数据到达时间"哈希进血缘，是一条不可能失败的 PIT 断言）；`crates/qx-data/Cargo.toml`
  卸下只为这条桥存在的 `qx-provider` + `qx-guanxing` 依赖边。
- **`TestClock` / `advance_to` 删除（#116）**：README 的"回测不用系统时间"改指三处真闸门
  （入库前排序 + 同标的 `ts` 严格递增 + 帧读侧乱序报错），各配行为用例。

### Registered（§18-B：接不了的事实写进对外矩阵）

- **#115**：`event_verified` 不再是快照里手抄的一格 bool —— `crates/qx-cli/src/readiness.rs` 按本地
  `runs/*.json` 的 `RunManifest` 重算摘要并核对血缘与区间覆盖，摘要口径取自 qx-factor 的 `mark_event_verified`；
  三个研究快照读点都必经绑定入口。
- **#117**：调度装载入口按运行时真实能力拒形状（`unsupported_dispatch_shape`，写状态之前即拒并点名作业），
  一次 tick 先 `manifest.validate()` 再给 `JobRun` 盖血缘。
- **#118 / #119**：公司行为三条只读查询、因子物化写侧五个入口、`artifacts[].values` 全部进允许清单 +
  limitation（`corporate_action_read_side_has_no_production_reader`、
  `factor_snapshot_json_has_no_in_repo_producer`、`feature_artifact_values_have_no_production_reader`），
  接口文档逐层对齐五层快照字段并声明"由仓库外导出、拼错的键会被拒"。

### Added（门禁，397 → **449**）

- 九颗新判据函数共 51 条：`reconcile_action_and_absence_check` 8、`lease_clock_domain_check` 11、
  `bar_frame_pit_honesty_check` 4、`event_backtest_evidence_check` 5、`backtest_clock_honesty_check` 5、
  `scheduler_dispatch_honesty_check` 5、`corporate_action_read_side_check` 2、
  `factor_research_honesty_check` 7、`scheduler_retry_honesty_check` 4。
- `wheel_builder_check` 第 4 条（#132）：README 里每条 ASCII 的 PowerShell 入口都必须带
  `-ExecutionPolicy Bypass`，一条入口都没有也红。
- **#87 收口**：能力矩阵证据路径判据从"只认行首唯一路径"改成**整行每个 repo 前缀 token 都核对**
  （带 `:行号` 的剥后缀、glob 不强判）—— §16 把四份用例集拆目录后，五条指向已消失单文件的证据行
  在旧口径下继续全绿。
- `GATE_CHECK_FLOOR = 449`、`WORKSPACE_TEST_FLOOR = 851`（都是本轮实测值）。
- 两条正则口径修正：数生产调用点时接收者前缀 `.`/`::` 已是分隔符，再加 `(?<!\w)` 会让判据恒绿
  （#119 的 t3 首版就是这样不咬）；`SCHEDULER_FINISH_CALL` 之类紧化断言不能带尾参（rustfmt 保留尾逗号）。

### Docs

- `deploy/README.md`：一次作业运行的收口口径（不自动重跑、结果码不进 `JobRun`、超时如何升级）、
  调度派发只认 Cron + `window=Any`、研究快照五层字段与"仓库外导出"的契约。
- `README.md`：wheel 入口与三条前置（pip 探测、ASCII 行尾、执行策略）；`maturity/capabilities.yaml`
  新增 3 条 limitation + 8 条证据行。

### Verified（本回合实测，逐条可 grep）

| 门槛 | 结果 | 日志 |
|---|---|---|
| 架构门禁 | `GATE_EXIT=0`，**449 项全绿 / 0 条 FAIL**；爬升 405→425→430→435→444→448→449 | `s20r_gate_after_docs.txt`（README 台账与 §18.6 回写后复跑） |
| 整树测试 | `TEST_EXIT=0`，**92 段全 ok / 843 passed / 0 failed / 0 ignored**（§16.7 归零后的 821 → 843；逐名归账 = 新增 24 颗、删除 2 颗，删掉的两颗分别跟着 #116 的 `TestClock` 与 #112/#114 的三条假桥走） | `s20q_test_workspace.txt`（clippy 修复后重跑；修复前的 `s20a` 同数，中间还有一次因 `QX_PYTHON` 被我写成目录路径而作废的 `s20p`） |
| 全仓 `#[test]` 属性 | **851**（829 → 851，与整树同 +22；差的 8 条被 `nats`/`sqlite` feature 门控） | 门禁的 `workspace_test_floor_check`（同一份 `s20r_gate_after_docs.txt`） |
| 存储（sqlite feature） | `EXIT=0`，**56 passed / 0 failed** | `s20d_storage_sqlite.txt` |
| Clippy | **先 101 后 0**：`crates/qx-cli/src/tests/scheduler_dispatch_support.rs:92` 的 `&[job.clone()]`（`cloned_ref_to_slice_refs`）改 `std::slice::from_ref` | `s20b_clippy.txt`、`s20c_clippy2.txt` |
| 格式 | `cargo fmt --all -- --check` 退出码 0 | `s20f_fmt.txt` |
| Release 构建 | `cargo build --release --offline` `REL_EXIT=0` | `s20g_chain.txt` |
| Python 边界 / 核心语义 | `Ran 49 tests … OK (skipped=1)`；`全部自校验通过 ✓` | `s20e_pytests.txt`、`s20e_validate.txt` |
| 安装包（两条入口） | `SH_EXIT=0`（`sha256=4ef59cf7…`）、`PS_EXIT=0`（`sha256=c352502f…`，带 Bypass） | `s20g_chain.txt`、`s20i_wheel_ps1_bypass.log` |
| 安装包 = 当轮代码 | wheel 216440 字节；内嵌 `_qianxing_native.pyd` md5 `372906ad…` **等于**本轮 `target/release/_qianxing_native.dll`；12 份 `*.py` 与 `python/` 逐文件相等 `mismatch: none` | `s20j_wheel_verify.txt` |
| 装进干净 venv | `native.available() → True`；三字段 `cross/hedge/3` 往返 `True`；`margin_mode="weird"` 仍被拒 | `s20k_wheel_install_probe.txt` |
| 变异反向验证 | **70 颗**（结构 64 + 行为 6）逐颗红在预期判据、按字节还原后全绿 | §18.4 的表 + `s20h/s20l/s20m/s20n` 四份补跑日志 |

### 未收口（如实记录）

1. `sandbox_tested` 全部保持 `false`：本轮没有任何外部服务与凭据参与。
2. "每遍都要重跑 `build.bat` 全 8 步"仍无量具 —— 本轮那颗 clippy 命中正是第 4 步补跑才红的。
3. 门禁总数地板与用例地板只咬下跌侧，449 / 851 仍是人工回合的产物。
4. #106 / #107 待确认后关单（R4-d 的 `zero_reference_public_surface_check` 已实现同一口径）；
   #53（Q1b 可复现性绑定）未排期；`build.sh` 可执行位与 wheel 脚本的 `--offline` 口径未动。
5. 本地 `main` 与 `origin/main` 分叉未解：本轮成果全在工作树，未 stage、未 commit、未 push。


## Unreleased — V12 §17：第四遍清点构建与安装面，判据可以整段消失而门禁全绿（2026-09-25）

前三遍数的是产品代码，这一遍数的是"用户怎么把这个仓库跑起来"。结果是 `build.bat` 三处互相独立的缺陷
同时在场：双击必然中途死在 `\'不是内部或外部命令\'`、解释器探测只认退出码所以把 WindowsApps 的 `python`
存根当成可用解释器、步骤口径与 `build.sh` 已经分叉。收口时又撞到第四类：README 承诺的两条 wheel 入口
**两条都不通**（`.ps1` 的中文行尾被 PowerShell 5.1 按 ANSI 码页吞掉换行，紧跟其后的 pip 探测整行被并进注释、从不执行；`build.sh`
用的 `python/.venv` 是 uv 建的、里面没有 pip），而 `dist/` 里那份 wheel 内嵌的原生扩展是六天前的。
全过程与十一颗变异记录见
[docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §17，日志在 `%TEMP%/qx_v12p2/logs/`。
本遍被改动的产品代码只有一颗，而且是收口重跑 8 步时才红的：`[4/8]` clippy
（`crates/qx-genglu/src/reconcile/tests.rs:43` 两处 `cloned_ref_to_slice_refs`，V12 §17.6）；
其余改动全在构建脚本、`.gitattributes`、门禁与文档。收获是一条量具盲区：
**门禁自己的判据可以被整段删掉而它照样打印"全部通过 ✓"**。

### Fixed（构建与安装面）

- **`build.bat` 重写**：`chcp 65001` 只修显示、不修解析，带 UTF-8 中文的 LF 版批处理 cmd.exe 读不了，
  于是强制 CRLF 并保证每行以 ASCII 字节结尾；第 `[0/8]` 步的探测改为要求候选**把版本号印出来**
  （退出码不算证据：Store 存根与 `cmd.exe` 都能以 0 退出而不打印任何 Python 版本），随后再探
  `import tzdata`（`python/pyproject.toml` 里声明的 Windows 依赖）。候选顺序 `QX_PYTHON` →
  `python\.venv\Scripts\python.exe` → PATH `python`，三项都不合格时退出码 1 并印出候选清单与两条修法。
- **`build.sh` 同口径重写**：同一组 8 步、同一串 7 条 `cargo` 命令、同一个探测契约（`QX_PYTHON` →
  `python/.venv/bin/python` → `python`）。
- **新增 `.gitattributes`**：`*.bat`/`*.cmd` 钉 `eol=crlf`、`*.sh` 钉 `eol=lf`。在此之前 CRLF 只是本机的
  巧合（`core.autocrlf=true`），换一台机器或换一种 checkout 方式就会把可运行的 `build.bat` 变成跑不了的。
- **仓库 venv 离线重建**（`uv venv .venv --python 3.12 --clear` + `uv pip install tzdata`，3.12.13 /
  tzdata 2026.4）：它曾在并发 uv 进程干扰下消失，导致 `[5/8]` 的 A 股用例以 `ZoneInfoNotFoundError` 失败。
  重建后 Python 边界测试从 `Ran 34 … FAILED(errors=1)` 变成 **`Ran 49 tests … OK (skipped=1)`** ——
  `test_ashare.py` 的 16 条原本整文件不跑（导入期 `ZoneInfoNotFoundError`），只留 1 条 loader 占位用例，
  所以差值是 15（`s17_py_no_tzdata.log`，本回合用缺 tzdata 的解释器复现）。
- **`[4/8]` clippy 补跑抓到的一颗 lint**：`crates/qx-genglu/src/reconcile/tests.rs:43` 用
  `&[local.clone()]` 造单元素切片，`-D warnings` 下 `CLIPPY_EXIT=101`（`cargo test`、架构门禁、
  release 构建全都不会红）。改法即 clippy 建议的 `std::slice::from_ref(&local)`，行为不变。
  这颗在 §16 那一遍改这个文件时就该红 —— 那一遍没跑第 4 步，也没写下"没跑"（V12 §17.6）。
- **两条 wheel 构建入口修通**：`tools/build_python_wheel.ps1` 与 `.sh` 都在跑 `cargo` 之前先探测
  `-m pip --version`，缺 pip 即以退出码 1 停下并印两条修法（装 pip / 离线 `uv build --no-build-isolation`）；
  `.ps1` 的注释与抛出消息改为 ASCII-only（中文行尾会被 PowerShell 5.1 按 ANSI 码页解码、吞掉换行，
  让下一行代码整行消失），并把被 dedent 出 `try` 块的四段缩进归位。两条入口修好后**各自真跑通一次**
  （`PS_EXIT=0` / `SH_EXIT=0`，`s17_wheel_ps1_2.log`、`s17_wheel_sh.log`）。
- **安装包按当轮代码重建**：旧 wheel 先 `cp -p` 镜像到 `%TEMP%`，重跑 `cargo build -p qx-python --release`
  → 新的 `_qianxing_native.dll`（md5 `c1991c58…`）→ 打包 → `dist/qianxing_bridge-0.1.0-cp312-cp312-win_amd64.whl`
  （216440 字节）。否证与复核都按 md5 而不是"文件存在"：旧 wheel 内嵌的是 `0cb3981c…`（六天前的扩展），
  新 wheel 内嵌的 `.pyd` 与本轮 `.dll` 同 md5，wheel 里 12 份 Python 源逐文件与仓库相等
  （`s17_wheel_verify.txt`）。`/dist/` 在 `.gitignore` 里，所以安装包是**本地产物**、不随提交分发。

### Added（门禁，387 → **397**）

- `windows_batch_parse_check`（4 条 + 1 条属性判据）：逐个 `.bat`/`.cmd` 核对无孤立 LF、每行以 ASCII 结尾，
  并**按实际存在的扩展名**各自要求一条 `eol=crlf` 属性规则（早先写法允许 `*.cmd` 替 `*.bat` 交差，被变异 M4 证伪后收紧）。
- `build_script_parity_check`（2 条）：两份构建脚本的 `echo [n/N]` 步骤逐项同名同序（8 步），
  `cargo` 命令多重集相等（7 条）。
- `wheel_builder_check`（3 条）：两个 wheel 脚本都要在 `-m pip wheel` **之前**探测 `-m pip --version`
  （按字符位置比较，缺一或顺序颠倒即红），且 `.ps1` 每一行以 ASCII 字节结尾 —— 这条正是本轮那两处
  安装面失效各自的判据。
- `GATE_CHECK_FLOOR = 397` 门禁自身总数地板：本轮两次手滑删掉 `TEST_MODULES` 的元组项时，
  门禁静默少跑 4 条判据却仍然报"全部通过 ✓" —— 现在少一条就红。

### Docs

- `README.md`：构建段从"Windows 下可直接双击 `build.bat`"扩成解释器契约 + 两份脚本同口径 + `.gitattributes`
  的因果；"当前状态"按本轮重测回写（387 → **397 项**、Python 边界 47 → **49 条**、
  `qx-storage --features sqlite` 9 段 51 → **10 段 56 passed**、venv 已重建但探测不因此取消）；文档指向段补 §17。
- V12 §17：三处缺陷的四象限否证表、修到什么程度、venv 重建前后对比、10 条判据的逐颗变异反向验证、
  本轮测量表、§17.7 未收口、§17.8 安装包重建（两条入口的失败原因表 + 修后跑通与 md5 复核）。

### Verified（本轮日志，非旧文档摘录）

| 门槛 | 本轮结果 |
|---|---|
| 架构门禁 | 退出码 0，**397 项全绿**（`s17_gate_397.txt` 抬地板后首跑；本轮文档回写后再跑一次仍 0 / 397、无 `FAIL` 行） |
| 整树测试 | `TEST_EXIT=0`，90 段全 ok / **821 passed** / 0 failed（`s17_full_test_225330.log`；lint 修好后复跑 `s17_full_test_230131.log` 同数） |
| fmt / release / clippy | `FMT_EXIT=0`、`REL_EXIT=0`（`s17_final_225640.log`）；`CLIPPY_EXIT` 从 101 → **0**（`s17_clippy.log`、`s17_clippy2.log`） |
| `qx-storage --features sqlite` | 退出码 0，10 段 / **56 passed** / 0 failed（`s17_storage_sqlite.log`） |
| Python 边界 / 核心语义 | `Ran 49 tests … OK (skipped=1)`、`全部自校验通过 ✓`，均退出码 0 |
| wheel 两条入口 | `PS_EXIT=0`（sha256 `8c6906c9…`）、`SH_EXIT=0`（sha256 `f58d2a62…`），同尺寸 216440；安装到干净 venv 后 `native.available()` 为 `True`，三字段往返相等 |
| 反向验证 | 十一颗变异（M1–M7 + M8/M8b/M9/M10）各自**只红一条**，`cp -p` 还原后逐字节 `identical=True` |
| 解释器回落 | 5 个 `QX_PYTHON` 场景 × `build.bat`/`build.sh` 两侧，全部按预期成功或 fail closed |

**仍不宣称**：`sandbox_tested` 依旧全为 `false`（本轮没有任何外部服务或凭据参与）；门禁总数地板只咬下跌侧；
wheel 脚本里的 `cargo build -p qx-python` 不带 `--offline`，且 wheel 文件名不带世代（V12 §17.7 第 5、6 条）。


## Unreleased — V12 §16：三遍连通性清点在合流后的代码上重跑，量具的第三类瞎法被点名（2026-09-25）

按"至少三遍、每遍全修再进下一遍"的口径，三遍分别在合流之后的工作树上重跑：第一遍找孤儿逻辑，
第二遍找无退出条件的循环，第三遍找前后端/三语言契约断链。全过程与逐颗反向验证记录见
[docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §16。
本轮产品侧只修 4 个点，另加 6 条门禁判据 —— 收获是识别出**契约判据只覆盖三语言里的两种**这类失效：
它不会误报，也不会红，只会让第三种语言静默漂移。

### Fixed（三遍各一颗到两颗）

- **健康快照的 stale 判定不再恒不成立**（§16.1 #122）：`runtime-check` 调
  `HealthRegistry::snapshot(now_ms, stale_after_ms)` 时把 `now_ms` 传成 `0`、把预算传成
  `shutdown_timeout_ms`，饱和减法使"心跳过期"在任何配置下都不成立。现在传
  `runtime_timestamp_ms()` 与 `messaging.worker_stale_after_ms` —— "心跳多久算陈旧"全仓库只剩这一个窗口。
  同轮把 `runtime_check.rs` 里两处重复的报告数组读取抽成 `string_array()`（缺字段/非数组按空处理），
  以付该文件 500 行的预算账，落 498 行。
- **API 两条 accept 循环接上停机出口**（§16.2）：明文 `serve` 与 mTLS `serve_tls_mtls_with_stores`
  原先都阻塞在 `incoming()` 上，监督器的 `ShutdownToken` 递不进去 —— 没有连接就一直等。两条循环现在
  共用 `await_connection`（先 `stopped()` 判定，再 `set_nonblocking(false)` 取连接），并由 3 条用例
  （`crates/qx-api/tests/accept_loop_shutdown.rs`）+ 6 条判据按名字钉住。`SchedulerWorker` 与策略宿主
  循环也改为读同一个停机令牌。
- **Python SDK 的多腿衍生品意图补回三个字段**（§16.3 #123）：Rust `StrategyContractIntent` 与
  `schemas/strategy_api_v1.schema.json` 都认 `margin_mode` / `position_mode` / `leverage`，
  `python/qianxing_bridge/strategy.py` 的 `StrategyIntent` 没有这三格 —— Python 策略作者写不出这三字段，
  而带这三键的输入会被 Rust 的 `deny_unknown_fields` 整体拒绝。声明、校验、序列化、反序列化四段一并补齐，
  校验档位与 `contract.rs` 同口径（`cash|cross|isolated`、`one_way|hedge`、`leverage > 0`）。
- **outbox 的 `schema_version` 回到常量**（#120）、**`run_manifest.input_components` 有人写也有人读**（#121）。

### Added（门禁）

- `health_snapshot_knob_check`：读 `collect_runtime_check_report` 的**函数体**，要求真时钟与
  `worker_stale_after_ms` 在场、`snapshot(0` 与 `shutdown_timeout_ms` 不在场（只读代码，不接受注释）。
- `strategy_intent_three_language_check`：三侧字段集**两两逐项相等**（Python SDK↔Rust、JSON schema↔Rust）
  + 三条衍生品字段在位 + 两条跨语言往返用例点名钉住。这是本仓库第一条"schema 文件真的被门禁读过"的判据。
- `CLI_TEST_FLOOR` 243 → **244**（磁盘重测：`src/tests` 192 + `crates/qx-cli/tests` 递归 52）。
- `workspace_test_floor_check` / `WORKSPACE_TEST_FLOOR = 829`：递归数 `crates/*/src` 与 `crates/*/tests`
  下的 `#[test]`，全仓总数跌破即红，失败信息按 crate 印出分布。这条是本轮账目回合的直接产物 ——
  两个 crate 的地板护得住那两个 crate，护不住另外二十个（见下"Reconciled"）。

### Reconciled（整树 passed 从 §15 的 843 掉到 821，逐名归零后才写进文档）

用例只增不减是这批工作的自述，所以一次下降必须被解释。把两份日志的 `test <name> ... ok` 做集合差：
本轮少 39 个名字、多 17 个名字。39 个里 15 个住在本批整份删除的文件里（`qx-core/src/{engine,queue,fenye}.rs`、
`qx-data/src/cache.rs`、`qx-genglu/src/reconcile/account.rs`、`qx-zhenlu/src/portfolio/optimizer.rs`，
以及被目录模块接管的 3 份用例单文件，`git status` 里全是 `D`）；其余 24 个逐条判过，结论都是
**被测代码本身也不在了**或**同批改名且新名在跑**：`RestVenue`/`RequestSigner` 脚手架那 7 条（模块头就写明随装配读者一起删）、
`crossed_quote`/`vector_scan`/`signed_transport`/`hmac_signer`/`QuoteBook` 在整棵 `crates/` grep 为零、
`drawdown_computed`/`total_return_signed`/`attribution_keeps_strategy_chain` 是 V10 §4.8 的重复概念归位、
5 条旧对账用例由同文件的 canonical 裁决用例承担、重试分类改由 `qx-core/src/retry.rs` 的 3 条钉住。
**没有一条是"代码还活着而用例消失"**；全仓 `#[test]` 属性总数 `HEAD = 72c347e` 的 752 → 工作树 829。

### Docs

- `deploy/README.md`：新增"心跳新鲜度只有一个窗口"段（并明写 `runtime-check` 的 `health` 块是拓扑快照、
  全部 `starting`，运行期健康以 `/ready`、`/metrics` 为准）；新增 `## 自检与内部命令` 段覆盖
  `ecosystem` / `paper` / `verify` / `all` 四条此前无文档的命令，**每条文案都用
  `./target/debug/qx-cli.exe <cmd>` 实跑否证或证实过**（第一版把 `ecosystem` 写成覆盖 PaperVenue，
  实测 `grep -c PaperVenue` 得 0，已改为实测口径；`all` 改为"`verify` 与 `paper` 的并集"）。
- `maturity/line_budgets.yaml` 按 `--snapshot` 重算：37 项，46395 → **44120（−2275）**，20 项下降共 −2208。
  两处增长逐条判过并留结论：`crates/qx-cli/src/strategy_contract.rs` 822 → 840（R4-k 与 #123 的三侧口径
  都落在这里）、`crates/qx-storage/src/lib.rs` 2405 → 2406（#120 的常量绑定 1 行）。形状变化 4 项：跌出
  `qx-factor/src/materializer.rs`（507）与 `qx-plugin/src/lib.rs`（583），新进 `qx-cli/src/worker_entry.rs`（505）
  与 `qx-cli/src/tests/api_snapshot_money_fields.rs`（518）。

### Verified（本回合实测，逐条可 grep）

| 门槛 | 命令 | 本轮结果 |
|---|---|---|
| 架构门禁 | `tools/check_architecture.py` | `ARCH_EXIT=0`、**387 项全绿**；6 条新判据 `[PASS]` 文案逐字点名在 §16.5 |
| 全仓用例地板 | 同上 | `[PASS] 全仓行为用例不少于 829 条`；变异：抽掉 `qx-guanxing/src/lib.rs` 一条 `#[test]` → **只红这一条**（`当前 828 条 … qx-guanxing=7`），`cp -p` 还原 `RESTORED-IDENTICAL` 后回到 387 项全绿（`%TEMP%/qx_v12p2/logs/arch_mut1.log`、`arch_restored.log`） |
| 整树测试（带解释器） | `QX_PYTHON=<解释器> cargo test --offline --workspace` | `exit=0`、**90 段 / 821 passed / 0 failed / 0 ignored** —— 两次抓到同一组数：`final_r3_205909.log`（venv 解释器）与文档回写后的 `final_s16_docs.log`（基础解释器），均在 `%TEMP%/qx_v12p2/logs/` |
| Python 契约 | `cd python && PYTHONPATH=. .venv/Scripts/python.exe -m unittest discover -s tests -p "test_strategy_contract.py"` | `Ran 10 tests … OK`（含本轮新增 2 条跨语言往返用例） |
| fmt / check | `cargo fmt --all -- --check` / `cargo check --offline -p qx-cli --all-targets` | 退出码 0 / 无 warning |
| 命令面 | `./target/debug/qx-cli.exe ecosystem` / `paper` / `verify` / `all` | 四条均退出码 0 |
| 能力矩阵 | `tools/check_architecture.py` 的 `未拿到外部沙盒记录前 sandbox_tested 全为 false` | `[PASS]` —— 本轮没有任何外部服务或凭据参与 |

上表的 Python 两行是同一回合抓到两次的：第一次用 `python/.venv/Scripts/python.exe`（386 项全绿 / `Ran 10 tests OK`），
第二次在文档回写之后。两次之间本机 `python/.venv/Scripts/python.exe` 被一个并发的 uv 进程删掉了（该目录 mtime 21:39，
同机可见的并发命令行属于另一份仓库的 `-m pytest tests/architecture`），所以复跑改用 uv 基础解释器
`%APPDATA%\uv\python\cpython-3.12.13-windows-x86_64-none\python.exe`，结果一致（387 项 / `Ran 10 tests … OK`）。
这条不是链路缺陷，但它证伪了 README 与 `build.bat` 里"`python/.venv` 恒可依赖"的假设 —— 本机 venv 的启动器
仍缺 `python.exe`，需要 `uv venv` 重建（见 V12 §16.5 表下的说明）。

### 未收口（如实记录，不写进"已达成"）

1. `RuntimeSupervisor::new()` 只 `register`（状态 `Starting`）、从不 `spawn`，所以修好的 stale 判定在 CLI
   自检路径上仍无生产触发者；真读者在 `qx-api` 的 `/ready`。本轮把口径写进文档，没有把 `health` 块伪装成运行期健康。
2. "地板等于磁盘值"这件事仍无判据，244 与 829 两个数仍是人工回合的产物（§15.6 第 5 条原样成立）。
   新增的全仓地板补的是**下跌侧**：删用例现在会红；写歪的数它不知道，它只知道不得低于 829。
3. 三遍清点不动公共 API，13 颗零消费者/无生产装配的断链发现（#106/#107/#108/#109/#110/#112/#114–#119）
   留给单独一轮。
4. `maturity/capabilities.yaml` 的 `sandbox_tested` 保持 `false`。


## Unreleased — V12 §15：上游 4 个提交合流到本地 V12 R4 工作树，冲突面全在口径而不是代码（2026-09-25）

`origin/main` 领先 4 个提交（`b4921ea` V11 R/S 两轮、`5cd11f8` T1–T4 常驻牙齿、`2bf8ad6` 与
`dbfc429` 两次把 `main` 合进 `p0-on-v10`），且本地 `HEAD = 72c347e` 是它的祖先 —— 提交层面是纯
fast-forward，未提交层面 61 个文件与本批 V12 R4 改动重叠，逐文件走三方合并（`git merge-file -p`，
输出留在 `%TEMP%/qx_v12r5_pull/merge2/`）。判定口径：**同一缺陷的双轨修复，编码单源化取上游、
结构取本地**。合流记录（7 颗变异、门禁量具自身被打坏的三处）见
[docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §15。

### Fixed（口径以哪一份为准，这一轮逐点判过）

- **账户快照四张键表的编码整份交给 serde，取上游那份更彻底的实现**（V11 R14/R15 → 与本地 TX5 同点）：
  `json_table_entries` / `json_position_entries` / `instrument_key` 三个渲染点各一处，`to_json` 的写入段
  不再自己抄序列化；`side_code` / `order_status_code` 只服务 `state_hash`，JSON 通道与读侧折算层都碰不到它们。
  上游在同批带来的两条协议用例（`stable_json_tables_round_trip_through_the_reader`、
  `the_cross_language_sample_is_what_the_writer_emits`）搬进本地的目录模块新文件
  `crates/qx-protocol/tests/snapshot_single_source/stable_json_tables.rs`，与 `optional_money.rs`
  共用同一个 `position_row`（可见性改成 `pub(super)`，仓库那份夹具由它产出）。
- **上游其余单源化一并取入**：`schemas/account-snapshot-v1.json` 经 `include_str!` 成为契约正文的唯一来源、
  `INIT_PROFILES` 一张表、`default_account_event_log`（无键读模型说的是哪个账户只有一处答案）、
  `ApiQueryModels::with_query_models_provider`（运维读模型每请求现读）、`publish_snapshot_for`、
  对账两格 `Option` + `apply_reconcile_reports`（没有报告 = `null`，对过且无差异 = `Some(0)`）。
- **`doctor` 的拓扑那一项改名 `runtime_supervisor_build`**（上游）：`doctor` 从不启动 worker，
  旧的 `runtime_topology: pass` 会被读成"运行拓扑健康"。现在通过语是
  `监督器可构建，启用 worker=<N>（未启动，不代表运行健康）`，运行健康仍归 `runtime-check`。
- **门禁量具被合流打坏三处，本轮修好并逐颗证明它咬得住**（`tools/check_architecture.py`）：
  `snapshot_row_wire_check` 里那份合并残留引用了本函数根本不存在的变量、钉的是已被删掉的手写编码器，
  整段重写为"写入段自己不碰 serde、持仓换键只在编码器里发生一次"；它与上游新增的
  `snapshot_json_table_check` 的重复判据删掉（同一条纪律两处钉，漂移时只会红错的那一处）；
  把合法业务档位 `OrderStatus::Unknown` 误判成 serde 兜底变体的子判据删掉，改名或兜底仍由
  `#[serde(...)]`/`#[repr(...)]` 那两条属性判据守住。`snapshot_contract_version_check` 的 needle 按
  当前代码逐条重校准，并新增 `without_line_comments()` —— 注释里复述一遍判据字符串，不该让门禁变绿。
  `case_source()` 改为递归读目录模块，用例搬家不再改常量。CLI 用例地板按磁盘重测抬起：合流后
  `^#[test]$` 实测 **243** 条（`src/tests` 191 + `crates/qx-cli/tests` 递归 52），而地板停在合流前的
  历史值 210 —— 落后 33 条的地板等于没有防守（`qx-execution` 同轮重测 15 + 10 = 25，恰好仍是磁盘值）。
- **README 两处说假话当场改掉**（本轮实测否证）：`--fill-tier` 只有 `l1`/`l2` 两档，原文写的"l2/l3 走
  订单簿内核"里那一档旗标根本不存在；`reconcile` 无参数运行打印的是用法并以退出码 2 结束，原文把它写成
  了一条"本地对账契约 smoke"命令。`multi-builtin` 的产物口径同时补上：只有 `--root` 点名时写 1 份归因产物。
- **行数棘轮按合并后的真实形状重算**（`maturity/line_budgets.yaml --snapshot`）：39 项，
  `crates/qx-adapter/src/binance.rs` 2291 → 2221（上游重连预算与本地拆分的净效果），
  新登记两项越过 500 行的文件（`qx-cli/src/worker_entry.rs` 505、`qx-cli/src/tests/api_snapshot_money_fields.rs` 518），
  六处因合并下降的文件同步下调。

### Verified（本轮实测，逐条可在 `%TEMP%/qx_v12r5_merge/` 的日志里 grep 到）

| 门槛 | 命令 | 本轮结果 |
|---|---|---|
| 架构门禁 | `python/.venv/Scripts/python.exe tools/check_architecture.py` | `ARCH_EXIT=0`、**370 项全绿**（`arch5.log`；合并当场红 9 项 + 残留 2 项，见 §15.3） |
| 整树测试（带解释器） | `QX_PYTHON=python/.venv/Scripts/python.exe cargo test --offline --workspace` | `TEST_EXIT=0`、**88 段 / 843 passed / 0 failed**（`test5.log`） |
| 整树测试（不带 `QX_PYTHON`） | 同上，去掉 `QX_PYTHON` | `TEST_EXIT=101`、**2 条 Python 桥用例红**（`test2.log`，本机 PATH `python` 是 WindowsApps 占位桩 —— 环境门槛，不是链路缺陷） |
| SQLite 后端 | `cargo test --offline -p qx-storage --features sqlite` | `SQLITE_EXIT=0`、9 段 / 51 passed / 0 failed（`sqlite4.log`） |
| fmt / clippy | `cargo fmt --all -- --check` / `cargo clippy --offline --workspace --all-targets -- -D warnings` | 均退出码 0（`clippy2.log`，`CLIPPY_EXIT=0`） |
| Python 侧 | `tools/validate_core.py` / `python -m unittest discover -s python/tests -q` | 退出码 0 / `Ran 47 tests … OK (skipped=1)`（`validate4.log`、`pyunit4.log`） |
| 命令面 | `qx-cli help` / `qx-cli strategy list` | 52 行用法 / 去重 41 个入口名（documented = clap 40 ∪ `help` = dispatched）；17 个内置策略 |
| 反向验证 | 7 颗变异（写入段自抄 serde、`side_from_code` 复活、版本闸门 `!=`→`>`、header 声明洗成合法值、`Side` 加 `#[serde(rename_all)]`、夹具被篡改、编码器不剥大括号） | **7/7 红在预期的那一条判据或用例上**，`cp -p` 还原后 `diff` 逐字节相同（`mut_m*.log`） |



## Unreleased — V12 R4：三遍清点（孤儿逻辑 / 无退出条件 / 契约贯通），当场抓出两条"账户一下单就读不回来"的断链（2026-09-24）

发布条件三遍清点的落地批。**第一遍**查孤儿逻辑：公共面上"只有别处测试在调"的死入口、看起来在读其实
读的是清单外的值；**第二遍**查无退出条件与阻塞点：停机令牌只有消费者没有生产者、重连上限按累计次数计、
一条用户流根本没有预算；**第三遍**查前后端与契约贯通，并当场抓出三条客户端或存储**立刻会失败**的断链
（包络不满足本进程自己公布的 schema、契约正文有两份互相矛盾的手抄、带订单的快照写得出读不回）。
收口记录（44 颗变异逐条对账、本轮量具自身失效的三处）见
[docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §14。

### Fixed（死入口删掉、等待有上限、契约与实现对齐）

- **三条零生产消费者的公共入口全部删除**（V12 §4.6 / R4-d）：`submit_order_via_gateway`、
  `submit_order_via_gateway_with_risk` 与第二条多腿编排入口（`MultiVenueSpreadExecutionService` +
  `VenueRouterMap`）—— 现在 `git grep` 这四个符号在 `*.rs` 里 0 命中。同时把这条纪律变成判据：
  受管入口族（`pub fn submit_order*`、`pub fn (load|save)_control_state`）的每个 `pub fn` 必须在
  **定义文件之外、且不在测试路径里**有引用（`dead_public_entry_check`）。D3 搬家之前这三条入口
  谁都看不见，搬家之后 `git grep` 才第一次暴露"只有测试在调"。
- **停机阶梯接上了生产者**（R4-a）：`ShutdownToken` 此前只有读它的人，`Ctrl+C`/`SIGTERM` 从不落到
  `RuntimeSupervisor::request_shutdown`，所以那条按预算汇合 worker 的阶梯永远等不到令牌。
  新增 `crates/qx-runtime/src/supervision/shutdown.rs`（80 行）：信号处理置进程级标志、阶梯把请求转发给
  supervisor、到 `shutdown_timeout_ms` 报 `StopTimedOut` 而不是无限等 —— 不假装能强杀线程。
- **两条用户流的重连预算改成按连续失败计**（R4-b/R4-c）：Binance 用户流原上限按**累计**次数计，
  一条长期健康的流只要累计满 10 次就永久放弃、失败一次也不清零；CCXT Pro `watch_orders` 会话
  **完全没有预算**（固定 `base_delay` 无限重连）。现在前者 `delivered > 0` 即清零、退避统一走
  `qx-core::retry`，后者抽出 `ccxt_stream_retry.rs::CcxtStreamReconnectBudget`
  （500ms 起 / 8s 封顶 / 10 次连续），放弃时给具名原因。
- **`run backtest <cfg> <outer>` 的外层位置参数不再被静默吞掉**（R4-e）：以前传了东西没人读，
  现在 `cli.rs` / `cli_args.rs` 当场拒（`backtest_subcommand_rejects_outer_positionals_it_never_reads`）。
- **`--json` 必须真的产出机器可读正文**（R4-f）：`config explain` / `config doctor` 等命令上那个旗标
  原先只是把人类文案换个地方印；现在三条命令各自有用例钉住"旗标兑现"，
  `config validate` 保留它那行人读 PASS 线（`cli_json_surface.rs` 3 条）。
- **两条事件读链的 `after` 语义统一成"严格大于"**（R4-g）：内存游标含 `after`、SQLite 不含，
  同一个请求两处给出不同条数；`qx-api` + `qx-storage` 现在同侧（`event_cursor_after_semantics.rs` 2 条，
  其中一条同时跑两条链、另一条钉住过期与超前游标的拒绝）。
- **账户快照包络真的满足本进程公布的 schema**（R4-h）：`/account/snapshot/envelope` 原先把 serde 派生
  形状塞进 `data`，而 `/schema/account-snapshot-v1` 公布的是 `to_json()` 线格式（无 `protocol`、
  无顶层 `schema_version`、键从数字变字符串）—— 客户端照第 7 号路由的 schema 校验，第一道门就过不去。
  现在包络复用唯一编码器，`data` 与扁平路由逐字段等值（`snapshot_envelope_contract.rs` 2 条）。
- **服务端不再公布手抄的第二份 schema**（A2）：`ACCOUNT_SNAPSHOT_JSON_SCHEMA` 改成
  `include_str!("../../../schemas/account-snapshot-v1.json")`，仓库那份成为唯一正文 —— 内嵌那份
  少了 `positions/orders/fills/transfers` 的 `additionalProperties` 与顶层 `additionalProperties:true`，
  而 `deploy/README.md` 一直宣称两边是同一份。
- **`RunManifest` 的指针有了生产读者**（R4-i）：`run_manifest.json` 兄弟路径、`data_fingerprint`、
  产物身份此前只写不读，等于声明了一条永远不会失败的复核。现在数据集指纹回落接**被复核过**的那一份、
  引擎自哈希不得冒充输入身份，`report` 真的按声明比对兄弟 manifest，缺失/被篡改都拒。
- **回测摘要落"这一轮按哪几个参数跑"**（R4-j）：`backtests/artifacts.rs` 写 `signal{knobs,declared_unused}`
  与下单数量格，两条 Bar 链各按自己的 `--config` 读，读者第一次能从产物里区分"没配"与"配了但不生效"。
- **`schemas/strategy_api_v1.schema.json` 与 Rust/Python 两份实现对齐**（R4-k）：`required` 名单按
  Rust 非 `#[serde(default)]` 字段逐项重排、`raw` 定点值只走 JSON 整数（原先放行十进制字符串）、
  补上 `margin_mode`/`position_mode`/`leverage` 三格、`additionalProperties:false` 对齐
  `deny_unknown_fields`；"大写枚举名过得了运行时但过不了 schema"这条差异写进 `description` 而不是靠
  改口径掩盖，Python 桥 `_reject_unknown_keys` 同步。
- **带订单的账户快照写得出也读得回**（交易 TX5，本轮唯一一条"账户一旦真下过单就坏"的缺陷）：
  `AccountSnapshot::to_json()` 是手写稳定编码器，而 `orders`/`fills`/`transfers` 按 `u64` 建键 ——
  键原样拼进 `{}` **产出的不是合法 JSON**；订单行的 `instrument` 落显示串、`side`/`status` 落数字码，
  而 `OrderSnapshot` 的派生反序列化认的是内核身份对象与枚举名，折算层根本不存在。合起来的后果是
  `SqliteSnapshotStore`、`FileSnapshotStore`、`GET /account/snapshot` 三条读法在"下过单"的快照上当场失败，
  而仓库原有往返用例只放 `cash_raw` 与 `positions`。写侧三张表的键一律
  `json_string(&id.to_string())`（`crates/qx-protocol/src/lib.rs:409/427/444`）；读侧新增
  `wire.rs::fold_stable_order_row`（`:244`）与**唯一一份**方向码/状态码表（`:201`–`:237`），
  未知码报 `ProtocolError::Invalid`，不兜底猜方向。

### Added（旋钮单源、用例、门禁判据）

- **#102：四个信号旋钮按 kind 收成一张表**（本轮最实质的口径修正）。`fast_window`/`slow_window`/
  `period`/`threshold_bps` 此前无条件接受配置，但 `macd` 一个都不读（写死 `MACD_WINDOWS (12,26,9)`、
  `MACD_REQUIRED_BARS 35`）、`rsi` 只读 `period` —— "改了参数没反应"是设计内的，**播报和产物却把它说成
  生效了**。现在 `BuiltinStrategyKind::signal_knobs()`（Macd 为 `&[]`）是唯一一张表，
  内核历史需求、运行时体检、`builtin-strategies` 的生效列、stdout 的 `knobs=… declared_unused=…`、
  摘要的 `signal{}` 块全部问它，四条"看起来在读"的 phantom 通道删除。
  **一条对 §6 原方案的偏离写清楚**：清单外的旋钮**不 fail-closed**，照常跑并列进 `declared_unused` ——
  这些键同时是历史配置项，拒跑会让既有 runtime 全部起不来；代价是"配了但不生效"只在播报与产物里可见。
- **TX5 用例三层**：协议层 `rows_keyed_by_integer_still_produce_and_recover_valid_json`
  （写出→读回→篡改 `side=9`/`status=99` 必须被拒）、存储层新增
  `crates/qx-storage/tests/snapshot_rows_persist.rs`（110 行，文件与 `--features sqlite` 两个后端
  各跑一次带 order/fill/transfer 行的密封快照，SQLite 那一条带 `#[cfg(feature = "sqlite")]`）、
  门禁层 `snapshot_row_wire_check` 6 项（键引号恰好 3 次、折算调用点恰好 1 次、码表在 crate 根缺席且
  在 `wire.rs` 单源、读侧不得出现 `unwrap_or`/`.or(Some` 兜底、两侧用例名在位）。
- **R4 用例新增/扩充**：`crates/qx-runtime/src/supervision/tests.rs`（停机阶梯 4 条 + 汇合工具）、
  `crates/qx-adapter/tests/user_stream_retry.rs`（4 条）、
  `crates/qx-cli/src/tests/ccxt_stream_retry_budget.rs`（2 条）、
  `crates/qx-api/tests/event_cursor_after_semantics.rs`（2 条）、
  `crates/qx-api/tests/snapshot_envelope_contract.rs`（2 条）、
  `crates/qx-cli/src/tests/cli_json_surface.rs`（3 条）、
  `crates/qx-cli/src/tests/backtest_input_provenance.rs`（基线 9 条）、
  `crates/qx-cli/src/tests/backtest_signal_provenance.rs`（5 条）、
  `crates/qx-strategy/tests/builtin_signal_knobs.rs`（4 条：清单外旋钮改不动一步、清单内每个旋钮仍改得动、
  Macd 无可调旋钮仍出信号、清单覆盖全部 kind）、
  `crates/qx-runtime/tests/strategy_contract_schema.rs`（新增 4 条）与
  `python/tests/test_strategy_contract.py`（新增 1 条，output 与 intent 两类未知键各一次断言）。
- **门禁 315 → 336 项**（+21）：`dead_public_entry_check`（1）、`snapshot_row_wire_check`（6）、
  `builtin_knob_list_check`（9），以及 A2/R4-i/R4-j 在既有函数里补的判据 —— 其中
  `snapshot_contract_version_check` 那条"常量与内嵌文本同号"改成"常量/仓库契约/Python 桥三处同号"，
  因为内嵌那份已经没了，旧判据会自证成空转。
- **行数棘轮在本轮的处理方式**：`crates/qx-protocol/src/lib.rs` 因 TX5 涨到 902 行 > 预算 831，
  做法是**把折算搬进 `wire.rs`**（198 → 277 行，低于 500 行登记门槛）而不是抬预算；
  `crates/qx-storage/src/sqlite.rs` 的落库用例同理（2579 → 2593 > 预算）改写成独立集成用例文件。

### 验收（`/tmp/qx_v12r4_close/`，2026-09-24 收口本轮台账后重跑）

- 门禁：`tools/check_architecture.py` `ARCH_EXIT=0`、`架构不变量自检全部通过 ✓（336 项）`
  （`arch.log`），含"能力矩阵证据路径全部存在""未拿到外部沙盒记录前 `sandbox_tested` 全为 false"
  "单文件行数预算只降不升"三条。
- 整树 `cargo test --offline --workspace`（带 `QX_PYTHON`）：`SUITES=84 / PASSED_TOTAL=807 /
  test result: FAILED 0 行 / WS_EXIT=0`（`workspace.log`），其中 `--bin qx-cli` 那一段
  `169 passed; 0 failed`。
- 被 `#[cfg(feature = "sqlite")]` 挡住的那一条必须单独跑：`cargo test --offline -p qx-storage --features sqlite`
  → `SQLITE_EXIT=0`，`tests/snapshot_rows_persist.rs` 段 `2 passed; 0 failed`
  （`storage-sqlite.log`）—— 不带 `--features sqlite` 时 rusqlite 后端整段被编译掉，"全绿"里根本没有它。
- **fmt 与 clippy 两道门槛是写台账这一轮才补跑的，且各抓到东西**：`cargo fmt --all --check` 先报 4 处
  rustfmt 漂移（`wire.rs:245/261/270` 与 `snapshot_rows_persist.rs:58`，全在 TX5 新写的代码里），
  按 rustfmt 期望手工落定后 `^Diff in` 计数为 0；`cargo clippy --offline --workspace --all-targets
  --features qx-storage/sqlite -- -D warnings` 抓到三处本轮新代码的 lint
  （`qx-strategy/src/builtin.rs:196` collapsible_if、`qx-strategy/tests/builtin_signal_knobs.rs` 三处
  manual_is_multiple_of、`qx-runtime/tests/strategy_contract_schema.rs` 两处 err_expect），
  改掉后 `CLIPPY_EXIT=0` 且 `CHECKED=22` 个 crate 全部在这次里被重查。
  改过的包逐个复测：qx-protocol 10+10、qx-storage(sqlite) 49、qx-strategy 18+4、
  strategy_contract_schema 4，门禁复跑仍 `336 项 / EXIT=0`（文本判据最怕重排版，实测未失效）。
  **教训与 §13.4 第 5 类同形**：R4 的收口只跑了门禁与整树测试，"用例红绿成对"证明的是判据咬得住，
  不证明代码过了 lint 或 fmt —— 三道门槛得各走一遍。
- 变异反向验证 44 颗（43 颗必须打红 + 1 颗正对照必须保持绿），颗颗在对应日志里 grep 得到"红"与"还原复绿"
  （§14.6 的表按项给出颗数与日志名；该表第一次写的合计是 43，与自己的列和不等，本轮按日志重数后改正）。
  本轮**另有三处失败红在量具而不是代码上**并一并留档：R4-e 的第一次测量 `gate.log` 里 `MUTATED_=0`
  （红绿探测没跑起来）、R4-f 的 `r4f.log` 里 M4FC `RESTORE_M4FC=identical` 却 `FAILED_CASES=3`
  （还原了源码没重建被测 binary）、TX5 的 T3 第一次测时门禁不红（判据只扫 `fold_stable_order_row`
  函数体，兜底落在 `side_from_code` 里）—— 三处修完才复跑成对红。
- **README 的"当前状态"三处数字按本轮实测改正**（细节见 V12 §14.9 末条）：门禁 315 → **336 项**
  （`qx_v12r4_readme/arch.log`、`arch2.log` 两次都是 `ARCH_EXIT=0`）；整树 58 段 / 768 passed →
  **84 段 / 807 passed / 0 failed**（`qx_v12r4_close/workspace2.log`，`WS2_EXIT=0`），并补一句
  "`--features sqlite` 那一段不在整树里、必须另跑"；`help` 的用法行按门禁自己的两个口径重数成
  **52 行 / 去重 41 个入口名**。链路表四行同步补上本轮能力：回测行加 #102 旋钮单源与 R4-i 的
  manifest 生产读者，Paper 行加 TX5 的"带订单写得出也读得回"，实盘行加 R4-b/R4-c 两条重连预算，
  运行时行加 R4-a 停机生产者与 R4-h/A2/R4-g 的契约贯通；四条对应 limitation 一并进表。
- 本轮**没有**接任何外部服务或凭据，`sandbox_tested` 全部维持 `false`：停机阶梯、两条重连预算与
  契约贯通只在进程内与本机后端上证过。


## Unreleased — V12 R1+R2+R3+D3：读侧不许把缺席念成数、契约版本真的认版本、一份 runtime 一个本金口径（2026-09-24）

V12 §4 前四条 P0 的落地轮。前三轮（Q67/Q68/Q70/Q72）把"没算过的钱"从**写侧**赶了出去，
本轮管的是另外三件同类的事：**读侧**仍然能把缺席念成一个合法值、账户快照的"版本校验"其实不会失败、
同一份 runtime 里可以并存两个互不相等的账户本金而没人说一句话。
收口记录（含 20 颗变异逐条对账、门禁量具本轮修掉的六类静默失效）见
[docs/archive/自研量化框架审计与重构方案-V12.md](docs/archive/自研量化框架审计与重构方案-V12.md) §13。

### Fixed（缺席就是缺席，版本必须会失败，本金只有一份）

- **报告与 `status` 的读侧换成 `Option` 语义**（V12 §4.1 / R1）：`report` 与 `[Latest Backtest]`
  原先用 `pointer(...).unwrap_or(0)` / `unwrap_or_default()` 渲染缺键，把 v1/v2 世代的产物念成
  `fills=0 return_bps=0 final_equity_raw=0`。现在排版集中在唯一一处
  `crates/qx-cli/src/report_readout.rs:91`（`report_readout_lines`）与 `:135`
  （`latest_backtest_readout_lines`），缺键一律印 `absent`，而**声明过的 0 仍印 0** ——
  两者必须在同一条断言里成对出现才算区分了"没数"与"数是零"。
  多腿 `cost_bps` 在 `turnover_raw == 0` 时报"算不出"（没有分母），`i64` 越界不再夹到 `i64::MAX`。
- **`report` 先报产物自己的世代**（`report_readout.rs:63` `summary_generation_note`）：
  `input` / `account` / `replay` 三块分别说清是"那一代还没这个块"（`not_declared_before_vN`）
  还是"本世代缺这块"（`MISSING_IN_THIS_GENERATION`），不再用同一句"没声明"糊住两种事实。
- **两个排队入口补做 A 股段闸门**（V12 §4.20 的漏项）：`api_service.rs`（`serve` 受理 Trading 命令）
  与 `workers.rs`（`strategy-worker` 入队）原先只在提交路径上问过 §4.20 的检查，排队路径放行 A 股段
  —— 订单形状在入队前就定死了，等到提交时再拒已经晚了。现在两处都调
  `reject_ashare_rules_on_submit_path`。
- **账户快照的"版本校验"现在真的会失败**（V12 §4.2 / R2）：`qx-protocol` 引入
  `ACCOUNT_SNAPSHOT_SCHEMA_VERSION: u32 = 1`（`lib.rs:53`，与内嵌契约、仓库契约文件、Python 桥四处同号），
  结构体入口（`validate()`，`lib.rs:207`）与字节入口（`from_json`，`lib.rs:496`）各有一道常量闸门。
  改之前只看"顶层与 header 自不自洽"，所以一份两边都写 7 的快照进得来。
  **§6 原文"缺 header 版本就拒"这一支被实测否证**：写侧本来就不在 header 里抄第二份版本号
  （契约的 header `required` 里没有它，`to_json` 与 `FileSnapshotStore` 也不写），拒缺席等于让自家存储读不回来 ——
  现口径是"缺席按顶层归一，声明了却读不出整数即拒"，边界钉在
  `header_without_version_is_the_stored_shape_and_still_round_trips`。
- **一份 runtime 只有一个账户本金口径**（V12 §4.4 / R3）：`strategy.initial_cash_raw`（回测）与
  `worker.paper_initial_cash_raw`（paper）此前互不知情，使用者可以回测按 A、paper 按 B 再拿两边数字互相印证。
  现在 `reject_split_account_principal()`（`backtests/account_base.rs:96`）在回测出口
  （`account_base_from_config`，`:117`）与**两个 Paper 入账入口**（`venue_runtime/paper_submit.rs:50`、
  `venue_runtime/paper_worker.rs:103`）三处都先拒，报错并列点出两侧名字与各自的数
  （`strategy.initial_cash_raw=… vs worker[<id>]=…`）；两处等值时来源才改口成第四格
  `strategy-initial-cash+paper-worker-cash`（`BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE`，`:21`），
  孤立声明不许冒充"两处一致"，Paper 侧等值时另印一行 `[Paper · Account] 账户本金两处声明一致`。

### Added（用例、门禁判据、棘轮范围）

- **读侧用例 11 条**：`crates/qx-cli/tests/report_readout_honesty.rs`（5 条真子进程：v1 产物缺键印
  `absent`、v4 声明过的 0 印 0、`status` 与 `report` 同一份排版、无产物要说没有产物、
  复核结论 `input_verified=` 单列一格）+ `src/tests/report_readout.rs`（6 条排版单元）。
- **本金单源用例 4 条**（`src/tests/account_principal_source.rs`）与命令行侧 2 条
  （`crates/qx-cli/tests/backtest_account_base.rs`）；排队入口的 A 股闸门用例并进
  `src/tests/ashare_submit_guard.rs`（现 7 条）。
- **契约版本用例 5 条**（`crates/qx-protocol/src/tests.rs`，从 `lib.rs` 末尾搬家）：自洽高版本被拒、
  假版本号（`"1"`/`true`/`null`/`0`/`-1`/`1.0`）被拒、缺席仍是存储常态、常量与契约同号、
  **未来版本且本构建读不出的文档也必须先被版本闸门叫住**（最后一条把字节入口那道闸门的独有职责钉住）。
- **门禁 293 → 315 项**（`tools/check_architecture.py` +435 行）：新增三个判据函数
  `report_readout_honesty_check()`、`snapshot_contract_version_check()`、
  `account_principal_single_source_check()`。每条新判据都配了一颗"摘掉它"的变异：
  M1–M8 与 M10–M14 静态红且行为红，M9/M15–M20 注入的是测试代码/路径/行数本身，故 static-only。
- **证据路径判据从"整行只有一个路径"收紧成"行首第一个 token 是路径就要存在"**：旧口径从不核对
  "路径 + 一句说明"这类最好写的证据行，于是 D3 搬家后 7 处死路径留在 `maturity/capabilities.yaml` 里
  而 315 项全绿。M20 专门注入这一类行，轮 7 实测 `ARCH_FAIL_LINES=1 / WANT_HITS=1 / OTHER_FAILS=0`。
  台账同步补齐：R1/R2/R3 共 14 条证据行、撤销一条已被 R3 闭合的 limitation
  （`backtest_initial_cash_and_paper_worker_cash_never_meet`）、新增 #82 那条未跟踪产物限制。
- **500 行棘轮扩到 `crates/*/tests/**/*.rs`（D3）并当场把 4 个超线文件搬家**：
  `multi_leg_attribution` 792 → 3 文件（最长 388）、`ledger` 1031 → 4 文件（最长 446）、
  `venue_report_contract` 943 → 3 文件（最长 467）、`snapshot_single_source` 669 → 3 文件（最长 333）；
  扩口径后登记表里 `crates/*/tests/` 条目为 0。`workers.rs` 里与 worker 循环无关的"闭合 Bar 指纹"
  那一簇搬进 `src/strategy_live_state.rs`（137 行）。用例计数判据改为按**目录拼接**取数
  （`case_source()`），搬家不再让任何一条"某个用例必须在位"的判据失效。
- **`CLI_TEST_FLOOR` 191 → 208 → 210**、`DISK_CLI_TESTS=210`、`DISK_EXEC_TESTS=25`。

### 验收（`/tmp/qx_v12_gate7.log`，2026-09-24 01:46:20–01:53:07 +08:00；567 行）

轮 6（01:16–01:23）是修好量具后的第一次有效测量，本轮把台账、地板与证据路径判据都改完之后
用**同一套脚本重跑一轮**作为收口口径 —— 下面每个数字都在这份日志里 grep 得到。

- 前置门槛：`cargo check --workspace --all-targets` `STAGE1_CHECK_EXIT=0`、`cargo fmt --all --check`
  `STAGE3_FMT_EXIT=0`、`clippy -D warnings` 四家 `WARNLINES=0`、20 颗变异锚点 `PREFLIGHT_BAD=0`。
- 变异反向验证：`grep -c 'MUT_STATE' = 20`，且 20 行状态全是 `= red`（`green`/`unexpected` 各 0），
  每颗 `ARCH_FAIL_LINES=1 / OTHER_FAILS=0`；`RESTORE_EXACT` 20 行、`RESTORE_DRIFT` 0 行、
  `RESIDUE_FILES=0 none`、`RESIDUE_MUTATIONS=none`、`PRISTINE_OK final`。
  行为口径 13 颗全红（M1/M2 各 `6 passed; 1 failed`，M3 `4;2`，M4 两口径 `4;1` 与 `5;1`，
  M5/M6 各 `8;1`，M7 `7;2`，M8 两段 `2;7` 与 `8;2`，M10 `6;1`，M11/M12 各 `3;1`+`6;1`，
  M13 `3;1`，M14 `2;2`）；其余 7 颗（M9、M15–M20）注入的是测试代码/路径/行数本身，static-only。
- 整树（带 `QX_PYTHON`）：`WS_SUITES=58 / WS_OK_SUITES=58 / WS_PASSED_TOTAL=768 / WS_FAILED_TOTAL=0`、
  `STAGE8_WS_EXIT=0`；整树（不带 `QX_PYTHON`）：`WS_NOPY_SUITES=78 / WS_NOPY_OK_SUITES=77 /
  WS_NOPY_FAILED_TOTAL=2`（`STAGE8_WS_NOPY_EXIT=101`），两条失败点名
  `rust_invokes_python_multi_intent_strategy_contract` 与 `…strategy_jsonl_worker_through_versioned_contract`
  （同一段 `153 passed; 2 failed`）—— §4.19 那条"Python 桥用例需要解释器"从断言变成实测。
- 门禁项数 `BASELINE_ARCH=315 → FINAL_ARCH_TOTAL=315`（`STAGE9_ARCH_EXIT=0`）；
  `DISK_CLI_TESTS=210`、`DISK_EXEC_TESTS=25`。
- 静态 `#[test]` 776 条（144 个文件；其中包内 `crates/<pkg>/tests/` 157 条 + `src/tests/` 173 条）
  − 执行 768 = **8**，与 V12 §12 R0 的归因表（5 条 `nats` + 3 条 `postgres`）逐条对上。
- 行数棘轮 `BUDGET_DIFF_LINES=0`（快照没改写任何预算）、`FINAL_BUDGET_SUM=37 files 46160 lines`、
  `TESTS_DIR_REGISTERED=0`；被跟踪的 `deploy` 产物被改写 `MODIFIED_DEPLOY=0` 项，
  但跑完仍有 **72 条未跟踪产物**（任务 #82 的根因，已作为 limitation 记进能力矩阵）。
- **门禁量具本轮修掉六类静默失效**（CRLF 污染 `mapfile`、heredoc 里的管道解析、`env -u` 被
  `~/.local/bin/env` 遮蔽、grep 不存在的键、**`-- <filter> --no-fail-fast` 被 libtest 拒认而基线 spec
  整轮空转**、**证据路径判据只核对"整行只有一个路径"的行**）。纪律随本轮钉下：
  **退出码与"0 失败"都不构成测量发生的证据，必须同时取到该跑必然产生的标记行**。
- 本轮**没有**接任何外部服务或凭据，`sandbox_tested` 全部维持 `false`。
## Unreleased — V11 合流轮（两次）：第一次契约对自家写侧说了谎，第二次冲突的全是文档口径（2026-09-23）

T 轮落进本地之后 `git fetch` 才看到 `origin/main` 上多了 Q70/Q71 两颗；把那颗合到全绿，远端又多出 Q72（72c347e），于是同一轮里合了两次。两侧各自跑完自己的门禁与用例都是绿的，
合流之后才发现**对外契约对它自家读模型每天印出的 `"equity_raw":null` 说了谎**：Q70 把协议侧 `equity_raw` 变成
`Option<i128>`（缺标记价的持仓不算权益，而不是拿剩余现金冒充），T3 刚把契约钉成"七格可空、权益不可空"。
本轮跟上事实，并把三处判据从"抄字段名单"换成"问写侧产物与协议类型"。逐项证据、5 颗变异与一句被当场证伪的话见
[docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §39；第二次合流的四处口径判断在同文档 §39.5。

### Fixed

- **`schemas/account-snapshot-v1.json` 的 `equity_raw` 改为 `["integer", "null"]`**，判据同时换掉三处：
  `crates/qx-protocol/tests/snapshot_schema_contract.rs` 那条用例改由写侧自己交两份产物（八项全未算 /
  八项全算得出）逐格比契约，并要求那份"全部未算"能被 `from_json` 读回 `None`；门禁
  `account_snapshot_schema_check` 第 4 项直接正则读协议源码，契约里可空的那些必须正好等于类型为
  `Option<i128>` 的那些；`python/tests/test_bridge.py` 补"契约允许 null 的键，对面 Python 读侧也收得下"。
  名单与类型任何一侧单独漂都会红。
- **门禁的一条失败文案停在旧口径**：`/account/balances` 那一项的条件里 `equity_raw` 是第三条，文案只点名
  available/margin——判据对、文案错，一样会把人往错的方向上带（M5 顺手查出来的）。
- 合流余下的三处编译错与一处夹具分叉：T3 的两份夹具赋值补 `Some(...)`（`snapshot_schema_contract.rs:279`、
  `snapshot_single_source.rs:679`），`api_snapshot_money_fields.rs:249` 改调 `src/tests/mod.rs` 里那份共享
  paper 夹具 `paper_runtime_config`。

### 合流时塌掉的那一份夹具

- 上游那份文件顶部的 `paper_runtime`（`origin/main` 版 `:13`）与本仓 `crates/qx-cli/src/tests/mod.rs:192` 的
  `paper_runtime_config` 是同一份配置的两份手写字面量——逐行比过，除函数名与 `pub(crate)` 外完全相同；
  `seed_paper_fill_with_fee` 在 `:31` 与 `mod.rs:209` 也是同一对。合流取共享那一份，上游新增用例的调用点改名到
  `paper_runtime_config`（`api_snapshot_money_fields.rs:249`）。合并后"同一张 paper 模板 + 临时 data_dir"这一族
  只剩 `mod.rs` 一处公共夹具；`ashare_submit_guard.rs:22` 那个同名 helper 读的是同一张模板但额外写那三个
  A 股键、返回的是路径对，它是这一族的第三个成员，本轮不动它（动它就是又一次夹具收敛）。

### 第二次合流（Q72）：代码零重叠，四处口径要判

Q72 动的是 `crates/qx-cli/src/backtests/*` 与新增的 `account_base.rs`，与本仓 T 轮改动不重叠；五份冲突文件
全是文档与门禁。判断逐条落在 V11 §39.5，这里只记结论：

- **上游那份收口指南仍写 `--fill-tier` 有 `l2`/`l3` 两档订单簿内核**，合流后的代码只认 `l1`/`l2`
  （`cli_help.rs:67-68`，R11 的收口）。照抄上游就是把一条已修的缺陷重新写成现状：取本仓那份，
  只把"多腿链当场拒收 `strategy.initial_cash_raw`"折进同一格。
- **README 的 blessed 摘要那格两侧都漂**：实测被 git 跟踪的 16 份 `*.summary.json` 全停在
  `schema_version: 1` 且缺 `input`/`account` 两块，当前代码写 4（`backtests/artifacts.rs:301`）——
  本仓那句"（无 `input` 块）"已经少说了一块。
- **上游 §34.5 的"只报不修"清单回读过现状**：`multi_builtin.rs` 498 行 / 线 500 行属实，回测与 paper
  两格本金互不知情属实（`strategy_schema.rs:133` 的注释自己就写着"回测读不到它"），两条原样留在 README。
- **编号位移不能全交给正则**：机械抬位把"`§32`–`§35` 在远端已是收口记录"抬成了 `§36`，范围两端只有
  一端该动；回读手写改回。远端现占 §32（Q70）/§33（Q71）/§34（Q72），本仓五节落到 §35–§39。
- **两次合流各抬长了上游的文件，最先烂掉的是 `file:NNN` 这一层**：结构性引用（某个函数、
  判据、产物字段住在第几行）按**当前代码**逐条复位，取证性引用（"改前那一行长这样"、
  "这条变异打在哪"）留在原处——它们钉的是那一轮的树，复位等于把记录改成假话。本轮复位
  29 条（V11 22 / CHANGELOG 7），口径见 V11 §39.8。

### 门禁数字（两次合流后本轮实测，覆盖 §38.6）

| 项 | 第一次合流后 | 第二次合流后（本轮实测） |
|---|---|---|
| 架构不变量 | 318 项全绿 | **328 项全绿**（+13 / −3 逐标签比过：10 项 Q72 新增，3 项按现状改名——用例地板 202→210、内置链包装定义、摘要 `input` 块随 v3→v4 改口） |
| 整树 `cargo test --workspace` | 770 passed / 0 failed | **778 passed / 0 failed / 0 ignored**（+8 条全来自 Q72：三条读法单元 + 五条命令行）；`--all-features` 另测 **782 passed / 4 ignored** |
| `CLI_TEST_FLOOR` | 202 | **210**（两侧各报过一个历史值 202 / 191，合流后按磁盘 `^#[test]$` 重测：`src/tests` 165 + `crates/qx-cli/tests` 45） |
| Python `unittest` | 46 OK（2 skip） | **46 OK（2 skip）**（上游本轮没动 Python 侧） |
| `cargo fmt` / `clippy -D warnings` | 0 / 0 | **0 / 0**（base + `sqlite` / `postgres` / `nats` / `postgres,nats` 四个组合各跑一次） |
| 行数预算 | 全过（三处随合流改快照） | **全过且 `--snapshot` 后 0 改动**：Q72 的新文件全在 500 行线下，`multi_builtin.rs` 498 行仍未入册 |
| 命令面与示例配置 | 52 条入口行 / 41 个命令名 | **同值重测通过**（`target/debug/qx-cli.exe help` 52 条 / 41 个、`deploy/*.json` 顶层 52 份），README 那三处实测口径照旧 |
| 文档编号 | R=§34、S=§36、T=§37、合流 §38 | **远端占 §32（Q70）/§33（Q71）/§34（Q72），本仓五节整批落到 R=§35、三遍扫描=§36、S=§37、T=§38、合流轮=§39** |

## Unreleased — V11 Q72：回测压在多少钱上，可声明、来源可见（2026-09-23）

回测链路实测（V11 #54）排到的第九颗：回测 FN9。§28/§29/§32/§33 那条纪律管"没算过的钱不许印成 0"，
这一颗管**被假定的钱**：一个数字如果每条链都心里有数、嘴上不说，它和"没算过"是同一类缺陷 ——
它决定收益率的分母、风控门看到的可用现金，而使用者从头到尾没被问过一句，也没在哪个产物里读得到它。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §34。

### Fixed（本金从三处字面量变成一格可声明、四处可见）

- **默认本金只剩一处具名常数**：`Money::from_i64(100_000)` 在生产代码里原有 **6 次**
  （`backtests/mod.rs:49` 那个从没被人读过的装配默认、`depth.rs:80/81/144/157`、
  `single_strategy.rs:58`，另加 `ecosystem_smoke.rs` 的自检副本）。现在它是
  `crates/qx-cli/src/backtests/account_base.rs:12` 的 `DEFAULT_BACKTEST_INITIAL_CASH`，
  门禁把"全 qx-cli 生产代码扫不到 `from_i64(100_000)` 字面量"写成判据（跳过 `tests` 目录与
  文件尾 `#[cfg(test)]`），抄第二份当场红。
- **本金改成装配的必答题**：`BarBacktestAssembly::new` 多一个 `initial_cash: Money` 入参
  （`backtests/mod.rs:48`），字段去掉 `pub(crate)`（`:16`）—— 新链漏答过不了编译，答完也不许事后改写，
  `single_strategy.rs` 那行 `assembly.initial_cash = initial_cash;` 随之删除。
- **非正声明当场报错而不是回落默认**（`account_base.rs:31-48`）：0 元账户上的 `return_bps=0`
  是一句假话，报错文案格出键名、值与理由。
- **多腿链当场拒收那一格单账户数字**（`leg_funding.rs:73-86` 的 `reject_configured_initial_cash`，
  `multi_builtin.rs:44` 调用）：两条腿的本金各按本腿行情定资，一份数字定不了两条腿，收下再静默丢掉
  等于配置说假话 —— 与 §22 的 A 股段、§23 的延迟设置同一条纪律。
- **实测分叉**（同一份 `sma_cross` + 同一份 `pairs-primary` 夹具 + 同一个 `--quantity 2`，
  本轮 `/tmp/qx_q72_probe/probe.log`）：不声明 → `fills=0 return_bps=0`
  （两笔买入被"买入成本超过账户可用现金"挡下）、`result_hash=892f8478c281a3e5`；
  声明 `200000000000000` → `fills=1 return_bps=-23`、`result_hash=4faa4db9d340864d`。
  改前 `fills=0 return_bps=0` 念出来像"这策略不赚不赔"，真相是"这两单位没人给它算过钱"。

### Added（来源可见、摘要落盘、用例与门禁）

- **stdout 三行本金播报**：`[Strategy · Account]`（`single_strategy.rs:215-216`）、
  `[Builtin · Account]`（`:371-372`，这条链不落摘要，stdout 是本金对使用者唯一的出口）、
  `[Depth · Account]`（`depth.rs:228-229`），排版函数只有一处。来源分三种且可区分：
  `builtin-default` / `strategy-initial-cash` / `multi-leg-funding-rule` —— "没配"与"配了同一个数"必须分得开。
- **摘要升到 schema v4**（`artifacts.rs:301`）：`BacktestArtifactsInput` 多一个必填字段
  `account_base`（`:223`，新增落摘要的链必须答它），`:305-308` 落 `account` 块两格
  （期初本金与来源，定点整数按仓库惯例写成字符串）。
- **配置面那一格**（`crates/qx-runtime/src/runtime_config/strategy_schema.rs:129-137`）：
  `initial_cash_raw: Option<i128>`（`Money` 是 i128 定点数，没有 i64 天花板），带
  `skip_serializing_if = "Option::is_none"` —— 省略时配置序列化字节逐字节不变，
  否则已 bless 的 `config_fingerprint` / `RunManifest` 会因一个不改变行为的字段集体失真。
- **用例 8 条，`CLI_TEST_FLOOR` 183 → 191**：单元 `crates/qx-cli/src/tests/backtest_account_base.rs`
  （3 条：默认必须有名字、声明值原样折成 `Money` 不被单位截断、`0/-1/-1e9` 全部 `unwrap_err`）；
  命令行 `crates/qx-cli/tests/backtest_account_base.rs`（5 条真实子进程，其中一条是**同一份配置只差
  那一格本金 → 结果必须不同**，断言 `fills` 0→1 且 `result_hash` 变化，另加来源两写法可区分）。
  既有两条跟着改口径：装配用例不再比对"装配自带的默认"，改成"必须原样带出调用方答的本金"。
- `tools/check_architecture.py` 门禁 **283 → 293 项**（3783 → 3964 行）：新增
  `backtest_account_base_check()`（10 条，字面量唯一 / 三个标签 / 装配私有 / 三条链各自读过 /
  三行播报 / 非正闸门 / 摘要 v4 / schema 字段形态 / 多腿拒绝必须带 `?` 传播 / 八条用例名齐备）。
- **一次被行数逼出来的搬家**：`single_strategy.rs` 加完两处读点与两行打印会越过 Phase 4s 的
  `cli_backtest_module_check()`，把与本金无关、但同样"三条链共用一句措辞"的信号参数侧
  （`apply_configured_builtin_signal`、`builtin_signal_note`）搬进
  `backtests/signal_binding.rs`（0 → 47 行），`single_strategy.rs` 496 → 477 行。
  `maturity/line_budgets.yaml` 本轮**一个字节都没动**（`BUDGET_DIFF_LINES=0`）。

### 验收（`/tmp/qx_q72_gate3.log`，整树 744 passed / 0 failed，77 个目标）

M1–M5 五条变异全部 `MUT_STATE=red` 且 `OTHER_FAILS=0`（每条只点亮被测那一项），五次还原均
`RESTORE_EXACT`，`PRISTINE_OK final`、`MODIFIED_DEPLOY=0`、`ARCH_ITEM_TOTAL=293`、`DISK_TEST_TOTAL=191`。
本轮另有两份作废日志，原因都写进 §34.3：`gate1` 的 M5 判据只数调用点、抽掉 `?` 仍然绿（一条真空判据，
预检变异当场抓到）；`gate2` 的 M2 改了判据文案没改脚本里的 want 串。**只有 gate3 是本轮验收依据。**
## Unreleased — V11 T 轮收口：上一轮"只报不修"里缺证据的四项，这轮全部改成有人咬着（2026-09-23，T1–T4）

§37.5 / §35.5 的清单里有四项，原因并不是"需要拍板"，而是**证据留不住**：修复已经在了（或两份读法已经漂了），
但全仓没有一条常驻用例会在它被改回缺陷态时变红。本轮只补这一件事，判据一句话——"任何人把它退回缺陷态，
CI 会不会响"。四项共 **33 颗变异逐颗验证**，每颗跑完按字节还原。逐项证据、变异报告与本轮踩到的两条见
[docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §38。

### Fixed

- **T2（R17）日历组件指纹两侧一宽一严**：Python `AshareTradingCalendar.component_fingerprint` 把摘要登记进
  DatasetBundle，CLI 的 `dataset_component_file_fingerprint` 在回测启动前对同一份文件重算，两份 canonical 字节
  各写一遍，中间只有一句"必须和 CLI 保持一致"的 docstring。实读到的分叉是具体的：旧格式日历（顶层没有
  `sessions`）Python 与回测规则装载都读成"没有时段"，指纹侧却直接报错——**Python 登记好的 bundle 会被 CLI 判成
  组件非法而拒启**。现在 `None => &[]` 对齐宽度（`sessions` 是非数组仍然报错，fail-closed 的那一半不动），
  两侧改为读同一对夹具，摘要只存在 `.fingerprint` 那一份文件里，代码里出现抄本会被门禁点名。
- **T3（S10）账户快照的对外契约有两份手抄，且文件那份零消费者**：`ACCOUNT_SNAPSHOT_JSON_SCHEMA` 与
  `schemas/account-snapshot-v1.json` 逐键比过（已声明字段全等、差 6 个键），但**两份都漏掉了写侧恒印的八个钱
  字段**。改为常量按字节 `include_str!` 仓库那份文件——漂移在编译期就没有退路；文件补齐八个钱字段（那一轮
  是"七格可空、权益不可空"，权益那一格在同日合流轮被 Q70 推翻，见上方「合流轮」一节）。另一半是读侧：`from_json`
  此前只核对"顶层与表内版本号是否自相矛盾"，一份按别的
  版本自洽封存的文档会被当 v1 解出来，而对面 Python `load_account_snapshot` 对同一份产物直接抛错；现在补
  `ACCOUNT_SNAPSHOT_SCHEMA_VERSION` 闸门，Python 侧必填集合同步补 `equity_raw`（必填说的是键必须在，值可以是 null）。
- **T4（S12）`reconcile` 的默认 worker 名是字面量**：`cli_args.rs:269` 是全仓唯一一个可选 `worker_id`，省略时
  HEAD 回落 `"reconciler-main"`，而 CCXT 那条链的 reconciler 实际叫 `ccxt-reconciler-main`。两个方向都撒谎：
  名字合法改动的拓扑上报"找不到 worker: reconciler-main"（把"没解析"说成"不存在"），恰好有个同名 execution
  worker 时会真跑一次下单执行。现在按 **role + Venue 绑定**解析（与 S1 同一判据），零个或多个候选都报错并列出
  候选要求点名。

### Added

- **T1（R13）：已修的判定补上常驻反例**，生产代码 0 行改动（`git diff HEAD -- crates/qx-api/src/lib.rs` 为空）。
  一条用例三向钉住：本账户的无 venue 事实必须全投影、换账户必须拒（订单与账簿各一次）、带显式 venue 的成交
  仍必须守住交易所边界——退回收紧与放宽过头两个方向各咬一半。
- 新常驻用例：`crates/qx-api/tests/account_projection_identity.rs`（1）、
  `crates/qx-protocol/tests/snapshot_schema_contract.rs`（6）、
  `crates/qx-cli/src/tests/calendar_component_fingerprint.rs`（3）、
  `crates/qx-cli/src/tests/reconcile_worker_identity.rs`（5）、`python/tests/test_ashare.py`（2），
  `test_bridge.py` 那条改为必填与版本双向断言。
- 新夹具：`python/tests/fixtures/calendar-component-v1.json` 与 `…-legacy.json`，各配一份 `.fingerprint`
  （由 Python 写侧函数产出，Rust 读侧只重算不另抄）。
- 新门禁 12 项：`calendar_fingerprint_caliper_check()`（4：夹具成对且两侧都读、摘要无抄本、
  两侧字段白名单与契约版本号逐项相等且声明个数相符、5 条常驻反例都在）、
  `account_snapshot_schema_check()`（8：只有一份文本且生产 Rust 无字面量、契约自洽、键集合等于写侧产物、
  可空口径、协议名与版本同源、路由发出的就是它、对面 Python 同宽、六条判据各有常驻用例）。

### 门禁数字（本轮实测）

| 项 | T 轮前 | 现在 |
|---|---|---|
| 架构不变量 | 302 项 | **314 项**（302 → +8 T3 → +4 T2） |
| 整树 `cargo test --workspace` | 752 passed | **767 passed / 0 failed / 80 个测试壳**（+1 T1 +5 T4 +6 T3 +3 T2） |
| `CLI_TEST_FLOOR` | 191 | **199**（实测 `src/tests` 160 + `crates/qx-cli/tests` 39） |
| Python `unittest` | 44 | **46 OK（2 skip）** |
| `cargo fmt` / `clippy -D warnings` | 0 | 0（`--workspace --all-targets --all-features`），`tools/validate_core.py` 全过 |
| 行数预算 | — | 新入册 `worker_entry.rs: 509`，`cli.rs` 675→674；`qx-api/src/lib.rs` 与 `qx-protocol/src/lib.rs` 均零增行 |

### 本轮踩到（§38.4）

- **变异电池里的"红"必须是断言红**：一颗变异的红其实是 `link.exe exit code 1104`（另有一颗 cargo 在抢同一个
  `target/`），脚本按"编译失败也算被抓"把它记成了通过。单独重跑才拿到真红；判定式改为编译/链接失败一律记
  "这颗没做完"。
- **变异要改判据，不是改判据所在的语法结构**：把 `if let Some((a, b)) = identity {` 整段换成 `if false {` 会连
  块体引用的绑定一起删掉（E0425），这颗不合法。CRLF 仓库里用 `\n` 拼锚点 0 命中那条老教训本轮又撞一次。

### 只报不修（升级路径写在 §38.5）

上一轮清单全部维持（R8、C3、C4、C6、C9、S8、S9、S11、S13 行为侧、`qx-execution` 1619 行结构债）。本轮新立四条，
都不需要拍板但一拍就得改口径：T2 的夹具模式只钉住了 `calendar` 一条分支，`corporate_actions` 那半边
（Python 的 `source_hash` 与 CLI 的 `serde_json::to_vec` sha256）今天靠"看起来一样"活着、没有用例也没有示例绑上去
（`bars` 那条看着同类，但 `dataset_component_paths` 里 `kind == "bars"` 被拓扑校验当场拒，遇不上 JSON 复检，已排除）；
旧格式（无 `schema_version`）日历的宽度现在靠两侧各自同意，"谁该写 `sessions`"仍无声明；T3 之后契约仍无字段级 description、顶层保留
`additionalProperties: true`，未知顶层键的行为没用例；`$id` 指向的 `https://qianxing.dev/...` 解析不到东西，
真接 JSON Schema 校验器要引依赖，与零新依赖纪律冲突。`maturity/capabilities.yaml` 里 `sandbox_tested` 仍全为
`false`。

## Unreleased — V11 R/S 两轮收口：三遍连通性扫描，从数据面查到控制面与插件边界（2026-09-23）

判据是"主体流程全部联通、核心链路无断链、无孤儿逻辑、无死循环、前后端贯通，至少查三遍"。三遍各换一个
视角：第一遍顺主链路（配置 → 绑定 → 装配 → 提交/成交 → 记账 → 投影 → 读模型 → 产物），第二遍看运行期
与外部接口（worker 生命周期、重连、健康结论、跨语言边界），第三遍反方向核对（声明了没人读的东西、读了
没人声明的东西、循环能否终止、两侧契约是否互拒），末了把同一套反向核对推到仓库外那一端（C++ 插件的 ABI
镜像）。两轮共立案 30 项（R 轮 18 项、S 轮 12 项），23 项落代码并
留常驻证明，7 项判为"需要部署侧或契约决策"只报不修；§35.5 另挂着前几轮立案的 C3/C4/C6/C9 与两条证明缺口
（R13 无常驻反例、`qx-execution` 1619 行结构债），本轮复核后仍未动。方法、逐项证据与变异报告见
[docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §35–§37。

### Fixed（R 轮 R1–R16、R18：数据面与读侧诚实）

- **一条事实只留一份编码**（R14/R15/R16/R18）：稳定 JSON 曾把裸 `u64` 当对象键，产出的 `{77:{…}}` 连合法
  JSON 都不是、三个读侧一起失败；`positions` 表仍是手写 `format!`，少印 `instrument` 且每加一个字段就静默
  丢一处；BarFrame 文档不自述契约版本，Rust 自己写出的帧在自家读侧被降级成 legacy 宽松分支；跨语言用例吃
  的是手抄字典（四张键表全填 `{}`）而不是写侧产物。四处统一成"读侧认的那一份 = 唯一被产出的那一份"，再用
  共用夹具与两侧断言焊死。
- **缺席不再印成零**（R7/R10/R12）：账户快照 `reconcile` 两格补"有没有来源"这一位；无键读模型端点过去各自
  挑各的"默认账户"（账簿说 A、快照说 B），现在 `default_account_event_log` 是唯一答案。
- **健康结论必须与"真的测过"绑定**（R4/R5/R6）：CCXT 行情 worker 整轮全部标的取数失败仍报 Ready、`/ready`
  把"一个 worker 指标都没有"读成依赖健康、doctor 的运行拓扑项只打印不判定（构建失败也记 pass）——同一个布尔
  在"没有观测"与"观测通过"之间被混用的三处，各自换成会红的判据来源。
- **运行期两处自欺**（R2/R3）：Binance 对账器重启后把自己发布的余额事实当成交易所回报，自比对自 → 永远
  "无差异"（身份只构造一次）；用户流重连预算按累计次数计，进程跑够久就永久断开且退避单调上涨（改按连续失败
  计数，`report.reconnects` 退回纯统计口径）。
- **配置与身份判定不再静默走偏**（R1/R11/R13）：五条回测链只有一部分问 `strategies[]` 的绑定，A 股段能从策略数组
  绕开"当场拒"闸门；深度链把配置里的 `strategy.fill_model` 丢掉（用户以为换了撮合，其实没有，现已加"零产物"
  负行）；API 投影拿标的后缀当账户 venue，把 paper 账户自己的订单事实判成外来事实。
- **文档面与命令面一致**（R9）：help 与 README 列过不存在的入口、漏过新增入口，用户照抄就跑不通。

### Fixed（S 轮 S1–S3、S5–S7、S13：控制面自身，以及验证手段本身可信）

- **S1 `serve` 启动即死**：监督器的 `HealthRegistry` 按 `worker.id` 建键，而两处 `spawn_worker` 写死字面量
  `"api"`；worker id 是自由命名、拓扑校验只数"启用的 api role worker 恰好一个"，所以任何改名后的**合法**配置
  都会让进程在启动瞬间死于"未知 worker: api"。改为 `configured_api_worker_id()` 按 role 解析（与拓扑校验同
  一判据），解析不到就报错而不是回落。
- **S2 两条生产循环永不自然结束**：`workers.rs` 的 scheduler 与 strategy 循环是唯二不读 `should_stop()` 的
  循环，`once` 之外的唯一出口是抛错。补停机轮询；令牌本身仍无人翻，立案为 R8（见下）。
- **S3 只读运维端点念开机那一刻**：`/scheduler/runs`、`/account/ledger`、`/reconcile/reports` 读的是启动时
  装进 `ApiState` 的三份读模型，worker 之后落的对账报告、追加的成交在 HTTP 侧永远看不见。新增
  `ApiQueryModels` + `with_query_models_provider`，每请求现读并回填 `ApiState`，使 trait 与 HTTP 同一口径。
  账户族四端点仍读 boot 投影（扩它要重投影泵），登记未动。
- **S5 可用值抄了三份**：`init --profile` 的接受集合在 match 里，help 的 `<…>` 表与"未知 profile"文案各抄
  一份且都漏了 `builtin`——用户被告知的取值集合里恰好少了那个真正能用的取值。改为 `INIT_PROFILES` 一份、
  三处由它生成。
- **S6 过期 binary 让子进程用例假绿**（本轮唯一一次"验证手段说谎"）：`tests/mod.rs` 的新鲜度守卫只装在
  `option_env!("CARGO_BIN_EXE_qx-cli")` 取得到值的分支上，而本 crate 的 `--bin` 单元测试走的是回落分支，
  `cargo test --bin` 又不重链 `qx-cli.exe`；于是 M4 变异第一次跑成全绿。回落分支补同一道守卫。
- **S7 对外端点没有文档面**：17 条路由里 15 条在全仓 markdown 零命中。`deploy/README.md` 补端点表（语义、
  查询串、非 200 口径、限流先于鉴权），并让"表内 `METHOD 路径` 集合 == 路由集合"与"未列路径一律 404"成为
  门禁项。
- **S13 C ABI 两份手抄镜像零比对**：插件契约由 `cpp/include/qianxing_strategy.h` 与
  `crates/qx-strategy/src/c_api.rs` 的 `#[repr(C)]` 各抄一份，头文件侧连一个 `static_assert` 都没有，Rust 侧
  的布局自证只钉自家 `QxRaw128` 与自家版本常量；CI 从没用宿主加载过 C++ 插件，所以改乱任一侧的字段顺序或
  类型宽度在 CI 里都是绿的。新增 `c_abi_header_check()` 做逐字段比对（10 类型 / 64 个结构字段 / 6 个判别值 /
  版本 `1`↔`1`，当前全等），M8–M12 五条变异证明"少一个字段、换一次顺序、改一下宽度、动一下版本、多一个
  类型"各当场红掉对应的门禁项。

### Added（用例与门禁）

- 新用例文件：`crates/qx-cli/src/tests/runtime_api_worker_identity.rs`（S1 两条）、
  `api_query_models_live.rs`（S3 两条）、`api_default_account_reads.rs`（R12）、
  `crates/qx-adapter/tests/binance_stream_retry.rs`（R3）、`crates/qx-datastruct/tests/frame_contract.rs`
  （R16 三条）、`crates/qx-datastruct/src/tests.rs`（用例搬家，lib 从 767 行降到 702 行）、
  `crates/qx-cli/src/backtests/config_declarations.rs`（R1/R11 共用的策略块解析入口）、
  `python/tests/fixtures/account-snapshot-v1.sample.json`（R18：写侧 `to_json` 原样输出，两侧各钉一次）。
- 新门禁：`control_plane_honesty_check()`（8 项，S1/S2/S3/S5/S6）、`api_surface_doc_check()`（2 项，S7）、
  `c_abi_header_check()`（4 项，S13：两侧类型集合对称差、字段名与顺序逐位、字段类型按映射表、枚举判别值与
  ABI 版本常量相等；未登记的 C 类型直接报错而不是跳过）。
  两轮的三遍扫描本身不新增依赖、不新增配置键：§36.3 的反向核对查到一处示例覆盖缺口（`cost_rules_path` 有文档
  无示例），已记为"缺口"而非"断链"，本轮未动。

### 门禁数字（本轮实测，覆盖 §31.4 / §35.4 口径）

| 项 | R 轮前（`99c7051`） | 现在 |
|---|---|---|
| 架构不变量 | 279 项 | 302 项（S 轮：288 → 295 → 296 → 298 → 302） |
| 整树 `cargo test --workspace` | 733 passed | **752 passed / 0 failed / 0 ignored**（78 个测试壳） |
| `CLI_TEST_FLOOR` | 178 | 191（实测 `src/tests` 152 + `crates/qx-cli/tests` 39） |
| Python `unittest` | 43 | 44（2 skip，均为本机缺依赖） |
| `cargo fmt` / `clippy -D warnings` | 0 | 0，`qx-cli` 特性矩阵四档（sqlite / postgres / nats / postgres,nats）同绿 |
| 行数预算 | — | 抬升：`qx-api` 3167→3224、`strategy_contract.rs` 822→840、`workers.rs` 718→728；下降：`binance.rs` 2291→2217、`qx-datastruct` 767→702、`qx-protocol` 885→844 |

### 本轮踩到（§36.2、§37.4）

- **委派报告里的每个 file:line 都要自己 grep 才算证据**：`/live` 端点、`is_backtest_artifact_path`、
  `CLI_TEST_FLOOR` 过期三条候选全经自跑驳回，未据此改代码。
- **绿色的子进程用例不等于绿色**（S6），**变异必须双向看**（M1a 只红用例、M1b 只红门禁），
  **门禁的锚点不能撞上自己要防的字符串**，**CRLF 仓库里用 `\n` 拼字节锚点会 0 命中**。
- **静态比对门禁要先证明解析器没在偷读**（S13）：比对器定稿前三次"报错"都是它自己没读对——tag 写在闭括号后的
  typedef 看不见、vtable 只取到回调名丢了 `abi_version`、`*const c_char` 一侧去空格另一侧不去（实测误报 18 处
  "类型不等"）。红得没有名字比不红更难查，所以未登记的 C 类型改为直接报错，并用五条变异证明它看得见差异。
- **日志为空不等于通过**：一次变异把输出写到 `$TEMP` 拼错的路径，读到的是另一个项目的过期日志，
  差点把编译失败的 `exit 101` 记成测试结果。

### 只报不修（升级路径写在 §35.5 / §37.5）

R8（停机令牌无人触发：出口有了、谁翻令牌是部署侧决策）、R17（日历组件指纹两侧各写一遍 canonical 字节，
无实测分叉但互不拒绝；**T 轮已收口**）、R13 无常驻反例（**T 轮已补**）、C3（账单水位是跨币种/跨腿标量）、
C4（Binance 行情流被对端关闭后不重连）、C6（实盘与回测对同一份帧给出两种数据集身份）、
C9（`valid_from/valid_to` 从不按交易时钟校验）、S8（产物摘要里的路径编码）、S9（`ProjectionLineage` 两格恒空）、
S10（账户快照 schema 两份、服务侧更松；**T 轮已收口**）、S11（API 限流参数硬编码）、
S12（对账默认 worker id 与 CCXT 链分叉；**T 轮已收口**）。另有一条不是缺陷而是证明形态缺口：
CI 从没把 C++ 插件交给 Rust 宿主加载过（`qianxing_strategy_example` 确实被 build 出来却无人 `load_verified`），
所以 S13 的 ABI 镜像一致性目前只有静态门禁一层保护。`maturity/capabilities.yaml` 里
`sandbox_tested` 仍全为 `false`——本轮全部是本机行为用例与静态门禁，不含真实下单回报。


## Unreleased — V11 Q71：多腿组合收益按两条腿的钱算，不再把两腿 bps 平均（2026-09-23）

回测链路实测（V11 #54）排到的第八颗：回测 FN8。§28/§29/§32 那条纪律管"没算过的钱不许印成 0"，
这一颗管**加权**：把两个分母不同的比率等权平均，念出来的既不是组合收益率、也不是任何一条腿的收益率，
而它在终端上看起来完全像一个合理的组合收益率。收口记录见
[docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §33。

### Fixed（一个印在终端上的加权错误）

- **组合收益改为两条腿按钱合计**：`crates/qx-cli/src/backtests/multi_builtin.rs:274` 此前印
  `(i64::from(primary_report.return_bps) + i64::from(reference_report.return_bps)) / 2`。两条腿的本金由
  `multi_leg_leg_cash` 各按**本腿行情帧的最高价**定资，天然不等 —— 仓库自带的那对现货夹具按 `quantity=100`
  跑，主腿本金正好是对冲腿的 **21.0 倍**，两腿分别 -197bp 与 -7bp，平均念 **-102bp**，按钱算实际是
  **-189bp**：这个组合的真实亏损被念轻了 87bp，接近一半。现在口径只有一处
  `Σ(期末权益 − 期初本金) × 10000 ÷ Σ期初本金`（`crates/qx-cli/src/backtests/leg_funding.rs:161-185`，
  与定资同处一文件，"权重"因此不再有第二个答案）。
- **算不出时报错而不是折成 0**：合计本金 ≤ 0、`× 10000` 越界、结果超出 `i64` 三种情形一律 `Err`。
  印 0 会把"这个组合根本没法度量"伪装成"这单套利不赚不赔"，与 `multi_leg_leg_cash` 撤掉静默截断同一条纪律。
- **一次被行数逼出来的搬家**：`multi_builtin.rs` 加完新代码会越过 Phase 4s 的
  `cli_backtest_module_check()`（`tools/check_architecture.py:1104`，`>= OVERSIZED(500)` 即红，登记进预算表
  也救不了）。把入口那 11 行组级合计折叠搬进归因内核 `crates/qx-cli/src/multi_leg.rs:452-470`
  （新增 `multi_leg_group_totals`，保证金仍取各组峰值而非求和），入口只留 1 行调用；
  `maturity/line_budgets.yaml` 本轮**一个字节都没动**。

### Added（用例、产物两端与门禁）

- `crates/qx-cli/src/tests/execution_and_multi_leg.rs:304-331`
  （`multi_leg_combined_return_weights_each_leg_by_its_own_capital`）：本金 3:1、两腿 +100bp/+1000bp 时必须
  给 **325bp** 而不是等权的 550bp；本金相等时两口径重合（排除"只是换了个说法"）；亏 1000bp 的主腿压过
  持平的对冲腿给 -750bp 而不是 -500bp；全零本金必须 `Err` 且文案含"本金合计必须为正"。
- `crates/qx-cli/tests/multi_leg_attribution/margin_and_return.rs:202-268`（当轮记作 `multi_leg_attribution.rs:726-792`；那个文件之后拆成了同名目录，用例本身没动 —— V13 R1-F 按代码事实重钉）
  （`combined_return_pools_both_legs_by_capital_instead_of_averaging_bps`）：跑真实 CLI，用产物自己声明的
  `accounts/*_initial_cash` 与 `accounts/*_final_equity_raw` 复算钱口径，要求 stdout 与
  `totals.combined_return_bps` 都等于它，并**另加一条 `!=` 两腿平均** —— 少了这条，用例在旧实现下也是绿的。
- **产物补齐复算所需的两端**：`multi_builtin.rs:445-446` 把两条腿的期末权益写进 `accounts`（期初本金本来就在
  同一块，缺一端别人复算不出来），`:459` 落 `totals.combined_return_bps`。
- `tools/check_architecture.py` 门禁 280 → **283 项**（四缩进 `check(` 201 → 204），
  `multi_leg_honesty_check()` 十二条长到十五条：调用点只许出现那一份实现且 `i64::from(primary_report.return_bps)`
  不得复活、实现体内必须留错误文案且不得出现 `Ok(0)` / `unwrap_or`；两条用例名 + 产物两腿期末权益键齐备；
  组级合计折叠只在 `multi_leg_group_totals` 一处。
- 用例地板 `CLI_TEST_FLOOR` 178 → **183**。这个数字本轮实测落后两次：Q69/Q70 新增的三条用例没回写地板
  （磁盘 181 而门禁仍写 178），连同 Q71 两条一起按磁盘总数（`src/tests` 143 + `crates/qx-cli/tests` 40）抬平。

### 本轮日志实测（`/tmp/qx_q71_gate1.log` 20:03:48 → 20:05:28，明细见 §33.4）

- `STAGE1_CHECK_EXIT=0`、`STAGE3_FMT_EXIT=0`、`STAGE4_CLIPPY[qx-cli]_EXIT=0`；`SNAPSHOT_EXIT=0` 且
  `BUDGET_DIFF_LINES=0`；基线三组用例 11 / 1 / 7 条通过，`0 failed`；
- 四条变异全部 `MUT_STATE=red`、`ARCH_FAIL_LINES=1`、`OTHER_FAILS=0`、`PASS_LINES=282`（每条只点亮被测那一项），
  四次还原全 `RESTORE_EXACT`。M1（调用点退回平均）红在 `multi_leg_attribution.rs:774`，M2（`Ok(0)`）红在
  `execution_and_multi_leg.rs:326`，M3（产物缺两腿期末权益）红在 `:756` 的缺键 panic，M4（入口重新自己折
  合计）只有结构项红 —— 算的是同一份五元组，行为用例分不出来，本轮如实记为"静态防守"而未补假用例；
- `QX_PYTHON` 指向 venv 的整树 `STAGE7_WS_EXIT=0`、`WS_OK_LINES=76`、合计 `736 passed / 0 failed`；
  `STAGE8_ARCH_EXIT=0`（283 项全绿）、`MODIFIED_DEPLOY=0`、`PRISTINE_OK final`。`gate1` 即验收轮，无作废轮次。

### 本轮踩到（记在 §33.3）

- **我自己写的第一版判据是假防守，被预检变异当场抓到**：原判据含 `"funded_raw <= 0" in combined_body`，而
  M2 的坏形状恰恰是保留该条件、只把 `Err` 换成 `Ok(0)` —— 条件还在，判据照样绿。改成钉错误文案 + 两种折零
  形状缺席。**每写一条"某串文本必须在/不在"的判据，先问一句"我要防的那个坏形状写出来还带着它吗"。**
- **rustfmt 按 76 字符把元组实参拆成 4 行**，两处共 8 行的增量正好把入口文件顶过 500 行线。修法不是压注释，
  而是先 `let primary_equity = primary_report.final_equity();` 再传，且这两个绑定在产物侧复用 —— 调用点与
  `accounts` 读的是同一个值。
- **零成交那档新旧口径恰好重合**：`crates/qx-cli/tests/builtin_signal_from_config.rs:433` 要求
  `combined_return_bps == "0"`（远档 `fills=0`，两腿权益都等于各自本金）。它在旧实现下也成立，因此**不构成
  新口径的证据**；加权与平均的分叉必须由本金不等的真实夹具来证明。

## Unreleased — V11 Q70：账户权益不再拿剩余现金冒充"算不出的权益"（2026-09-23）

交易链路实测（V11 #54）排到的第四颗：交易 TX4，是 Q67/Q68 那条"缺席不等于零"纪律的最后一颗。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §32。

### Fixed（最后一个不可区分的钱标量）

- **读模型不再替账户宣称"压在持仓上的那一腿不值钱"**：`crates/qx-cli/src/api_service.rs:367-370` 此前写
  `equity_for(...).unwrap_or_else(|| cash_for(...))`。内核 `Ledger::equity_for`
  （`crates/qx-core/src/ledger/query.rs:156-204`）的 `None` 含义明确 —— 有任意一条持仓拿不到标记价、或估值
  溢出。折成剩余现金之后，一个只有成交、没有行情事实的账户会把现金长期报成权益，且哈希/稳定 JSON/线格式/
  diff 全链路自洽，读侧无从发现。现在这个 `None` 原样发布为 `null`。
- **协议层第一次能表达"权益没算"**：`crates/qx-protocol/src/lib.rs:109` 的 `equity_raw` 由 `i128` 变
  `Option<i128>`（构造函数 `None`、`ScalarState` 同步），八个汇总钱字段从此共用 Q67 那一条带存在性标记的
  哈希与序列化通道，本轮没有新增任何一份手抄字段清单。
- **风控侧同一表达式是保守用法，明确不动**：`crates/qx-cli/src/venue_runtime/worker_runtime.rs:189-191`
  的 Paper 现货分支 `unwrap_or(cash_only)` 把权益往小里说、闸门只会更紧，其注释已写明；本轮红线只划在读模型侧。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/api_snapshot_money_fields.rs` 新增
  `equity_without_a_mark_price_is_absent_rather_than_the_remaining_cash`：成对钉"缺标记价 → `None` + 两份
  JSON 印 `null` + 可用资金照旧"与"补一条报价之后同一个账户必须重新算得出权益"。
- `crates/qx-protocol/tests/snapshot_single_source.rs` 的 `OPTIONAL_MONEY_FIELDS` 从七个变**八个**，
  Q67 那条"缺席≠零"遍历（哈希、seal、两份线格式、两个 diff 方向）由此自动覆盖权益，无需另写用例。
- `crates/qx-api/src/lib.rs` 的端点用例加第二段：默认快照在 `/account/balances` 上必须印
  `"equity_raw":null`（该端点生产体无需改动即不错报，但没有这一段就测不到权益的缺席态）。
- `tools/check_architecture.py` 门禁 279 → **280 项**（四缩进 `check(` 调用点 200 → 201）：新增"权益只在
  现金与每一条持仓的标记价都读得出时发布"一项（按语句区域判定，对 rustfmt 折行不敏感）；"八个字段全是
  `Option<i128>`"并入 equity；端点项加 `equity_raw` 的 null 发布判据；取法项从"数一行字面量"改成
  "盯整个 `fn scalar_money_raw` 函数体、禁 `Some(` 与 `unwrap_or`"。
- 行数预算：`crates/qx-protocol/src/lib.rs` 885 → 888、`crates/qx-api/src/lib.rs` 3167 → 3180（本轮唯一两处）。

### 本轮日志实测（`/tmp/qx_q70_gate1.log` 19:05:08 → 19:07:55，明细见 §32.4）

- `STAGE1_CHECK_EXIT=0`、`STAGE3_FMT_EXIT=0`、`qx-protocol|qx-api|qx-cli` 三格 clippy `-D warnings` 均 0；
  基线四组用例 10 / 7 / 11 / 1 条通过，`0 failed`；
- 六条变异（M1 读模型兜底、M2 取法折零、M3 端点折零、M4 共用 null 编码器印 0、M5 字段退回 `i128`、
  M6 读模型不算权益）全部 `MUT_STATE=red` 且 `OTHER_FAILS=0`，六次还原全 `RESTORE_EXACT`；其中五条
  （M1/M2/M3/M4/M6）都有行为用例同红，M5 属类型层、由静态项与编译器承担证明（`STAGE6_M5_CHECK_EXIT=101`）；
- `QX_PYTHON` 指向 venv 的整树 `STAGE7_WS_EXIT=0`、76 条 `test result: ok`、合计 `734 passed / 0 failed`。

### 本轮踩到（记在 §32.3）

- **第一版取法门禁项挡不住本轮自己的缺陷变异**：`Some(self.equity_raw.unwrap_or(0)),` 既不命中
  `"Some(self.equity_raw),"` 也不改变 `"self.equity_raw,"` 的计数，写出来就是一张不会红的静态项。判据由此
  改成盯函数体。**字面量计数型判据要先拿变异过一遍，再决定是否算"钉住了"。**
- **给字段新增"缺席"状态时，要逐个发布面检查是否真有一条用例让它缺席**：端点用例原本只把 equity 设成
  `Some(500)`，那一条"把 `None` 折成 0"的变异动不了它 —— M3 的 panic 点 `crates/qx-api/src/lib.rs:2339`
  正是本轮新增的那一段，没有它这个发布面就没有行为证明（日志里 M3 另外三组用例全绿）。
- 本轮 `gate1` 即验收日志：fmt/clippy 在变异段之前先跑绿，是 §31.3 那条教训的落地（无作废轮次）。

## Unreleased — V11 Q69：CCXT 对账的两半发现同时进事实流、报告与健康（2026-09-23）

交易链路实测（V11 #54）排到的第三颗：交易 TX3，§28.5 第 3、4 条（在 §29.5 以第 4、5 条重挂）。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §31。

### Fixed（一轮对账有三份互相矛盾的"本轮结论"）

- **两半发现改由一个汇总点出口**：`crates/qx-cli/src/venue_runtime/ccxt_reconcile_worker.rs` 新增纯函数
  `ccxt_reconcile_round(remote_open_issues, local_order_issues)`，返回一份 `order_issues` 与一份
  `require_reconcile`。此前远端孤单挂单只进持久报告与健康判定，本地"查不到远端结果 / `sync_order` 报错"两处
  只进事实流 —— 于是这一轮已经把某张单子推到 `OrderStatus::Unknown`，报告里却写着"本轮没有 order 问题"，
  worker 心跳仍是 `Ready`。现在事实流（单一写入点）、`additional_order_issues`、`ServiceStatus` 与运行明细
  读的都是同一份 `round.*`。
- **"能否落事实流"改为按句柄判定，而不是按属于哪一半**：只有 `client_order_id` 能解析成 JSON 数字的发现才落
  `ReconcileRequired`（OMS 需要一个真实句柄），对不上的远端孤单留在报告里等人工确认归属。用例把
  "同一轮报告 5 条 / 事实流 3 条"钉成正确答案。
- **对账报告的三个覆盖度计数不再把"没取"印成 0**：`crates/qx-api/src/lib.rs` 的
  `position_snapshots_count` / `funding_rate_snapshots_count` / `cashflow_count` 变 `Option<usize>`
  （`#[serde(default)]`，落盘的老报告仍读得回来），CCXT 侧由 `positions_observed` /
  `funding_rates_observed` / `cashflows_observed` 三个观测位门控取值，Binance Spot 这条链对自己从不查询的三项
  直接报 `None`。Q67/Q68 的"缺席不等于零"纪律由此落到对账产物上。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/ccxt_reconcile_round.rs` 两条用例：`ccxt_reconcile_round_routes_both_discovery_halves…`
  断言报告 5 条 / 事实流 3 条与 `(client_order_id, event_tag)` 三元组，并禁止把字符串客户号当本地句柄；
  `ccxt_reconcile_service_status_degrades_on_a_local_only_finding…` 钉住"只有一张本地单子查不到远端结果"
  那一半也必须降级。为此把健康结论从 worker 循环里抽成纯函数 `ccxt_reconcile_service_status`。
- `crates/qx-cli/src/tests/e2e_and_python_contract.rs` 的
  `reconcile_report_persists_structured_balance_discrepancy` 扩到"落盘 JSON 里 `null` 与 `0` 可区分"，
  另加两条旧报告反序列化断言（数字读成 `Some(0)`、缺键读成 `None`）。
- `tools/check_architecture.py` 新增 `reconcile_round_honesty_check()`（10 项）：汇总只一次、三面读同一份、
  事实写入点唯一、`as_u64` 句柄判据、字段形态、Binance 报 `None`、三个观测位、用例在位。
  门禁 269 → 279 项；`crates/qx-api/src/lib.rs` 行数预算 3160 → 3167（唯一变动项），
  `ccxt_reconcile_worker.rs` 341 → 469 行。

### 本轮日志实测（`/tmp/qx_q69_gate4.log` 08:59:35 → 09:01:15，明细见 §31.4）

- `STAGE1_CHECK_EXIT=0`、`STAGE3_FMT_EXIT=0`、`STAGE4_CLIPPY[qx-api|qx-cli]_EXIT=0`、门禁 `279 项` 全绿；
- 六条变异全 `MUT_STATE=red` 且 `OTHER_FAILS=0`，六次还原全 `RESTORE_EXACT`；其中只有 M4（句柄判据）与
  M6（健康结论忽略发现清单）让行为用例真失败，M1/M2/M3/M5 只由静态门禁抓到；
- `QX_PYTHON` 指向 venv 的整树 `733 passed / 0 failed`（76 条 `test result:` 行，退出码 0）；不设该变量的
  gate4 内基线是 `2 failed` —— 本机无解释器的两条已知 Python worker 契约项，与 §29.4 同一口径。

### 本轮踩到（记在 §31.3）

- 三轮日志作废后才拿到验收日志：`/tmp/qx_q69_gate1.log` 里 `cargo clippy -D warnings` 把 `.then(|| …)` 报成
  `unnecessary_lazy_evaluations` 硬错（`CLIPPY[qx-cli]_EXIT=101`）、`rustfmt --check` 两处不过；
  `/tmp/qx_q69_gate2.log` 里 rustfmt 把 `let service_status = if …` 折成一行，M2 的 needle 0 次命中；
  `/tmp/qx_q69_gate3.log` 走完 M1–M5，但暴露出 M1/M2/M3/M5 四条**只有静态门禁红、行为用例全绿**。
  于是把健康结论抽成 `ccxt_reconcile_service_status` 并加 M6（判定忽略发现清单），M6 是唯一一条让行为用例真
  失败的缺陷变异。**教训：变异轮之前先把 fmt/clippy 跑绿、按格式化后的文本取锚点；"静态门禁能抓"不等于
  "行为用例能抓"，两件事要分别在日志里看见。**
- 两条门禁项最初共用一条标签，导致"健康只看远端一半"的变异红得没名字 → 拆成两条独立项。

## Unreleased — V11 文档收口轮：仓库里只留"今天仍然正确"的文档（2026-09-23）

用户请求的四件事（提交代码 / 删除历史无效文档 / 更新接口使用文档 / 更新项目说明文档）。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §30。

### Changed（文档面）

- **删除 23 份历史文档**（`git rm`，原文全部在 git 历史里）：V1–V10 各代方案与审计（`牵星完整架构方案-V1`、
  `牵星最终架构方案-V2-Barter对齐版`、`牵星架构拆分与扩展决策-V1`、`牵星终极改造计划-V1` 与其实施状态附录、
  `自研量化框架重构方案-V6/V7/V8/V9/V10`、`Qianxing-Multi-Asset-Quant-Infrastructure-V2`、`QIANXING_CODE_AUDIT_V2`、
  四份 V5 计划/审计/状态/RC + `v0.0.1` release note、根目录 V4/V5.1 两份、可视化终态稿、产品化路线图、差距清单）。
  判据不是"旧"，而是**文档里的模块表或命令名在当前代码里已经不存在**：23 份里有 14 份、合计 115 行点名
  `qx-domain` / `qx-kernel` / `qx-application` / `qx-oms` / `qx-portfolio` / `qx-fenye` / `qx-viz` /
  `qx-server` / `qx-schema`（行数按 `git show HEAD:<路径>` 逐份数命中行；这九个名字在当前 workspace 实测
  23 个 crate 里一个都没有，其中也含"该 crate 已并入 x"这类当时正确的记述，故 115 是上限），
  README 也已经不链接其中大部分。
- **保留的专题文档只改错、不删除**：CCXT worker 契约、A 股接入、外部链路验收阶梯、衍生品统一模型、
  多语言策略契约仍是这些知识的唯一副本（`deploy/README.md` 只做摘要）。
- **`Qianxing-Visualization-Architecture-V1.md` 的去留**：正文是未落地的 Web/桌面端设计（dashboard、market
  terminal、前端接线），其 V1.1 修订自己承认 `qx-viz`/`qx-server`/`qx-schema`/`/ws` 不存在。唯一值得留的是那条
  边界（只读投影不做第二个事实源）——搬进 `README.md` 的"设计底线"第 8 条后删除整篇。

### Fixed（文档里的死命令名与不存在的文件）

- `builtin-backtest` → `backtest builtin`、`ccxt-builtin-backtest` → `backtest ccxt-builtin`、
  `strategy-backtest` → `strategy backtest`、`ccxt-backtest` → `backtest builtin`（前三条入口在 V9 Phase 2
  已整体删除，今天调用得到"未知命令"和退出码 2；本轮用当前 binary 实测确认）。
  位置：`docs/工业化易用性收口指南-V1.md`、`docs/CCXT多交易所接入与策略运行方案-V1.md`、
  `docs/A股数据源接入与快速选股回测方案-V1.md`、`docs/工业级多语言策略与高性能交易方案-V1.md`、`deploy/README.md`。
- 收口指南里 4 处 `deploy/qianxing.runtime.production.json` 指向**不存在的文件**，改为
  `deploy/qianxing.runtime.production.example.json`；同一篇的"策略包括 …10 个"改为按 `builtin-strategies`
  实测的 17 个（13 单标的 + 4 个只被 `backtest multi-builtin` 接受的套利 kind）并停止复述名单。
- CHANGELOG 与 V11 中指向已删文档的 4 处链接改为"已删除，原文在 git 历史"的注记；历史条目本身不改写。

### Added（接口使用文档）

`docs/工业化易用性收口指南-V1.md` 补三节，全部按当轮实测写：

- **§3.2 回测入口族与配置落点**：七条回测入口 × "哪些策略段在这条链上有落点 / 配了即整轮拒绝"的对照表
  （A 股段在 `multi-builtin`/`book` 无落点、深度档拒绝非零延迟、`--fee-bps` 优先级、衍生品只向声明为衍生品的腿计提）。
- **§3.3 产物、输入身份与重放结论**：`*.run.json` 16 个键与 `*.summary.json` 关键段（实抓自本轮一次
  `backtest builtin macd` 运行），并说明 `replay.log_digest == result_hash` 是**重新驱动事件流的结果**，
  而不是旧那个恒等式。
- **§3.4 读模型里的 `null` 与 `0`**：把 Q67/Q68 的钱/价格两套编码写成面向消费方的口径，含"Ledger 回退行恒定
  报 `null`"与"CCXT 缺 `side` 整条拒绝"两条生产者事实。
- `README.md` 新增**文档地图**（每份保留文档一句"什么时候读"）与**当前状态**的链路成熟度表，替换掉原来那段
  无法核对的能力长句和 V5 时代阶段表；演示输出块换成实抓文本。

### Verified（本轮实测，数字不来自旧文档）

| 事实 | 实测值 |
|---|---|
| workspace crate 数 = README 模块表行数 | 23（表内另列 `python/qianxing_ccxt`、`cpp/`）；脚本核对表内无缺项 |
| `qx-cli --help` 入口条数 | 50 |
| `builtin-strategies` | 17（13 单标的可走 builtin / ccxt-builtin / book；4 套利 kind 只被 multi-builtin 接受） |
| `deploy/*.json` 示例配置 | 52 |
| `tools/check_architecture.py` | `架构不变量自检全部通过 ✓（269 项）` |
| `qx-cli all`（进程内自校验） | 退出码 0，`全部自校验通过 ✓` + PaperVenue 契约行 |
| `backtest builtin macd deploy/qianxing.bar-frame.example.json` | `bars=70 fills=1`，产物四件套字段逐条读出 |
| 能力矩阵 | 18 个能力块；`implementation`+`code_tested` 双真 15；`sandbox_tested` 0；`production_approved` 0；证据 155 条 / limitation 52 条 |
| README 命令示例 ≡ help 入口 | 脚本比对：README 里所有 `-- <cmd>` 提及都能在 help 的 50 条中找到（0 例外） |
| 全仓 markdown 相对链接 | 14 份 `.md`，悬空链接 0 |

### Known issues（本轮没做）

- README 的快速开始命令表**没有门禁**：`check_architecture.py` 只钉「clap 命令表 ≡ 派发 ≡ help」，
  README/文档里的命令是 prose，漂移只能靠人工核对（本轮就是这么抓的）。
- 收口指南与 `deploy/README.md` 仍有重叠段（`storage.consistency` 三档、CCXT 凭据键、发布前检查），
  合并方向未定：要么把指南并成一篇教程、要么把 `deploy/README` 缩成配置键参考。
- 被删文档中的"三轮端到端链路审计"结论只留在 git 历史，未回填到 V11；如果以后要复用，从 commit 里取。

## Unreleased — V11 Q68：持仓行的未算钱不再印成 0，缺方向不再靠猜（2026-09-22）

交易链路实测（V11 #54）排到的第三颗：TX2 的下半截（持仓行）+ 实盘回报入口（TX2b），不在 §6 排期内。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §29。

### Fixed（交易链路：持仓行的三个钱字段与方向）

- **内核持仓观察的 `unrealized_pnl` / `initial_margin` / `maintenance_margin` 改 `Option<Money>`**
  （`crates/qx-core/src/event.rs:187-191`，各带 `#[serde(default)]` 让老日志缺键读成未报）：改前三者是裸
  `Money`，摘要里 `h.write_i128(position.unrealized_pnl.raw())`（HEAD `:451-453`）——"这个交易所不报维护保证金"
  与"报了零"在类型、在 `Event::digest`、在线格式上是同一份状态。现在摘要逐字段先写存在性标记再写值（`:463-466`）。
- **线格式行的两列钱改 `Option<i128>`，`Default` 不再兜 0**（`crates/qx-protocol/src/wire.rs:34-35`、`:46-47`）：
  任何 `..PositionSnapshot::default()` 起步的构造不再自动"报一个零"。**价格列刻意保持不变** —— 定点价格里 `0`
  不是合法值，钱没有这个性质；这条区分写进字段文档并由门禁钉住。
- **读模型的 Ledger 回退行不再替交易所报数**（`crates/qx-cli/src/api_service.rs:451-452`）：改前是写死的
  `unrealized_pnl_raw: 0, margin_raw: 0`（HEAD `:332-333`），而 Ledger 里没有这两个量（要按冻结的 market spec
  逐标的算，读侧没有规格来源）——一个上涨 5% 的现货仓位会长期报着"没有浮亏"。
- **CCXT 持仓方向不再靠猜**（`crates/qx-cli/src/ccxt_facts.rs:110-122`）：改前
  `.unwrap_or("long")`（HEAD `:109`），而上游连接器（`python/qianxing_ccxt/__init__.py:1240`）在交易所不给方向时
  印的是字面量 `"unknown"` —— HEAD 的实际行为是数量按**正数**入账、`position_side` 记 `"unknown"`，同一份观察里
  数量与方向自相矛盾。现在只接受 `long`/`short`，其余连 symbol 带"读到什么"一起拒绝；零数量行仍在闸门**之前**跳过。
- **CCXT 三项钱读成"没报"而不是零**（`:146-154`）：改前缺键与 `null` 一律 `Ok(Money::ZERO)`（HEAD `:131-140`），
  即生产方说"没报"、消费方改口说"零"。守卫侧（`crates/qx-runtime/src/pipeline.rs:1649-1657`）改成"报了才判"，
  不报保证金的交易所不会被判非法回报。
- **折算与哈希收口到共用写法**：行哈希与账户标量共用 `write_optional_money`（`crates/qx-protocol/src/lib.rs:236`、
  `:287-288`），`null` 的写法全仓唯一（`:248` `money_json`），线格式没有的那一列保证金折算回 `None` 而不是 0。

### Added（用例与门禁）

- 8 条新行为用例，全部走真实入口：`crates/qx-cli/src/tests/ccxt_position_facts_honesty.rs`（146 行 3 条——方向
  fail-closed、平仓位先跳过、省略/`null`/`"0"`/`0` 四态区分）、`api_snapshot_money_fields.rs` 加 2 条行侧
  （回退行缺席、venue 报的行保留两态）、`event.rs` 的摘要两态用例、`snapshot_single_source.rs` 加 3 条
  （折算两态、行两态逐个字段跑哈希/seal/两种线格式往返/增量双向、`Default` 不报钱）；`pipeline.rs` 落盘回读用例
  加第二行，让"报了零"与"没报"同时出现在一份快照里并要求回读后不收敛。
- `tools/check_architecture.py:4641` `position_money_honesty_check()` 13 项（静态门禁 256 → **269**）：内核
  Option 形状、摘要标记唯一、行字段形状**且价格列保持 0 哨兵**、折算与槽位各只有一处写法、全仓逐行禁止给这五个
  字段兜 `0`/`Money::ZERO`、方向闸门禁抄列表、跳过点必须先于闸门、守卫的 `is_some_and` 序列、八处用例按
  `fn NAME(` 取证。`CLI_TEST_FLOOR` 173 → **178**；行数棘轮重登记 `event.rs` 674→749、
  `qx-protocol/src/lib.rs` 856→885、`pipeline.rs` 2349→2391。

### Changed（有意的破坏性后果）

- `Event::digest` 的形状变了（多一次存在性标记）：**含 venue 持仓观察的既有事件日志**会在 manifest 核对处被拒
  （`crates/qx-storage/src/lib.rs:310-312`、`crates/qx-storage/src/sqlite.rs:2139`）。这是有意的 fail-closed ——
  旧日志里"没报"与"报零"本来就是同一份内容，无法事后区分。本轮实测被跟踪的产物侧不含这类日志
  （`deploy/` 中 `AccountPositionSnapshot`/`account_positions`/`unrealized_pnl`/`"digest"` 各 0 命中，
  `MODIFIED_DEPLOY=0`），但真实跑过 paper/CCXT 的用户目录会受影响，升级路径尚未设计。

### Verified（两轮门禁，数字取自 `/tmp/qx_q68_gate2.log`）

- 20:54:05 → 21:00:10：`cargo check --workspace --all-targets` `=0`；`rustfmt --check` `=0`；`cargo clippy`
  四个 crate 全 `=0`；7 条行为变异 + 1 条声明型 + 4 条静态变异全部 `MUT_STATE=red` 且各只点亮被测项
  （`OTHER_FAILS=0`、`MUT_STATE=unexpected` 0 次），12 次还原全部 `RESTORE_EXACT`；设 `QX_PYTHON` 的整树
  `731 passed / 0 failed`，不设 `729 passed / 2 failed`（本机无解释器的两条已知 Python worker 契约项）；
  `Ran 43 tests` Python 侧 `OK`；`MUTATION_RESIDUE=` 空。
- 首轮 `/tmp/qx_q68_gate1.log` 作废重跑，原因有三条（都记在 §29.3）：`MQ68a` 摘 `#[serde(default)]` 行为不变
  （serde 对 `Option<T>` 缺键本就补 `None`）→ 改登记为声明型；`MQ68d`（`Default` 兜零）当时没有任何用例失败 →
  补钉构造入口的用例；`MQ68h` 第一版 needle 与原守卫语义等价、不是可达缺陷 → 换成 `map_or(true, …)`。

### Known issues（本轮实测钉出的，交给下一轮）

- 持仓行的浮亏仍然**没有生产者**（只是不再撒谎）：要先把冻结的 market spec 送到读侧。账户级五个字段同样仍无生产者。
- `crates/qx-cli/src/ccxt_facts.rs:195-198` 资金费的 `timestamp_ms` 缺失仍回落 `0`（本轮只收了持仓侧）。
- ~~CCXT 对账的两半仍不相交（in-loop 发现只进事实流、`open_order_issues` 只进报告，worker 在已有单子"结果未知"时
  仍标 `Ready`）；`binance_reconcile.rs:154-156` 三个覆盖度计数恒 0~~ —— 两条均已由同日的 Q69 关闭（见顶部与 §31）。

## Unreleased — V11 Q67：账户快照不再把"没算过的钱"印成 0（2026-09-22）

交易链路实测（V11 #54）排到的第二颗：账户级读模型 TX2，不在 §6 排期内。收口记录见
[docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §28。

### Fixed（交易链路：七个汇总钱字段里六个没有生产者）

- **`available_raw` 不再是 `equity_raw` 的副本**：改前 `crates/qx-cli/src/api_service.rs:374` 就是
  `snapshot.available_raw = snapshot.equity_raw;` —— 把已经压在持仓上的那段钱说成可自由花掉。现在取本条快照记账
  的那一本结算账簿现金 `cash_for(account_id, settlement_currency())`（`:276`）。
- **`fees_raw` 与同一条快照的逐笔成交同源**：此前账户级恒 0，而同一份 JSON 的 `fills[].fee_raw` 逐笔是真数。
  现在由这些 `fee_raw` 以 `checked_add` 加出（`:296-302`），**合计溢出即拒绝发布这份快照**而不是印一个回绕过的
  数（用例把账户现金精确压到 `i128::MIN`，证明溢出只可能出现在读模型这一侧）。
- **算不出来的钱在协议上必须能缺席**：`AccountSnapshot` 的七个汇总钱字段由裸 `i128` 改 `Option<i128>`（权益仍
  是恒算得出的 `i128`）。改前 `frozen_raw` 在 `git grep` 下只命中 `crates/qx-protocol/src/lib.rs` 一个文件
  ——生产者数量为零，每条对外快照都在宣布"这个账户没有冻结资金"。`margin`/`realized_pnl`/`unrealized_pnl`/
  `funding` 同样只有读法。现在 `new()` 一律 `None`，稳定 JSON 与线格式印 `null`，`/account/balances`
  （`crates/qx-api/src/lib.rs:1615-1616`）随之从 `map(…)` 改 `and_then(…)`。
- **`None` 与 `Some(0)` 是两份状态**：`state_hash` / `scalar_hash` 共用的那一次标量写入先写存在性标记
  `write_u64(u64::from(value.is_some()))` 再写数值（`crates/qx-protocol/src/lib.rs:235`），八个钱的取法收敛到
  `scalar_money_raw()`（`:220`）唯一一处，稳定 JSON 槽位只由 `scalar_json_values()`（`:242`）填。未算状态再也
  改不出一个"看起来算过"的 0。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/api_snapshot_money_fields.rs`（253 行，4 条，走真实 paper 入口与真实读模型）：结算
  账簿口径、费用与逐笔成交同源、未算缺席、费用合计溢出即拒。
- `crates/qx-protocol/tests/snapshot_single_source.rs` 新增 `uncomputed_money_is_not_the_same_state_as_computed_zero`：
  七个字段逐个跑"哈希不同 → seal/validate 自洽 → 稳定 JSON `null` vs `0` → 两种线格式往返保住区分 →
  `diff`+`apply` 两个方向都是一次真实改动"。`qx-api` 新增端点用例锁 `margin_raw: null`。
- `tools/check_architecture.py:3975` `snapshot_money_honesty_check()` 11 项（静态门禁 245 → **256**）：Option
  形状、取法唯一、两处哈希共用、存在性标记、槽位唯一写法、"权益副本"禁抄（含改类型后**仍可写出**的四种形状）、
  available 取结算现金、费用 checked 加总、五个未算字段在任何产码里都不得出现赋值写法、端点两处 `and_then`、
  六条用例按 `fn NAME(` 取证。`CLI_TEST_FLOOR` 169 → **173**；`maturity/line_budgets.yaml` 重登记
  `qx-protocol/src/lib.rs` 856 → 874、`qx-api/src/lib.rs` 3137 → 3160。
- 门禁自查补强两处（同一轮）：新增的一条 `available` 兜底分支因 `LiveEventPipeline::open` 已拒空结算币种而是
  死分支，删除；"权益副本"禁抄项原来盯的字面量在字段改 `Option<i128>` 后**根本编译不过**，换成可写出的形态，
  `MQ67b` 实测一次点亮 2 条具名项。

### Verified（两轮门禁，数字取自当轮日志）

- `/tmp/qx_q67_gate2.log`（19:46:49 → 19:50:32）：`cargo check --workspace --all-targets` `=0`；`rustfmt --check`
  `=0`；`cargo clippy --all-targets -- -D warnings` 三个 crate 全 `=0`；6 条行为变异 + 3 条静态变异全部
  `MUT_STATE=red` 且各只点亮被测项（`MUT_STATE=unexpected` 0 次），9 次还原全部 `RESTORE_EXACT`；
  设 `QX_PYTHON` 的整树 `722 passed / 0 failed`，不设 `720 passed / 2 failed`（本机无解释器的两条已知 Python
  worker 契约项）；`Ran 43 tests` Python 侧 `OK`；`MUTATION_RESIDUE=` 空。
- 首轮 `/tmp/qx_q67_gate1.log` 唯一非绿项是 `STAGE4_CLIPPY[qx-cli]_EXIT=101`（新增用例里的 3 条 lint），第二轮
  修复后归零。

### Known issues（本轮实测钉出的，交给下一轮）

- **同一份 JSON 的持仓行仍犯同一个错**：`api_service.rs:340-351` 走 Ledger 回退时写死
  `unrealized_pnl_raw: 0, margin_raw: 0`，协议上这两项也是裸 `i128`（TX2 下半截）。
- **CCXT 事实折算把缺字段读成零/多头**：`crates/qx-cli/src/ccxt_facts.rs:106-110` 缺 `side` 默认 `"long"`、
  `:131-140` 缺失或 `null` 的三项保证金/盈亏一律 `Money::ZERO`、`:181-184` 缺时间戳回落 0。
- ~~**CCXT 对账的两半不相交**：`ccxt_reconcile_worker.rs:249`/`:283` 的发现只进事实流（把订单推到
  `OrderStatus::Unknown`），不进报告也不参与健康判定 → 已有单子"结果未知"时 worker 仍标 `Ready`；
  `open_order_issues` 反之只进报告。`binance_reconcile.rs:154-156` 三个覆盖度计数恒 0~~ —— 已由同日的 Q69 关闭（见顶部与 §31）。

## Unreleased — V11 Q66：回测产物声明"跑的是哪一份输入"，这句话由别人重算得出（2026-09-22）

Q1b（可复现性绑定）的第一批，不在 V11 §6 排期内。收口记录见
[docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §27。

### Fixed（回测链路：输入身份是引擎自哈希，摘要里根本没有）

- **`RunManifest.data_fingerprint` 在无数据集绑定时回落成引擎自哈希**：取的是 `report.input_data_hash`
  （`qx-guanxing::bars_digest`：条数 + ts/OHLCV，连 instrument 都不含），"输入身份"这一行答的是"引擎当时看见
  一串数字"。改前实测 `deploy/data` 里那份 OKX 示例 run.json 的 `data_fingerprint` 是 `d1eae507fc0a5d80`，与同
  文件的 `input_event_hash` 一字不差。现在回落取 `"{kind}:{被复核过的指纹}"`，那条示例的
  `data_fingerprint` 变成 `barframe:673fd995f8625b97`（产物文件名随之从 `…OKX-4bdd4398c79c6138` 换到
  `…OKX-5c38b56746b08948`）；旧形状 `format!("{:016x}", report.input_data_hash)` 全仓 0 处。
- **摘要不携带输入身份，而被注册表复核过的那份身份被打印后丢弃**：策略链跑完会拿到
  `DatasetManifest { dataset_id, version, fingerprint }`（`JsonDatasetRegistry::verify` 逐字节校验过），此前只
  印到 stdout。现在它随 `input` 块落进摘要（schema_version 2 → **3**），换一份同形状的输入文件再跑
  `qx report` 会直接拒绝，而不是像此前那样毫无反应。
- **读点唯一，声明只能是读出来的**：`crates/qx-cli/src/backtests/artifacts.rs:28`
  `read_bar_frame_for_backtest` / `:66` `read_depth_frame_for_backtest` 是全仓唯一两处解帧，`:37`
  `barframe_dataset_identity` 把帧喂回 `qx-data` 的 `JsonBarFrameProvider` 取回被复核过的身份并逐列比对；
  `:117` `recompute_declared_backtest_input` 用**同一个读点**重算，`dataset_id` / `dataset_version` /
  `fingerprint` 任一不符即拒（先整体校验声明块，`kind` 不认与缺键是两种不同的拒因）。
  `config_commands.rs:258` 以 `?` 传播复核失败，文本侧 `input_verified=<路径>`、旧 schema 只能印成
  "没有 input 块，输入身份未经核对"（未声明 ≠ 通过）。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/backtest_input_provenance.rs`（286 行，6 条，全部走真实入口）：声明等于重算、跑完
  篡改输入即拒、输入文件消失即拒、同一注册身份不容两种内容、深度链同形、无 `input` 块不算通过。
  `backtest_replay_gate.rs` 把摘要形状钉到 v3。
- `tools/check_architecture.py:2982` `input_provenance_check()` 9 项 + 6 条 `BACKTEST_ENTRY_OWNERS` 归属项
  （静态门禁 230 → 245）：`input` 块只写一次且 schema 为 3、三条链不自己解帧、身份步骤同时具备 Provider 读取
  与列式一致性、复核函数体形状、报告出口传播失败并区分未声明、回落不用引擎自哈希、两档版本号互不相同且字面量
  只在 `artifacts.rs`、六条用例按 `fn NAME(` 取证。
- `CLI_TEST_FLOOR` 163 → 169；`maturity/line_budgets.yaml` 重新登记 `config_commands.rs` 596 → 622。

### Known issues（本轮实测撞到的，交给下一轮）

- 产物文件名只由 `run_id` + `manifest.digest()` 决定，而 `digest()` 覆盖输入与模型、**不含摘要自身的
  schema 版本**。因此"只改摘要形状"的变更会让同一条示例命令在原地撞旧文件：本轮 STAGE8 三条集成用例
  （`builtin_signal_from_config.rs:292`、`fast_backtest_manifest.rs:65` / `:96`）就是如此，报"同一回测回测摘要
  路径已存在不同内容"。把 12 份改动前的本地 v2 产物移到 `%TEMP%\qx_q66_stale_v2_artifacts\` 复跑后
  `716 passed; 0 failed`。闸门本身没做错，缺的是一次真正的决策：摘要世代进文件名，或给示例集一条重 bless 命令。
- 被跟踪的 16 份示例摘要仍停在 `schema_version: 1`、`input` 块 0 份，当前代码在那些路径上不再落盘
  （本轮 `MODIFIED_DEPLOY=0` 是这个意思）—— 与 Q62 记下的
  `tracked_sample_artifacts_are_validated_by_nothing` 同一条，且随每轮变宽。

## Unreleased — V11 Q63：规格来源改成"实际读成的形状"，不再按"有没有传路径"印（2026-09-22）

同样不在 V11 §6 排期内，是"交易链路和回测链路还有哪些潜在问题，全部修复"这条实测指令的回测链路第六批
（FN6）。收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §26。

### Fixed（回测链路：产物上那行规格来源声明了错误的解析口径）

- **`instrument_spec_version` 问的是"有没有传 `--market-spec`"**：传了路径一律印 `ccxt-market-spec-v1`，于是
  仓库自己生成的产品规格 `deploy/qianxing.binance.spot.spec.json`（实测其键里带 `base_currency`，A 股那份带的是
  `base`）在产物里被写成 CCXT 形状。规格内容不进 `model_fingerprint`，`contract_size` 与三项定点精度也不进，
  这个字段是唯一交代"按哪种形状读的"的地方 —— 两种形状、两个解析分支顶着一个标签，等于声明两套记账口径是
  一回事。可达路径就是首屏命令：`init_project.rs:186` 给非 A 股 profile 印的那条回测命令带的正是这份产品形状
  文件。
- **判据与标签问同一个问题**：`crates/qx-cli/src/market_spec.rs:9` `market_value_is_product_spec`（唯一判据
  `base_currency` 存在）与 `:18` `market_spec_source_label` 同一处，`:26`–`:28` 三档常量各一名；`:48`
  `market_spec_from_value` 的解析分支复用同一个谓词，标签与实际解析不可能各答一份形状。
- **来源只在 loader 问一次**：`backtests/mod.rs:84` `market_spec_with_margin` 返回
  `MarketSpecLoad { spec, margin, source }`（结构体住在 `market_spec.rs:34`），`:108` 是全仓唯一一次
  `market_spec_source_label` 取用，"没给规格"那一档由 loader 独占。策略链与深度链以字段简写原样写进
  `RunManifestIdentity`（`single_strategy.rs:162`、`depth.rs:182`），不落摘要的内置链把它印成
  `[Builtin · Execution] … spec_source={}`（`single_strategy.rs:431`）。多腿链 `source: _`：一条命令两份帧、
  两条腿各一规格路径，产物上没有"这一份规格"可标，本轮不给它编一个。
- **示例集为什么看不出这条缺陷（如实记录）**：`deploy/data` 下带这个字段的 21 份 run.json 逐份读过 —— 被跟踪的
  18 份里 3 份 `ccxt-market-spec-v1` 全部来自真 CCXT 形状的 A 股规格（换口径前后同一标签）、15 份
  `default-instrument-spec-v1`（没传规格）。**没有一份 blessed 产物走过产品形状**，所以
  `MODIFIED_DEPLOY=0` 不是"改得没影响"，而是示例从没跑到这一档。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/market_spec_single_source.rs`（305 行）：`published_spec_source` 从落盘摘要顺着
  `run_manifest` 读回那一行；策略链与深度链各一条用例走**真实入口**跑三档可达输入，逐档断言标签并断言
  "三种输入给出三种来源"。
- `tools/check_architecture.py` 新增 `market_spec_source_check` 9 项（静态门禁 221 → 230）：谓词/标签各唯一且
  标签复用谓词、`base_currency` 判据只出现一次、三档常量取值互不相同、来源标签只在 loader 单点被问、
  兜底档由 loader 独占、两条链 **`RunManifestIdentity` 块内**只可能是 loader 返回的那个变量（新增
  `_identity_block` 按花括号配对取块）、builtin 那行占位符与实参一一对应、两条行为用例按 `fn NAME(` 取证。
- `CLI_TEST_FLOOR` 161 → 163。

门禁：`/tmp/qx_q63_gate2.log` 单轮通过 —— `cargo check` / `rustfmt --check` / `clippy -D warnings` 全 0、
行数棘轮 diff 为 0；3 条行为变异各红 1–2 条用例（MQ63a/MQ63b 同时各点亮 1 条具名静态项），7 条静态变异各
点亮 1 条（`MUT_STATE=unexpected` 0 次），10 次还原全 `RESTORE_EXACT`、`CONCURRENT_TREE_CHANGE` 0 次、
`MUTATION_RESIDUE` 空；全量测试设 `QX_PYTHON` `710 passed / 0 failed`（exit 0），不设
`708 passed / 2 failed`（本机无解释器的两条已知 Python worker 契约项）；Python 侧 `Ran 43 tests … OK
(skipped=1)`。第一轮日志作废：那条"链必须原样写 loader 来源"的判据被解构行 `source: instrument_spec_version,`
满足了子串，两个缺陷本体变异静态侧全绿 —— 作用域收到 identity 块内后同一对变异才咬住（§26.3）。MQ63c（产品
形状那一支接错常量）**静态全绿、只有行为用例能问出来**，§26.5 记了这条边界。另记一项环境事实：本机 venv 缺
`pyproject.toml` 已声明的 `tzdata`，导致 13 条 A 股 Python 用例长期被一个导入错误挡在门外（原基线读到
`Ran 30 tests … errors=1`）。

本轮未使用任何外部服务或凭据，`sandbox_tested` 全部保持 `false`。

## Unreleased — V11 Q62：重放改成真会失败的重新驱动，产物不再声明恒等（2026-09-22）

同样不在 V11 §6 排期内，是"交易链路和回测链路还有哪些潜在问题，全部修复"这条实测指令的回测链路第五批
（FN5）。收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §25。

### Fixed（回测链路：产物里那条"重放校验"在任何输入下都不可能失败）

- **`replay_hash` 是把同一段事件切片再哈希一遍**：`ReplayVerifier::rebuild_from` 与 `result_hash` 来自
  同一批事件，于是两者恒等。`deploy/data` 下被跟踪的摘要里有 16 份仍带着这个键，逐份读进来比对
  **16/16 相等** —— 一份都不例外。写在产物上像"结果被独立重算过一次"，实际什么都没问。
- **重放只有一个内核，做的是重新驱动**：`crates/qx-core/src/sourcing.rs:325` 的 `replay` 逐条
  `append_checked`（重复 seq、乱序、缺因果字段当场报错）+ 把 `LedgerApplied` 事实重新入账 + 整本
  `validate()`；`:349 replay_facts` / `:359 rebuild_ledger` 都是它的投影，`:368 verify(events, &Ledger)`
  再加一条真会失败的判据（重放账簿条数 ≠ 运行账簿条数即报 `replayed=/run=`）。`rebuild_from` 与
  `replay_hash` 全仓删除，门禁按指纹钉住不得复活。
- **闸门排在结果出口**：`qx-xingban` 两条链的 `run()` 各以
  `ReplayVerifier::verify(report.event_log.events(), &report.ledger)?;` 收尾（`backtest.rs:1049`、
  `orderbook_backtest.rs:742`），重放不过就不返回报告；`qx-cli` 落盘侧再问一次且问在写任何工件之前
  （`backtests/artifacts.rs:107`，首个写文件在 :164），另加 `replay.log_digest != result_hash` 即拒。
- **摘要形状换成可核对的三个事实**：`schema_version` 升到 2，`replay_hash` 键删除，代之以
  `"replay": { log_digest, events, ledger_entries, run_ledger_entries }`；`qx-cli report` 打印
  `replay_log_digest=` 与 `replay_events=` / `replay_ledger_entries=n/m`，读者能自己数而不必接受一个哈希。
- **顺带修掉深度链一条从没被问过的缺陷**：订单簿链的 `EventLog` 此前没做过规范序校验，接上引擎闸门后
  当场报错 —— 迟到成交回报（口径 `match-previous-submit-current`，因果上来自更早的提交）与账簿事实排在
  `Priority::APPLY`，会让同一时间戳上的策略 `COMMAND` 因果倒退。改占 `FEEDBACK` 槽
  （`orderbook_backtest.rs:434`、`:450`），初始入金排 `TIMER`（`:401`）。改的是引擎发出的事实序。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/backtest_replay_gate.rs`（162 行 / 3 条进程内用例）：摘要写的就是它查过的三个
  事实（并断言 `replay_hash` 键已消失）；账簿凭空多一条无事实支撑的条目 → 拒落盘且不产生任何文件；
  事实流同戳倒退 → 同样拒落盘。
- `backtest.rs` 的 `assert_replay_matches` helper（定义 + 两处调用）把原先两处"哈希相等"的自证换成逐条
  比对重放账簿，并在因果用例里加一条"凭空多记必须报错"的反向证据；`ashare_pit_asof.rs` 改走同一内核。
- `tools/check_architecture.py` 新增 `replay_kernel_check` 11 项（静态门禁 210 → 221）：内核四函数各自
  唯一、内核体三步齐、报告侧条数判据在位、`rebuild_from` / `"replay_hash":` / `fn replay_hash(` 指纹为 0、
  两条链报告出口各问一次且以 `?;` 向上抛、落盘点问在首个写文件之前、摘要 schema 与三个 replay 键、
  深度链三个因果槽位计数、两侧用例名按 `fn NAME(` 取证。
- `CLI_TEST_FLOOR` 158 → 161；`sourcing.rs`（567 行）新登记进行数棘轮，`config_commands.rs` 590→596、
  `strategy_host.rs` 812→801 一并快照。

门禁：`/tmp/qx_q62_gate3.log` 单轮通过 —— `cargo check` / `rustfmt --check` / `clippy -D warnings` 全 0；
11 次门禁变异各点亮 1 条具名条目（`MUT_STATE=unexpected` 0 次），4 条行为变异各红 1–3 条用例、
`RESTORE_EXACT` 11 次、`CONCURRENT_TREE_CHANGE` 0 次、`MUTATION_RESIDUE` 空；全量测试设 `QX_PYTHON`
`708 passed / 0 failed`（exit 0），不设 `706 passed / 2 failed`（本机无解释器的两条已知 Python worker 契约项）。
MQ62e（内核整本 `validate`）、MQ62h（入金槽位）、GA62e（`.ok();` 吞失败）三项**行为不可证伪、只由文本钉住**，
§25.3 记了原因与补法。

本轮未使用任何外部服务或凭据，`sandbox_tested` 全部保持 `false`。

## Unreleased — V11 Q65：A 股段在五个提交入口当场拒，拒在任何副作用之前（2026-09-22）

同样不在 V11 §6 排期内，是"交易链路和回测链路还有哪些潜在问题，全部修复"这条实测指令的交易链路第一批
（TX1）。收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §24。

### Fixed（交易链路：`strategy.ashare_rules_path` 在提交侧有声明、没有读者）

- **五个会提交新订单的入口此前都收下 A 股段再按无制度下单**：`paper-worker`（含 `paper-e2e` /
  `paper-check`）、`paper-submit-order`、`binance-worker`、`binance-submit-order`、`ccxt-worker` 走
  `read_runtime_config` 读得到整段配置，却没有任何一处读它 —— 订单照样 T+0、照样可以 150 股买入，
  产物与 stdout 上不显示这段被认了还是被丢了。Q61 只把回测侧那一半收干净了。
- **明确选择"整段拒"而不是"只接费率"**：费率确实有现成挂钩点（`execution_cost_binding_from_config`），
  但只接费率会让产物印着"A 股佣金"而制度项全空，比整段不认更容易骗人。缺的那一项写进拒绝文案
  （`ASHARE_SUBMIT_GAP`：T+1 需要"今日买入"的结算状态、整手与涨跌停要按板块和昨收逐单判定），
  并指向当前唯一能认这段的入口 `strategy backtest`。
- **闸门只有一处定义**：`crates/qx-cli/src/runtime_wiring.rs` 的 `reject_ashare_rules_on_submit_path`
  把 `--config` 转交回测侧唯一的 `reject_ashare_rules_config`，判据是"这条路径会不会把一笔新订单送进
  Venue" —— `Execution` 与 `SpreadRecovery` 算（补腿本身就是新订单，V10 Q57），Strategy / MarketData /
  UserStream / Reconciler / Scheduler / Api 不算，否则"研究用配置"连启动都做不到。
- **顺序就是危害边界**：`run_paper_submit_order` 的闸门是函数第一条语句，排在读配置与读命令之前；
  `binance-submit-order` 排在读命令之前；`ccxt-worker` 排在解析 CCXT 配置文件（再往后要起 Python 进程）之前。
- **命令面帮助改口**：五条提交入口各自写明"配了 `strategy.ashare_rules_path` 即在任何副作用之前拒绝"，
  并写明行情/策略角色不受影响。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/ashare_submit_guard.rs`（198 行，5 条进程内用例，走真实入口函数）：三个一次性
  提交入口 + `paper-worker` 的拒绝，每条都配"同一夹具只摘掉那三个 A 股键"的基线，证明红的原因是键而不是
  夹具；断言里点名"报错不能是读命令那一句"与"被拒入口不得留下 `data` 目录"，把副作用顺序也钉住。
- `only_roles_that_submit_orders_meet_the_ashare_gate`：2 个提交角色必须被拒、6 个非提交角色必须放行 ——
  闸门范围漂移的两个方向各有一条用例红（漏 `SpreadRecovery` 与扩到所有角色都落到这一条上）。
- `tools/check_architecture.py` 的 `ashare_submit_guard_check` 新增 9 项（静态门禁 201 → 210 项）：闸门与
  角色判定唯一定义、复用回测侧文案且点名 T+1、五个入口的调用点逐一计数、`venue_runtime` 不得自己装配
  A 股规则或费率、五条行为用例按 `fn NAME(` 取证、帮助文本口径在位。Q61 的两项锚点同轮升级为 `fn NAME(`。
- `CLI_TEST_FLOOR` 由 59 抬到实测的 158 条（地板停在历史值等于没有防守）。

门禁：`/tmp/qx_q65_gate.log` 单轮通过 —— `cargo check` / `rustfmt --check` / `clippy -D warnings` 全 0，
七个行为变异各红 1 条用例、十个门禁变异各点亮 1 条具名条目（`MUT_STATE=unexpected` 0 次），
`RESTORE_EXACT` 17 次且 `MUTATION_RESIDUE` 为空，全量 `cargo test --workspace --all-targets`
设 `QX_PYTHON` 时 `702 passed / 0 failed`。本轮未使用任何外部服务或凭据，`sandbox_tested` 保持 `false`。

## Unreleased — V11 Q64：四个内置信号参数五链同源，复检排在印口径之前（2026-09-22）

同样不在 V11 §6 排期内，是"回测链路还有哪些潜在问题，全部修复"这条实测指令的第五批（FN7）。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §23。

### Fixed（回测 + 交易链路：`--config` 的 `strategy.builtin_*` 有声明、只有一个读者）

- **四条回测链此前各自走 `BuiltinStrategyConfig::new` 的 5/20/14/100 写死默认**：`backtest builtin` /
  `ccxt-builtin`、`backtest book`、`backtest multi-builtin` 都收下 `--config`，也真读它做风控与成本，
  唯独 `builtin_fast_window` / `builtin_slow_window` / `builtin_period` / `builtin_threshold_bps` 没有
  落点 —— 同一份配置在 `strategy backtest` 与内置链上跑的是两套信号，stdout 形状还一样。
- **赋值收成一个点**（`crates/qx-cli/src/strategy_binding.rs` 的 `builtin_signal_source` +
  `apply_builtin_signal_overrides`）：全仓没有第二处 `config.fast_window = …`。包装函数
  `apply_configured_builtin_signal`（`single_strategy.rs`）被四条链共问一次。
- **覆盖之后必须复检，且复检要早于生效口径那一行**：运行时体检只比较"两个都给齐"的窗口键，
  单边覆盖（只写 `builtin_fast_window: 25`、慢窗留默认 20）只有并入默认值之后才成为非法组合。
  复检现在由 `apply_configured_builtin_signal` 与 `builtin_strategy_config_from_runtime` 各做一次，
  报错文案点名配置路径与覆盖后的四项值；非法配置一律 `exit=2` 且 stdout 不出现 `[X · Signal]` ——
  先宣告再报错等于往 stdout 写了一套没跑过的参数。
- **四条链各印一行生效口径**：`[Strategy · Signal]` / `[Builtin · Signal]` / `[Depth · Signal]` /
  `[Multi · Signal]`，文案由唯一的 `builtin_signal_note` 生成（`source=config|builtin-default` + 四项值），
  读者不用猜本轮到底用的是配置还是默认。
- **paper/live worker 同读**：`invoke_builtin_strategy` 走的正是 `builtin_strategy_config_from_runtime`，
  本轮为交易链路补上进程内用例，"配置在 worker 侧也生效"不再只由回测链证明。
- **命令面帮助改口**：四个信号键在 `backtest builtin` / `book` / `multi-builtin` 的说明里写明在本链生效、
  并会印出生效口径。

### Added（用例与门禁）

- `crates/qx-cli/tests/builtin_signal_from_config.rs`（442 行，7 条真实子进程用例）：窗口对换出成交、
  周期与阈值各自三档给出三个不同 `result_hash`、非法窗口组合 fail-closed、两条 Bar 链同源、深度档与
  多腿链同样认账，以及新增的单边覆盖复检用例（同时跑三个入口，断言报错文案与"口径行未印出"）。
- `crates/qx-cli/src/tests/strategy_worker_entries.rs` 的 `builtin_worker_reads_the_declared_signal_parameters`：
  在克隆配置上自设 2/3/9/50 与"四项全缺回落 5/20/14/100"两档 —— 示例配置写的恰好就是默认值，
  拿它做"配置值"对比的用例在实现完全没读配置时也会通过。
- `tools/check_architecture.py` 的 `builtin_signal_check` 新增 15 项（静态门禁 186 → 201 项）：赋值点全仓
  唯一、四条链各问过它、复检形状钉在 `config.validate()`、判词单一定义 + 四处调用、帮助点名四个键、
  七个行为用例按 `fn NAME(` 取证、交易链路侧一条进程内用例在位。

## Unreleased — V11 Q61：A 股段在四条回测链上要么生效、要么当场拒（2026-09-22）

同样不在 V11 §6 排期内，是"回测链路还有哪些潜在问题，全部修复"这条实测指令的第四批（FN4）。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §22。

### Fixed（回测链路：`--config` 里的 A 股段有声明、无读者）

- **`backtest builtin` / `backtest ccxt-builtin` 收下 `--config` 却不读
  `strategy.ashare_rules_path|ashare_actions_path|ashare_calendar_path`**：同一份配置在
  `strategy backtest` 上按 T+1、整手、涨跌停与 A 股佣金跑，在内置链上退成"无交易制度 +
  maker/taker 兜底费率"，两边 stdout 形状完全相同。
- **读法收进单一模块**（新文件 `crates/qx-cli/src/backtests/ashare_binding.rs`，118 行）：
  配对（只给 actions/calendar 即错）、fail-closed（配了快照必须 `enabled: true`，JSON 非法或
  `validate()` 不过整轮失败）、规则与 `AShareFeeModel` 成对进入装配。两条 Bar 链各自绑定
  （策略链走已解析的三元组、内置链走 `--config` 路径），成本来源改写为
  `source=ashare-rules:{路径}`，并新增 `[Builtin · A 股规则] t_plus_one=… lot_size=… price_tick=…
  commission_bp=… stamp_duty_bp=… transfer_fee_bp=… path=…` 一行。
- **承不了这个段的两条链当场拒绝**（`reject_ashare_rules_config`）：`backtest book` 的 L1/L2
  盘口引擎没有 T+1/整手/涨跌停挂钩点；`backtest multi-builtin` 一份策略段套不住两条腿各自的交易制度。
- **命令面帮助改口**：`backtest builtin` / `ccxt-builtin` 的 `[--config <runtime.json>]` 写明读取的是
  策略口径、配了就必须生效；`book` / `multi-builtin` 写明 A 股段在这里没有落点、配置了即整轮拒绝。
- **门禁脚本自身的死代码**：`check_architecture.py` 的"四个文件状态存储各只有一份定义"（`# (e)`）
  自 V10 P1c 起写在 `module_declarations()` 的 `return` 之后，语法合法而永不执行。搬回
  `storage_retry_check` 后静态门禁由 182 项变 186 项，四项当轮即 PASS —— 半个月里这条约束无人看守。
- **两处挂载判定由子串包含改为整行正则**（`mount_pair_present`，Phase 4p/4s 同口径）：把
  `pub(crate) use x::*;` 注释掉过去照样绿。
- **A 股快照解析点唯一性的作用域扩到 `crates/*/src/**`**（排除用例盘）：原来只扫 `backtests/` 目录，
  抄第二份读法到目录外不会红。

### Added（用例与门禁）

- `crates/qx-cli/tests/ashare_builtin_backtest.rs`（新文件，300 行，5 条真实子进程用例）：
  整手闸门改变结果、A 股佣金单独改变结果（成交笔数与拒单数全同）、`enabled:false` 与
  actions/calendar 配对缺失两处 fail-closed、`book` 与 `multi-builtin` 两处拒绝、
  `strategy backtest` 报同一条配对规则（证明校验确实住在共用加载器里）。
- `tools/check_architecture.py` 的 `ashare_backtest_binding_check` 6 项：解析点全仓唯一、
  `enabled` 判定单点且文案钉住、两条 Bar 链各绑规则与费率（`binding.fee` / `assembly.fee = ` 各 2 处）、
  `depth.rs` 与 `multi_builtin.rs` 各一处当场拒绝、Q61 行为用例逐条在位；新模块与新入口进登记表。

### Validation（本轮日志实测，`/tmp/qx_q61_gate2.log`）

- `cargo check --workspace --all-targets` `STAGE1_CHECK_EXIT=0`；`cargo fmt --all --check`
  `STAGE2_FMT_EXIT=0`；`cargo clippy -p qx-cli --all-targets` `CLIPPY_WARNING_LINES=0`。
- `cargo test -p qx-cli --test ashare_builtin_backtest` `5 passed; 0 failed`；
  `QX_PYTHON=… cargo test -p qx-cli --bins` `110 passed; 0 failed`。
- 整仓测试：设 `QX_PYTHON` 时 `WORKSPACE_WITH_QX_PYTHON_EXIT=0`、
  `SUM_with_qxpython_passed=689 failed=0`、`OK_SUITES=75`；不设时 `WORKSPACE_EXIT=101`、
  `SUM_without_qxpython_passed=168 failed=2`（本机 WindowsApps 占位桩那两条必然失败项）。
- 静态门禁：基线 `ARCH_FAIL_LINES=0`，收口 `FINAL_ARCH_EXIT=0`、
  `架构不变量自检全部通过 ✓（186 项）`。
- 变异成对（行为级 6 项，各红后还原即 `5 passed; 0 failed`）：MQ61a 内置链不读 A 股段 → 4 条失败；
  MQ61b 只接规则不接费率 → 2 条；MQ61c 深度档不拒 → 2 条；MQ61d 多腿链不拒 → 2 条；
  MQ61e `enabled:false` 放行 → 2 条；MQ61f 配对校验失效 → 3 条（覆盖两条 Bar 链）。
- 变异成对（静态门禁级 9 项，每项 `MUT_STATE=red` 且只点亮一项）：G1 解析点复制进同目录、
  G1b 复制进 `dataset_commands.rs`（目录外，本轮新增的作用域才咬得住）；G2 内置链不绑定、
  G7 费率件不装配，同点"两条 Bar 回测链各自绑定"；G3/G4 两条链的拒绝调用消失；G5/G5b 任一 Q61
  用例改名点证据项；G6 注释掉 `pub(crate) use ashare_binding::*;` 点挂载配对。
  G8 探针确认存储定义检查已回到活路径（`STORE_DEFINITION_ITEMS=4`、`G8_STORE_CHECK_LIVE=True`、
  `G8_DEAD_CODE_IN_HELPER=False`）。
- 收口：`PRISTINE_OK final`、每次还原 `RESTORE_EXACT`、`MUTATION_RESIDUE=` 空、`BUILD_final_EXIT=0`。
- 行数：`ashare_binding.rs=118`、`single_strategy.rs=435`、`depth.rs=272`、`multi_builtin.rs=478`、
  `cli_help.rs=176`、`tests/ashare_builtin_backtest.rs=300`、`check_architecture.py=2563`；
  `MODIFIED_TRACKED_FILES=32` 是 Q54–Q61 未提交成果的合计。

### Known（本轮记录、未修的相邻缺陷）

- **Q64**：`--config` 的 `strategy.builtin_*` 信号参数同样在内置链没人读（写死 5/20/14/100），
  同一份配置在两条 Bar 链上给出不同信号（实测 `fills=1 / 3efef64ed899097a` 对
  `fills=0 / 216c9e646a571444`）。与 Q61 同族，本轮刻意不同批修，以免"哪一处让结果变了"重新变模糊。
- **Q65**：A 股制度在 paper/live 交易链路无任何落点（反序列化点全仓 1 处且在回测侧、
  `validate_order` 只被回测引擎调用、`venue_runtime` / `runtime_wiring` / `qx-execution` 里 `ashare`
  零命中）。配置面依旧"配了不认"，下一轮要么接线要么按同一口径当场拒。

本轮未使用任何外部服务或凭据，`sandbox_tested` 全部保持 `false`。


## Unreleased — V11 Q60：A 股涨跌停锚到上一交易日收价，封死的板不再成交（2026-09-22）

同样不在 V11 §6 排期内，是"回测链路还有哪些潜在问题，全部修复"这条实测指令的第三批（FN3）。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §21。

### Fixed（回测链路：涨跌停的锚锚错了对象）

- **`AshareRuleConfig::previous_close` 拿"上一根 Bar"当昨收**（HEAD 版 `ashare/trading.rs:28-35`：
  覆盖表 miss 就 `index.checked_sub(1)`）。日线数据上上一根恰好是昨天，缺陷因此不可见；
  分钟线上进来的是同一个交易日里 5 分钟前的收价 —— ±10% 的板被窄化成"最近 5 分钟 ±10%"，
  于是**涨停封死的那根分钟线照样买入成交**、跌停封死的照样卖出。
- **改后的锚（`ashare/trading.rs:40-51`）**：`previous_close_raw` 仍按当前 Bar 的 ts 优先
  （除权除息日的昨收只能由数据侧给，仓库内不产生该映射 —— `deploy/qianxing.ashare.rules.json:17`
  里它就是空的），否则向前扫到第一根跨日的 Bar，即上一交易日的**最后一根**（`.rev()` +
  `Self::day_key(bar.ts) != day`）；整个历史里没有上一交易日时返回 `None`，`blocks_fill` 因此不判板 ——
  宁可不判，也不能拿当天的价格当昨收，那会把板算成 ±0%。
- **引擎侧不改**：唯一调用点仍是 `backtest.rs:504`（`rules.previous_close(bars, i)`），
  本轮把它钉成"只有一个定义点、一个调用点"，防止有人在高保真链旁边再抄一份"上一根 Bar"。

### Added（门禁与用例）

- `crates/qx-xingban/tests/ashare_limit_anchor.rs`（新文件，2 条）：
  `a_sealed_intraday_limit_up_blocks_the_fill_when_anchored_to_the_session_close` ——
  昨天 14:55 收在 10.00、今天封死在涨停价 11.00 的那根分钟线上，买入单必须不成交且现金额不动；
  `anchoring_to_the_previous_bar_would_fill_on_that_same_board` —— 同一份行情、同一根封板线，
  只把锚换成"上一根 Bar 的收价 10.50"（经 `previous_close_raw` 注入，即缺陷口径），
  这单就在 11.00 成交。两条合起来才是"结果确实变化"的证据：光有前一条，改坏锚也可能被别的口径兜住。
- `crates/qx-xingban/src/ashare/tests.rs` 的 `limit_band_anchors_to_the_previous_session_close_not_the_previous_bar`：
  逐 Bar 验锚本身（首两根 `None`、跨日两根都锚 10.00），并证明按旧口径推出的 11.55 那条板
  不再拦得住同一笔买入。
- `tools/check_architecture.py` 的 `ashare_limit_anchor_check` 新增 4 项（169 → 173）：锚只有一个
  定义点与一个引擎调用点、覆盖表优先 + 跨日取该日最后一根、锚的口径与"覆盖表才是除权除息出口"
  写进代码文档、上述行为用例与单元测试在位。

### Validation（本轮日志实测，`/tmp/qx_q60_gate.log`）

- `cargo check --workspace --all-targets` `STAGE1_CHECK_EXIT=0`；`cargo fmt --all --check` `STAGE2_FMT_EXIT=0`；
  `cargo clippy --workspace --all-targets -- -D warnings` `STAGE3_CLIPPY_EXIT=0`、`CLIPPY_WARNING_LINES=0`。
- `cargo test -p qx-xingban --all-targets`：`CARGO_TEST_xingban_EXIT=0`、`SUM_xingban_passed=87 failed=0`
  （其中新用例 `2 passed` + 单元 `1 passed`；邻近的 A 股 PIT 链 `ashare_pit_asof` 仍 `3 passed`，
  说明锚的改动没有改口 PIT 语义）。
- 整仓测试：未设 `QX_PYTHON` 时 `CARGO_TEST_workspace_EXIT=101`、`SUM_workspace_passed=168 failed=2`
  （断言 `WORKSPACE_FAILED_WITHOUT_QX_PYTHON=2`，即本机 WindowsApps 占位桩那两条必然失败项）；
  设解释器后 `CARGO_TEST_WORKSPACE_WITH_PYTHON_EXIT=0`、`SUM_workspace_with_python_passed=684 failed=0`、
  `OK_SUITES=54`；含 doc-test `CARGO_TEST_WITH_DOC_EXIT=0`、`SUM_with_doc_passed=684 failed=0`。
- 静态门禁：`STAGE5_ARCH_EXIT=0`、`架构不变量自检全部通过 ✓（173 项）`、`STAGE5_ARCH_FAIL_LINES=0`、
  `STAGE5_ARCH_PASS_LINES=173`。
- 变异成对（cargo 级）：MQ60a（锚退回上一根 Bar）红在集成 `ashare_limit_anchor.rs:143` 与单元
  `ashare/tests.rs:361`，`1 passed; 1 failed` / `0 passed; 1 failed`；MQ60b（覆盖表失效）
  红在**另一条**集成用例 `:163` 与单元 `:377`；MQ60c（去掉 `.rev()`，锚落到整段历史的第一根）
  红在 `:143` 与单元 `:362`。三处还原后各自回到 `2 passed; 0 failed` 与 `1 passed; 0 failed`。
- 变异成对（静态门禁级）：G1 删"不复权"口径文案 → `[FAIL] 锚的口径与…写进代码文档`；
  G2 删 `.rev()`、G3 把覆盖表按错的 key 取 → 双双点亮 `昨收锚按上一交易日推导`；
  G4 在引擎里再抄一份锚 → `涨跌停的昨收锚只有一个定义点与一个引擎调用点 — 定义 1 处、引擎调用 2 处`；
  G5/G6 分别重命名集成/单元用例 → 点亮 `同一根封板线在两种锚下结果不同…`。
  六项 `ARCH_MUTATED_G*_EXIT=1` 且各自只亮一项（不牵连行数棘轮），`ARCH_RESTORED_G*_EXIT=0` ×6。
- 收口：`PRISTINE_OK` 20 处逐个 `cmp` 通过、`MUTATION_RESIDUE=none`、`FINAL_ARCH_EXIT=0`、
  `BUILD_final_EXIT=0`。
- 行数：`trading.rs=168`、`ashare.rs=1202`（钉在预算上，本轮把字段文档压成一行才没有抬棘轮）、
  `src/ashare/tests.rs=378`、`tests/ashare_limit_anchor.rs=165`；登记预算 `HEAD 37 项 / 46411` →
  `本轮 36 项 / 45573`（未新增登记项）。`MODIFIED_TRACKED_FILES=31` 是 Q54–Q60 未提交成果的合计。

本轮未使用任何外部服务或凭据，`sandbox_tested` 全部保持 `false`。

## Unreleased — V11 Q58：多腿回测的规格闸门换成真会红的那一份，保证金/资金费只向衍生品腿计提（2026-09-22）

同样不在 V11 §6 排期内，是"回测链路还有哪些潜在问题，全部修复"这条实测指令的第一批（FN1+FN2 合并一轮）。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §20。

### Fixed（回测链路：`multi-builtin` 的记账前提没人守）

- **原有守卫是死代码**：`multi_builtin.rs`（HEAD 版 :133-139）判的是"主腿 spec 是衍生品 **且**
  `primary_spec_path` 没给"，而 spec 只可能从那个 path 解析出来 —— 条件永不成立。于是
  `--funding-bps 25` 一条 spec 都不给也照样跑通：名义额按现货乘数 1 记账、保证金与资金费
  静默记 0，而 stdout 与产物里硬编码的 `margin_model=realized-initial-margin-leverage-1`
  还在宣称本轮按真实保证金记了账。
- **现货腿被按全额名义额记保证金**：`multi_leg_leg_margin` 只要 `leg.spec` 是 `Some` 就计费，
  不看产品形态。Bar 引擎对现货用 `NoMargin`（衍生品规格才有 `leverage_tiers`），归因侧却给
  现货腿再记一笔等于全额现金的保证金 —— 同一笔现金算两次，`margin_peak_raw` 与引擎口径直接矛盾。
- **资金费落到现货腿 = 造一笔不存在的成本**：`multi_leg_leg_buckets` 同样只看"有没有 spec"。
- **新的规格闸门 `multi_leg_spec_guard`（`backtests/leg_funding.rs:82-130`）四条判据，先于任何腿级
  撮合**（`multi_builtin.rs:69-78`，腿级 `run_leg` 在 :204）：① `--config` 声明衍生品却没给主腿
  spec 直接拒；② `--funding-bps` 非零时两条腿都必须带 spec（缺 spec 既算不出金额也判不出形态）；
  ③ 两条腿都不是衍生品时拒绝计提资金费；④ 其余组合放行。产品形态的第二个事实来源由
  `configured_instrument_product`（同文件 :66-80）读 `strategy.product`，与 `backtest_risk_binding`
  / `configured_fill_model` 同构：给了 `--config` 就必须认它。
- **计费只向衍生品腿**：`multi_leg.rs:191-195`（资金费）与 `:241-244`（保证金）都改成
  `leg.spec.filter(|spec| spec.product.is_derivative())`，其余腿记 0；衍生品缺 spec 那种
  组合已在闸门处被拒，所以这里不会再静默把衍生品腿的钱记成 0。
- **产物与 stdout 如实披露**：`margin_model` 按规格真实推导（`multi_builtin.rs:153-161`，
  有衍生品腿规格 → `realized-initial-margin-leverage-1`，否则 `none-no-derivative-leg-spec`），
  stdout 的 Attribution 行带上它（:366），产物新增 `market_specs`（逐腿 spec 路径来源，:415-417）
  与 `margin_model`（:419），`assumptions` 里的资金费/保证金口径同步改写。`cli_help.rs` 的
  `multi-builtin` 说明补上这条命令面契约。

### Added（门禁与用例）

- `crates/qx-cli/tests/multi_leg_attribution.rs` 四个 Q58 行为用例：
  `funding_without_leg_spec_is_refused_before_matching`（缺规格先拒、且不出现任何腿级输出行）、
  `funding_on_spot_only_legs_is_refused`（全现货要求资金费被拒，文案点名两条腿的产品形态）、
  `only_derivative_legs_bear_margin_and_funding`（混合规格：现货腿保证金与资金费列合计为 0、
  衍生品腿资金费合计等于产物总额、`margin_model` 与 `market_specs` 来源如实）、
  `declared_derivative_product_without_primary_spec_is_refused`（配置声明 perpetual 而无主腿规格）。
- `tools/check_architecture.py` 的 `multi_leg_honesty_check` 新增 5 项（164 → 169）：闸门覆盖两类
  非法组合、闸门调用点先于 `run_leg(`、腿级计费的衍生品过滤与 0 记账、产物披露规格来源且
  `margin_model` 由规格推导、四种规格组合各有行为用例。
- 既有回测用例中被本轮口径改写的部分：六条 `--funding-bps 25` 却不给任何 spec 的调用改成 0
  （它们验的是成本/风险/成交溯源，不是资金费），`backtest_entries.rs` 的多腿入口用例补
  `market_specs` 两项为 `null`、`margin_model=none-no-derivative-leg-spec`、
  `margin_peak_raw=0`、`funding_raw=0` 的断言，`vetoed_leg_never_pairs...` 补齐两条 swap 规格
  —— 它声明 `product=perpetual` 且 `allow_short`，现货口径在配置校验期就被拒，原本就是不可能
  无规格跑通的组合。

### Validation（本轮日志实测，`/tmp/qx_q58_gate.log`）

- `cargo check --workspace --all-targets` `STAGE1_CHECK_EXIT=0`；`cargo fmt --all --check` `STAGE2_FMT_EXIT=0`；
  `cargo clippy --workspace --all-targets -- -D warnings` `STAGE3_CLIPPY_EXIT=0`、`CLIPPY_WARNING_LINES=0`。
- 整仓测试：未设 `QX_PYTHON` 时 `CARGO_TEST_workspace_EXIT=101`、`SUM_workspace_passed=168 failed=2`
  （断言 `WORKSPACE_FAILED_WITHOUT_QX_PYTHON=2`）；设解释器后 `CARGO_TEST_WORKSPACE_WITH_PYTHON_EXIT=0`、
  `SUM_workspace_with_python_passed=681 failed=0`、`OK_SUITES=53`；含 doc-test `CARGO_TEST_WITH_DOC_EXIT=0`、
  `SUM_with_doc_passed=681 failed=0`。
- 静态门禁：`STAGE5_ARCH_EXIT=0`、`架构不变量自检全部通过 ✓（169 项）`、`STAGE5_ARCH_FAIL_LINES=0`、
  `STAGE5_ARCH_PASS_LINES=169`。
- 变异成对（cargo 级，整跑 `--test multi_leg_attribution` 全 10 条）：MQ58a（现货腿也记保证金）
  红在 `multi_leg_attribution.rs:653`、`9 passed; 1 failed`；MQ58b（现货腿也计提资金费）红在 `:654`；
  MQ58c（`funding_bps >= 0` 让两条资金费判据重新变死代码）一次点亮两条用例、`8 passed; 2 failed`
  （`:577` 与 `:616`）；MQ58d（声明侧判据永不成立）红在 `:703`；MQ58e（`margin_model` 退回常量）
  红在库侧入口用例 `src/tests/backtest_entries.rs:141`（`8 passed; 1 failed`）。还原侧
  MQ58a/b/c/e 与 STAGE 8 均为 `10 passed; 0 failed`（MQ58e 另带 `9 passed`）。
  **MQ58d 的还原行是 Windows 链接锁报错**（`failed to remove target\debug\qx-cli.exe`）而不是绿 ——
  同一批 pristine 字节（`PRISTINE_OK at restored-after-MQ58d`）随后在 MQ58e 还原行与 STAGE 8
  各跑到一次 `10 passed`，故按那两次记账。
- 变异成对（静态门禁级）：G1 改文案 → `[FAIL] 多腿规格闸门必须对缺规格与全现货两种组合都报错`；
  G2 删闸门调用 → `=1`（"规格闸门落到了撮合之后"）；G3 删两处衍生品过滤 → `=1`；
  G4 `|| true` 让 `margin_model` 不再由规格决定 → `=1`；G5 重命名 Q58 用例 → `=1`；
  五项 `ARCH_RESTORED_G*_EXIT=0`。
- 收口：`PRISTINE_OK` 22 处逐个 `cmp` 通过、`MUTATION_RESIDUE=none`、`FINAL_ARCH_EXIT=0`、
  `BUILD_final_EXIT=0`。
- 行数：`multi_builtin.rs=470`、`single_strategy.rs=428`、`leg_funding.rs=130`、`multi_leg.rs=450`、
  `backtest_entries.rs=423`、`multi_leg_attribution.rs=717`；登记预算 `HEAD 37 项 / 46411` →
  `本轮 36 项 / 45573`（继续下行，未新增登记项）。`MODIFIED_TRACKED_FILES=28` 是 Q54–Q58 未提交成果的合计。

为守住 `< 500` 结构线做了三处搬家（没有抬棘轮、没有改门槛）：`run_ccxt_builtin_backtest` 搬进它自己
委派的那条链 `single_strategy.rs`（`BACKTEST_ENTRY_OWNERS` 随之改指），`configured_instrument_product`
搬进同域的 `leg_funding.rs`，`backtest_entries.rs` 里三条 worker 入口用例拆成新主题文件
`src/tests/strategy_worker_entries.rs` 并补 `mod` 挂载。本轮未使用任何外部服务或凭据，
`sandbox_tested` 全部保持 `false`。

## Unreleased — V11 Q57：对冲补偿提交并入同一道闸门，补偿成交不再按乘数 1 记账（2026-09-22）

同样不在 V11 §6 排期内，是"交易链路还有哪些潜在问题，全部修复"这条实测指令的第三批。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §19。

### Fixed（交易链路：第三条入账入口同时绕过形状契约与精度闸门）

- **`HedgeRecoveryWorker::execute_with_validator` 是 §18 之后剩下的第三条提交入口**：多腿对冲补偿
  腿自己 `router.submit_order(...)` 拿回包，却既不做形状校验（`validate_submit_events`）也不做
  tick/step 校验，直接把回包事实 append 落库。上一轮把"越界回报只能转待对账"补成两条入口同纪律，
  这一轮发现真正在跑资金的那第三条仍然独走 —— 补偿腿由故障恢复自动触发，无人复核。
  现在两条提交入口共用同一个守卫 `gate_submit_facts`（形状 + 精度），越界只留一条
  `ReconcileRequired`（相关串带 `submit-fill-out-of-spec`），并冒泡进本轮诊断。
- **补偿成交退回乘数 1 记账**：该入口 append 的是裸 `ExecutionEvent::Fill`，经
  `RuntimeExternalEvent::Fill` → `apply_fill` → `FillTerms::LegacyMultiplier(1)`，衍生品腿因此按
  `contract_size = 1` 入账 —— 与实盘/回测的冻结规格口径不同源，名义额与保证金都会算错。
  现在 `HedgeOrderValidator` 新增 `instrument_spec()`（默认 `Ok(None)`，保持既有 blanket impl），
  `append_fact` 在带规格时把 `Fill` 升成 `FillWithSpec`。
- **规格解析失败时拒绝补偿**：`instrument_spec()` 返回 `Err` 时该腿不提交、不落事实，诊断点名
  "未解析到产品规格，拒绝自动对冲"；返回 `Ok(None)`（未配置 `instrument_spec_path`，即现货乘数 1
  旧口径）时保持原行为。这是刻意的不对称：缺配置可以继续，配置读坏了不能继续。
- **Paper 补偿重放侧同步接线**：`crates/qx-cli/src/spread.rs` 的 `RecoveryGuard` 把 worker 冻结的
  market spec 交给 `ingest_venue_events_with_spec`，补偿成交落进账本时用的就是那一份规格。

### Added（门禁与用例）

- `crates/qx-execution/src/tests/recovery_and_replay.rs`
  `hedge_compensation_submit_shares_the_submit_precision_gate_and_spec_bookkeeping`：4 行场景表
  （越界回包只留待对账 / 在 tick 上带规格落库且 `contract_size` 保持 / 未配规格回落裸 `Fill` /
  规格解析失败不提交），断言事实的**形状标签序列**与 router 调用次数。
- `crates/qx-cli/src/tests/paper_hedge_recovery.rs`（新模块，从 `execution_and_multi_leg.rs` 拆出）：
  原有部分成交重放用例搬迁 + 新用例
  `paper_hedge_recovery_ingests_venue_fills_through_the_worker_frozen_spec`（在 tick / 越界两档
  验证 Paper 补偿链的落库口径）。
- `tools/check_architecture.py` 新增 5 项（159 → 164）：两道提交入口共用同一闸门（`lib.rs` 内
  `gate_submit_facts(` 调用点恰为 2）、worker 体过闸门且规格解析失败拒绝补偿、`append_fact` 带
  规格升级、两类 reason 段各只有一处定义、每条生产回报路径都把冻结规格交给归约入口
  （`LIVE_REPORT_FILES` 计入 `spread.rs`）；`EXECUTION_TEST_FLOOR` 13 → 25（口径含集成测试目录，
  并把"删一条就红"的注释改成实测总数地板）。

### Validation（本轮日志实测，`/tmp/qx_q57_gate.log`）

- `cargo check --workspace --all-targets` `STAGE1_CHECK_EXIT=0`；`cargo fmt --all --check` `=0`；
  `cargo clippy --workspace --all-targets -- -D warnings` `STAGE3_CLIPPY_EXIT=0`、`CLIPPY_WARNING_LINES=0`。
- 整仓测试：未设 `QX_PYTHON` 时 `SUM_workspace_passed=168 failed=2`（WindowsApps 占位桩必然项，
  断言 `WORKSPACE_FAILED_WITHOUT_QX_PYTHON=2`），设解释器后 `CARGO_TEST_WORKSPACE_WITH_PYTHON_EXIT=0`、
  `SUM_workspace_with_python_passed=677 failed=0`、`OK_SUITES=53`；含 doc-test `CARGO_TEST_WITH_DOC_EXIT=0`、
  `SUM_with_doc_passed=677 failed=0`。
- 静态门禁：`架构不变量自检全部通过 ✓（164 项）`、`STAGE5_ARCH_FAIL_LINES=0`、`FINAL_ARCH_EXIT=0`。
- 变异成对（cargo 级）：MQ57a（worker 里把规格传成 `None`）与 MQ57b（`append_fact` 丢弃规格）
  各自让 qx-execution 用例 `0 passed; 1 failed`，MQ57c（`spread.rs` 忽略规格）让 qx-cli 新用例
  `1 passed; 1 failed`；三者还原后分别 `1 passed` / `1 passed` / `2 passed`。
- 变异成对（静态门禁级）：G1 删 worker 闸门 → `=1`（同时点亮"调用点 1 处（期望 2）"）；
  G2 删 `append_fact` 升级 → `=1`；G3 破坏用例证据 → `=1`；G4 复制精度 reason 段 → `=1`（"2 处（期望 1）"）；
  G5 让 `RecoveryGuard::instrument_spec` 恒返回 `Ok(None)` → `=1`（点名 `crates/qx-cli/src/spread.rs`）；
  五项 `ARCH_RESTORED_G*_EXIT=0`。
- `PRISTINE_OK` 18 处逐个 `cmp` 通过，`MUTATION_RESIDUE=none`，`BUILD_final_EXIT=0`。
- 行数：`crates/qx-execution/src/lib.rs` 1,517 → 1,619（连续第二轮上调，已在 §19.4 记为下一轮先拆）；
  `crates/qx-cli/src/spread.rs` 486 行；登记集 37 → 36 项、总和 46,411 → 45,573。

## Unreleased — V11 Q56：提交同步返回的成交纳入同一条精度闸门（2026-09-22）

同样不在 V11 §6 排期内，来自"交易链路还有哪些潜在问题，全部修复"这条实测指令的第二批。
收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §18。

### Fixed（交易链路：越界成交只剩半条纪律）

- **实盘 `submit` 同步返回的成交回报绕过精度闸门**：`TradingInstrumentSpec::validate_fill` 此前只在
  用户流归约入口 `ingest_venue_events_with_pipeline` 调用。同一条落在 tick/step 之外的成交，
  走"交易所把回报推下来"会转 `ReconcileRequired`、不记账；走"Venue 在 submit 里直接返回"
  却经 `PortExecutionService::submit` 直接 append 入账——两家适配器（Paper 的即时成交、
  Binance/CCXT 的一次性回包）都属后者，"越界回报只能转待对账"因此只剩半条纪律。
  现在提交路径在形状校验（`validate_submit_events`）之后过同一个谓词：规格优先取回报自带的
  `FillWithSpec.spec`（落库保留的就是它），缺省回落到服务冻结的那份；越界即
  `mark_reconcile(order, "submit-fill-out-of-spec")` 并冒泡错误，Accepted 与成交一条都不落，
  与既有"未知结果"契约同构。

### Added（门禁与用例）

- `crates/qx-execution/src/tests/venue_submit_contract.rs`
  `submit_returned_fills_share_the_precision_gate_with_user_stream_reports`（新，5 形状一张表）：
  裸 `Fill` 在 tick / 越界、自带规格的 `FillWithSpec` 在 tick / 越界、以及"冻结规格宽 + 回报规格严"
  的优先级用例；断言用回报事实的**形状标签序列**（`["accepted","fill-with-spec"]` vs `["reconcile"]`），
  而不是数量相等。
- `tools/check_architecture.py` 新增 3 项（156 → 159）：提交同步回报也过同一闸门并转待对账、
  `lib.rs` 内 `validate_fill(` 调用点**恰为两处**（第三处即红）、上述行为用例在位。
  原"只有一个谓词、一个调用点"改名为"一个谓词（调用点只允许归约入口与 submit）"。
- `EXECUTION_TEST_FLOOR` 13 → 14：新用例进棘轮下限，删掉即红。
- `maturity/capabilities.yaml` `paper_execution` 挂上这条新证据。

### Validation（本轮日志实测，`/tmp/qx_q56_gate.log`）

- `cargo check --workspace --all-targets` `STAGE1_CHECK_EXIT=0`；`cargo fmt --all --check` `=0`；
  `cargo clippy --workspace --all-targets -- -D warnings` `STAGE3_CLIPPY_EXIT=0`。
- 整仓测试：未设 `QX_PYTHON` 时 `SUM_workspace_passed=167 failed=2`（那 2 条是 WindowsApps
  `python` 占位桩必然失败项），带解释器重跑 qx-cli 单 binary `test result: ok. 109 passed; 0 failed`，
  含 doc-test 的整仓跑 `SUM_with_doc_passed=675 failed=0`（`CARGO_TEST_WITH_DOC_EXIT=0`）。
- 静态门禁：`架构不变量自检全部通过 ✓（159 项）`，`FINAL_ARCH_EXIT=0`。
- 变异成对（cargo 级）：MQ56a 把闸门变成永不相中 → 用例 `0 passed; 1 failed`；
  MQ56b 把规格优先级反转成"冻结优先" → 同样转红；两者 `RESTORE_OK` 后 `1 passed; 0 failed`。
- 变异成对（静态门禁级）：G1 删掉整个提交侧闸门 → `ARCH_MUTATED_G1_EXIT=1`（同时点亮新两项），
  G2 在 `mark_reconcile` 前插第三处 `validate_fill(` 调用 → `=1`（点名"3 处（期望 2）"），
  G3 破坏用例里的 `vec!["reconcile"]` 证据 → `=1`；三项 `ARCH_RESTORED_G*_EXIT=0`。
- `PRISTINE_OK` 在 8 个位置逐个 `cmp` 通过，`MUTATION_RESIDUE=none`。
- 行数：`crates/qx-execution/src/lib.rs` 1,498 → 1,517（+19，本轮首次也是唯一一处上调，
  已 `--snapshot` 记账）；登记集仍 36 项，总和 `WORK_BUDGET_SUM=45471`（HEAD 为 37 项 / 46,411）。

## Unreleased — V11 Q54/Q55：回测链与 onboarding 的规格同源，首屏命令不再注定失败（2026-09-22）

本轮不在 V11 §6 排期内：它来自"交易链路与回测链路还有哪些潜在问题，全部修复"这条实测指令，
顺带关掉 §16.4 第 4 条。收口记录见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md) §17。

### Fixed（回测链与首屏命令）

- **D1 回测链只认一种 spec 形状**：`init` 生成的 `qianxing.binance.spot.spec.json` 是冻结产品规格，
  而回测入口此前只吃 CCXT 归一化形状，首屏第一条回测命令必报 `CCXT market 缺少 base`。
  现在回测链与 worker 链共用 `market_spec_from_value`（按 `base_currency` 判形状，不做 try-parse 回落）。
- **D2 缺精度即兜底 = fail-open**：`price_tick_raw` / `qty_step_raw` / `min_qty_raw` 缺失时以前退给 `1`，
  衍生品缺 `max_leverage` / `maintenance_margin_bps` 时退给 1x / 500bp。编造值既通过
  `TradingInstrumentSpec::validate()`，又在产物里与真实声明长得一模一样，滑点与取整两道闸门同时静默失效。
  现在一律拒绝并点名缺哪一项、该改传哪种形状。
- **D3 / D4 `init` 首屏命令**：7 个 profile 曾印同一行回测命令，其中 `base`/`paper` 没绑策略、
  `ccxt`/`multi-venue` 连 BarFrame 都没复制；印出的路径是裸文件名，隐含"先 cd 到项目目录"这条从未写在屏幕上的前提。
  现在只印"文件真的复制进项目 **且** 入口真的读得动这份绑定"的命令，且路径带上项目目录。
- **D5 `strategy init` 生成不可自包含的项目**（本轮实测新发现）：模板里的 `deploy/…` 相对路径被原样
  写进输出配置，而运行时相对路径按**配置文件所在目录**解析，于是刚生成的配置读不到自己的行情夹具。
  现在与 `init` 共用同一套归一：键改裸文件名、资产复制进项目、命令给绝对路径。

### Changed（概念与结构单点）

- `crates/qx-cli/src/market_spec.rs`（新，198 行）：产品规格 JSON 的唯一读法 + CCXT 快照到规格的转换。
- `crates/qx-cli/src/init_project.rs`（新，486 行）：从 `config_commands.rs`（965 → 590 行）逐字拆出的
  onboarding 命令面；`worker_entry.rs` 拆出 `market_spec.rs` 后 460 行，退出行数登记集。
- "哪些内置 kind 是双腿"只剩 `BuiltinStrategyKind::needs_reference_leg()` 一个谓词，
  内核与运行时配置面的三处四元 `matches!` 名单删除；CLI 侧准入名单与之由新门禁逐项对齐。

### Added（门禁 8 项 + 用例 9 条）

- 门禁（148 → 156）：规格解析/构造单点、三项定点精度必须声明、两形状按 `base_currency` 判定、
  回测链与 worker 链各自经 loader、同源用例在位、双腿 kind 内核谓词≡CLI 名单、
  `init` 两个单标的入口按谓词拒绝、拆出模块成对挂载。
- 用例：`crates/qx-cli/src/tests/init_onboarding.rs`（新，4 条，把 `init` 印出的每条命令原样执行）、
  `crates/qx-cli/src/tests/market_spec_single_source.rs`（新，5 条，含"回测链吃 CCXT 形状且与冻结形状同 `result_hash`"）。
- 门禁脚本新增并发保护：开跑时给变异目标拍 pristine 快照，每次动手前 `cmp`，被第三方改过即
  `CONCURRENT_TREE_CHANGE` + `exit 6`（本轮实测：单实例锁会被手工 `rmdir` 弄失效，而 `TaskStop`
  只杀掉外层包装、旧脚本继续跑完整个变异段，两份日志同时作废）。

### Validation（本轮日志实测，`/tmp/qx_q55_gate.log`，2026-09-22）

- 前置与静态面：`cargo check --workspace --all-targets` = 0；`cargo fmt --all --check` = 0；
  `cargo clippy --workspace --all-targets` = 0 且 `CLIPPY_WARNING_LINES=0`；门禁 156 项全绿。
- 行数棘轮：`HEAD_BUDGET_ENTRIES=37 / SUM=46411` → `BUDGET_ENTRIES=36 / SUM=45452`（−1 项、−959 行），
  与 HEAD 登记表的 diff 只含下行与删项。
- qx-cli 用例两个口径：不设 `QX_PYTHON` 退 101 且 `106 passed; 2 failed`（既知的两条解释器依赖用例）；
  设 `QX_PYTHON` 退 0 且 `108 passed; 0 failed`；`CLI_TEST_FUNCS=112`。
- 反向验证 7 个变异逐个红、还原逐个绿：M1 `ARCH=1` + `TESTS=101`（新补的那条用例死在
  `missing field \`instrument\``）、M2 `ARCH=1` + `TESTS=101`、M3 `ARCH=1`、M4/M5/M6 `TESTS=101`、
  M7 `ARCH=1`；还原侧 `ARCH_*_RESTORED=0` / `TESTS_*_RESTORED=0`、`RESTORE[…]=identical` ×7、
  `MUTATION_RESIDUE=no`、`PRISTINE_OK` ×8 且无 `CONCURRENT_TREE_CHANGE`。
- 全量：`QX_PYTHON=… cargo test --workspace --all-targets --no-fail-fast` = 0，
  `OK_SUITES=53`、`RUST_PASSED=674`、`RUST_FAILED=0`（上一轮基线 673，本轮净 +1 条用例，没有靠删用例换绿）。
- 产物零污染：`MODIFIED_DEPLOY=0`（`DEPLOY_LINES=48` 全是 Q1a 遗留的未跟踪 run 目录）。
- 本轮不使用任何外部服务或凭据，`maturity/capabilities.yaml` 的 `sandbox_tested` 保持 `false`。

### Changed（文档与能力表）

- `cli_scenario_init` 的证据路径改指 `init_project.rs` 与 `init_onboarding.rs`（此前指向的代码已拆走），
  并新增 limitation `ccxt_and_multi_venue_profiles_print_no_backtest_step_because_init_copies_no_market_frames_for_them`；
  `local_backtest` 增记单一读法文件与"两形状同 `result_hash`"用例。改完单跑门禁三次均 `EXIT=0`。

## Unreleased — V11 Q1a 第二批：Bar 链撮合口径从配置面可达且看得见（2026-09-22）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§6 的 Q1a 行；本轮结掉"Bar 链撮合模型接到命令面"，`--virtual-trading` 未做（前置条件见
同文档 §16.4 第 1 条），收口记录见同文档 §16。

### Added（`strategy.fill_model` 从"没人读的配置名"变成生效口径）

- `crates/qx-cli/src/backtests/fill_model.rs`（新，167 行）：`BarFillModel` 三成员
  （`next_bar_open` / `best_price` / `one_tick_slippage`）是**这张表就是命令面**，
  `bar_fill_model(configured, instrument_spec)` 是四条 Bar 回测链唯一的取口径入口，返回
  `BarFillModelBinding { fill, name, source }`。`source` 区分"配置没提这一项"与"配置里声明了
  同一个模型"，与 `ExecutionCostBinding::source` 同一条理由：分不清来源等于让默认值冒充选择。
- `strategy.fill_model`（`crates/qx-runtime/src/runtime_config/strategy_schema.rs:162`）：
  `#[serde(default, skip_serializing_if = "Option::is_none")]`，与 `cost_rules_path` 同口径。
  三条 Bar 装配链（策略 / 内置 / 多腿的每一条腿）都读它；深度链不读，因为它不经过 `FillModel`。
- 产物留痕三处：策略链摘要 `fill_model: {name, source}`（`backtests/artifacts.rs:137`）、
  多腿归因产物同键（`multi_builtin.rs`）、stdout 的 `[Builtin · Execution]` 与
  `[Multi-leg · Execution]` 两行。内核侧 `model_descriptors[0]` 从此不再是恒为
  `NextBarOpen@v1` 的假话。
- **多腿腿级口径守卫**（`multi_builtin.rs:214-223`）：逐腿记录 `(name, source)`，两腿不一致或
  一条都没记录都报错；一档滑点的**大小**允许随各腿自己的 `price_tick` 变化（标的规格，非口径分叉）。
- **不可达者按档位 fail-closed**：`probabilistic` → "需要 L1 一档盘口"、`volume_sensitive` →
  "需要 L2/L3 深度盘口"，并说明 Bar 输入只有 OHLCV、深度链不经过 `FillModel`；
  `one_tick_slippage` 缺 market spec 或 spec 的 `price_tick` 非正数一律拒绝，不退成
  `ccxt_market_to_spec` 的兜底 `1`。拼错的名字才报"未知"并列全清单。
- 架构不变量 137 → **142 项**（`backtest_assembly_check()` 新增 5 条）：撮合模型只在
  `fill_model.rs` 构造且装配字段取自 `self.fill`；三条链的口径全部经 `bar_fill_model` 解析
  （计数 ≥3 且 `configured_fill_model` 在位）；schema 声明该字段；`config validate` 调用同一判据；
  模型驱动用例在位。

### Changed（校验与装配同源）

- `fill_model_problem()` 是唯一判据，`bar_fill_model()` 用同一张表，`config validate` 经
  `fill_model_failure()` 只多套一层字段名前缀——"validate 放行过的名字，装配要么跑得动要么只缺
  market spec"由用例 `unreachable_or_under_specified_fill_models_fail_closed` 钉住。
- `BarBacktestAssembly::new()` 多一个 `BarFillModelBinding` 必答题，装配处不再给撮合模型默认值；
  `ecosystem_smoke.rs:357` 显式传 `bar_fill_model(None, None)`（内核默认不需要 market spec）。
- 能力矩阵 `local_backtest` 的限制项按事实拆开：原"Bar 链撮合模型与虚拟成交配置都不可达"一条
  改为"虚拟成交缺标记价输入" + "深档模型缺盘口输入"两条，证据加 `backtests/fill_model.rs`
  与 `tests/backtest_fill_model.rs`。

### Added（用例：5 条，删 0 条）

- `crates/qx-cli/src/tests/backtest_fill_model.rs`（新，368 行，挂在 `tests/mod.rs:243`）：
  `each_reachable_fill_model_changes_the_result_and_is_declared`（三种口径逐个与同 spec 基线比
  `result_hash` 与 `turnover_raw`，并断言名称/来源/描述子三处留痕）、
  `unconfigured_fill_model_is_absent_from_the_runtime_bytes`（省略即序列化字节不变，护住 66 份
  blessed 产物）、`unreachable_or_under_specified_fill_models_fail_closed`（四条拒绝路径的理由
  与"校验=装配"同判据）、`command_line_entries_read_the_declared_fill_model`（内置链缺 spec 先红
  后绿、多腿归因产物两种来源标注）、`depth_summary_carries_no_fill_model_key`（深度链不得冒充
  Bar `FillModel` 描述子）。

### Validation（本轮日志实测，`/tmp/qx_q1a2b_gate.log` + `/tmp/qx_q1a2_ev.log`）

- 基线：`cargo check --workspace` 0（`CHECK_ERROR_LINES=0`）、`cargo fmt --all --check` 0、
  `cargo clippy --workspace --all-targets` 0 且 `CLIPPY_WARNING_LINES=0`、
  `check_architecture.py` 0（142 项全过）。
- 全量 `cargo test --workspace --all-targets --no-fail-fast` 两口径：不设 `QX_PYTHON` 退 101、
  53 目标、`NOENV_PASSED=663`（本机必然失败的两条 Python strategy worker 用例，PATH `python`
  是占位桩）；设 `QX_PYTHON` 后 `FULLTEST_QXPYTHON_EXIT=0`、53 目标、**665 通过 / 0 失败**。
  对上一轮记录的 660 是净增 5 条，恰为本轮新增用例数，删 0 条。
- 反向验证 13 对，锚点预检 `ANCHOR_PRECHECK_BAD=0`：静态段 S1–S7 逐个
  `MUTATED_*_ARCH_EXIT=1` → `RESTORE[*]=identical` → `RESTORED_*_ARCH_EXIT=0`；行为段 B1–B6 逐个
  `MUTATED_*_EXIT=101`（每例 `4 passed; 1 failed`，非编译失败）→ `RESTORE[*]=identical` →
  `RESTORED_*_EXIT=0`（`5 passed`）。收口 `RESIDUE[*]=clean` ×13、`FINAL_ARCH_EXIT=0`、
  被测面复跑 17 passed。
- 命令行可区分性（同一份 Bar 帧 + 同一份 `sma_cross` 配置，只改 `strategy.fill_model`）：
  未声明与声明 `next_bar_open` 的 `result_hash` 逐位相同（`4b128bdee0ce75dd`）而摘要
  `source` 分别为 `builtin-default` / `runtime-config`；`best_price` → `629219d09096e384`、
  `one_tick_slippage`（带 spec，`tick=1000000`）→ `96de2663bbc67d3c`，两者相对基线的
  `turnover_raw`（86500000000 → 87500000000 / 87501000000）、`fees_raw`（43250000 →
  43750000 / 43750500）、`final_equity_raw`、`result_hash` 四项全变。
  同一份配置只改这一项时 `config fingerprint` 五值两两不同
  （`d2a4c4e335209221` / `813ede121ff6d041` / `7f606946e554a65c` / `463d9c0e60fc0c58` /
  `ba153af4bd0baf3c`）——这是"改做配置字段而非旗标"的全部理由。
- 四条拒绝路径（缺 spec、`next-bar-open`、`probabilistic`、`volume_sensitive`）全部退 2、
  stderr 原文说清理由，且 `SUMMARIES_AFTER_FAILURES=0`（失败轮不留产物）；
  `config validate` 对 `probabilistic` 退 2 印 `[FAIL] strategy.fill_model …`，对 `best_price`
  与"未声明"退 0 且 0 次提及该字段。
- 行数棘轮：`SNAPSHOT_EXIT=0`，`BUDGET_DIFF_EXIT=1` 的唯一差异是
  `crates/qx-xingban/src/orderbook_backtest.rs 1247 → 1245`（收紧，本轮未触及该文件）。
  **本轮无增长项**：新 `fill_model.rs` 167 行与用例 368 行都在 500 门槛外，无需登记；
  触及的 `runtime_check.rs` 与 `tests/backtest_entries.rs` 均停在 499（P4P 兄弟模块门槛是严格
  `< 500`，rustfmt 会把嵌套调用折成 4 行，故改用 `let` + `extend` 两行形状）。
- 产物卫生：跟踪的 66 个 `deploy/data/**/runs/*` 产物在本轮全量用例后
  `MODIFIED_DEPLOY=0`（逐个未被改写）；本轮测试另产生 12 个未跟踪文件（3 个 run 哈希 × 4 文件），
  连同上一轮遗留共 24 个未跟踪——这条脏源与 16 份过期 blessed 摘要一起留给 Q1b 裁决。
- 诚实性边界：本轮未使用网络、凭据或外部服务，`maturity/capabilities.yaml` 的 `sandbox_tested`
  仍 18 条全为 `false`。

### 本轮踩到并记进 §16.2 的坑

- `cargo check -p qx-cli --all-targets` **不编译依赖 crate 的测试**：给 `StrategyRuntimeConfig`
  加必填字段后，只有 `cargo test --workspace --all-targets` 才报出
  `qx-runtime/src/runtime_config/strategy_tests.rs:259` 的 `E0063 missing field fill_model`。
- 回测入口的 market spec 是 **CCXT 形状**（`base`/`price_tick_raw`），与 worker 侧
  `instrument_spec_path` 吃的 `TradingInstrumentSpec` 不是一种文件：把
  `deploy/qianxing.binance.spot.spec.json` 传给回测入口退 2 报 `CCXT market 缺少 base`。
  仓库里没有现货的 CCXT 形状规格，故命令行证据用合约规格配现货帧（只证"档位取自 spec"），
  用例自造 `price_tick_raw=3_000_000` 夹具以避开兜底 `1` 与仓库里的 `1_000_000`。
- `BarFillModelBinding` 装 `Box<dyn FillModel>` 因而没有 `Debug`，用例取 `unwrap_err()` 要过一层
  `map(|binding| binding.name)`——顺带钉住"错误里带模型名"。

## Unreleased — V11 Q1a 第一批：示例真成交、深度链参数可达且看得见（2026-09-22）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§6 的 Q1a 行；本轮结掉"深度链撮合参数 + 现金门 + 信号层两缺陷 + 三份示例夹具"，
Bar 链的 `--fill-model` / `--virtual-trading` 未做（前置条件见同文档 §15.4），
收口记录见同文档 §15。

### Fixed（信号层：内置策略此前"永远不交叉"与"永远不信号"）

- `crates/qx-strategy/src/builtin.rs:777`：`ema()` 的旧值权重从 `window` 改成 `window-1`。
  α = 2/(window+1) 时旧值权重必须是 1−α，写成 `window` 让两权重之和成为 `(window+2)/(window+1)`，
  直流增益不为 1、常数价格列收敛到 2× 该常数，快慢线相对位置随窗口大小漂移。`ema_cross` /
  `macd` / `keltner_trend` 共用该函数，交叉判定此前无从谈起。
- 同文件 `:225`/`:233`/`:349`：滚动历史上限低于信号门槛。`max_history()` 原来只按
  `slow_window`/`period` 取值（默认 5/20/14 得 32 根），而 MACD 第一个信号需要 35 根，
  于是示例帧再长也永不信号。新增 `required_bars()` 作为唯一门槛清单，历史上限取
  `max(required_bars, 窗口上限)`，信号门槛读同一函数。
- `crates/qx-xingban/src/orderbook_backtest.rs:573-586`：深度链补上**无 market spec 时的现货买入
  现金门**，与 Bar 链及同文件 `:535-556` 的有规格分支同口径（名义额 + 名义额 × `fee_bps`/10000
  超过可用现金即写 `Rejected` 事件并跳过），不再允许账本记成负现金。

### Added（命令面：高保真撮合参数接线）

- `backtest book` 新增 `--latency-snapshots` / `--market-impact-bps`（`cli_args.rs:477/479`，
  越界由 `parse_bps()` `:34` 在 clap 层挡住）→ `DepthExecutionModel`（`backtests/depth.rs:13`）
  → 内核 `OrderBookExecutionModel`，L1 与 L2 两条装配各自 `with_execution_model()`。三处同时留痕：
  `[Depth · Execution]` 行、产物 `model_descriptors` 的四参数描述子
  （`orderbook_backtest.rs:361`）、`depth_run_config_hash()`（`depth.rs:258-259`）——参数不进
  config hash 会让两个不同结果抢同一个内容寻址路径。缺省全 0 时与改动前逐位一致。
- **内核第三项 `queue_position_bps` 故意不做成旗标**：它只作用于限价单档位
  （`orderbook.rs:332`），而内置策略 intent 恒 `limit: None`（`builtin.rs:721`），17 条策略全发
  市价单。用例反向钉住它不存在（`--queue-position-bps` 退非 0 并点名旗标）。
- 拒单事实从 Q0e 的多腿私有实现提升为三条链共用：`backtests/artifacts.rs:43/63/76`
  （`rejection_facts` / `rejection_facts_line` / `rejection_count`），摘要产物新增
  `rejected_orders` 与 `rejection_reasons`，stdout 新增 `[Builtin · Integrity]` 与
  `[Strategy · Integrity]` 两行。`fills=0` 从此能区分"没发信号"与"全被挡"。

### Changed（示例夹具与口径文案）

- `deploy/qianxing.bar-frame.example.json` 5 → 70 根（旧夹具短于任何策略的预热窗口）；
  `deploy/qianxing.ashare.bar-frame.example.json` 时间戳换成真实交易时段 epoch 毫秒、价格改成
  先跌后涨；`python/examples/backtest_momentum.py` 的目标仓位 `1` → `ONE_UNIT = 1e9`（raw 口径）；
  四份 runtime 模板共 5 处 `builtin_quantity: 1 → 1000000000`，并在 `strategy_schema.rs` 写明
  runtime 用 raw、CLI 位置参数用整数单位，两者相差 1e9 倍。
- Q0c 的深度链延迟冲突文案改成可执行指令（`depth.rs:99`）：点名"把 `latency_base_ns` /
  `latency_insert_ns` 置 0，或改用 `--latency-snapshots`"。

### Added（用例：6 条，删 0 条）

`ema_keeps_unit_dc_gain_and_lags_on_the_trend_side`、`macd_signals_within_the_rolling_history_cap`
（qx-strategy）、`specless_spot_buy_beyond_available_cash_is_rejected_not_overdrawn`（qx-xingban）、
`depth_execution_model_flags_change_results_and_are_recorded`、
`shipped_examples_fill_positions_and_pay_nonzero_fees`、
`blocked_signals_are_distinguishable_from_silent_strategies`（qx-cli 集成用例）。

### Validation（本轮日志实测，`/tmp/qx_gateQ1a2_v3.log` + `/tmp/qx_q1a_evidence_000305.log`）

- `cargo fmt --all --check` 与 `cargo clippy --workspace --all-targets` 退出 0、warning 0 行；
  `cargo test --workspace --all-targets --no-fail-fast` `TEST_TARGETS=53`、
  `TEST_PASSED=660 TEST_FAILED=0`（对照上一轮基线 654，净增 6 条即上述新用例）；
  本轮改过的两份模板 `config validate` 退出 0；`tools/check_architecture.py` `ARCH_EXIT=0`、
  137 项不变量全过；`MUTATION_RESIDUE=none`。
- 反向验证成对红/绿：M1（摘掉 `[Depth · Execution]` 行）`MUT_M1_EXIT=101` →
  `RESTORE[M1]=identical` → `MUT_M1R_EXIT=0`；M2（四参数描述子退回只写 `fee_bps`）双红
  （引擎 `MUT_M2_EXIT=101` + CLI `MUT_M2T_EXIT=101`）→ `RESTORE[M2]=identical` →
  `MUT_M2R_EXIT=0`。M2 首轮只红在 CLI 侧、引擎 76 passed 全绿，于是补了引擎断言再跑一遍——
  只有消费者侧断言的门禁不算门禁。
- 撮合参数六 case（`/tmp/qx_q1a_ev_table.txt`）：L2/L1 在缺省 / 冲击 50bp / 延迟 2 快照三个口径下
  `result_hash` 分别为 `32d8ea011a3ec946` / `ccc1ea04991c300b` / `71d8e01d91418b7e`，
  六行 `model_fingerprint` 两两不同（`31f6292c7d0acf38` / `230e5a68855baf50` / `509ea904c41e2caa`
  / `1f6ef283165e7476` / `920aa1e71f2cc44a` / `868e9e5f10cf32c4`）；冲击 50bp 让
  `fees_raw` 32411000000 → 32573055000、`final_equity_raw` 100265589000000 → 99941316945000、
  `return_bps` 26 → -5；`--queue-position-bps 5000` 实测 `QUEUE_FLAG_EXIT=2`。
- 13 条内置策略在同一份 Bar 帧上（全部退出 0）：6 条 `fills=1`、4 条 `fills=2`、
  3 条 `fills=0`；其中 `bollinger` 带 3 次 `NoShort` 拒单（原因看得见），`atr_trend` 与
  `volatility_breakout` 的 `rejected_orders=0` 且可证明是夹具性质——70 根里
  `|Δclose| > ATR14` 的根数为 0/56（最大单根变动 1.0e9，最小 ATR14 1.5e9），
  波动率突破需要振幅比 > 2 而实测最大 1.333。
- 棘轮 re-baseline（本轮唯一增长项）：`crates/qx-cli/src/cli.rs` 668 → 675、
  `crates/qx-strategy/src/builtin.rs` 1009 → 1097、
  `crates/qx-xingban/src/orderbook_backtest.rs` 1112 → 1247。
- 产物卫生：跟踪的 `deploy/data/**/runs/*` 66 个（`*.summary.json` 16 份），其中含
  `rejected_orders` 的 0 份、含 `latency_snapshots=` 描述子的 0 份 —— 全部 blessed 产物早于本轮
  两次 schema 变更，移交 Q1b 重 bless；本轮测试另产生 12 个未跟踪 run 文件。
- 诚实性边界：本轮未使用网络、凭据或外部服务，`maturity/capabilities.yaml` 的 `sandbox_tested`
  仍 18 条全 `false`（`SANDBOX_TESTED_TRUE=0 / SANDBOX_TESTED_TOTAL=18`）。

### 本轮踩到并记进 §15.2 的坑

- **陈旧二进制冒充证据**：门禁末尾 `cp -p` 还原把源码 mtime 带回变异前，独立使用的
  `target/debug/qx-cli.exe`（23:51:40.74）于是是变异期产物，跑出的表里描述子只剩 `fee_bps=5`、
  六 case 出现两个相同 `model_fingerprint`，看着像真缺陷。重链后自洽；证据日志从此第一段
  先打印 exe 与相关源码 mtime。
- **MSYS `/tmp` ≠ 原生解释器 `/tmp`**（本轮两次）与 **`cargo test -- <filter>` 的 0 命中假绿**
  （过滤器匹配函数名，写错得到 `0 passed` 且退出 0）；变异段改为把 `test result:` 原文打进日志。

## Unreleased — V11 Q0e：多腿归因只承认实际成交（2026-09-21）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§4.18 与 §6 的 Q0e 行；一腿被挡时的显式动作按编号默认值取**标记 `PendingReconcile`**
（不做自动收口），收口记录见同文档 §14。

### Changed（`multi-builtin` 的归因与产物不再乐观记账）

- `crates/qx-cli/src/multi_leg.rs`：套利组**只由实际成交配对**（`:381-387`）。改之前只要某条腿
  在信号计划里出现过量，即使它一笔都没成交（现金不足、名义额上限、reduce-only 被挡），组里照样
  挂上它的计划数量，净敞口 / 保证金峰值 / 归因费用可以描述一个现实中拿不到的组合；改后
  `filled_qty_raw` 为零即不成组，该腿成本与成交量原样进 `residual_*`，一腿成交而对手腿落空时
  登记 `MultiLegPendingReconcile`（含对手腿成交量），策略写死为
  `mark-pending-reconcile-no-auto-close`。每腿新增"计划 vs 成交"事实
  （`planned / filled / unfilled_planned_qty_raw / vetoed_signal_ts / rejected_orders /
  rejection_reasons`），拒单原因从该腿事件日志的 `EventKind::Rejected` 归并而来，
  **不新增事件、`result_hash` 不变**。
- 单腿定资拆到 `crates/qx-cli/src/backtests/leg_funding.rs`（新模块，60 行）：改用**本腿自己的**
  全帧最高价（旧口径取两腿全局最大值，一条 1e8 倍的参考价腿会把主腿账户撑爆）、手续费余量按
  **当前生效的成本绑定** `taker_bp` 折算（旧口径是 ×2 的估算），并且全程 checked——算不出来
  直接报错，不再 `.min(i64::MAX as i128)` 静默截断。截断正是"买不起的计划被伪装成跑通且零成交"
  的机制（§4.18 的同一类失真）。
- 产物升 `schema_version: 2`，新增 `accounts`（两腿初始现金与定资规则原文）、`legs`、
  `pending_reconcile`；stdout 增 `[Multi-leg · Integrity]` 与 `[Multi-leg · Reconcile]` 两行，
  归因行增 `residual_filled_qty_raw` / `residual_fees_raw`。两道闭合守卫
  （`multi_builtin.rs:285`、`:301`）让"裸腿事实 vs 残余成交"和"归因成交量 vs 撮合 fills"
  不闭合时直接失败。

### Added（用例与门禁）

- `crates/qx-cli/tests/multi_leg_attribution.rs` 用例 3 → 6 条：四条多腿 kind 各一条端到端
  （过去只有 `pairs_arbitrage` 被覆盖）、风控挡腿场景（用运行时配置
  `risk_rules.max_notional_raw = 10_000_000_000_000` 落在 ETH 腿 6e12 与 BTC 腿 1.2e14 之间，
  实测主腿 `fills=0 / rejected_orders=34 / unfilled_planned_qty_raw=6000000000`、
  `groups=0`、`pending=3`、`residual_filled_qty_raw=6000000000`）、定资越界必须报错
  （退出码非零 + stderr 点名上限与 `quantity` + 不得产出归因摘要）。
- `tools/check_architecture.py` 新增 `multi_leg_honesty_check()`（挂在 `kernel_claim_check()` 后），
  架构不变量 **130 → 137 项**；七条判据逐条注入实测红（§14.3）。回测主题模块登记新增
  `leg_funding`，`backtests/mod.rs` 的顶层条目保持在 8 个上限内。
- 反向验证成对记录在 §14.3：行为侧 R1（抽掉"只看成交才成组"）与 R2（抽掉裸腿登记）都让
  `vetoed_leg_never_pairs_against_a_filled_counterpart` 红、R3（定资退回静默截断）让
  `multi_leg_funding_bound_fails_loudly_instead_of_capping_cash` 红，门禁侧 G1–G7 七次注入全红，
  十次还原全部 `RESTORE[*]=identical` 且还原后复跑绿。

### 已知偏离（写进产物与能力矩阵，不当作已完成）

- §6 的"每腿费用按各自 venue spec"**未落地**：`TradingInstrumentSpec` 没有 maker/taker 字段，
  唯一生效费率来源 `ExecutionCostRules` 是全局的；仓库里带分档费率的
  `crates/qx-core/src/fenye.rs`（464 行）在自身文件之外零消费者，是 V10 P2a 留下的死码。
  两腿因此共用一份成本绑定，该事实写进产物 assumptions 与
  `capabilities.yaml` 的 `multi_leg_execution.limitations`。
- 策略侧仓位仍是"意图"口径（`StrategyContext::positions` 由策略自持、成交不回填），
  本轮只把归因与产物改成实际成交，并把 `vetoed_signal_ts` 留作后续接线的入口，见 §14.4。

## Unreleased — V11 Q0d：撮合内核表述如实化（2026-09-21）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§4.3 第 3 条与 §6 的 Q0d 行；本轮按其编号默认值执行——**只改表述 + 建 Q1d 排期，不改行为**，
收口记录见同文档 §13。

### Added（`kernel_claim_check()`：把"共用内核"这类说法钉成可判的六条）

- `tools/check_architecture.py` 新增 `kernel_claim_check()`，挂在 `paper_fee_same_source_check()`
  之后，架构不变量 **124 → 130 项**。六条判据各自"抽掉就变红"：
  paper 侧（`crates/qx-zhenlu/src/**`，整文件、剥注释）不得出现 `OrderBook` / `BookLevel` 符号；
  文档指向的成交入口必须真实存在（`impl PaperVenue` 与 `pub fn on_quote(`，实测在
  `crates/qx-zhenlu/src/lib.rs:1330`）；Tick 链必须确实复用 L2 引擎
  （`crates/qx-xingban/src/tick_backtest.rs:12-16` 的 `use crate::{… OrderBookBacktestEngine …}`）；
  `orderbook.rs` 模块文档必须点名两处真实消费者、并写明 paper 走首档 touch 与 Q1d 指向；
  最后是全局连坐——`crates/*/src/**`（跳过用例文件）加 `README.md`、`deploy/README.md` 里，
  任何把 Paper 与"共用 / 共享 / 同一 / 都走 / 复用 + 内核 / 撮合 / 订单簿"写进**同一句**、
  又不引用真实共享符号（`FeeModel` / `apply_fill_to_books` / `apply_ledger_fill` / `Ledger` / `Oms`）
  的表述一律红；否定语（不成立 / 不得 / 没有 / 不走 / 谎称 …）豁免，
  否则诚实记录错误说法的文字本身过不了门禁。
- 新判据的反向验证成对记录在 §13.3：M1（文档退回旧版）、M2（README 写入"Paper 与回测共用撮合内核"）、
  M3（`oms.rs` 引用 `OrderBookSnapshot`）、M4（Tick 不再复用 L2 引擎）、M5（`on_quote` 改名）五条红，
  M3n（同位置只加一行 `OrderBook` 注释）**阴性对照保持绿**。

### Changed（三处表述按事实改写，README 经核算不改）

- `crates/qx-xingban/src/orderbook.rs` 模块文档：原第 3 行"可被历史 Tick 回放、Paper 模拟和性能基准
  共同使用"不成立。新文档给出真实消费者两处、**Paper 不走这里**（首档一次性 touch、无逐档队列、
  无排队中的部分成交）、paper 与回测目前真正共享的只有执行平面下游三件
  （`FeeModel`、`qx_core::apply_fill_to_books`（调用点：`qx-runtime/src/pipeline.rs:1448,1462`、
  `qx-xingban/src/backtest.rs:848`、`qx-xingban/src/orderbook_backtest.rs:405`）、`Ledger`），
  并把"改接本内核"显式指向 V11 §9 的 Q1d。
- `crates/qx-cli/src/backtests/kernels.rs`：该文件是产物清单里 `matching_kernel` 三个名字的家，
  模块文档补明"还有第四套撮合不在清单里，因为它不产出回测产物"，避免读者以为内核只有三个。
- 给当时的 `docs/牵星完整架构方案-V1.md` §2.3 加现状对照（该文档已在文档收口时删除，原文在 git 历史）：核对后确认该节没说谎（它把撮合适配器放在"可替换"
  那一侧），缺的是现状标注——事件 / 归约 / `Oms` / `Ledger` / `FeeModel` 四模式同源已成立且有门禁，
  **撮合尚未同源**。`README.md:20` 的"回测与实盘共享同一规则内核"是**规则**口径而非撮合口径
  （风控与费用同源已由 Q0a/Q0b/Q0c 接成配置驱动），故保留原文。

### Fixed（门禁自身的缺陷）

- 判据 1 最初照惯例套了 `non_test_source()`，而 `crates/qx-zhenlu/src/lib.rs:16` 就挂着一行
  `#[cfg(test)] use`，按"首个 `#[cfg(test)]` 之前"截断后这条判据实际只扫了 15 行，
  `PaperVenue` 本体根本没进扫描——M3 第一次跑没红才暴露它。改为整文件扫描 + 剥注释。
  这条缺陷不影响 Q0a–Q0c 的任何结论（那些判据不依赖被截断的区段）。

### Validation（本轮日志实测，`/tmp/qx_q0d_round_main.log` + `/tmp/qx_q0d_gate_203911.*.log`）

- 前置门槛（`GIT_HEAD=b311f90`）：`CARGO_CHECK_EXIT=0`、`FMT_CHECK_PREFLIGHT_EXIT=0`、
  `CLIPPY_DENY_EXIT=0 CLIPPY_WARNING_LINES=0`、`ARCH_BASELINE_EXIT=0 ARCH_PASS_LINES=130`、
  `BUDGET_DRIFT_LINES=0`、`BUDGET_RESTORED=identical`。
- 用例基线与收口一致（无行为改动，本轮不新增 Rust 用例）：
  `SUITE_BASELINE_EXIT=0 TARGETS=53 PASSED=651 FAILED=0` →
  `SUITE_FINAL_EXIT=0 TARGETS=53 PASSED=651 FAILED=0`；`qx-cli` 单测 95 条全绿。
- 六次注入均 `INJECT_OK`（anchor 唯一）、六次还原均 `RESTORE[*]=identical` + `ANCHOR_BACK[*]=yes`，
  每次还原后复跑 `ARCH_PASS_LINES=130`（六次）。`MUT_RESIDUE[M2]` / `[M3n]` 为 yes 是 §7.4
  已登记的判据假象（M2 的 replacement 首行等于 anchor 首行；M3n 的 replacement 以换行起头使首行为空串），
  不是残留。
- 收口复跑：`FINAL_CHECK_EXIT=0`、`FMT_CHECK_EXIT=0`、`ARCH_FINAL_EXIT=0 ARCH_PASS_LINES=130`、
  `SUITE_FINAL_*` 同上。`maturity/capabilities.yaml` 的 `sandbox_tested` 仍 18 条全为 `false`；
  本轮未使用任何网络、凭据或外部服务。
- 行数棘轮例外（显式声明）：`orderbook.rs` 664 → 671 行，增量全在模块文档，按快照头部规则重 bless，
  `--snapshot` 的 diff 只有这一行。

### Known issues（移交，见 §13.4）

- Q1d 落地时判据是**成对**的：行为改接簿内核那一次必须同时改掉判据 1、判据 4 与 `kernels.rs` 的说法，
  否则门禁会把旧口径钉成化石；该轮的"paper vs book 成对数字"目前没有可复用的夹具，需要新造
  "同一 L1 输入两跑"的最小夹具。
- 本轮没跑任何 CLI，仅两次全量用例就把 `deploy/data/**/runs/` 的未跟踪文件推到 **40 条**，
  §12.4 第 1 条的产物冲突已升到必须裁决，归 Q1b。

## Unreleased — V11 Q0c：执行成本配置面接线（2026-09-21）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§3B 末条（死配置面）与 §6；本轮采用其 §0 的编号默认值 **E3 = 接入**（而不是删净），
收口记录见同文档 §12。上游 `77eb160`（成交归约 + 账本记账币种）在本轮断点合并进主线，
合并提交 `ef461b4`，取舍口径见 §12.1 末段。

### Added（`strategy.cost_rules_path` 从死配置变成生效配置）

- **成本规则文件有了第一个生产读者**：`ExecutionCostRules::load`（`crates/qx-xingban/src/cost_rules.rs:20`）
  此前全仓零消费者，`deploy/qianxing.costs.example.json` 是一行"宣称能配、实际没人读"的死配置。
  现在唯一读取点是 `runtime_wiring::execution_cost_binding_from_config`（`crates/qx-cli/src/runtime_wiring.rs:184`），
  它返回的 `ExecutionCostBinding`（`runtime_wiring.rs:156`）同时提供 `fee_model()` 与 `latency_model()`，
  消费方为 Bar 两条链（`backtests/single_strategy.rs:94` 与 `:278`）、多腿链
  （`backtests/multi_builtin.rs:155`，两条腿共用同一份绑定）、深度档（`backtests/depth.rs:69`）
  与三处 Paper 构造点（`venue_runtime/paper_worker.rs:48/191/244`、`paper_submit.rs:121`）——
  Q0a 解决的"两条链取同一个模型"从此不必再靠两边都写同一个常数。
- **产物记录成本来源**：回测摘要新增 `execution_costs.source`（`backtests/artifacts.rs:81`），
  取值 `cost-rules-file:<绝对路径>` / `runtime-config-default`（给了配置但没配这一项）/
  `builtin-default`（根本没给配置）三态；A 股规则链自带佣金模型时打印
  `ashare-rules:<path>+cost-rules-file:<path>`，把"费率来自规则快照、延迟来自成本文件"两件事分开记账
  （`backtests/single_strategy.rs:105-118`）。`backtest builtin` 另印一行 `[Builtin · Cost] source=… maker_bp=… taker_bp=… latency_base_ns=… latency_insert_ns=…`。
- **深度档的三层优先级**：显式 `--fee-bps` > 成本文件 `taker_bp` > 内核默认
  （`backtests/depth.rs` 的 `fee_bps.unwrap_or(costs.rules.taker_bp)`）。深度内核只有单一吃单费率，
  成本规则里的延迟与 maker 费率在这条链上无处落地，因此**延迟非零时直接报错**并点名来源文件，
  而不是静默跑出一份"配置写着 2ms、实际零延迟"的产物（Q0b 删 `--config` 判掉的正是这个形状）。
- **校验与装配同一个读者**：`config validate` / `runtime-check` 走 `cost_rules_problem()`
  （`runtime_check.rs:306-312`），与装配调用同一份 `ExecutionCostRules::load`，
  缺文件、坏 JSON、bp 越界都带解析后的路径失败，不可能再出现"校验说没问题、装配跑不动"。
- 新用例文件 `crates/qx-cli/src/tests/backtest_cost_provenance.rs`（334 行、5 条）逐条钉住上面四件事，
  共享夹具 `read_first_artifact` / `read_first_backtest_summary` 上移到 `tests/mod.rs`
  （`backtest_risk_provenance.rs` 里的两份私有副本删除）。

### Changed（使用者可见，但不破坏既有产物）

- `strategy.cost_rules_path` 声明为 `#[serde(default, skip_serializing_if = "Option::is_none")]`
  （`qx-runtime/src/runtime_config/strategy_schema.rs:138`）：**没配置时连键都不序列化**，
  因此仓库里 66 份已 bless 的 `deploy/data/**/runs/*.run.json` 与全部 `config_fingerprint` 逐字节不变，
  本轮无需重 bless（对比 Q0a 需要成对改 5 条 paper 断言）。
- 深度档 `--fee-bps` 的缺省值不再由 `cli.rs` 分派层给出：Q0b 装的
  `unwrap_or(qx_core::DEFAULT_TAKER_BP)` 下沉进深度入口，分派只把 `Option<i64>` 原样传下去。

### Fixed（本轮顺带消灭的两处分裂）

- Bar 装配此前写死 `latency: Box::new(ZeroLatency)` —— 成本规则里的延迟字段配了也不生效；
  现在延迟与费用成对取自同一份绑定，`ZeroLatency` 字面量在 `backtests/mod.rs` 已不允许出现。
- 默认费率常数的定义点收敲为唯一一处：`qx-core/src/fee.rs` 定义 `DEFAULT_MAKER_BP` / `DEFAULT_TAKER_BP`，
  `qx-xingban/src/cost_rules.rs` 只做再导出（门禁按 `pub const` 形状区分"定义"与"再导出"）。

### Added（架构不变量 117 → 124 项）

`paper_fee_same_source_check()` 从 8 项扩到 15 项，新增 7 条：成本规则文件的读者全仓唯一、
成本规则模板随仓库发布、Bar 装配不得写死零延迟、深度档缺省费率取自成本绑定、
运行时配置声明 `strategy.cost_rules_path`、`config validate` 覆盖该项、存在成本驱动行为用例；
另把两条旧判据改写为"缺省费率不再由分派层写死"与"费用与延迟成对来自同一份绑定"。
新增共享谓词 `is_cli_case_file()`（`tools/check_architecture.py`）：`qx-cli/src/tests/` 下的主题文件
整体就是用例现场，按路径认领（原 `definitions` / `loaders` / Paper 构造点三处各写一份过滤）。

### Validation（v11-q0c 轮实测，日志 `/tmp/qx_q0c_gate_194740.log`，2026-09-21 19:47:40–19:49:15）

- 前置门槛（HEAD 为合并提交 `ef461b4`）：`CARGO_CHECK_EXIT=0`、`FMT_CHECK_PREFLIGHT_EXIT=0`、
  `CLIPPY_DENY_EXIT=0` 且 `CLIPPY_WARNING_LINES=0`、`ARCH_BASELINE_EXIT=0 ARCH_PASS_LINES=124`。
- 行数棘轮：`SNAPSHOT_EXIT=0`、`BUDGET_DRIFT_LINES=0`、`BUDGET_RESTORED=identical`
  —— 本轮改动（含上游合入）之后无需新登记；合并时按既有先例（`ec57a62`）重基线一次，
  结果是 `crates/qx-execution/src/lib.rs` 1496 → **1498（上游 `fill_tag` 语义修复 +2）**，
  另有 `config_commands.rs` 971 → 965、`worker_entry.rs` 586 → 578 两项下降。
- 基线与收口（`cargo test --workspace --all-targets --no-fail-fast`，覆盖 `crates/*/tests/`）：
  `SUITE_BASELINE_EXIT=0 TARGETS=53 PASSED=651 FAILED=0` → `SUITE_FINAL_EXIT=0 TARGETS=53 PASSED=651 FAILED=0`，
  逐项一致，没靠删用例换绿；收口另加 `FINAL_CHECK_EXIT=0`、`FMT_CHECK_EXIT=0`、
  `ARCH_FINAL_EXIT=0 ARCH_PASS_LINES=124`。与 Q0b 轮基线（51 目标 / 604 例）的差里，
  本轮自身贡献是 `backtest_cost_provenance.rs` 的 5 条，其余来自上游 7 个提交带的新用例文件。
- 反向验证八组全部成对（静态面 M2/M3/M4/M5/M6，行为面 M1/M3/M4/M7/M8）：
  M1 让加载器读完文件后仍返回 `ExecutionCostRules::default()` → `M1_TEST_EXIT=101`
  （成本驱动用例红）→ `restore_M1_TEST_EXIT=0`；
  M2 在 `runtime_check.rs` 里造第二个 `ExecutionCostRules::load` 读者 → `ARCH_MUT_M2_EXIT=1`
  （`ARCH_PASS_LINES=123`，"读者全仓唯一"点名两个文件）→ 还原 `ARCH_PASS_LINES=124`；
  M3 装配退回写死 `ZeroLatency` → `ARCH_MUT_M3_EXIT=1`（`122`，成对性与零延迟两条同时红）
  且 `M3_CHECK_EXIT=0`、`M3_TEST_EXIT=101` → 两项还原后均 0；
  M4 深度档缺省费率退回 `qx_core::DEFAULT_TAKER_BP` → `ARCH_MUT_M4_EXIT=1`（`123`）
  + `M4_TEST_EXIT=101` → 均 0；M5 让 `config validate` 不再报告成本文件问题 → `ARCH_MUT_M5_EXIT=1` → 0；
  M6 把成本驱动用例改名 → `ARCH_MUT_M6_EXIT=1`（"存在 Q0c 证据"红）→ 0；
  M7 把多腿归因产物里的 `execution_costs` 键挪走 → `M7_TEST_EXIT=101` → 0；
  M8 关掉深度档的延迟拒绝 → `M8_TEST_EXIT=101` → 0。
  八组 `RESTORE[*]=identical`、`ANCHOR_BACK[*]=yes`。
- 一处如实记录的判据假象：`MUT_RESIDUE[M2]=yes`。残留检查只 grep 替换文本的首行，
  而 M2 的替换首行恰好就是锚点原行，于是报 yes；同轮 `RESTORE[M2]=identical`、
  `ARCH_RESTORE_M2_EXIT=0 ARCH_PASS_LINES=124`，且收口前的全仓残留扫描（`RESIDUE_SCAN_DONE` 之前）
  没有输出任何文件，可判无残留。该判据本身的这一盲点属 §7 门禁设计待办，不改本轮结论。
- 模板整跑（本轮实测，非日志内条目）：18 份 `deploy/qianxing.runtime*.json` 逐项
  `qx-cli config validate` → `TEMPLATE_SWEEP_OK=17 FAIL=1`，唯一红项是 production 模板的
  `[FAIL] strategy.research_snapshot_path / dataset_bundle_path 文件不存在: /var/lib/qianxing/research/*`
  两条（部署机绝对路径，与成本面无关，自 V10 起就是记录性条目）；
  CI 侧 `deploy/qianxing.runtime*.json` 的 for-loop（`.github/workflows/ci.yml:213-222`）
  经过 `runtime_check.rs:306` 的新校验，因此成本文件错误会在这 18 份模板上自动暴露。

### Known issues（本轮记录、未修）

- **`code_commit` 进了 RunManifest 文件名**（上游 `crates/qx-cli/build.rs` 把 `QX_GIT_COMMIT` 烧进产物）：
  每次提交后跑全量用例，`deploy/data/*/runs/` 就多出一批新的未跟踪产物 —— 本轮收口时实测
  未跟踪 24 个文件（`git ls-files --others` 计 runs 条目），仓库内已 bless 的 runs 文件 66 个。
  这是"产物可追溯"与"测试不留垃圾"的正面冲突，需在 Q1b 一并决定（候选：文件名不含 commit、
  或 runs 目录整体 `.gitignore` 只 bless 摘要白名单）。
- **工作树全量 CRLF**：`core.autocrlf=true` 下 `git ls-files --eol` 记到 224 个 `.rs` 里有 **201 个**
  是 `i/lf w/crlf`（索引内 LF、工作树 CRLF），任何跨行匹配源码文本的断言都会脆断。
  本轮踩到一次并修在测试侧（`crates/qx-cli/src/tests/backtest_entries.rs:373` 先 `.replace('\r', "")`
  再匹配帮助文本），未动 git 配置（属用户级设置，本轮不改）。


## Unreleased — V11 Q0b：命令面旗标诚实性与回测准入分区（2026-09-21）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§4 P0 第 2 项与 §6；本轮采用其 §0 的编号默认值 **E2 = 删除被吞的 `--config`**（而不是接线），
延续 E1 的"改动可见即记账"口径。收口记录见同文档 §11。

### Changed（破坏性 CLI 行为）

- **`qx-cli backtest --config <path>` 这一形态不再存在**（Q0b，§4 P0 第 2 项）：`Command::Backtest`
  父命令此前声明 `--config`、分派处以 `config: _` 收下并丢掉 —— 统一回测链路根本不读这份配置，
  使用者以为换了风控与费用口径而实际什么都没生效，这比没有旗标更坏。按 E2 选择删旗标而非接线：
  现在该命令行按 clap 未知参数**用法错误退出 2**、stderr 点名 `--config`；真正吃配置的子入口
  （`backtest builtin` / `multi-builtin` / `ccxt-builtin` / `book`）保留各自的 `--config` 且确实读取
  （V10 P0b 已收口），帮助里也继续暴露。脚本若曾给统一回测传 `--config`，需改用位置参数
  `runtime`/`frame`/`spec` 或改走子入口。
- 深度档回测的缺省手续费不再写死游离字面量：`cli.rs` 改为
  `fee_bps.unwrap_or(qx_core::DEFAULT_TAKER_BP)`，缺省值取自 Q0a 收进内核的同一常数（当前 5bp），
  Bar 链与深度链的默认口径失去各自漂移的可能；`book` 的帮助文本同步写明该来源。

### Fixed（能力自述诚实性）

- **"17 个内置策略都可直接用于回测/Paper/策略接入"这类笼统宣称**：准入分区收敛为唯一事实来源
  `cli_help::MULTI_LEG_KINDS`（4 个套利 kind 需两条对齐 BarFrame，只被 `backtest multi-builtin` 接受）
  与 `backtest_entry_of()`；此前是两处各写 4 个 kind 的字面量判断（`backtests/depth.rs` 的 `matches!`
  与 `backtests/multi_builtin.rs` 的反向判断）加一句帮助文案。现在两个入口的门、
  `builtin-strategies` 与 `strategy list` 的第三列输出、帮助里的 13/4 计数全部消费同一个列表
  （`depth.rs:30` / `multi_builtin.rs:21` 改为 `MULTI_LEG_KINDS.contains(&kind)`）。
  `print_builtin_strategies` 从 `cli.rs` 迁入 `cli_help.rs`，输出由"名称 + 说明"变成
  "名称 + 说明 + 真正接受它的回测入口"。
- `paper-check` 的帮助此前只说"验收主体链路"，未交代行情来源；现与 `paper-e2e` 同口径写明注入一条
  固定合成 L1 报价（99/100）、不接真实 feed。

### Added（架构不变量 114 → 117 项，用例 602 → 604 条）

- `cli_flag_honesty_check()` 两项：`cli.rs` 分派正文不得出现任何 `ident: _` 丢弃绑定；
  `cli_args.rs` 声明的 12 个长旗标字段逐个必须在 `cli.rs` 被点名（新声明却没人读的旗标同样红）。
- `paper_fee_same_source_check()` 追加一项：深度档 `fee_bps` 缺省值必须引用
  `qx_core::DEFAULT_TAKER_BP`。
- `src/tests/cli_surface.rs::backtest_rejects_the_config_flag_it_used_to_swallow`：子进程验证
  `backtest --config` 退出码 2 且点名旗标，同时验证 `backtest builtin --help` 仍暴露 `--config`。
- `src/tests/backtest_entries.rs::builtin_strategy_entries_match_the_printed_partition`：逐 17 个
  `BuiltinStrategyKind` 打通三条事实做交叉断言 —— 内核侧 `BuiltinStrategyConfig::new` 的合法性、
  深度档与多腿两个真实入口的错误文案、以及 `(单标的, 套利) = (13, 4)` 的计数；再断言帮助文本里的
  计数与 `builtin-strategies` 实际印出的准入列逐项等于 `backtest_entry_of`。

### Validation（v11-q0b 轮实测，日志 `/tmp/qx_q0b_gate_091927.log`，2026-09-21 09:19:27–09:20:48）

- 前置门槛：`CARGO_CHECK_EXIT=0`、`FMT_EXIT=0`、`CLIPPY_DENY_EXIT=0` 且 `CLIPPY_WARNING_LINES=0`。
- 基线（本轮基线是"Q0b 改动已落地、变异前"的状态，`cargo test --workspace --all-targets --no-fail-fast`
  覆盖 `crates/*/tests/`）：`BASELINE_TARGETS=51`、`BASELINE_PASSED=604`、`BASELINE_FAILED=0`、
  `BASELINE_EXIT=0`；`ARCH_BEFORE_MUTATIONS_EXIT=0`（117 项全绿）。
- 行数棘轮：`SNAPSHOT_EXIT=0`、`BUDGET_DIFF_EXIT=1`，本轮唯一差异是
  `crates/qx-cli/src/cli.rs` 登记预算 674 → **668（下降）**，无增长项、无需例外申报。
  实际行数 cli.rs 668 / cli_args.rs 475 / cli_help.rs 167 / tests/cli_surface.rs 368 /
  tests/backtest_entries.rs 417 / tests/mod.rs 206，测试文件均在 500 行阈值之下。
  快照后 `ARCH_AFTER_SNAPSHOT_EXIT=0`。
- 反向验证（六组，静态判据 N1/N2/N3/N5 走架构自检，行为判据 N4/N5/N6 走子进程用例）：
  `ARCH_MUT_N1_EXIT=1`（写回 `config: _,` → 两项红：丢弃形状 `['config']` + cli.rs 669 > 预算 668）
  → `ARCH_REST_N1_EXIT=0`；
  `ARCH_MUT_N2_EXIT=1`（新声明没人读的 `--experimental-never-read` → "13 个长旗标全部被读到"红）
  → `ARCH_REST_N2_EXIT=0`；
  `ARCH_MUT_N3_EXIT=1`（缺省费率退回 `unwrap_or(5)` → 内核常数引用判据红）
  → `ARCH_REST_N3_EXIT=0`；
  `TEST_N4_RED_EXIT=101`（分区里 `PairsArbitrage` 换成 `VolatilityBreakout` → 逐 kind 用例在
  `backtest_entries.rs:348` FAILED，其余 62 例被过滤）→ `TEST_N4_GREEN_EXIT=0`；
  `ARCH_MUT_N5_EXIT=1` + `TEST_N5_RED_EXIT=101`（两个文件同时复活"父命令声明、分派丢弃"的形状 →
  架构红且 `backtest_rejects_the_config_flag_it_used_to_swallow` 在 `cli_surface.rs:351` FAILED）
  → `ARCH_REST_N5_EXIT=0` + `TEST_N5_GREEN_EXIT=0`；
  `TEST_N6_RED_EXIT=101`（帮助退回旧的笼统宣称 → 帮助口径断言在 `backtest_entries.rs:395` FAILED）
  → `TEST_N6_GREEN_EXIT=0`。
  七个 `RESTORE[*]=identical` 且 `MUTATION_STILL_PRESENT[*]=no`；N5a 恢复后 `config: Option<PathBuf>,`
  字段总数回到 4（四个子入口各自合法声明，计入式守卫 `POST_N5a_CONFIG_FIELDS=4`）；
  收口 `MUT_RESIDUE=no`。
- 收口复跑：`ARCH_FINAL_EXIT=0`（117 项）、`BUILD_FINAL_EXIT=0`、`FINAL_EXIT=0`、
  `FINAL_TARGETS=51`、`FINAL_PASSED=604`、`FINAL_FAILED=0`、`FMT_FINAL_EXIT=0` —— 与基线逐项一致，
  没靠删用例换绿；`qx-cli` 单 binary 用例数 61 → 63（本轮新增 2 条）。
- 门禁日志之后仅追加一处文档注释修正（`cli_help.rs` 的 `MULTI_LEG_KINDS` 注释把行为用例文件指向
  `cli_surface.rs`，实际落在 `backtest_entries.rs`），不涉及任何可执行语义；复跑记在
  `/tmp/qx_q0b_postcomment_092425.log`：`FMT_EXIT=0`、架构自检 117 项通过、
  增量重编 `qx-cli` 后 `TARGETS=51`、`PASSED=604`、`FAILED=0`、`TEST_EXIT=0`。


## Unreleased — V11 Q0a：Paper 成交费用与回测同源（2026-09-21）

方案与逐阶段验收口径见 [docs/archive/自研量化框架重构方案-V11.md](docs/archive/自研量化框架重构方案-V11.md)
§4.1 与 §6；本轮采用其 §0 的编号默认值 E1（允许受控重 bless paper 常数并成对记录）、
E2–E5 留待后续阶段。

### Fixed（正确性）

- **Paper 成交恒定零手续费，与它自己的文档注释相反**（Q0a，§4.1）：`PaperVenue::new` 自带
  `ZeroFeeModel` 默认、唯一的换费率入口 `with_fee_model` 只有单元测试调用，于是生产 Paper 的
  `Fill.fee` 恒为 0 —— 同一份策略在 Paper 上系统性优于回测与实盘，属于"执行平面成本口径分叉"
  而不是显示问题。现在费用模型是 `PaperVenue::new` 的**必填构造参数**（默认值这个形状被删除，
  编译期就不许回落），并沿 `execute_paper_submit_effect` 的形参显式上溯到调用方；qx-cli 侧
  新增唯一构造点 `runtime_wiring::execution_fee_model()`，Bar 回测装配（`backtests/mod.rs`）与
  全部 Paper 构造点（`paper_submit.rs` / `paper_worker.rs` / `spread.rs` / `ecosystem_smoke.rs`）
  共用它，因此"同源"是构造出来的而非两份相同字面量的巧合。默认费率常数的定义点收进内核
  （`qx_core::fee::DEFAULT_MAKER_BP/DEFAULT_TAKER_BP` + `MakerTakerFeeModel::default_maker_taker()`），
  `qx_xingban::cost_rules` 只做再导出。不计费的用例一律显式传 `Box::new(ZeroFeeModel)`。
- 新增行为面证据 `crates/qx-execution/tests/paper_accounting.rs::paper_spot_fill_charges_the_shared_fee_model_into_the_ledger`：
  现货买入成交后按 `bp_amount(notional(...), DEFAULT_TAKER_BP)` 推导费用，并要求 Ledger 里
  `Fee` 分录的求和等于该值（`Ledger::apply_fill*` 只在 `fill.fee` 非零时写 Fee 分录，所以
  "费用真的进了账"是可观测的 +1 条分录）。`qx-zhenlu` 侧另把 Paper 费用模型的 descriptor 冻结为
  `MakerTaker@v1[params=maker_bp=2;taker_bp=5]` —— descriptor 变了就是执行平面成本口径变了，须与
  Bar 回测装配同步审阅。

### Added（架构不变量 107 → 114 项）

- `paper_fee_same_source_check()` 七项：默认 maker/taker 常数只在 `qx-core::fee` 定义（再导出不算
  第二处）、`cost_rules` 保留再导出、`execution_fee_model` 在 qx-cli 只有一个定义点、每个生产
  Paper 构造点的实参显式回答费用模型、qx-cli 生产代码不得出现 `ZeroFeeModel`、`backtests/mod.rs`
  的 `fee` 字段调用同一函数、上述 Ledger 用例存在。

### Validation（v11-q0a 轮实测，日志 `/tmp/qx_q0a_gate_084708.log`，2026-09-21）

- 前置门槛：`CARGO_CHECK_EXIT=0`、`FMT_EXIT=0`、`CLIPPY_DENY_EXIT=0` 且 `CLIPPY_WARNING_LINES=0`。
- 基线（`cargo test --workspace --all-targets --no-fail-fast`，覆盖 `crates/*/tests/`）：
  `BASELINE_TARGETS=51`、`BASELINE_PASSED=602`、`BASELINE_FAILED=0`、`BASELINE_EXIT=0`。
  快照前的架构自检 `ARCH_PRESNAPSHOT_EXIT=1`，唯一红项就是下面申报的行数棘轮增长。
- 行数棘轮：`SNAPSHOT_EXIT=0`、`BUDGET_DIFF_EXIT=1`，逐行差异只有两项 ——
  **唯一增长项** `crates/qx-execution/src/lib.rs` 1494 → 1496（+2 行：`execute_paper_submit_effect`
  新增的 `fee_model` 形参与其一行文档；这是第二次为正确性修复上调预算，不构成"后续无需再降行数"的
  结论，本轮曾试过用格式化腾挪压成零增量，`cargo fmt` 后仍回到 +2 故按既有出口记账）。
  另一项 `crates/qx-zhenlu/src/lib.rs` 2270 → 2269（−1，删掉 `zero_fee_paper()` 辅助与重复断言）。
  快照后 `ARCH_AFTER_SNAPSHOT_EXIT=0`，114 项全绿。
- 反向验证（M1/M5 走子进程行为，M2/M3/M4/M6/M7 是静态文本判据，配对记录）：
  `TEST_M1_RED_EXIT=101`（把注入退回 `ZeroFeeModel` → `paper_spot_fill_charges_…` FAILED，
  其余 3 例仍过）→ 还原 `TEST_M1_GREEN_EXIT=0`；
  `TEST_M5_RED_EXIT=101`（`Ledger` 两处 `if !fill.fee.is_zero()` 改成恒假 → 同一用例 FAILED）
  → 还原 `TEST_M5_GREEN_EXIT=0`；
  `ARCH_MUT_M2_EXIT=1` / `ARCH_MUT_M3_EXIT=1`（两项：构造点未给出 + 生产代码出现零费）/
  `ARCH_MUT_M4_EXIT=1`（两项：常数第二定义点 + 再导出缺失）/ `ARCH_MUT_M6_EXIT=1` /
  `ARCH_MUT_M7_EXIT=1`，各自 `ARCH_REST_*_EXIT=0`。七个 `RESTORE[*]=identical` 且
  `MUTATION_STILL_PRESENT[*]=no`，收口 `MUT_RESIDUE=no`。
- 收口复跑：`ARCH_FINAL_EXIT=0`（114 项）、`FINAL_EXIT=0`、`FINAL_TARGETS=51`、
  `FINAL_PASSED=602`、`FINAL_FAILED=0` —— 与基线逐项目标数一致，没靠删用例换绿。
- **受控重 bless（E1）：旧 → 新成对**。Paper 成交开始计费后，五条"Ledger 分录条数"断言各多 1 条
  `Fee` 分录：
  `paper_strategy_reads_filled_position_before_emitting_next_order` 2 → 3（`e2e_and_python_contract.rs:149`）、
  `paper_worker_cleans_stale_queue_after_terminal_commit` 3 → 4（同文件 :221）、
  `paper_e2e_entrypoint_runs_scheduler_strategy_execution_and_ledger` 3 → 4（同文件 :278）、
  `paper_multi_leg_spread_submits_each_leg_through_single_track_and_reduces_group`
  (1,1,2) → (1,1,3)（`execution_and_multi_leg.rs:274`，每腿成交各多 1 条）、
  `paper_submit_order_runs_queue_pipeline_ledger_and_ack` 3 → 4（`paper_and_strategy_worker.rs:82`）。
  名义额、持仓与成交条数均未变，变的只有费用分录。

## Unreleased — V10 重构收口（2026-09-20）

方案、逐阶段验收口径与实测数字见已删除的 `docs/自研量化框架重构方案-V10.md`（文档收口时移除，原文在 git 历史）
§6 与收口记录；本轮四决策（D1 实盘一律 fail-closed / D2 回测风控同源 / D3 命令面按审计收口 /
D4 外部验收只交付可执行方案）记在同文档 §0、§8.1。

### Fixed（正确性）

- **实盘存在静默降级的无风控提交路径**（P0a，§4.1）：`worker_risk_context` 在
  `instrument_spec_path` 缺失时返回 `Ok(None)`，调用方随即走不带风控的提交分支，而 paper 侧
  对同一缺口是显式拒绝——即缺规格的部署照样下单且无人察觉。现在判定收敛为唯一的
  `require_worker_risk_spec`，Binance / CCXT / Paper 三条链的每个提交点都先过它，缺配置一律
  `FAIL_CLOSED: … 缺少风控配置（instrument_spec_path），拒绝提交订单` 且不留任何成交事实；
  `crates/qx-cli/src/tests/live_submit_fail_closed.rs` 钉住"被判 Failed 且 EventLog 无成交事实"。
- **两条回测入口根本不读风控配置**（P0b，§4.2）：`multi-builtin` 与深度档此前把规则集写死成
  `None`，同一份 `strategy.risk_rules` 在四条链上得到不同门禁（回测"通过"而实盘被拒，或反之更危险）。
  现在命令行型入口经唯一的 `backtest_risk_binding` 取配置，产物里显式写明规则来源
  （`runtime-config` / `conservative-default`），深度档另在清单里声明自己用的是
  `TickBacktestEngine` / `OrderBookBacktestEngine`，不再谎称与 Bar 链同一内核。
  `src/tests/backtest_risk_provenance.rs` 断言四条链得到同一规则集版本。
- **假数据被当作能力入口**（P0c，§4.3/§4.4）：`reconcile` 无参数时手写两组持仓打印差异的行为
  删除，改为必须给本地与远端来源，否则用法错误退出码 2；`run` 的错误文案此前宣称支持
  `backtest` 而 match 无该分支，现帮助表与派发集合由门禁做集合相等校验；
  `all` / `verify` 自带的第二套撮合循环删除，改调 `qx-xingban` 真实内核（只有输入序列是合成的，
  产物里 `input_fingerprint` 明写 `synthetic:*`）。
- **现货多腿归因少一条腿成交**（§10.12，原推送阻断项）：上游合并带进的"现货买入必须由账户
  可用现金支付"检查让 `multi-builtin` 的示例数据首次暴露问题——每腿装配仍沿用默认的
  100_000 USDT，2-BTC 腿第三笔买入的名义 ≈122_400 直接被内核拒成废单，成交额常数从
  `385_200_000_000_000` 掉到 `262_800_000_000_000`。定因排除了两个嫌疑（费用原语换实现前后逐字符
  相同、成交关联号形状回退后同一断言以同样数字失败），并按门禁变异流程证明共享风控实例不是原因
  （`backtest_risk_binding` 每次 `gate()` 都新构造规则集）。修法是补齐装配而非重 bless：每腿起始
  现金按"全帧最高价 × quantity × 2"取足余量且不低于默认值，期望常数一字未改即回到 green；
  M11 把该行退回默认值后测试立刻以同样的数字转红，证明修复是承重的。

### Refactored（架构与边界收敛）

- **概念单点化**（P1a，§4.5/§4.7/§4.8）：`qx-zhenlu::RiskGate` 的默认构造绕过点归零，回测与
  实盘的风控门全部经 `strategy_risk_gate` 构造；`PositionSnapshot` 收在 `qx-protocol` 线格式一处、
  `TargetPosition` 收在一处；对账归一为 `qx-genglu` 的 `order_reconcile_verdict`
  （`Consistent` / `PendingReconcile` / `AutoConverge` / `NeedsHuman`）+ 唯一动作映射，
  Binance / CCXT / EventLog 三处不再各自推导"是否一致"。
- **spread 屏障下沉网关**（P1b，§4.10）：屏障判定与执行只在 `qx_execution::spread_group_barrier`，
  CLI 侧那份薄壳连同"绕过网关的预检"一并删除；五个提交入口一律新增组存储形参，让编译器强制
  每个调用点回答它；命令带 `spread_group_id` 而提交路径未注入组存储不再是"跳过屏障"，
  而是 `FAIL_CLOSED` 拒绝提交。门禁改查"判定原语不得搬回 CLI"。
- **存储写路径与重试退避**（P1c，§4.9）：四个 JSON 文件状态存储的序列化 / schema 版本拒绝 /
  损坏判定 / 读改写事务收敛为 `qx-storage/src/state_envelope.rs` 一份实现（原子替换与追加锁
  逐字节沿用，磁盘兼容是硬约束），并拆出 `src/file/` 目录模块；退避与尝试计数收敛为
  `qx-core::retry`（`Backoff::Fixed|Exponential` + `RetryPolicy`，纯函数、不读系统时钟），
  连接器重连、调度器重试、存储计数三处只承载形状参数并委托它。
- **薄壳 crate 与中文代号**（P2a，§4.11）：四个薄壳 crate 按语义归属并入并留下出处注释——
  `qx-oms` → `qx-zhenlu/src/oms.rs`、`qx-portfolio` → `qx-zhenlu/src/portfolio/`、
  `qx-application` → `qx-execution/src/application.rs`、`qx-fenye` → `qx-core/src/fenye.rs`，
  workspace 成员随之减少；中文代号（牵星 / 观星 / 星板 / 针路 / 更路 / 卯眼榫头，以及并入内核的
  分野）在 README 的模块表逐条给出一句话职责，并补齐此前漏登记的 `qx-api` / `qx-data` /
  `qx-orchestrator` / `qx-risk` 四行、删掉已不存在的 `qx-application` 行（表与 `crates/`
  目录现已逐项对齐）。同一段里"`cli.rs` 是命令名→处理器唯一分派点"的旧描述也随 P2b 改为
  clap 派生口径。能力矩阵的失效证据路径与"行为用例只增不减"普查口径同步收口（§10.10）。
- **CLI 参数框架**（P2b，§4.12）：约 40 个命令的手写字符串派发迁移到 clap 派生，命令表只有一份，
  `qx-cli` 仍是单 binary，未知参数退出码保持 2。
- **超大文件真拆分**（P2c，§4.13）：`qx-xingban/src/ashare.rs` 与 `qx-runtime/src/lib.rs`
  按职责边界拆目录模块，等价性用符号 token 多重集比对证明；登记集规模与逐文件行数只降不升。

### Verification（内部门禁：M1–M11 突变复跑，收口轮）

- 日志 `qx_m1m11_round_231438.log`（本轮抓到）：前置编译 `PRECHECK_EXIT=0`，`FMT_CHECK_EXIT=0`、
  `CLIPPY_EXIT=0`（0 warning 行）、`ARCH_EXIT=0`（107 项不变量全过），基线 `61 passed; 0 failed`。
  十二个红绿对全部咬住：M1 实盘 fail-closed、M2A/M2B 回测读同一份风控配置、M4 run 帮助谎报入口、
  M5 用法退出码、M11 现货多腿起始现金——变异后测试退出 101 并打印点名断言，还原后退出 0；
  M3 与 M6–M10 走静态架构门禁，变异时 `ARCH_EXIT=1` 并打印对应 `[FAIL]` 不变量原文，还原回到 107 项全过。
  `MUTATION_RESIDUE=none`，终态 `FINAL_EXIT=0`、`61 passed; 0 failed`。
- 整工作区同轮复跑（`qx_workspace_test_231340.log`）：51 个测试目标、`601 passed / 0 failed`。
- 验证脚本自身有两处失真在本轮被修掉并记入方案文档 §10.13：还原用 `cp -p` 会把备份的旧 mtime 带回
  去，cargo 按 mtime 判新鲜于是"还原后的绿"跑在变异版 binary 上（现还原后 `touch` 并自检
  `FRESHNESS_STALE`）；P2b 之后 M5 的锚点字符串已随手写派发一起删除，锚点未命中会伪装成红绿同向
  （现 `swap` 未命中即 `SWAP_ABORTED` 终止本轮）。

### Verification（外部验收，D4：本轮不执行）

- `tools/binance_testnet_acceptance.py` 现在逐段记录退出码 / 耗时 / 输出末行，并把带时间戳的
  结果包落到 `maturity/evidence/testnet/<UTC>-{orders|dryrun}/`（不再跑完即删临时目录）；
  缺凭据时仍只跑离线两段并以退出码 3 结束。结果包里的 `sandbox_tested_flip` 是
  `maturity/capabilities.yaml` 翻转的唯一依据。
- 新增 [docs/外部链路验收执行方案-V1.md](docs/外部链路验收执行方案-V1.md)：三段链路的前置条件、
  崩溃后远端未知态的处置程序、以及**当前缺口如实记录**（CCXT 侧没有 `ccxt-submit-order`，
  因此第二交易所的第三段暂不可跑）。
- CI 的 wheel 腿补 macOS 平台；`sandbox_tested` 在未拿到真实外部结果包前全量保持 `false`，
  §5 的"能力齐备、内部闭环、外部未证"结论本轮不变。

### Merged（上游分叉收口，2026-09-20 追加）

- 拉取并合并上游 `6743ae0`（"fix: close audit gaps in execution, backtest and CLI layout"）。
  该提交与本地 V9/V10 在 `296e56e` 分叉，**独立重做了一遍 CLI 巨型文件拆分**（把当时的
  `qx-cli/src/main.rs` 拆成 19 个扁平顶层模块并改用宏命令表），与本仓库的目录模块布局
  （`backtests/`、`venue_runtime/`、`tests/`）和按路径取数的架构门禁互斥。
  合并口径逐 hunk 判定：**结构与 API 形状取本仓库**（受祝福风控构造、`spread_group_barrier`
  单点、`runtime_config/` 目录模块、config 持有 `risk` 字段、存储信封与 `qx-core::retry` 单点），
  **行为与修复移植上游**（`qx-core::fee` 统一 `FeeModel` 并把合约乘数与反向计费基准折进费用价、
  `cost_rules.rs` 费用规则单点、单向净持仓强平、强平按 taker 费率、报表费用与成交额改由成交账本推导）。
- 同一"第二笔成交被静默丢弃"缺陷两侧各修了一次：本仓库按订单定序关联号（V9 §8.2 反向验证 F），
  上游改按成交内容判重。合并后以本仓库口径为准并由既有用例钉住，不保留第二套幂等键推导。
- 被合并丢弃的上游模块随时可用 `git show 6743ae0:<path>` 取回；`crates/qx-{oms,application,portfolio,fenye}`
  在本仓库已由 P2a 并入语义归属 crate，合并未复活它们。

## Unreleased — V9 重构收口（2026-09-19）

方案与逐阶段判定见已删除的 `docs/自研量化框架重构方案-V9.md` §8（文档收口时移除，原文在 git 历史）。

### Fixed（正确性）

- 现货回测费用按 raw 名义额计收：`BarMatchingEngine::fee_price_multiplier` 与 `contract_size`
  同为 SCALE 定点倍数，历史上被当成整数乘数传入，现货手续费被整体压小 10⁹ 倍，
  而既有费用断言全部使用 0 bp 因此无人捕获；`crates/qx-xingban/src/backtest.rs`
  的 `fees_scale_with_raw_notional_for_spot_and_derivative` 现钉住现货与永续两条口径。
- 执行层关联号未按订单定序导致**同一 EventLog 的第二笔订单事实被静默丢弃**（P0）：EventLog 幂等键是
  `{correlation_id}:source:{source_seq}`，而 `execute_paper_submit_effect` 每收到一条 SubmitOrder 命令就把
  `source_seq` 从 0 重新计数，Venue 回报关联号只到 `{worker}:venue:{venue_id}`、成交回报关联号只到
  `{worker}:fill:{source_seq}`。于是多腿 spread 的第二条腿（以及策略第二轮迭代的订单）其 `Accepted`、
  `Filled` 与派生的两条 `LedgerApplied` 全被判为重放丢弃，组状态永远停在 `PartiallyFilled`，账本少记一笔；
  此前所有 Paper 用例与 smoke 都只跑一笔订单，因此从未暴露。现按订单定序
  （`{worker}:venue:{venue_id}:{client_order_id}`、`{worker}:fill:{order_id}:{source_seq}`、
  `paper-execution:market:{instrument}:{source_seq}`），同一订单内仍由 `source_seq` 区分，重复提交的幂等语义不变。
  由 `crates/qx-cli/src/tests_main.rs::paper_multi_leg_spread_submits_each_leg_through_single_track_and_reduces_group`
  逐单断言"每笔腿各留 Accepted + Fill + 双 Ledger 条目"钉住（门禁记录见 V9 §8.2 反向验证 F）。
- 交易所**回报侧**两类会污染账本的事实错误（Phase 4j）：
  (1) CCXT 与 Binance 适配器在订单已终态（`Cancelled`/`Rejected`/`Filled`）后仍会把远端累计量的推进
  当成新增量，凭空补出一条 `Fill`（撤单落地后远端仍有成交的常见场景），Binance 的 `mark_cancelled`
  亦会把终态订单回退成 `PartiallyFilled`；
  (2) 违反 tick/step 精度的成交回报被直接记入账本，而无精度校验。
  现统一拒收：终态不回退（各适配器把远端量映射成事件处先查本地状态——`crates/qx-adapter/src/binance.rs`
  的成交回报与 `mark_cancelled`、`crates/qx-adapter/src/ccxt.rs` 的 `sync_order`，本轮日志计得
  `TERMINAL_GUARD_HITS=6` 处）、归约入口 `crates/qx-runtime/src/pipeline.rs` 对终态后的变更
  只产出 `ReconcileRequired` 事实（拒绝必须以"结果未知"分类返回，否则 worker 会把它当硬错误而不入对账队列），
  绝不伪造成交/撤单。精度闸门落在三条生产回报路径的唯一漏斗
  `ingest_venue_events_with_pipeline`（`crates/qx-execution/src/lib.rs`）而非 `Ledger`，因此回测口径不受影响；
  `crates/qx-cli/src/venue_runtime/binance_stream_worker.rs` 的 Binance 用户流此前拿不到产品规格，
  现经 `worker_report_spec()` + `ingest_venue_events_with_spec` 接入冻结规格
  （模板 `deploy/qianxing.runtime.production.example.json` 的 `binance-user-main` 补 `instrument_spec_path`）。
- `tools/verify_cpp_worker.py` 曾把 `--protocol` 硬编码为 `shared_memory_json`，
  列式协议 `shared_memory_columnar` 从未被真正执行；改为透传协议后两种协议均通过。
- Python wheel 打包：`python/pyproject.toml` 显式收敛 `packages.find` 范围（此前会把
  `python/build/lib/...` 递归打进 wheel），构建脚本按平台改写导入名
  （`_qianxing_native.pyd` / `.so`），并声明 `tzdata; sys_platform == 'win32'`
  使 `zoneinfo` 用例在 Windows 不再整模块报错。
- `cpp/CMakeLists.txt` 在找不到系统 `nlohmann_json` 时回退到 FetchContent，
  Windows/macOS 无需预装依赖即可配置。

### Added（链路与验收）

- Binance Spot **testnet** 拓扑模板 `deploy/qianxing.runtime.binance-testnet.example.json`
  与 fail-closed 验收驱动 `tools/binance_testnet_acceptance.py`
  （无凭据退出 3，`--allow-skip` 供 CI 跑离线半边；重复 `request_id` 必须被幂等拒绝）。
- 后端契约测试：`crates/qx-storage/tests/outbox_backend_semantics.rs` 增加 PostgreSQL
  租约/围栏令牌/重试契约；新增 `crates/qx-storage/tests/nats_jetstream.rs`
  （JetStream 一发一收 + 按 `event_id` 幂等，流与消费者由测试自建自删）。
- CI 从 4 个作业扩为 7 个：新增 `python-wheel`（Linux/Windows × Py 3.10/3.12/3.13）、
  `service-backends`（postgres:16 + `nats -js` 服务容器跑 `--ignored` 契约）、
  `venue-acceptance`；`cpp-sdk` 扩为 ubuntu/windows/macos 三平台并覆盖两种共享内存协议。
- 三家共用的回报契约测试 `crates/qx-execution/tests/venue_report_contract.rs`（937 行）：
  Paper（真实 `PaperVenue` 撮合）、CCXT（脚本化 `CcxtRpc` 报文）、Binance
  （`NoRestTransport` + 真实 `executionReport` 用户流报文）跑同一份
  `assert_venue_report_contract` 断言序列（基线成交与重放幂等 → 乱序旧累计回报不回退 →
  撤单后迟到成交 → 成交后迟到撤单 → off-tick 回报 → off-step 回报），
  全部经同一个 `LiveEventPipeline` 归约，只断言可观察事实（Fill/Cancelled/Reconcile 计数、
  Ledger 条目数、订单状态）。`tools/check_architecture.py` 随之从 22 项扩到 26 项，新增
  "精度闸门只在唯一归约入口生效并转待对账""精度判定只有一个谓词与一个调用点（不在 Ledger/回测侧重复）"
  "每条生产回报路径都带冻结产品规格""三家共用契约测试在位"四条不变量；
  三项注入反向验证（G 摘掉精度闸门、H/I 分别摘掉 Binance 与 CCXT 的终态守卫）都令契约用例转红
  `MUTATED_*_TEST_EXIT=101` 且还原后 `RESTORE[*]=identical` 复绿。
  收口日志：`TEST_EXIT=0`、工作区 `RUST_PASSED=526`（`OK_LINES=68` 个测试套件全部 ok）、
  `--bin qx-cli` 默认 56 / 全特性 59、`ARCH_EXIT=0`（26 项）、Python 43 例 OK、
  同一回测命令两次 `result_hash=b26e1d4d4d430cb1` 一致。
- `python/tests/test_native_extension.py` 新增 Rust 扩展与纯 Python 指纹一致性用例。

### Removed（破坏性清理，决策 2）

- 回测入口从 8 种收敛到 `backtest builtin` / `backtest multi-builtin` 两条。
- `qx-domain`、`qx-kernel` crate 与 `qx-zhenlu` 死导出删除；`RiskGate` 退化为 `RuleSet` 门面。
- 第二个多腿编排入口删除：`MultiVenueSpreadExecutionService`（含 `SpreadExecutionOutcome` 与
  `apply_spread_application_event`）+ `VenueRouterMap` 共 347 行。它从未被生产代码构造，且缺少生产链路
  必需的两道门（无账户级风控预检、无"行情必须来自 EventLog 事实"的 fail-closed 门禁），若启用还会与
  worker 的逐腿提交双写事实。其唯一独有的安全语义（组内有腿结果未知或待补偿时禁止继续提交其余腿）
  收进 `crates/qx-zhenlu/src/lib.rs::SpreadOrderGroup::blocks_new_leg_submission()` 单点谓词，由
  `crates/qx-cli/src/spread.rs::spread_group_barrier()` 在五个腿提交点（Paper 一次性与 worker 循环、
  CCXT、Binance 一次性与 worker 循环）执行前拦截，被拒命令以 `FAIL_CLOSED:` 前缀进控制面终态审计。
  `VenueRouterPort`/`VenuePortAdapter`/`BorrowedVenuePort` 保留（`HedgeRecoveryWorker` 补偿路径在用）。
  架构门禁由 20 项增至 22 项：第二编排入口标识符出现即为违规、屏障谓词全仓唯一定义、
  任何调用腿提交入口的文件必须同时调用屏障。

### Changed（Phase 4：qx-cli 内部模块化，仍是单 binary）

- `crates/qx-cli/src/main.rs` 从 16,359 行降到 3,601 行，按职责拆出 13 个兄弟模块：
  `cli.rs`（命令名 → 处理器的唯一分派点）、`selfcheck.rs`、`config_commands.rs`、
  `strategy_host.rs`、`strategy_contract.rs`、`backtests.rs`、`multi_leg.rs`、`ccxt_facts.rs`、
  `spread.rs`、`venue_runtime.rs`、`worker_entry.rs`、`event_pipeline.rs`、`tests_main.rs`。
  搬迁以整块原样移动 + `pub(crate)` 收口进行，未改任何算法。
- 未知命令从"静默落到 `all` 自校验演示"改为打印 `未知命令: <x>`、帮助与退出码 2。
  既有命令与参数宽松度（`--json` 可出现在任意位置、`--strategy=` 混写等）保持不变，
  因此未引入 clap `Subcommand`；理由与保留项见 V9 §5 Phase 4 与 §8.3。
- CCXT 与 Binance 两条 worker 入口的角色校验合并为 `worker_entry.rs` 里的单张表：
  `VENUE_ROLES` 白名单 + `VenueEntry::{CCXT, BINANCE}` 登记表 + `venue_worker()` 校验入口，
  "存在性→启用→角色→Venue 绑定"四步与错误文案只有一份实现。
- `QX_PYTHON` 解释器解析从 9 处 `std::env::var` 收敛为 `python_interpreter()` 一处。
- `backtest builtin` 与多腿腿级回测补上 `strategy_risk_gate`（前者保守禁空，后者允许对冲空头腿），
  审计时点记录的"空风控门回测入口"至此全部走同一规则内核；进程内合成演示链路保持原样。
- 三条 Bar 回测链的引擎装配收进 `crates/qx-cli/src/backtests.rs::BarBacktestAssembly`：
  乘数 1、`NextBarOpenFillModel`、`ZeroLatency`、`DataTier::Bar`、初始资金 100_000 与
  "缺省即带风控门"只留一份实现，各入口只覆盖会分叉的费用、保证金、风控与种子；
  market spec 读取合并为 `market_spec_with_margin()`（策略/内置/多腿/深度四处，错误文案统一带上路径），
  内置策略执行段合并为 `run_builtin_strategy_on_bars()`。同一轮内两次运行
  `backtest builtin sma_cross` 的 `result_hash` 均为 `b26e1d4d4d430cb1`，装配收敛未破坏确定性。
- 两条此前无测试的内置回测入口补 3 例（共用装配默认口径、输入校验 fail-closed、
  多腿归因产物按腿级真实成交计提且费用等于 taker 5 bp），见 `crates/qx-cli/src/tests_main.rs`。
- `selfcheck::run(&str)` 里残留的第二处命令名分派（`mode == "backtest" || mode == "verify"`，
  其中 `backtest` 分支在 Phase 4 收敛后已永不可达）改由 `cli.rs` 以
  `selfcheck::Scope::{KernelOnly, Full}` 传意；`verify` 仍止于确定性内核、`all` 仍续跑插件装配与
  Paper 冒烟，两者的退出码与阶段结论文本均未变（由 `crates/qx-cli/tests/cli_dispatch.rs` 断言）。
- 新增 `tools/check_architecture.py`（架构不变量自检，落地时 12 项、本轮扩为 14 项）并接入 `ci.yml` 的 `rust-core` 作业：
  已删 crate 不得复活、`QX_PYTHON` 单点读取、命令名分派只允许出现在 `cli.rs`、
  生产代码不得有未登记或无规则的空风控门、`BacktestConfig` 装配字面量唯一、
  market spec 与 `strategy_risk_gate` 保持单一入口、能力矩阵四档状态与证据路径可核验且
  `sandbox_tested` 不得越界为 `true`、单文件行数按 `maturity/line_budgets.yaml` 快照只降不升
  （`--snapshot` 重新生成）。README 的本地验证段同步加入该命令。
- 新增 `crates/qx-cli/tests/cli_dispatch.rs` 3 例集成测试，从二进制外部钉住 `verify` / `all` /
  未知命令三条分派语义；README 快速开始里重复的一行裸 `backtest` 示例已删除。

### Fixed（正确性：Paper 主链路仍在第二套实现上）

- `crates/qx-execution/src/lib.rs::execute_paper_submit_effect`（qx-cli Paper 路径的唯一生产入口）此前仍
  构造端口化之前的遗留 `ExecutionService`，因此 V9 §8.1 的"执行统一走 `PortExecutionService`"对 Paper
  并不成立：遗留实现缺少 gateway 的"空 Venue 回报 = 未知结果 → 待对账"fail-closed 语义。现改为
  `ExecutionGateway`（`PortExecutionService` 别名）+ `BorrowedVenuePort`，风控预检、幂等与事件写入判定
  只由 gateway 一处决定；两条文档注释与 `ExecutionService` 的定位同步收口为"仅由两个多腿编排器复用"。
  改道后订单级风控由 gateway 的 `CanonicalRiskPort` 统一评估：缺 `TradingInstrumentSpec` 的 Paper 提交会以
  `订单级风控缺少 TradingInstrumentSpec` 被拒（本轮实测口径，新用例据此带上 spec；两套实现在这一点上的
  历史差异未做对照，故不声称行为变化方向）。
- 新增 `crates/qx-execution/src/tests.rs::paper_submit_reuses_gateway_idempotency_without_new_facts`：
  从生产入口重投同一 `client_id`，断言返回 `ALREADY_APPLIED_FROM_EVENT_LOG` 且订单数、账簿条目数均不增长。
- `tools/check_architecture.py` 增加两项执行侧不变量（共 14 项）：遗留 `ExecutionService` 不得被
  `qx-execution` 之外的任何路径引用；`execute_paper_submit_effect` 必须出现 `ExecutionGateway::new`
  且不得回退 `ExecutionService::new`。两项均已反向验证（注入占位实现 + 他 crate 注释提及 → 同时 FAIL，
  还原后 `cmp` 字节一致、14 项复绿）。
- `crates/qx-execution` 的 1,244 行内嵌 `#[cfg(test)] mod tests` 拆到 `src/tests.rs`（沿用 qx-cli 的
  `mod tests;` 形态，私有项可见性不变），`src/lib.rs` 从 3,225 行降到 2,072 行；
  `maturity/line_budgets.yaml` 相应登记 43 个超 500 行文件（新增 `src/tests.rs`，`lib.rs` 预算下调）。

### Removed（Phase 3 第 2 项收口：第二套执行实现整体删除）

- 删除端口化之前的 `ExecutionService`（含 238 行 `impl` 与从未被任何调用方使用的 `new_with_spec`）
  和唯一复用它的多腿编排器 `SpreadExecutionService`（文档+结构体+`impl` 136 行），连同只服务两者的
  `apply_spread_venue_events`（14 行），`crates/qx-execution/src/lib.rs` 从 2,072 行降到 1,662 行。
  至此本 crate 内不存在任何绕过 `ExecutionGateway` 的订单副作用路径，多腿编排入口只剩
  `MultiVenueSpreadExecutionService` 一个。删除前实测确认：两者在全仓 Rust 代码里的生产构造点为 0
  （仅 `src/tests.rs` 触达），Paper/Live/CCXT 三条单腿路径与 CLI 多腿路径均已走 gateway。
- 被删编排器的 Paper 多腿生命周期语义没有丢：`spread_execution_submits_all_legs_and_keeps_group_lifecycle`
  改写为 `crates/qx-execution/src/tests.rs::paper_multi_leg_spread_routes_each_leg_and_keeps_group_lifecycle`，
  改由 `MultiVenueSpreadExecutionService` + `VenueRouterMap`（两条腿各挂一个 `PaperVenue` 适配器）驱动，
  断言集合原样保留（无错误、组停在 `Submitting`、补偿目标为空、管线内两笔订单、快照持久化后状态一致）。
- `tools/check_architecture.py` 的执行侧不变量从"限制遗留实现扩散"升级为"禁止其存在"（共 16 项）：
  `ExecutionService`/`SpreadExecutionService` 两个标识符在全仓 Rust 代码中出现次数必须为 0；
  `execute_paper_submit_effect` 函数体必须含 `ExecutionGateway::new`；`qx-execution` 里
  `*SpreadExecutionService` 形态的 `pub struct` 必须恰好只有 `MultiVenueSpreadExecutionService` 一个；
  `pub type ExecutionGateway` 别名定义必须唯一。已知盲区记入 V9 §8.3 第 6 项：该判定是文本级，
  改名后的第三套实现不会被它拦住。
- `maturity/line_budgets.yaml` 用 `--snapshot` 同步：diff 只有两行且均为下降
  （`lib.rs: 2072 → 1662`、`tests.rs: 1244 → 1242`），其余 41 条一字未动。
- `maturity/capabilities.yaml` 的 `multi_leg_execution.limitations` 按实测重写：删除已失效的
  `spread_execution_services_are_constructed_only_in_unit_tests`，改为
  `multi_venue_orchestrator_is_constructed_only_in_unit_tests` 与
  `venue_router_map_has_no_production_registration_site` 两条准确表述。

### Validation（phase4f 轮实测，日志 `/tmp/qx_phase4f_gate.log` + `/tmp/qx_phase4f_mutation.log`，2026-09-19：Paper 并入 ExecutionGateway 单轨 + `qx-execution` 测试模块拆分后复跑）

```text
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=66; RUST_PASSED=523 RUST_FAILED_SUITES=0
cargo test -p qx-cli --bin qx-cli: 54 passed；--features sqlite,postgres,nats: 57 passed
PY_EXIT=0  Ran 43 tests in 0.064s  OK
VALIDATE_EXIT=0  全部自校验通过 ✓
ARCH_EXIT=0      架构不变量自检全部通过 ✓（14 项）
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 paper-e2e=0
                 backtest builtin=0 backtest multi-builtin=0 未知命令=2 binance-worker 非法角色=2
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1（与上一轮同值）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
         live-check 只读校验：network_accessed=False orders_sent=False
执行单轨门禁反向验证：注入占位 gateway + 他 crate 注释提及 ExecutionService → MUTATED_ARCH_EXIT=1
                     （两项 FAIL：Paper 共用 ExecutionGateway / 遗留实现不被外部引用）
                     还原后 cmp 判定两份文件字节一致、RESTORED_ARCH_EXIT=0（14 项）
未提交改动：UNCOMMITTED_PATHS=121（本轮全部工作仍未提交，未获提交授权）
```

`postgres` / `nats` 的契约测试仍以 `#[ignore]` 等待首次服务容器 CI 记录，
`maturity/capabilities.yaml` 中所有 `sandbox_tested` 保持 `false`。

### Validation（phase4g 轮实测，日志 `/tmp/qx_phase4g_gate.log`，2026-09-19：第二套执行实现删除后复跑）

```text
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=66; RUST_PASSED=523 RUST_FAILED_SUITES=0        # 与上一轮逐位相同（本轮是替换测试而非新增）
cargo test -p qx-cli --bin qx-cli: 54 passed；--features sqlite,postgres,nats: 57 passed
PY_EXIT=0  Ran 43 tests in 0.063s  OK
VALIDATE_EXIT=0  全部自校验通过 ✓
ARCH_EXIT=0  ARCH_ITEMS=16   架构不变量自检全部通过 ✓（16 项）
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 paper-e2e=0
                 backtest builtin=0 backtest multi-builtin=0 未知命令=2 binance-worker 非法角色=2
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1（与上一轮同值）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
         live-check 只读校验：network_accessed=False orders_sent=False
门禁反向验证：在 compensation_client_id() 注入一行 `ExecutionService::new(0)`
             → MUTATED_ARCH_EXIT=1（第二套实现已移除 / 行数棘轮 两项 FAIL）
             → RESTORE_CMP=identical、RESTORED_ARCH_EXIT=0（16 项）
未提交改动：UNCOMMITTED_PATHS=121（本轮全部工作仍未提交，未获提交授权）
```

Phase 3 第 2 项的判定随之收窄：单腿与多腿的**副作用实现**已合一（一套 gateway + 一个多腿编排器），
仍未闭合的是**编排入口**合一——CLI 多腿生产路径以逐腿命令 + 组快照归约推进生命周期，
`MultiVenueSpreadExecutionService` 与 `VenueRouterMap` 尚无生产构造点（V9 §8.3 第 7 项）。

### Added（Phase 5 门禁收口：A 股 PIT 结果可区分）

- 新增 `crates/qx-xingban/tests/ashare_pit_asof.rs`（3 例）：同一份录制分红快照在
  发布前/发布后两个研究截止日（`as_of`）下分别加载，走完整引擎后现金差额恰为
  `100 股 × 每股 1 元`、`result_hash` 互不相同；另有"同一截止日两次运行 `result_hash`
  与 `replay_hash` 一致"的可复现钉子。它兑现的是 V9 §5 Phase 5 门禁里此前唯一没有
  证据的条款（L1/L2 与 SQLite 两条早有覆盖，A 股 PIT 这条只写了文字）。
- `tools/check_architecture.py` 由 16 项扩到 20 项，新增 4 条 A 股 PIT 不变量：
  第二道 PIT 闸门（`is_visible_at`/`corporate_actions_visible_at`）出现即为违规、
  可见性谓词与加载闸门调用点各唯一、回测引擎不得自行判定 PIT 可见性、
  上述结果可区分测试必须在位。
- `maturity/capabilities.yaml` 的 `ashare_corporate_action_ledger.evidence` 登记新测试文件，
  `limitations` 增加 `pit_asof_filter_runs_only_at_corporate_action_json_load`；
  冒烟门禁新增 `fast-backtest ashare` 一条命令（phase5b 轮实测退出 0）。

### Removed（Phase 5 收口：第二道 PIT 闸门是零调用者死代码）

- 删除 `AshareCorporateActionEvent::is_visible_at` 与
  `AshareRuleConfig::corporate_actions_visible_at`：全仓零调用，且 `AshareRuleConfig`
  不携带 `as_of`，把它们接进运行时会等于新增语义而非收口。PIT 可见性从此只有
  公司行为 JSON 加载闸门一处判定，`published_at_ms` 字段文档同步改写为"随快照携带供审计"。
  `crates/qx-xingban/src/ashare.rs` 2,006 → 1,978 行，`maturity/line_budgets.yaml`
  重跑 `--snapshot` 后 diff 仅此一条下降，其余 42 条一字未动。

### Changed（Phase 4h：`venue_runtime.rs` 按 Venue 边界拆成目录模块，仍是单 binary）

- `crates/qx-cli/src/venue_runtime.rs`（2,823 行、39 个顶层条目）拆为
  `crates/qx-cli/src/venue_runtime/`：CCXT 侧 `ccxt_execution / ccxt_live_bars /
  ccxt_market_worker / ccxt_reconcile_worker`，Binance 侧 `binance_venue /
  binance_stream_worker / binance_reconcile / binance_submit`，跨 Venue 的
  `worker_runtime`（worker 路径解析、风控上下文、共享提交 effect），Paper 侧
  `paper_submit / paper_worker`，外加 27 行 `mod.rs` 做 `pub(crate) use …::*;` 再导出。
  最大子文件 423 行，12 个文件全部低于 500 行门槛，`maturity/line_budgets.yaml` 里的
  超 500 行文件随之从 43 个减到 42 个。
- 纯搬迁：39 个条目逐个与原文件比对，只有 `LiveStrategyBarSpec` 因跨子模块读字段把
  7 个字段升为 `pub(crate)`，其余逐字相同；crate 根的
  `pub(crate) use venue_runtime::*;` 一字未改，其他模块与 `tests_main.rs` 的引用路径不变。
- `tools/check_architecture.py` 的"命令分派单点"检查从单层 `glob("*.rs")` 改为
  `rglob("*.rs")`——这是拆目录的前置条件（旧写法会让子目录里的第二分派点逃出检查，
  此前作为已知盲区记录在 V9 §8.3 第 6 项），违规输出现在带模块内相对路径。
- `maturity/capabilities.yaml` 三条指向旧单文件的证据路径改指 `venue_runtime/` 内的新文件。

### Validation（本轮实测，日志 `/tmp/qx_phase4h_gate.log`，2026-09-19：venue_runtime 拆目录模块后复跑）

```text
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=67; RUST_PASSED=526 RUST_FAILED_SUITES=0     # 与上轮（拆分前）逐位相同
cargo test -p qx-cli --bin qx-cli: 54 passed；--features sqlite,postgres,nats: 57 passed
venue_runtime/：12 个文件 2,857 行，最大 423 行（ccxt_execution.rs），全部 < 500
PY_EXIT=0  Ran 43 tests in 0.063s  OK
VALIDATE_EXIT=0  全部自校验通过 ✓
ARCH_EXIT=0  ARCH_ITEMS=20   架构不变量自检全部通过 ✓（20 项，与上轮同数）
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 paper-e2e=0
                 backtest builtin=0 backtest multi-builtin=0 fast-backtest ashare=0
                 未知命令=2 binance-worker 非法角色=2
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1（与拆分前同值）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
         live-check 只读校验：network_accessed=False orders_sent=False
反向验证 C（递归分派检查真的生效）：在 venue_runtime/paper_worker.rs 注入 `if command == "paper"`
         → MUTATED_ARCH_C_EXIT=1，唯一 FAIL 是
           `命令名分派只存在于 cli.rs — 额外分派点 ['venue_runtime/paper_worker.rs:…']`
         → RESTORE_C_CMP=identical、RESTORED_ARCH_EXIT=0（20 项）
反向验证 D（旧单文件路径不得当证据）：把能力矩阵一条路径改回已删除的 venue_runtime.rs
         → MUTATED_ARCH_D_EXIT=1，`能力矩阵证据路径全部存在 — 失效路径 ['crates/qx-cli/src/venue_runtime.rs']`
         → 改回后 RESTORED_ARCH_D_EXIT=0
未提交改动：UNCOMMITTED_PATHS=122（Phase 0–6 全部工作仍未提交，未获提交授权）
```

同一轮还修正了门禁脚本自身的一处错误：`fast-backtest` 冒烟最初被写成
`fast-backtest ashare deploy/qianxing.runtime.ashare.example.json`（把 venue 名当 manifest
路径传）而退出 2；改为 `fast-backtest deploy/qianxing.fast-backtest.ashare.example.json`
后上面的 0 才是真实结果——上面的日志是修正后的复跑。

### Validation（phase5b 轮实测，日志 `/tmp/qx_phase5b_gate.log`，2026-09-19：A 股 PIT 收口后复跑）

```text
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=67; RUST_PASSED=526 RUST_FAILED_SUITES=0     # 上一轮 66 / 523，+1 目标 +3 例全在新测试文件
cargo test -p qx-xingban --test ashare_pit_asof: 3 passed
cargo test -p qx-cli --bin qx-cli: 54 passed；--features sqlite,postgres,nats: 57 passed（与上一轮相同）
PY_EXIT=0  Ran 43 tests in 0.176s  OK                 # 该轮无 Python 改动，用例数持平
VALIDATE_EXIT=0  全部自校验通过 ✓
ARCH_EXIT=0  ARCH_ITEMS=20   架构不变量自检全部通过 ✓（20 项）
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 paper-e2e=0
                 backtest builtin=0 backtest multi-builtin=0 fast-backtest ashare=0
                 未知命令=2 binance-worker 非法角色=2
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1（与上一轮同值）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
         live-check 只读校验：network_accessed=False orders_sent=False
门禁反向验证 A（死代码不得复活）：把 `is_visible_at` 原样注回 ashare.rs
         → MUTATED_ARCH_A_EXIT=1（第二道 PIT 闸门 / 行数棘轮 1985>1978 两项 FAIL）
         → RESTORE_A_CMP=identical
门禁反向验证 B（加载闸门必须真的在过滤）：`if !contract.visible_at(..)` 改成恒不隐藏
         → MUTATED_PIT_TEST_EXIT=101，3 例中 2 例转红，"同一截止日可复现"一例仍 ok
           （它只钉确定性、不依赖过滤，符合预期）
         → RESTORE_B_CMP=identical、RESTORED_PIT_TEST_EXIT=0（3 passed）、RESTORED_ARCH_EXIT=0（20 项）
未提交改动：UNCOMMITTED_PATHS=122（Phase 0–6 全部工作仍未提交，未获提交授权）
```

该轮同时记下门禁自身的边界（V9 §8.3 第 6 项，其中"命令分派只扫单层目录"的盲区已在 Phase 4h 闭合）：新增 4 条与既有各条同为文本/正则级形状检查，
谓词改名可绕过、测试辅助函数改名会误报缺失；命令分派单点那条只扫 `crates/qx-cli/src/*.rs`
单层 glob，后续把 `venue_runtime.rs` 拆进子目录前必须先改成递归。

### Changed（Phase 4k：内核 `Ledger` 按资产类别拆成目录模块，只拆文件不改语义）

- `crates/qx-core/src/ledger.rs`（2,876 行、42 个 `pub fn`，把现货成交、合约成交与乘数、现金
  （入金/资金费/利息/交收/调整/强平）、公司行为、权证与认购、可转债转股、查询投影混在一个文件里）
  拆为 `crates/qx-core/src/ledger/`：`fill.rs` 261 / `cash.rs` 127 / `corporate_action.rs` 173 /
  `rights.rs` 255 / `subscription.rs` 367 / `query.rs` 341，外加 `mod.rs` 373 行持有全部类型定义、
  `pub struct Ledger` 字段与唯一的归约入口 `apply_entry`。每个子文件恰含一个 `impl Ledger` 块，
  子模块直接读写 `Ledger` 的模块私有字段，因此没有为了拆文件而放宽任何 API：
  `crates/qx-core/src/lib.rs` 的 `pub use self::ledger::{…}` 出口一字未改。
- 18 例内核账簿用例从 `ledger.rs` 尾部的 `#[cfg(test)] mod tests` 搬到
  `crates/qx-core/tests/ledger.rs`（1,031 行），改成只用公开 API 的集成用例；`cargo test -p qx-core`
  从"lib 52 例"变成"lib 34 例 + 集成 18 例"，`CORE_TEST_FNS=52` 不变。
- `tools/check_architecture.py` 由 26 项扩到 30 项：新增"内核 `Ledger` 单文件已拆分且不得复活"
  "账簿状态结构体只有一处定义""账簿归约实现按资产类别分文件，且不在目录外另起 `impl Ledger`"
  "账簿各子模块都在单文件行数门槛之内"。`maturity/line_budgets.yaml` 登记数 42 → 41
  （完整 diff 只有一行：删掉 `crates/qx-core/src/ledger.rs: 2876`）；
  `maturity/capabilities.yaml` 的 `ashare_corporate_action_ledger` 证据路径改指新模块与新的集成用例。
- 中立性的额外证据（不只靠"测试没变红"）：把拆分前后所有行做归一化（去空行、`//!` 文档行、
  `use`/`mod` 行、裸 `}` 与 `impl Ledger {` 骨架行，并把 `pub(super) fn` 折回 `fn`）后取多重集比对，
  两侧各 2,578 行且双向差集为空（`CARVE_EQUIV_EXIT=0`）。
- 该轮留下的已知形状例外：行数棘轮只扫 `crates/*/src/**/*.rs`，因此 1,031 行的
  `crates/qx-core/tests/ledger.rs` 不在登记集内（V9 §8.3 第 5 项已记）。

### Validation（phase4k 轮实测，日志 `/tmp/qx_phase4k_gate.log`，2026-09-19：内核 Ledger 拆目录模块后复跑）

```text
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0
OK_LINES=69; RUST_PASSED=526 RUST_FAILED_SUITES=0     # 用例总数与上轮逐位相同；68 → 69 来自新测试二进制
cargo test -p qx-core: 34 passed（lib）+ 18 passed（tests/ledger.rs）+ 0 passed（doc），CORE_TEST_FNS=52
cargo test -p qx-execution --test venue_report_contract: 1 passed
cargo test -p qx-cli --bin qx-cli: 56 passed；--features sqlite,postgres,nats: 59 passed
src/ledger/：cash 127 / corporate_action 173 / fill 261 / mod 373 / query 341 / rights 255 /
             subscription 367 行；tests/ledger.rs 1,031 行；合计 2,928
IMPL_LEDGER_BLOCKS=7  IMPL_LEDGER_OUTSIDE_DIR=0  OLD_SINGLE_FILE_EXISTS=no
CARVE_EQUIV_EXIT=0（归一化 2,578 → 2,578，双向差集为空）  REGISTERED_OVERSIZED=41
PY_EXIT=0  Ran 43 tests in 0.063s  OK
VALIDATE_EXIT=0  全部自校验通过 ✓
ARCH_EXIT=0  ARCH_ITEMS=30   架构不变量自检全部通过 ✓（26 → 30 项）
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 runtime-check-binance=0 paper-e2e=0
                 backtest builtin=0 backtest multi-builtin=0 fast-backtest ashare=0
                 未知命令=2 binance-worker 非法角色=2
                 runtime-check production=2（模板引用部署机绝对路径，本机必然退 2，记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1
         与 Phase 4j 基线同值（DETERMINISM=same、HASH_VS_BASELINE=same）
门禁反向验证 J（不得在目录外另起 impl Ledger）：在 crates/qx-core/src/engine.rs 追加一个
         `impl Ledger { pub fn stray_impl_probe(…) -> u64 { 0 } }`
         → MUTATED_J_ARCH_EXIT=1（目录外 {'crates/qx-core/src/engine.rs': 1}）
         → RESTORE[J]=identical、RESTORED_J_ARCH_EXIT=0
门禁反向验证 K（被拆掉的 2,876 行单文件不得复活）：把原文件原样注回
         → MUTATED_K_ARCH_EXIT=1，四条同时转红：单文件复活 / `pub struct Ledger` 两处定义 /
           目录外 {'crates/qx-core/src/ledger.rs': 2} / 行数棘轮"2876 行未登记"
         → 确认 OLD_SINGLE_FILE_REMOVED=yes、RESTORED_K_ARCH_EXIT=0
用例反向验证 L（搬进 tests/ 的 18 例仍咬得住语义）：把 `apply_position_state_delta` 判定平仓方向的
         `if current > 0 {` 改成 `if current < -1 {`（多/空已实现盈亏符号分支互换）
         → MUTATED_L_TEST_EXIT=101、L_FAILED_CASES=3，含 tests/ledger.rs:285
           （accounts_and_realized_pnl_are_isolated）与 :452
           （multiplier_is_preserved_in_realized_pnl_replay）
         → RESTORE[L]=identical、RESTORED_L_TEST_EXIT=0（lib 34 + 集成 18 两侧复绿）
未提交改动：UNCOMMITTED_PATHS=131（Phase 0–6 全部工作仍未提交，未获提交授权）
```

### Removed（Phase 4l：持仓概念的第二份实现是逐字复制，删）

- `crates/qx-zhenlu/src/lib.rs` 的 `pub struct PositionSnapshot`（五个 `i128` 字段）与它的
  `impl`（`new` / `new_with_multiplier`（含 `multiplier.max(1)` 钳位）/ `with_hedge_legs` /
  `active_qty_for` / `canonical_position`）、`crates/qx-genglu/src/lib.rs` 的同名结构体，
  都是 `qx_risk::OrderRiskPosition` 的逐字复制：字段一一对应，`active_qty_for` 除 `qx_core::` 路径前缀
  与 `&self`/`self` 接收者写法外完全相同。三个构造函数搬到 `OrderRiskPosition` 上
  （孤儿规则不允许在 zhenlu 给外部类型写 impl），两份重复结构删除，
  `RiskContext` / `legacy_gate_context` / `RiskGate::check*` 的签名随之改用 `&OrderRiskPosition`。
- `qx-genglu::reconcile_positions` + `position_map` + 唯一由它构造的 `Discrepancy::PositionMismatch`
  变体删除（`qx-genglu/src/lib.rs` 627 → 559 行，`qx-zhenlu/src/lib.rs` 2,318 → 2,210 行）。
  删除依据是零调用者 + 职责已有归属：
  持仓快照在生产线由 CCXT worker 的 `fetch_positions` 采集
  （`crates/qx-cli/src/venue_runtime/ccxt_reconcile_worker.rs`）、由 Binance 对账 worker 以
  `position_snapshots_count` 上报（`binance_reconcile.rs`），而 genglu 那份既不接风控也不接 fail-closed 门禁。
- `OrderIntent::validate` / `OrderIntent::validate_against` 及只为它们服务的私有自由函数
  `validate_reduce_only(order, position)` 删除。该函数与 `qx_risk::OrderRiskContext::validate_reduce_only`
  逐字相同（连 `"reduce_only 订单必须只减少目标持仓腿且不得反向穿仓"` 文案都一致），
  而 reduce-only 判定在 `RiskGate::check → RuleSet::evaluate_rules_only → rule_violations`
  这条活路径上已由规范实现承担（`crates/qx-risk/src/rules.rs:203`）。
  原 zhenlu 用例 `rebalance_intent_rejects_quantity_overflow` 里对死校验器的那半段断言随之删除，
  保留 `target_qty: i128::MIN` 时 `rebalance_intent(...)` 返回 `None` 的断言。
- 核实后**保留**的两组同名概念：`Bar` 两份（`qx_guanxing::Bar` 是引擎六列定点值对象，
  从数据集进入引擎只有一次投影 `impl From<&BarFrame> for Vec<Bar>`；`qx_data::schema::Bar` 是数据集侧
  逐条记录，全仓只有 `qx-data` 内部使用、没有任何一处转成引擎 Bar）；
  Intent 四份（`StrategyOrderIntent` Rust SDK 原生 → `StrategyContractIntent` 跨语言 JSON 线格式，
  投影只有 `StrategyContractOutput::from_native_decision` 一处 → `QxOrderIntent` 是 `#[repr(C)]` ABI 镜像，
  消费方为 `cpp/include/qianxing_strategy.h` 与 `cpp/examples/momentum_strategy.cpp` →
  `OrderIntent` 是风控前的下单意图，唯一调用点 `crates/qx-cli/src/strategy_contract.rs` 的
  `rebalance_intent(...).into_order()`）。它们是分层投影而非重复实现，只做登记不做合并。

### Changed（Phase 4l：概念权威定义进登记表，架构不变量 30 → 41 项）

- `tools/check_architecture.py` 新增 `concept_registry_check()`，共 11 项：
  8 个概念名（`PositionState` / `OrderRiskPosition` / `PositionSnapshot` / `Bar` / `OrderIntent` /
  `StrategyOrderIntent` / `StrategyContractIntent` / `QxOrderIntent`）的"定义位置与登记表一致"——
  登记表外的新定义与登记表内被搬走或改名的定义**两侧都报红**；再加
  "数据集列式 BarFrame 到引擎 Bar 只有一次投影定义"、
  "已删除的第二套持仓归约与死校验器不得复活"（`reconcile_positions` / `validate_against` / `position_map`
  三个标识符全仓命中数必须为 0，本轮实测 `DEAD_IDENTS=0`）、"持仓可用量判定只有一个实现"
  （实测 `ACTIVE_QTY_FOR=1`，唯一实现是 `crates/qx-risk/src/lib.rs:114`）。
- 改名波及的引用点全部接线：`qx-cli`（`main.rs` / `strategy_contract.rs` / `tests_main.rs` /
  `venue_runtime/worker_runtime.rs`）、`qx-execution`（`lib.rs` 与 `src/tests.rs`）、
  `qx-xingban`（`backtest.rs` / `orderbook_backtest.rs`）、`qx-risk/tests/risk_parity.rs`、
  `qx-zhenlu/tests/risk_projection.rs`。`qx_protocol::PositionSnapshot`（交易所/账户回报线格式）
  与被误改到的同名引用已复原，全仓该名字只剩 `qx-protocol` 一处定义。
- `maturity/line_budgets.yaml` 登记条数不变（41），本轮触及的 5 个登记项之和 8,331 → 8,158 行：
  `crates/qx-zhenlu/src/lib.rs` 2,318 → 2,210、`crates/qx-genglu/src/lib.rs` 627 → 559，
  另有 `crates/qx-execution/src/lib.rs`、`crates/qx-xingban/src/backtest.rs`、
  `crates/qx-xingban/src/orderbook_backtest.rs` 各 +1 行——类型改名后 `use` 从 `qx_zhenlu` 组里
  拆成独立一行，是本轮唯一的增长，已逐条审阅。

### Validation（本轮实测，日志 `/tmp/qx_phase4l_gate.log`，2026-09-19：持仓概念收敛 + 概念登记表落地后复跑）

```text
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=69; RUST_PASSED=526 RUST_FAILED_SUITES=0     # 与 Phase 4k 逐位相同
qx-core: tests/ledger.rs 18 passed / lib 34 passed；venue_report_contract 1 passed
cargo test -p qx-cli --bin qx-cli: 56 passed；--features sqlite,postgres,nats: 59 passed
概念清点: PositionState=1 OrderRiskPosition=1 PositionSnapshot=1 Bar=2 OrderIntent=1
          StrategyOrderIntent=1 StrategyContractIntent=1 QxOrderIntent=1
ACTIVE_QTY_FOR=1  DEAD_IDENTS=0  文件规模 zhenlu 2,210 / genglu 559 / risk 393
ARCH_EXIT=0  架构不变量自检全部通过 ✓（41 项，Phase 4k 的 30 项 + 本轮 11 项）
PY_EXIT=0  Ran 43 tests in 0.064s  OK     VALIDATE_EXIT=0  全部自校验通过 ✓
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 runtime-check-binance=0 paper-e2e=0
                 backtest builtin=0 backtest multi-builtin=0 fast-backtest ashare=0
                 未知命令=2 binance-worker 非法角色=2
                 runtime-check production=2（模板引用部署机绝对路径，本机必然退 2，记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 Phase 4k 基线逐位相同
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（四次变异实验后工作树逐字节校验、无 .bak4l 残留）
UNCOMMITTED_PATHS=131
```

反向验证四次（同一段日志）：M 在 genglu 另写 `pub struct PositionSnapshot` → 登记项报红为
`实际 ['crates/qx-genglu/src/lib.rs', 'crates/qx-protocol/src/lib.rs']`（`MUTATED_M_ARCH_EXIT=1`）；
N 在回测内核另起 `fn active_qty_for` → 唯一实现项报红（`MUTATED_N_ARCH_EXIT=1`）；
O 注回 `pub fn reconcile_positions()` → "不得复活"项报
`{'reconcile_positions': ['crates/qx-genglu/src/lib.rs']}`（`MUTATED_O_ARCH_EXIT=1`）；
P 在第三处写 `pub struct Bar` → Bar 登记项报红（`MUTATED_P_ARCH_EXIT=1`）。
四次都还原到 `RESTORE[x]=identical` 且 `RESTORED_x_ARCH_EXIT=0`。

环境事实（本轮实测发现，已写进门禁脚本首行 `QX_PYTHON=…`）：`cargo test -p qx-cli --bin qx-cli` 的两条
Python strategy worker 用例依赖 `python_interpreter()` 的解释器解析，缺省回落 `python`；本机 `python` 是
WindowsApps 占位桩，不设 `QX_PYTHON` 时实测 `54 passed; 2 failed`，两条都 panic 于
"Strategy worker 已关闭输出"（当轮记作 `crates/qx-cli/src/tests_main.rs` 第 2812 / 2856 行；那个文件之后拆成 `src/tests/` 目录，两条用例今在 `crates/qx-cli/src/tests/e2e_and_python_contract.rs:433` 与 `:477` —— V13 R1-F 重钉）。
显式指向可用解释器后 `56 passed; 0 failed`。属本机环境约束而非代码回归（CI 侧 `python` 真实可用），
但它记下一条尚未收的 fail-closed 缺口：那条 `unwrap_or_else(|_| "python".into())` 回落找不到解释器时
不给任何提示。

### Changed（Phase 4m：运行时配置面 fail-closed，架构不变量 41 → 50 项）

- 新增 `crates/qx-runtime/src/worker_policy.rs`（322 行）：`FieldScope`（`Required`/`Allowed`/`Forbidden`）、
  `WorkerRoleFieldScopes`（九个字段组）、`WorkerRole::field_scopes()`（逐角色显式声明，无通配臂，
  新增变体在编译期必须补表）、`WorkerRole::is_venue_role()` / `uses_private_venue()`、
  `ALL_WORKER_ROLES`、`RoleFieldStatus` 与 `WorkerConfig::role_field_status()`。角色能配哪些字段
  第一次成为类型系统里的一张表，而不是散落在 `validate()` 与 CLI 分支里的判断。
  策略表逐字取自 18 份运行时模板与每个字段的实际消费点：`Api` 角色的字段全仓无人读取，
  `MarketData` 可以合法携带 CCXT 私有端点凭据，`UserStream` 会复用同一份冻结规格做精度预检。
- 12 个运行时配置结构体逐个加 `#[serde(deny_unknown_fields)]`：把 `max_order_notional_raw` 拼成
  `max_order_notional_raws` 不再是"这条风控没配"，而是启动即失败。为兼容 deploy 模板里的中文说明键，
  `RuntimeConfig::from_json` 在反序列化前递归剥离 `_` 前缀键，且剥离发生在指纹计算之前，
  加注释不会改变 `config_fingerprint`。
- `RuntimeConfig::validate()` 的 worker 循环改为先调 `worker.role_field_status()` 再判 `enabled`：
  角色必填三件套、"该角色永不读取却配了它"、凭据来源二选一且内容有效、`paper_initial_cash_raw`
  只能配在 Paper 场地，四类判定统一在策略表一处；被删的重复实现包括角色 `match`、
  MarketData/UserStream 的 endpoint 必填分支、Paper 场地初始资金分支，以及只在
  `is_binance` 分支里生效的凭据配对（凭据判定现在与 Venue 无关）。
- `qx-cli` 侧的角色白名单并入同一张表：`worker_entry.rs` 的 `const VENUE_ROLES` 删除，
  未知角色提示文案改由 `ALL_WORKER_ROLES.iter().filter(|role| role.is_venue_role())` 生成，
  `main.rs` 里手写的五角色 `worker_credentials_ready` 列表改判 `role.is_venue_role()`。
- `tools/check_architecture.py` 新增 `runtime_config_fail_closed_check()` 九项：结构体全部拒绝未知键、
  名单与源码一致（名单不再按结构体名字后缀猜测，而是扫"行首 `pub struct` + 派生 `Deserialize`"这一事实，
  并与 `LOOSE_DESERIALIZE_STRUCTS` 这 8 个刻意保留宽松反序列化的契约/快照结构体做差集比对——
  新写一个可反序列化的结构体却不进这两张表之一即报红）、注释键剥离只有一处且被 `from_json`
  使用、策略表只有一处声明、不得用通配臂、覆盖全部 `WorkerRole` 变体、凭据判定与 Venue 无关
  （`worker_policy.rs` 出现 `is_binance` 即红）、字段可见性判定先于 `enabled` 短路、
  角色白名单不得在策略表之外另抄一份。属性级判定统一走新的 `struct_attribute_blocks()`，
  因此属性块里再插别的属性或注释都不会误判为"缺少 `deny_unknown_fields`"。
- `crates/qx-runtime` 的角色策略用例（8 例）写在 `crates/qx-runtime/tests/worker_policy.rs`，
  只用公开 API；`deploy/qianxing.runtime.production.example.json` 的 api worker 删掉一条无人读取的
  `symbols`，而不是放宽策略表。

### Validation（phase4m 轮实测，日志 `/tmp/qx_phase4m_gate.log`，2026-09-19：运行时配置面 fail-closed 落地后复跑）

```text
QX_PYTHON=C:\Users\Administrator\AppData\Local\Temp\qxvenv\Scripts\python.exe
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=70; RUST_PASSED=536 RUST_FAILED_SUITES=0        # Phase 4l 的 526 + 本轮新增 10 例
qx-core: tests/ledger.rs 18 passed / lib 34 passed；venue_report_contract 1 passed
qx-runtime（--no-fail-fast）: RUNTIME_SUITES=4 RUNTIME_PASSED=57 RUNTIME_FAILED=0
  含 tests/worker_policy.rs 8 例 + lib 两条配置面用例
cargo test -p qx-cli --bin qx-cli: 56 passed；--features sqlite,postgres,nats: 59 passed
18 份运行时模板逐项 config validate: TEMPLATES_VALIDATED=17/18
未知键端到端: TYPO_EXIT=2（错误文本含 max_order_notional_raws）
注释键: COMMENT_KEYS_INJECTED=2 → COMMENT_ACCEPT_EXIT=0
        FINGERPRINT_STABLE=same（4d407ff36051fc81b1702bc0ef3cdd88df7d4bdc3af1ad801d4d81b6efe938c8）
非法角色字段: ROLE_FORBIDDEN_EXIT=2 / 禁用 worker: DISABLED_BYPASS_EXIT=2
  两者文案均为「credential_env 不能配置在 Api 角色；该角色的运行路径不会读取它」
ARCH_EXIT=0  架构不变量自检全部通过 ✓（50 项，Phase 4l 的 41 项 + 本轮 9 项）
PY_EXIT=0  Ran 43 tests in 0.067s  OK     VALIDATE_EXIT=0  全部自校验通过 ✓
CLI 冒烟退出码：verify=0 all=0 runtime-check=0 runtime-check-binance=0 paper-e2e=0
                 config-validate=0 backtest builtin=0 backtest multi-builtin=0 fast-backtest ashare=0
                 未知命令=2 binance-worker 非法角色=2
                 runtime-check production=2（模板引用部署机绝对路径，本机必然退 2，记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 Phase 4j/4k/4l 基线逐位相同
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（八次变异实验后工作树逐字节校验、无 .bak4m 残留）
UNCOMMITTED_PATHS=133
```

本轮唯一的行数增长项是 `crates/qx-runtime/src/lib.rs` 3,566 → 3,660（`deny_unknown_fields` × 12 +
`validate()` 改判 + 两条配置面用例），删除侧同量级：`crates/qx-cli/src/main.rs` 3,601 → 3,594、
`crates/qx-cli/src/worker_entry.rs` 594 → 583（两处手抄角色白名单并入策略表）；
新文件 `worker_policy.rs` 322 行与 `tests/worker_policy.rs` 222 行均低于 `OVERSIZED = 500`，无需登记。
`BUDGET_DIFF_EXIT=1` 只包含上述三项，本轮触及的 3 个登记项之和 7,761 → 7,837、41 条求和 59,785 → 59,861，
即棘轮落地以来**第一次净增 +76 行**，已逐条审阅后重新快照（并把上一轮遗留的偏高条目 3,662 收紧到实测的 3,660），
理由与边界写进 V9 §8.3 第 5 项。

反向验证八次（同一段日志，每次 `cp` 备份 → 注错 → 复跑 → `cp` 还原 → `cmp` 必须 `identical` → 复跑必须 `RESTORED_x_ARCH_EXIT=0`）：
Q 摘掉 `WorkerConfig` 的 `deny_unknown_fields` → "结构体全部拒绝未知键"报
`缺少 deny_unknown_fields: ['WorkerConfig']`，且 `runtime_config_rejects_unknown_keys_and_strips_comment_keys` 转红；
R 把策略表改回 `_ =>` 通配臂 → "逐角色声明"与"覆盖全部 `WorkerRole` 变体"同时报红，并列出
枚举 10 个变体 / 策略表 6 个的差集；
S 把凭据来源判定收回 `is_binance` 分支 → "凭据判定与 Venue 无关"报红，
`binance_private_workers_require_one_valid_credential_source` 与配置面用例双双转红；
T 把 `enabled` 短路挪到字段策略之前 → "字段可见性判定先于 `enabled` 短路"报红，
禁用 worker 的绕过用例复跑 `44 passed; 1 failed`；
U 在 `worker_entry.rs` 另抄一份角色白名单 → "角色白名单不得在策略表之外另抄一份"报
`重复出现于 ['crates/qx-cli/src/worker_entry.rs']`；
V 让 `from_json` 不再剥离注释键 → "注释键剥离只有一处实现且被 `from_json` 使用"报红。
W 在 `lib.rs` 插入一个未登记的 `pub struct RuntimeLimits`（派生 `Deserialize`、不带 `deny_unknown_fields`）
→ `MUTATED_W_ARCH_EXIT=1`，"运行时配置结构体名单与登记表一致"把三张集合全印出来
（名单 12 / 宽松 8 / 源码可反序列化 21，多出的正是 `RuntimeLimits`）——证明名单不再靠名字后缀猜测；
X 把 `TlsPaths` 的 `#[serde(deny_unknown_fields)]` 从"紧邻声明行"挪到 `#[derive(…)]` 之上（等行数改写）
→ `MUTATED_X_ARCH_EXIT=0`，两条属性级判定仍 `[PASS]`，说明判定读的是整个属性块而不是两行相邻的形状
（旧的紧邻正则会在这里误报缺失，反而逼后来人把属性顺序写死）。

环境事实（本轮新增三条，均已固化进 `qx_phase4m_gate.sh`）：
(1) `config validate` 按**配置文件所在目录**解析模板内的相对引用，把模板 `cp` 到别处再校验必然退 2，
所以注释键这条断言的证据改用 `config fingerprint`（指纹仅在 `RuntimeConfig::from_json` 成功后计算）；
(2) `python -c` 源码里的 `/tmp/...` 路径不会被 MSYS 转换而 argv 会，脚本改用 `SW=$(cygpath -w /tmp/qx4m_scratch)`，
避免"退出码看着正常、实际读的是另一个文件"；
(3) 期望"仍绿"的变异实验必须等行数改写：第一版 X 在两行属性之间插了空行与注释，`lib.rs` 由 3,660 变 3,662，
于是 `MUTATED_X_ARCH_EXIT=1` 红在**行数棘轮**那条而不是被检的属性判定上——结论正确、证据错项，
改成 `#[serde(deny_unknown_fields)]` 与 `#[derive(…)]` 两行互换后才是干净的"属性块形状无关"反证。
此外 `18 份模板` 中必然失败的
`qianxing.runtime.production.example.json` 只败在 `[FAIL] strategy.research_snapshot_path` /
`dataset_bundle_path` 两条 `/var/lib/qianxing/research/*` 部署机绝对路径，属结构校验之外的记录性条目。

### Changed（Phase 4n：跨语言 worker 启动/无响应失败必须自证原因，架构不变量 50 → 54 项）

- `crates/qx-cli/src/main.rs`：`python_interpreter()` 的 `unwrap_or_else(|_| "python".into())`
  静默回落改为 `python_interpreter_origin()`，返回「解释器路径 + 来源」二元组（来源只有两种文案：
  `来自 QX_PYTHON` 与 `QX_PYTHON 未设置，回落 PATH python`）。`QX_PYTHON` 的读取点仍只有
  `python_interpreter()` 一处，所以原有一条门禁继续有效。
- `crates/qx-cli/src/strategy_host.rs`：`WorkerProcess` 新增 `diagnostic_program` 字段（Python 路径把来源
  一起编进去），并新增单一诊断出口 `death_note()` —— 一次给出「程序名（含来源）+ 子进程状态
  （`try_wait()` 的退出码 / 信号终止 / 仍在运行 / 退出码不可读）+ stderr 尾部」，缺 stderr 时直接提示
  "若该程序是 WindowsApps 的 python 占位桩，请把 QX_PYTHON 指向可用解释器"。它接满四个原本各说各话的
  失败出口：共享 ring 超时、管道响应超时、响应通道断开、读线程送上来的"worker 已关闭输出"协议错误
  （最后一条此前被裸解包冒泡，把已抓到的 stderr 与退出码整个丢掉）；`spawn()` 失败则新增独立文案
  `启动 … worker 失败: <程序> 无法执行: <os 错误>`。诊断在 `child.kill()` **之前**取，否则退出码读不到。
- `crates/qx-adapter/src/ccxt.rs`：同一处"提交结果未知"的 EOF 文案追加死掉的是哪个程序，但**不改**
  `CCXT Worker 已退出，提交结果未知` 这段安全语义（订单是否已提交是资金安全问题，不能被诊断文本稀释）；
  `spawn()` 失败同样点名解释器。
- `strategy_contract.rs:56` 与 `workers.rs:306` 两处 `external_executable`（"外部 Strategy"）调用点显式传
  `None`：诊断只写程序名，不会给一个 CPython 之外的进程编上"QX_PYTHON 来源"。
- 新增黑盒用例 `crates/qx-cli/tests/worker_launch_diagnostics.rs`（128 行，3 例）：(1) 不存在的解释器
  → 退出码 2 且失败信息含该程序名与"无法执行"；(2) "存在但对协议完全沉默"的程序 → 必须同时给出
  `程序=`、来源、子进程状态与 `stderr=`；(3) 不注入任何解释器变量 → 回落路径必须在失败信息里说明
  "QX_PYTHON 未设置，回落 PATH python"。用例把运行时模板的 `data_dir` 重写到自己 `%TEMP%` 的副本里，
  不再往 `deploy/data/*/runs/` 写脏产物。
- `tools/check_architecture.py` 新增 `worker_diagnostics_check()` 四项：解释器来源判定只在
  `python_interpreter_origin()` 一处；worker 诊断只有一处实现且**接满每个出口**（`death_note` 定义 1 次、
  调用 ≥ 4 次，且必须用 `try_wait()` 取退出码）；`recv_timeout` 之后到解出响应之前那一段必须附诊断、
  不得用两个问号裸传；CCXT 的 EOF 文案必须同时保留"提交结果未知"与程序名。

### Validation（phase4n 轮实测，日志 `/tmp/qx_phase4n_gate.log`，2026-09-19：worker 失败自证原因落地后复跑）

```text
QX_PYTHON=C:\Users\Administrator\AppData\Local\Temp\qxvenv\Scripts\python.exe
FMT_EXIT=0  CLIPPY_DEFAULT_EXIT=0 (CLIPPY_WARNING_LINES=0)  CLIPPY_FEAT_EXIT=0  TEST_EXIT=0  BUILD_EXIT=0
OK_LINES=71; RUST_PASSED=539 RUST_FAILED_SUITES=0        # Phase 4m 的 536 + 本轮新增 3 例
WORKER_DIAG_PASSED=3；单独复跑（摘掉 QX_PYTHON）: 3 passed，DIAG_NO_ENV_EXIT=0
qx-core: tests/ledger.rs 18 passed / lib 34 passed；venue_report_contract 1 passed
qx-runtime（--no-fail-fast）: RUNTIME_SUITES=4 RUNTIME_PASSED=57 RUNTIME_FAILED=0
cargo test -p qx-cli --bin qx-cli: 56 passed；--features sqlite,postgres,nats: 59 passed
QX_PYTHON=python（占位桩）对照: STUB_COMPARE_EXIT=101，54 passed; 2 failed
解释器三口径端到端（同一条 strategy backtest 链）：
  A 回落 PATH python: A_EXIT=2
    策略回测失败: 跨语言策略回测失败: BusinessViolation("Strategy worker 已关闭输出（程序=python
    （QX_PYTHON 未设置，回落 PATH python），进程未退出，worker 无 stderr 输出；若该程序是 WindowsApps
    的 python 占位桩，请把 QX_PYTHON 指向可用解释器）")
  B 不存在的解释器: B_EXIT=2
    策略回测失败: 启动 Python Strategy worker 失败: qx-4n-no-such-interpreter（来自 QX_PYTHON） 无法执行: program not found
  C 可用解释器: C_OK_EXIT=0  result_hash=06ec367ae53ab542
18 份含 workers 段的模板逐项 config validate: TEMPLATES_VALIDATED=17/18
  唯一失败仍是 production 模板的两条 /var/lib/qianxing/research/* 部署机绝对路径（记录性条目）
快照前 ARCH_EXIT=1 / ARCH_ITEMS=53，唯一红项是行数棘轮；--snapshot 后
  ARCH_EXIT_AFTER_SNAPSHOT=0 / ARCH_ITEMS_AFTER_SNAPSHOT=54
PY_EXIT=0  Ran 43 tests in 0.088s  OK     VALIDATE_EXIT=0  全部自校验通过 ✓
CLI 冒烟：verify=0 all=0 runtime-check=0 runtime-check-binance=0 config-validate=0
          backtest builtin=0 backtest multi-builtin=0 fast-backtest ashare=0 paper-e2e=0
          strategy backtest=0（本轮新增口径） 未知命令=2 binance-worker 非法角色=2
          runtime-check production=2（绝对路径，本机必然）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 Phase 4j/4k/4l/4m 基线逐位相同
离线验收：NO_CREDS_EXIT=3 / --allow-skip ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（三次变异实验后工作树逐字节校验，无 .bak4n 残留）
UNCOMMITTED_PATHS=140
```

反向验证三次（`cp` 备份 → 锚点唯一替换 → 复跑 → `cp` 还原 → `cmp` `identical` → `RESTORED_x_ARCH_EXIT=0`）：
Y 把"读线程协议错误"的诊断包装退回裸 `return Err(error)` → `MUTATED_Y_ARCH_EXIT=1`（"接满每个出口"印出
`定义 1 次、调用 3 次`），且黑盒用例 `MUTATED_Y_TEST_EXIT=101`、`1 passed; 2 failed`，失败文本正是
"失败信息必须说明是哪个解释器" —— 门禁项与行为测试钉住的是同一件事；
W 把"响应通道断开"出口的诊断退回旧的只带 stderr 尾部 → 调用点变 3，`MUTATED_W_ARCH_EXIT=1`；
Z 把 CCXT 的 EOF 文案改成不含程序名 → `MUTATED_Z_ARCH_EXIT=1`，报
`CCXT 无响应既保留未知结果语义又点名解释器 — EOF 文案缺少程序名或被改写`。

本轮行数增长 5 个登记项：`strategy_host.rs` 772 → 812（`death_note()` 与其四个接入点）、
`main.rs` 3,594 → 3,604（来源二元组）、`ccxt.rs` 1,064 → 1,068、`strategy_contract.rs` 821 → 822、
`workers.rs` 717 → 718（各加一个实参）。本轮触及的 5 个登记项之和 6,968 → 7,024、41 条求和
59,861 → 59,917，即**连续第二次净增（+56 行）**，没有新增登记项（新测试文件 128 行在棘轮集之外）。
边界与下一步的回收计划记在 V9 §8.3 第 5、6 项：这一类"失败路径自证"的文本无法压缩到零，但
`main.rs` 与 `strategy_host.rs` 的下一个拆分点已经明确。

环境事实（本轮新增三条，均已固化进 `qx_phase4n_gate.sh`）：
(1) **本机 Git Bash 的 `env -u QX_PYTHON <cmd>` 会静默不执行命令**（实测退出码 0、零字节输出、38ms），
第一版门禁因此把"回落口径"记成了 `A_EXIT=0 / WORKER_DIAG_LINES=0` 的假绿；脚本改用子壳
`noenv() { ( unset QX_PYTHON; "$@" ); }` 后同一命令真实地报出 `A_EXIT=2` 与完整诊断文本；
(2) 需要"存在但对协议沉默、且跨平台一致"的假解释器时，用 `std::env::current_exe()`（测试二进制自身）：
它收到未知参数即退 101 且 stdout 为空、stderr 固定一行。`cmd.exe` 会往 stdout 吐 129 字节横幅、
`/bin/true` 只在类 Unix 存在、qx-cli 自身会把帮助写到 stdout，都不合格；
(3) `cargo test` 在用例失败时退 **101**（不是 1），"占位桩对照"这类"必然红"的步骤要把期望写成非 0；
`python -m unittest discover` 的正确调用是 `-s python/tests -q`，写成 `-s python -p "test_*.py"` 会得到
`NO TESTS RAN` + `PY_EXIT=5`，看着像"测试通过计数为 0"。

### Changed（Phase 4o：删除侧收敛 —— qx-cli 行为用例拆成 `src/tests/` 目录模块，架构不变量 54 → 58 项）

- `crates/qx-cli/src/tests_main.rs`（2,860 行，登记列表里第二大项）按主题拆成
  `crates/qx-cli/src/tests/` 目录模块：`cli_surface`（6 例）/ `paper_bridge_and_bundles`（7）/
  `worker_observability`（18）/ `execution_and_multi_leg`（4）/ `paper_and_strategy_worker`（7）/
  `backtest_entries`（9）/ `e2e_and_python_contract`（8），另有 103 行的 `mod.rs` 收 4 个共享夹具
  （`smoke_paper_risk_context`、`builtin_backtest_example_paths`、`temp_cli_case_dir`、
  `isolated_backtest_runtime`）。最大子文件 459 行，8 个文件合计 2,878 行（多出的 18 行是各主题文件的
  `use super::*;` 头与 `mod.rs` 的模块声明）。
- **只搬行、不改语义**：59 条 `#[test]` 按原顺序整段移动，用例体一字未改；`main.rs` 尾部的
  `#[cfg(test)]` + `#[path = "tests_main.rs"]` + `mod tests;` 三行变两行（3,604 → 3,603）。
  拆分动机不是观感：Phase 4m/4n 连续两轮净增之后，V9 §8.3 第 5 项要求下一轮必须是删除侧收敛，而登记列表里
  唯一能整体消失的大项就是这份测试单文件——棘轮只数 `crates/*/src/**/*.rs`，目录模块让每个文件落回 500 行门槛内。
- 新增 4 项架构不变量（54 → 58）钉住这次拆分自身：被拆掉的单文件不得复活；用例必须以目录模块挂载
  （`main.rs` 不得再出现 `#[path = "tests_main.rs"]`）；**行为用例条数下限棘轮 ≥ 59**（拆分让"删几条用例来压
  行数"成为可行路径，于是行数只降不升的同时用例数只能升）；4 个共享夹具只在 `tests/mod.rs` 定义一份。
- 两处既有门禁的豁免随形状调整而未削弱：命令分派单点检查原先按文件名 `tests_main.rs` 豁免，现按 `src/tests/`
  目录豁免；多腿屏障检查原先靠"文件名 stem 含 `test`"豁免用例文件，现同样按目录豁免，其可靠性由新增的第二项
  不变量兜住——该目录只能经 `#[cfg(test)] mod tests;` 挂载，生产提交入口不可能躲进去。

### Validation（phase4o 轮实测，日志 `/tmp/qx_phase4o_gate.log`，2026-09-19，本轮不做功能改动，只验收"搬家不改语义"与新增的四项形状门禁）

```text
FMT_EXIT=0
CLIPPY_DEFAULT_EXIT=0             # cargo clippy --workspace --all-targets
CLIPPY_WARNING_LINES=0
CLIPPY_FEAT_EXIT=0                # cargo clippy -p qx-cli --all-targets --features sqlite,postgres,nats
TEST_EXIT=0
OK_LINES=71  RUST_PASSED=539  RUST_FAILED_SUITES=0
                                  # 与 Phase 4n 逐位相同：本轮只搬行，用例不增不减
CORE_EXIT=0（tests/ledger.rs 18 passed）/ CORE_LIB_EXIT=0（lib 34 passed）
CONTRACT_EXIT=0（venue_report_contract 1 passed）
RUNTIME_SUITES=4 RUNTIME_PASSED=57 RUNTIME_FAILED=0
56 passed / 59 passed             # cargo test -p qx-cli --bin qx-cli 默认特性 / sqlite,postgres,nats
TEST_TOTAL=59                     # 6+7+18+4+7+9+8，等于拆分前 tests_main.rs 的 59 条
最大用例文件 459 行（paper_and_strategy_worker.rs）、mod.rs 103 行、8 个文件合计 2,878 行
ARCH_EXIT_BEFORE_SNAPSHOT=1  ARCH_ITEMS_BEFORE=58
  [FAIL] 单文件行数预算只降不升 — crates/qx-cli/src/tests_main.rs 已不存在，请重新生成快照
已写入 maturity/line_budgets.yaml（40 个超 500 行文件）
ARCH_EXIT_AFTER_SNAPSHOT=0  ARCH_ITEMS_AFTER_SNAPSHOT=58
entries 41 → 40 / sum 59,917 → 57,056（−2,861）
dropped=['crates/qx-cli/src/tests_main.rs']  added=[]  shrunk={main.rs: 3604 → 3603}
MUTATED_AA/BB/CC/DD_ARCH_EXIT=1 → RESTORE=identical → RESTORED_*_ARCH_EXIT=0
PY_EXIT=0（Ran 43 tests）/ VALIDATE_EXIT=0 / TEMPLATES_VALIDATED=17/18
cli smoke：verify / all / runtime-check / runtime-check-binance / config-validate /
           backtest-builtin / backtest-multi-builtin / fast-backtest-ashare / paper-e2e /
           strategy-backtest 全 0；runtime-check-production=2、unknown-command=2、
           binance-worker-bad-role=2（三条均为记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 4j/4k/4l/4m/4n 基线逐位相同
STUB_EXIT=2 + 自证文案仍在（QX_PYTHON 未设置时回落 WindowsApps 占位桩，Phase 4n 的诊断未因搬家退化）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（四次变异实验后工作树逐字节校验、无 tests_main.rs 残留）
UNCOMMITTED_PATHS=139
```

反向验证四次，各自只红在本轮新增的那一项：AA 在 `crates/qx-cli/src/` 建一个 1 行的 `tests_main.rs` →
"用例单文件已拆分且不得复活"报红；BB 把 `reconcile_report_persists_structured_balance_discrepancy` 的
`#[test]` 注释掉 → "qx-cli 行为用例不少于 59 条 — 当前 58 条"报红并把逐文件条数印出来，证明下限棘轮确实拦得住
"删用例换行数"这条被本轮拆分新打开的路径；CC 在 `backtest_entries.rs` 另抄一份 `fn temp_cli_case_dir` →
"共享测试夹具只在 tests/mod.rs 定义一份"报红（定义位置 `['backtest_entries.rs', 'mod.rs']`）；
DD 把 `#[cfg(test)]` 原地改成 `#[path = "tests_main.rs"]`（等行数改写，避免红因落到行数棘轮上）→
"用例以目录模块挂载"报红。四次实验后 `cp` 还原并 `cmp` 逐字节相同、复跑 `check_architecture.py` 全绿。

行数账：本轮是棘轮落地以来第一次**净降** —— 登记条数 41 → 40，41 条求和 59,917 → 57,056（−2,861 行），
唯一一条被删的登记项就是 `tests_main.rs`（2,860 行），另有 `main.rs` −1 行；新增的 8 个文件全部在
`OVERSIZED = 500` 门槛之下，按规则不需登记，因此没有新增登记项。上一轮记下的"连续两次净增需先做一次
删除侧收敛"的约束到此兑现，第三次增长的判断重新回到"是否有其它同量级回收点"，现存最大项依次是
`main.rs` 3,603、`ashare.rs` 1,978、`backtests.rs` 1,379、`crates/qx-execution/src/tests.rs` 1,088。

### Changed（Phase 4p：qx-cli crate 根职责簇拆分 —— `main.rs` 3,603 → 1,884 行，架构不变量 58 → 65 项）

- 将 Phase 4o 之后登记列表里的最大项、也是唯一一项仍可"纯搬家"回收的条目 —— `crates/qx-cli/src/main.rs`
  （3,603 行 / 此前把整仓 CLI 命令分派、冒烟自检、运行时装配都写在一起）—— 按职责簇拆出 5 个兄弟模块：
  `ecosystem_smoke.rs`（481 行，`run_ecosystem_smoke` / `run_paper_smoke` / `run_backtest` 三条冒烟链与
  `DemoProvider`、`Outcome`、`gen_bars`、`mk_order` 等自检夹具）、`runtime_wiring.rs`（344 行，
  `read_runtime_config`、`PipelineStorage`、`open_runtime_pipeline`、`strategy_risk_gate`、
  `config_margin_mode`、`CONSERVATIVE_MAX_QTY_RAW` 与 `worker_metrics_*` 七个观测口辅助）、
  `configured_backends.rs`（305 行，`ControlStateBackend` / `ConfiguredJobQueue` 两个后端口与其
  `configured_*` 构造器、`postgres_dsn`）、`api_service.rs`（386 行，`build_configured_api_service` 与
  账户快照 / 查询模型四个加载器）、`readiness.rs`（267 行，`configured_api_readiness`、
  `production_trading_assets_ready`、`worker_credentials_ready`、`validate_research_snapshot_binding` 等
  就绪判定）。5 个文件全部落在 `OVERSIZED = 500` 门槛内，按棘轮规则不需登记，因此登记条数不变而最大项腰斩。
- **只搬行、不改语义**：由脚本按锚点整段切移，`use super::*;` + `pub(crate)` 提升是全部形态变化。等价性证据
  是符号 token 多重集 25,157 → 25,157（lost=0 / gained=0）；行级多重集的 9 减 33 增逐条核对为 rustfmt 把
  9 个超长签名改成多行（每处换行即多出一行），不是代码增删。用例数与结果口径逐项与 Phase 4o 相同
  （workspace 539、`--bin qx-cli` 默认 56 / 全特性 59、`src/tests/` 内 59 条），`result_hash` 逐位未变。
- 新增 7 项架构不变量（58 → 65）钉住这次拆分自身，而不只是记录它：5 个拆出模块逐个在 500 行门槛内；
  每个模块必须以 `mod x;` + `pub(crate) use x::*;` 成对挂载在 crate 根；**根内顶层条目数上限 41**
  （`CLI_ROOT_ITEM_CEILING`，拦"实现又长回 main.rs"这条由本轮新打开的出口 —— 行数棘轮只约束登记项，
  把函数挪回根目录不再违反任何既有门禁）；`run_ecosystem_smoke` / `run_paper_smoke` / `run_backtest` /
  `DemoProvider` 四个冒烟链路入口的定义点必须唯一且落在 `ecosystem_smoke.rs`。
- 一处既有门禁的豁免键随代码搬家：`BARE_RISK_GATE_ALLOWLIST`（"生产代码不存在无规则的风控门"）原先记
  `main.rs` 里的 1 处裸 `RiskGate::new`（`run_backtest` 的冒烟装配），现随函数移到
  `ecosystem_smoke.rs` 且计数仍为 1。这是按文件名索引的白名单的固有耦合，边界记入 §8.3 第 6 项。

### Validation（phase4p 轮实测，日志 `/tmp/qx_phase4p_gate.log`，2026-09-19，本轮不做功能改动，只验收"搬家不改语义"与新增的七项形状门禁）

```text
FMT_EXIT=0
CLIPPY_DEFAULT_EXIT=0             # cargo clippy --workspace --all-targets
CLIPPY_WARNING_LINES=0
CLIPPY_FEAT_EXIT=0                # cargo clippy -p qx-cli --all-targets --features sqlite,postgres,nats
TEST_EXIT=0
OK_LINES=71  RUST_PASSED=539  RUST_FAILED_SUITES=0
                                  # 与 Phase 4o/4n 逐位相同：本轮只搬行，用例不增不减
CORE_EXIT=0（tests/ledger.rs 18 passed）/ CORE_LIB_EXIT=0（lib 34 passed）
CONTRACT_EXIT=0（venue_report_contract 1 passed）
RUNTIME_SUITES=4 RUNTIME_PASSED=57 RUNTIME_FAILED=0
56 passed / 59 passed             # cargo test -p qx-cli --bin qx-cli 默认特性 / sqlite,postgres,nats
SHAPE: main.rs 1,884 + ecosystem_smoke 481 + runtime_wiring 344 + configured_backends 305
       + api_service 386 + readiness 267 = 3,667 行（合计比拆前 3,603 多 64 行，即 5 份模块头
       + `use super::*;` 与根里 10 行 `mod`/`pub(crate) use` 挂载的固定代价）
ROOT_ITEMS=41  TEST_TOTAL=59
SIG_LINES before=3458 after=3482
LINE_MULTISET lost=9 gained=33    # 逐条核对为 9 个签名的 rustfmt 重排，非代码增删
TOKENS before=25157 after=25157
TOKEN_MULTISET lost=0 gained=0    # 真正的"只搬家"判据
ARCH_EXIT_BEFORE_SNAPSHOT=0       # 与 Phase 4o 不同：本轮登记项只降不升，快照前不该有红，全绿是预期
已写入 maturity/line_budgets.yaml（40 个超 500 行文件）
ARCH_EXIT_AFTER_SNAPSHOT=0  ARCH_ITEMS_AFTER_SNAPSHOT=65
entries 40 → 40 / sum 57,056 → 55,337（−1,719）
dropped=[]  added=[]  changed={'crates/qx-cli/src/main.rs': (3603, 1884)}
MUTATED_EE/FF/GG/HH_ARCH_EXIT=1 → RESTORE=identical → RESTORED_*_ARCH_EXIT=0
PY_EXIT=0（Ran 43 tests）/ VALIDATE_EXIT=0 / TEMPLATES_VALIDATED=17/18
cli smoke：verify / all / runtime-check / runtime-check-binance / config-validate /
           backtest-builtin / backtest-multi-builtin / fast-backtest-ashare / paper-e2e /
           strategy-backtest 全 0；runtime-check-production=2、unknown-command=2、
           binance-worker-bad-role=2（三条均为记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 4j/4k/4l/4m/4n/4o 基线逐位相同
STUB_EXIT=2 + 自证文案仍在（QX_PYTHON 未设置时回落 WindowsApps 占位桩，Phase 4n 的诊断未因搬家退化）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（四次变异实验后工作树逐字节校验）
UNCOMMITTED_PATHS=144
```

反向验证四次，各自只红在本轮新增的那一项：EE 在 `main.rs` 里等行数注入一个新的 `pub(crate) fn`（根条目
41 → 42）→ "crate 根顶层条目不多于 41 个（实现不得长回 main.rs）"报红，证明这条上限确实拦得住拆分带来的
新出口；FF 把 `api_service.rs` 撑到恰好 500 行（越出 `< OVERSIZED` 判定、但仍不需登记，故红因不可能落到
行数棘轮上）→ "Phase 4p 拆出的兄弟模块逐个在单文件行数门槛内"报红；GG 在 `main.rs` 里等行数改写既有签名、
顺手再定义一份 `run_ecosystem_smoke` → "smoke 链路入口 … 定义点唯一且在 ecosystem_smoke.rs — 定义于
['ecosystem_smoke.rs', 'main.rs']"报红；HH 删掉 `readiness` 的 `pub(crate) use readiness::*;` 一行 →
"拆出的模块在 crate 根以 mod + pub(crate) use x::* 成对挂载 — 缺配对 ['readiness']"报红。四次实验后
`cp` 还原并 `cmp` 逐字节相同、复跑 `check_architecture.py` 全绿。

行数账：登记条数 40 → 40（不增项），40 条求和 57,056 → 55,337（−1,719 行），唯一变化项是
`crates/qx-cli/src/main.rs` 3,603 → 1,884。这是棘轮落地以来第一次连续两轮净降（4o −2,861、4p −1,719），
`main.rs` 也从登记列表第 1 大项掉到第 11 位（首位是 `crates/qx-runtime/src/lib.rs` 3,660，第 10 位
`ashare.rs` 1,978 紧接在 `main.rs` 之前）。代价是真实代码总量没减：六个文件合计 3,667 行，比拆前的
3,603 行多 64 行，纯粹是模块头、`use super::*;` 与根里成对挂载行的固定开销，因此本轮收敛的是"单文件
长度"这一个可维护性口径，不是删掉了任何功能。剩余登记项 `main.rs` 1,884 / `ashare.rs` 1,978 /
`backtests.rs` 1,379 / `crates/qx-execution/src/tests.rs` 1,088 此后都要按真实职责重写才能再拆，
同量级的"纯搬家"回收点已尽。

### Fixed（Phase 4t：qx-storage 追加锁在 Windows 上的偶发拒绝访问 —— 有界重试覆盖 PermissionDenied）

- 根因：`acquire_storage_lock`（crates/qx-storage/src/lib.rs）用 `create_new(true)` 抢锁，只对 `AlreadyExists` 重试；`StorageLock::drop` 删除锁文件后存在短暂的 delete-pending 窗口，此窗口内的 `create_new` 在 Windows 上返回 `ERROR_ACCESS_DENIED`（os error 5），不落入重试分支就直接抛 `StorageError::Io`。16 线程 × 200 次并发取令牌的压力探针改动前为 ok 3142 / os_error_5 8 / other_io 0 / conflict 50，把 `PermissionDenied` 并入同一有界重试后为 ok 3163 / os_error_5 0 / other_io 0 / conflict 37（探针日志由 cargo test -- --nocapture 当场捕获）。
- 排除过的错误假设：一度以为是 `write_atomic_path` 里 `std::fs::rename` 撞上目标文件被占用的共享冲突。为此新写的回归用例直接把它证伪 —— 同进程持有目标文件句柄时 rename 照样成功（`unwrap_err()` 拿到 `Ok(true)`）。那套 `src/atomic.rs` 重试与配套用例已整体撤销，未留下半套改动。
- 代价如实记录：修复让 `crates/qx-storage/src/lib.rs` 从登记值 3,536 增长到 3,541（rustfmt 把多行 `matches!` 与中文注释按 100 列重新展开）。行数棘轮本轮被有意放宽 5 行并由 `--snapshot` 记为 3,541 —— 这是首次为经过验证的正确性修复上调预算，与「纯搬家类回收点已尽」是两件事。
- 本轮实测（日志 /tmp/qx_phase4t_gate.log）：专项 `cargo fmt --all` 通过、`cargo clippy -p qx-storage --all-targets` 零告警、`cargo test -p qx-storage --no-fail-fast` 6 个套件全绿 0 失败；随后补跑全量门禁，`FMT=0`、`CLIPPY=0 / CLIPPY_WARNING_LINES=0`、`TEST_EXIT=0`、`OK_SUITES=71 / RUST_PASSED=539 / RUST_FAILED_SUITES=0`（与 Phase 4s 复跑基线一致，未因本轮改动新增或丢失用例）、架构不变量 88 项全绿。151 个路径仍全部未提交。

### Changed（Phase 4s：qx-cli 回测编排目录模块拆分 —— `src/backtests.rs` 1,379 行整体退出登记集，架构不变量 76 → 88 项）

- 把登记列表里最后一项"仍能靠纯搬家收掉"的目标搬走：`crates/qx-cli/src/backtests.rs`（1,379 行）按回测
  入口拆成 `src/backtests/` 七个文件 —— `mod.rs` 172 行只留共享装配（`BarBacktestAssembly`、
  `market_spec_with_margin`、`run_builtin_strategy_on_bars`，以及多腿共用的 `ScheduledTargetStrategy` 与
  `read_bar_frame_for_multi_backtest`）、`multi_builtin.rs` 347、`single_strategy.rs` 296、
  `strategy_backtest.rs` 234、`depth.rs` 180、`artifacts.rs` 109、`fast_backtest.rs` 77。原单文件删除，
  `main.rs` 的 `mod backtests;` 不改一字即指向目录模块。
- 拆完暴露两处真实耦合，都按最小面修掉而非绕开：`BacktestArtifacts` 的 21 个字段与 `DatasetRunBinding`
  的 2 个字段从私有提到 `pub(crate)`（兄弟模块要用结构体字面量构造，否则 E0451）；第 6 项"Bar 回测引擎
  装配只有一份"原先读死单文件路径，现改为**按目录聚合**读七个子文件 —— 不改这一条，在拆出去的子文件里
  再写一份 `BacktestConfig {` 字面量就是门禁盲区（本轮反向验证 SS 正是用它证明新口径咬得住）。
- 架构不变量新增第 14 项，把 Phase 4o / 4r 只用在行为用例上的那套形状约束搬到**生产代码**：单文件不得
  复活、六个主题模块逐个低于门槛、在 `mod.rs` 以 `mod x;` + `pub(crate) use x::*;` 成对挂载、共享装配的
  顶层条目数不增（当前 7 个，上限 8）、八条回测链入口的定义点唯一。`maturity/capabilities.yaml` 里三条
  指向 `backtests.rs` 的证据路径同步改成真实子文件（这项由第 7 项不变量自动报红，不靠人记住）。

### Validation（phase4s 轮实测，日志 `/tmp/qx_phase4s_gate.log`，2026-09-19：回测编排拆目录模块后复跑）

```
FMT_EXIT=0
CLIPPY_DEFAULT_EXIT=0 / CLIPPY_WARNING_LINES=0 / CLIPPY_FEAT_EXIT=0
TEST_EXIT=101  OK_LINES=70  RUST_PASSED=517  RUST_FAILED_SUITES=1        # 首轮：见下方"同轮复跑"
XINGBAN_EXIT=0 test result: ok. 58 / 3 / 1 / 0
BIN_DEFAULT: test result: ok. 56 passed  /  BIN_FEATURES: test result: ok. 59 passed
SHAPE: mod.rs 172 artifacts.rs 109 depth.rs 180 fast_backtest.rs 77
       multi_builtin.rs 347 single_strategy.rs 296 strategy_backtest.rs 234  total 1415
TOP_ITEMS mod.rs=7（其余 1–3）  DIR_TOTAL=1415
tokens 10693 -> 10693   lost=0 gained=0   EQUIV
ARCH_EXIT_BEFORE_SNAPSHOT=1  PRE_PASS_LINES: 87  唯一红因：backtests.rs 已不存在，请重新生成快照
已写入 maturity/line_budgets.yaml（37 个超 500 行文件）
ARCH_EXIT_AFTER_SNAPSHOT=0  ARCH_ITEMS_AFTER_SNAPSHOT=88  架构不变量自检全部通过 ✓（88 项）
MUTATED_NN/OO/PP/QQ/RR/SS_ARCH_EXIT=1（各命中对应 [FAIL]）RESTORE[*]=identical RESTORED_*_ARCH_EXIT=0
7d6 < crates/qx-cli/src/backtests.rs: 1379   BUDGET_DIFF_EXIT=1
entries 38 -> 37   sum 52365 -> 50986 (-1379)   dropped: ['crates/qx-cli/src/backtests.rs']
added: []   changed: {}
PY_EXIT=0 Ran 43 tests / VALIDATE_EXIT=0 全部自校验通过 ✓ / TEMPLATES_VALIDATED=17/18
cli smoke：全部 exit=0，仅 runtime-check-production=2、unknown-command=2、binance-worker-bad-role=2
DETERMINISM=same  HASH_VS_BASELINE=same（result_hash=b26e1d4d4d430cb1）
STUB_EXIT=2（QX_PYTHON 未设置时回落桩仍自证占位桩）  NO_CREDS_EXIT=3 / ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes  FMT_EXIT_AFTER_MUTATION=0  CHECK_EXIT_AFTER_MUTATION=0  UNCOMMITTED_PATHS=150
```

**首轮红因不是本轮代码**。首轮 `TEST_EXIT=101` 的那一条是 `qx-storage --lib` 的
`file_token_bucket_is_persistent_and_serializes_concurrent_consumers`，panic 在
`crates/qx-storage/src/lib.rs` 里那条用例的 `unwrap()` 收到 `Io("拒绝访问。 (os error 5)")` —— Windows
下跨进程令牌桶在全量并发跑时的文件共享冲突（当轮记作 `:3521`；该文件现已缩短到 2446 行，用例 `file_token_bucket_is_persistent_and_serializes_concurrent_consumers` 今在 `:2415` 起 —— V13 R1-F 重钉）。同轮复跑把它隔离出来连跑 8 次全部 `17 passed; 0 failed`，
再整跑一遍 `cargo test --workspace`（导出 `QX_PYTHON`）得 `TEST_EXIT3=0 / OK_LINES3=71 /
RUST_PASSED3=539 / RUST_FAILED_SUITES3=0`，与 Phase 4r 末的 539 条逐位相同。复跑还包含一次**故意**
不导出 `QX_PYTHON` 的对照：`RUST_PASSED=483`、红在 `crates/qx-cli/src/tests/e2e_and_python_contract.rs`
两条跨语言契约用例 —— 这正是 Phase 4n 要的 fail-closed 回落，不是回归。三段结果都已写进同一份日志末尾，
但**这条 Windows 抖动没有修**，属本轮遗留（下条）。

反向验证六次：NN 复活空单文件 → "不得复活"报红；OO 去掉 `pub(crate) use depth::*;` 的出口 →
"缺配对 ['depth']"；PP 在 `fast_backtest.rs` 里另写一份 `fn run_depth_backtest(` → 入口唯一性报红并
打印出 `['depth.rs', 'fast_backtest.rs']` 两处；QQ 把 `fast_backtest.rs` 撑到 532 行 → 主题模块门槛报红
（同一变异还额外触发棘轮的"未登记超大文件"，两条门禁重叠）；RR 往 `mod.rs` 写回两条实现 →
条目数 9 > 8 报红；SS 在子模块 `depth.rs` 里第二处装配 `BacktestConfig {` → 第 6 项聚合口径报红。

行数账：登记条数 38 → 37、37 条求和 52,365 → 50,986（−1,379），变化只有 `backtests.rs` 整项退出。
连续五轮净降（4o −2,861、4p −1,719、4q −1,884、4r −1,088、4s −1,379）。当前最大项依次是
`qx-runtime/src/lib.rs` 3,660、`qx-storage/src/lib.rs` 3,536、`qx-api/src/lib.rs` 3,137、
`qx-xingban/src/backtest.rs` 3,024、`qx-storage/src/sqlite.rs` 2,565、`qx-adapter/src/binance.rs` 2,307。
代价与前几轮同性质，且这轮回填得比"收敛"更多：七个文件合计 1,415 行，比原单文件多 **36 行**模块头与
挂载脚手架 —— 收敛的仍只是"单文件长度"这一个可维护性口径，真实代码总量没降。
`ashare.rs` 1,978 是下一个同量级目标，但它与 3,660 行的 `qx-runtime/src/lib.rs` 一样需要按真实职责重写，
不属"纯搬家"这一类；本轮遗留另有一条：上面那个 `os error 5` 抖动需要在令牌桶的文件读写上加有界重试
（属正确性收口，不该混进搬家轮）。

### Changed（Phase 4r：qx-execution 用例目录模块拆分 —— `src/tests.rs` 1,088 行整体退出登记集，架构不变量 72 → 76 项）

- 将登记列表里最后一项"可以纯搬家回收"的条目 —— `crates/qx-execution/src/tests.rs`（1,088 行，
  Phase 4i 删第二入口用例后由 1,242 降来的那一版）—— 按职责拆成 `src/tests/` 目录模块：
  `mod.rs`（223 行，模块头 + `use super::*;` + crate 内 `use` 段 + 六个共享夹具
  `PortState` / `PortVenue` / `NeverCalledVenue` / `RejectingRisk` / `PortRouter` / `port_order`
  与四条 `mod` 声明）、`gateway_port.rs`（103 行 / 5 例，`PortExecutionService` 的注册与标准事实
  追加、空响应与非法事实的 fail-closed、注册前拒绝、`CanonicalRiskPort` 无需 zhenlu 上下文转换）、
  `venue_submit_contract.rs`（200 行 / 2 例，Paper/CCXT/Binance 三家共用的提交-取消端口契约与
  未知提交事实契约）、`recovery_and_replay.rs`（303 行 / 3 例，`HedgeRecoveryWorker` 幂等且对未知
  状态 fail-closed、按 correlation 重放、风控预检先于 EventLog 与 Venue 副作用）、
  `paper_accounting.rs`（268 行 / 3 例，衍生品成交按规格 PnL 记账而非现货现金、缺风控上下文/行情
  时 fail-closed、复用网关幂等不产生新事实）。五文件合计 1,097 行（比拆前多 9 行模块头与挂载声明），
  逐个都在 `OVERSIZED = 500` 门槛内，因此 `src/tests.rs` 整项退出登记列表。`lib.rs` 的
  `#[cfg(test)]\nmod tests;` 挂载未改。与 Phase 4p / 4q 不同，本轮**没有任何一处需要提升可见性**：
  夹具全部留在父模块 `tests/mod.rs`，子模块经 `use super::*;` 看到的是祖先模块的私有项，
  这是 4o 已经验证过的形态。
- **只搬行、不改语义**：符号 token 多重集 6,985 → 6,985（lost=0 / gained=0）；13 条用例逐条以新路径
  运行并通过（`tests::gateway_port::…` 等，`--lib` 目标 `13 passed`）。用例条数逐文件为
  5 / 0 / 2 / 3 / 3，与拆前同一分组。
- 把 Phase 4o 的四项"用例目录模块形状"门禁**参数化**成一张 `TEST_MODULES` 表，对 qx-cli 与
  qx-execution 各查一遍（不变量 72 → 76）：被拆掉的单文件不得复活、不得用 `#[path]` 指回单文件、
  用例条数只增不减（新下限 13）、共享夹具只在 `tests/mod.rs` 定义一份。夹具判定同时把 Phase 4o 的
  `f"fn {name}("` 放宽为按行首的 `fn|struct|enum|trait|type {name}\b`（可带可见性前缀）——
  六条 qx-execution 夹具里有五条是结构体，沿用旧判定会让这五项在无人察觉的情况下恒为绿。
- 一处随代码搬家的**证据链修复**：`maturity/capabilities.yaml` 的 `paper_execution` 与
  `multi_leg_execution` 两条能力项把证据指向 `crates/qx-execution/src/tests.rs`，文件删除后即成为
  失效路径（拆分之前的 `check_architecture.py` 实测红为
  `能力矩阵证据路径全部存在 — 失效路径 ['crates/qx-execution/src/tests.rs']`，记录在
  `/tmp/qx4r_arch_pre1.txt`），改指到承接该断言的三个新文件。该项由"证据路径必须真实存在"这条
  既有门禁自动发现，本轮再用一次性失效路径（`..._typo.rs`）反向验证它仍然拦得住。

### Validation（phase4r 轮实测，日志 `/tmp/qx_phase4r_gate.log`，2026-09-19，本轮不做功能改动，只验收"搬家不改语义"与新增的四项参数化门禁）

```text
FMT_EXIT=0
CLIPPY_DEFAULT_EXIT=0  CLIPPY_WARNING_LINES=0
CLIPPY_FEAT_EXIT=0                       # -p qx-cli --all-targets --features sqlite,postgres,nats
TEST_EXIT=0
OK_LINES=71  RUST_PASSED=539  RUST_FAILED_SUITES=0
                                         # 与 Phase 4n/4o/4p/4q 逐位相同：本轮只搬行，用例不增不减
CORE_EXIT=0（tests/ledger.rs 18 passed）/ CORE_LIB_EXIT=0（lib 34 passed）
EXECUTION_EXIT=0  EXECUTION_TARGET=13 + 3 + 1 + 0（lib / 集成 / 文档 / doctest）
EXEC_LIB=13 passed 且 13 条用例逐条以 tests::<主题>::<用例名> 打印
RUNTIME_SUITES=4 RUNTIME_PASSED=57 RUNTIME_FAILED=0
56 passed / 59 passed                    # cargo test -p qx-cli --bin qx-cli 默认特性 / sqlite,postgres,nats
SHAPE: mod.rs 223 + gateway_port.rs 103 + venue_submit_contract.rs 200
       + recovery_and_replay.rs 303 + paper_accounting.rs 268 = 1,097 行（拆前 1,088）
TESTS_IN 逐文件 5 / 0 / 3 / 3 / 2      TOP_ITEMS mod.rs=12 其余 3–8  EXEC_LIB_ITEMS=38
TOKENS before=6985 after=6985  TOKEN_MULTISET lost=0 gained=0 → EQUIV
ARCH_EXIT_BEFORE_SNAPSHOT=1  PRE_PASS_LINES=75
  [FAIL] 单文件行数预算只降不升 — crates/qx-execution/src/tests.rs 已不存在，请重新生成快照
                                        # 快照前唯一的红因就是预算表仍写着被删文件，符合预期
已写入 maturity/line_budgets.yaml（38 个超 500 行文件）
ARCH_EXIT_AFTER_SNAPSHOT=0  ARCH_ITEMS_AFTER_SNAPSHOT=76  架构不变量自检全部通过 ✓（76 项）
MUTATED_II/JJ/KK/LL/MM_ARCH_EXIT=1 → RESTORE[*]=identical → RESTORED_*_ARCH_EXIT=0
19d18 < crates/qx-execution/src/tests.rs: 1088   BUDGET_DIFF_EXIT=1
entries 39 → 38  sum 53,453 → 52,365（−1,088）  dropped=['…/tests.rs']  added=[]  changed={}
PY_EXIT=0（Ran 43 tests）/ VALIDATE_EXIT=0 / TEMPLATES_VALIDATED=17/18
cli smoke：verify / all / runtime-check / runtime-check-binance / config-validate /
           backtest-builtin / backtest-multi-builtin / fast-backtest-ashare / paper-e2e /
           strategy-backtest 全 0；runtime-check-production=2、unknown-command=2、
           binance-worker-bad-role=2（三条均为记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 4j–4q 基线逐位相同
STUB_EXIT=2 + 自证文案仍在（QX_PYTHON 未设置时回落 WindowsApps 占位桩）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（五次变异实验后逐字节校验，且 tests.rs 未复活）
UNCOMMITTED_PATHS=150
```

反向验证五次，各自只红在本轮新增（或本轮刚重连）的那一项：II 让 `src/tests.rs` 以空文件复活
（0 行不足以触发行数门禁，红因不可能落到棘轮上）→ "qx-execution 用例单文件 tests.rs 已拆分且不得
复活"报红；JJ 把 `lib.rs` 的挂载原地改成 `#[path = "tests.rs"] mod tests;`（等行数）→ "用例以目录
模块挂载，不得用 #[path] 指回单文件"报红；KK 把一条 `#[test]` 原地改成 `#[cfg(any())]`（等行数、
仍可编译，条数 13 → 12）→ "行为用例不少于 13 条 — 当前 12 条"报红，并打印出逐文件计数差；LL 在
`gateway_port.rs` 末尾再定义一份 `struct PortState` → "共享测试夹具只在 tests/mod.rs 定义一份 —
定义位置 {'PortState': ['gateway_port.rs', 'mod.rs'], …其余五项 ['mod.rs']"报红，这一次同时证明
放宽后的结构体判定不是摆设；MM 把 `capabilities.yaml` 里本轮新接的那条证据路径改写成不存在的文件名 →
"能力矩阵证据路径全部存在"报红。五次实验后 `cp` 还原并 `cmp` 逐字节相同、复跑 `check_architecture.py`
全绿，且额外断言了 `src/tests.rs` 未被留下。

行数账：登记条数 39 → 38（本轮唯一退出项 `crates/qx-execution/src/tests.rs` 1,088），38 条求和
53,453 → 52,365（−1,088），`added=[] changed={}`。这是连续第四轮净降（4o −2,861、4p −1,719、
4q −1,884、4r −1,088），退出登记集后列表首位仍是 `crates/qx-runtime/src/lib.rs` 3,660，前十位与
Phase 4q 记录完全一致（`ashare.rs` 1,978 仍在第 10）。**口径要如实说明**：这已是第二次让用例文件
整体离开计数集（第一次是 Phase 4o 的 `tests_main.rs`），棘轮从来只数 `crates/*/src/**/*.rs`，
本轮新写的五个文件仍在 `src/` 下、只是各自低于 500 行才不需登记，因此约束由"登记行数"换成了
"目录内文件逐个 < 500 行 + 用例总数 ≥ 13 只增不减"这两条形状门禁 —— 与 4o 同一套做法，代价是真实
代码总量没减（1,097 vs 1,088，多 9 行脚手架），收敛的仍是"单文件长度"这一个可维护性口径。

### Changed（Phase 4q：qx-cli crate 根第二批六簇拆分 —— `main.rs` 1,884 → 288 行，架构不变量 65 → 72 项）

- Phase 4p 之后 `main.rs` 还剩 1,884 行、41 个顶层条目，仍是登记列表第 11 项。本轮把剩余的六条职责链
  整簇搬出，crate 根只留命令分派壳（288 行 / 7 个顶层条目：`main`、`run_unified_backtest`、
  `run_recovery_child`、`parse_backtest_quantity`、`python_interpreter`、`python_interpreter_origin`、
  `static NEXT_STRATEGY_RING_ID`）：`runtime_check.rs`（490 行 5 项，`collect_runtime_check_report` /
  `run_runtime_check` / `validate_runtime_references` / `validate_ashare_component_json` /
  `validate_dataset_bundle_component_references`）、`live_check.rs`（336 行 3 项，
  `push_live_check` / `collect_live_check_report` / `run_live_check`）、`market_bridges.rs`（264 行 10 项，
  行情桥一侧的 `account_event_log_name` / `configured_account_event_logs` /
  `spawn_api_projection_bridge` / `ccxt_event_log_name` / `ccxt_market_event_log_name` /
  `PaperMarketBridge` / `PaperMarketQuote` / `paper_market_worker_matches_instrument` /
  `open_paper_market_bridges` / `bridge_market_quote_to_paper`）、`strategy_binding.rs`（210 行 4 项，
  `validate_ccxt_worker_binding` / `strategy_current_qty` / `strategy_current_qty_for` /
  `build_strategy_contract_input`）、`path_resolution.rs`（192 行 6 项，`resolve_ccxt_config_path` /
  `is_explicit_absolute_path` / `resolve_runtime_relative_path` / `resolve_runtime_asset_path` /
  `resolve_strategy_runtime_paths` / `verify_strategy_artifact`）、`scheduler.rs`（143 行 6 项，
  `utc_schedule_tick` / `civil_from_days` / `scheduler_manifest` / `scheduler_jobs_path` /
  `load_scheduler_state` / `dispatch_scheduled_jobs`）。六个文件全部在 `OVERSIZED = 500` 门槛内，
  而 crate 根自身也降到门槛以下 —— 于是它整项退出登记列表。
- **仍然只搬行、不改语义**：形态变化只有 `//!` 模块头、`use super::*;`、`pub(crate)` 提升与根里的成对挂载，
  共 34 个条目升为 `pub(crate)`；`PaperMarketBridge` / `PaperMarketQuote` 的 12 个字段一并提升
  （否则 `E0616` / `E0451`，即 Phase 4p 记过的同一类可见性错误）。等价性判据是符号 token 多重集
  13,653 → 13,653（lost=0 / gained=0），且两侧经同一个 `strip()` 处理 —— 首轮版本只剥了拼接侧，
  把 `main.rs` 自带的 6 行 `//!` 与 16 处既有 `pub(crate)` 只计在左边，报出 `LOST 209` 的假差异。
  用例口径逐项未变（workspace 539、`--bin qx-cli` 默认 56 / 全特性 59、`src/tests/` 内 59 条），
  `result_hash=b26e1d4d4d430cb1` 与 4j～4p 基线逐位相同。
- 新增 7 项不变量（65 → 72），全部针对本轮新打开的出口：职责簇模块清单从 5 个扩到 11 个（逐个 500 行门槛，
  同一条检查覆盖）；`CLI_ROOT_ITEM_CEILING` 从 41 收到 **7**（根已是分派壳，任何实现回流都会立刻越限）；
  链路入口唯一性检查从 4 个冒烟标识符扩到 10 个（新增
  `collect_runtime_check_report` / `collect_live_check_report` / `dispatch_scheduled_jobs` /
  `open_paper_market_bridges` / `resolve_strategy_runtime_paths` / `build_strategy_contract_input`
  六个定义点，共 6 项检查）；再新增"crate 根 `main.rs` 已退出行数登记集（低于门槛且不在预算表内）"，
  把"拆到不再登记"这件事本身钉成门禁 —— 否则往根里写回 500 行以下代码只违反条目上限、不违反棘轮。
- `crates/qx-cli/src/` 现在是 25 个 `.rs` + `venue_runtime/` 目录 + `tests/` 目录。本轮没有改动任何
  一条 CLI 语义、任何一份模板、任何一个测试断言。

### Validation（phase4q 轮实测，日志 `/tmp/qx_phase4q_gate.log`，2026-09-19，本轮不做功能改动，只验收第二批搬家与新增的七项形状门禁）

```text
FMT_EXIT=0
CLIPPY_DEFAULT_EXIT=0             # cargo clippy --workspace --all-targets
CLIPPY_WARNING_LINES=0
CLIPPY_FEAT_EXIT=0                # cargo clippy -p qx-cli --all-targets --features sqlite,postgres,nats
TEST_EXIT=0
OK_LINES=71  RUST_PASSED=539  RUST_FAILED_SUITES=0
                                  # 与 Phase 4p/4o 逐位相同：本轮只搬行，用例不增不减
CORE_EXIT=0（tests/ledger.rs 18 passed）/ CORE_LIB_EXIT=0（lib 34 passed）
CONTRACT_EXIT=0（venue_report_contract 1 passed）
RUNTIME_SUITES=4 RUNTIME_PASSED=57 RUNTIME_FAILED=0
56 passed / 59 passed             # cargo test -p qx-cli --bin qx-cli 默认特性 / sqlite,postgres,nats
SHAPE: main.rs 288 + ecosystem_smoke 481 + runtime_wiring 344 + readiness 267 + configured_backends 305
       + api_service 386 + runtime_check 490 + live_check 336 + scheduler 143 + market_bridges 264
       + path_resolution 192 + strategy_binding 210 = 3,706 行
       （本轮六文件 1,635 行 + 根 288 行 = 1,923，比拆前根 1,884 多 39 行模块头/挂载固定代价）
ROOT_ITEMS=7  ROOT_LINES=288  TEST_TOTAL=59
TOKENS before=13653 after=13653  LOST 0 GAINED 0  EQUIV
ARCH_EXIT_BEFORE_SNAPSHOT=1       # 唯一红项："crate 根 main.rs 已退出行数登记集 … 登记=True"
                                  # 预算表此刻仍写着 main.rs: 1884，快照前必然红，属预期
已写入 maturity/line_budgets.yaml（39 个超 500 行文件）
ARCH_EXIT_AFTER_SNAPSHOT=0  ARCH_ITEMS_AFTER_SNAPSHOT=72
diff: 11d10  < crates/qx-cli/src/main.rs: 1884   BUDGET_DIFF_EXIT=1
entries 40 → 39 / sum 55,337 → 53,453（−1,884）
dropped=['crates/qx-cli/src/main.rs']  added=[]  changed={}
MUTATED_II_ARCH_EXIT=1   → "crate 根顶层条目不多于 7 个" — 当前 8 个
MUTATED_JJ_ARCH_EXIT=1   → "Phase 4p/4q 拆出的兄弟模块逐个在单文件行数门槛内" — 越界 ['scheduler.rs']（JJ_LINES=500）
MUTATED_KK_ARCH_EXIT=1   → "链路入口 collect_live_check_report 的定义点唯一且在 live_check.rs" — 定义于 ['live_check.rs', 'main.rs']
MUTATED_LL_ARCH_EXIT=1   → "拆出的模块在 crate 根以 mod + pub(crate) use x::* 成对挂载" — 缺配对 ['scheduler']
MUTATED_MM_ARCH_EXIT=1   → "crate 根 main.rs 已退出行数登记集" — 288 行，门槛 500，登记=True
RESTORE[II/JJ/KK/LL/MM]=identical  RESTORED_*=0（五次实验后逐字节还原并复跑全绿）
PY_EXIT=0（Ran 43 tests）/ VALIDATE_EXIT=0 / TEMPLATES_VALIDATED=17/18
cli smoke：verify / all / runtime-check / runtime-check-binance / config-validate /
           backtest-builtin / backtest-multi-builtin / fast-backtest-ashare / paper-e2e /
           strategy-backtest 全 0；runtime-check-production=2、unknown-command=2、
           binance-worker-bad-role=2（三条均为记录性条目）
确定性：backtest builtin sma_cross 两次 result_hash=b26e1d4d4d430cb1，与 4j～4p 基线逐位相同
STUB_EXIT=2 + 自证文案仍在（QX_PYTHON 未设置时回落 WindowsApps 占位桩，Phase 4n 的诊断未因搬家退化）
离线验收：binance_testnet_acceptance.py → NO_CREDS_EXIT=3 / --allow-skip → ALLOW_SKIP_EXIT=0
MUTATION_RESIDUE=yes（五次变异实验后工作树逐字节校验）
UNCOMMITTED_PATHS=150
```

反向验证五次，各自只红在本轮新增（或收紧）的那一项：II 在根里等行数注入一个新的 `pub(crate) fn`（7 → 8）
→ 条目上限报红，证明 Phase 4p 那条 41 的上限收到 7 之后确实咬得住；JJ 把 `scheduler.rs` 撑到恰好 500 行
→ 模块门槛报红（`>= OVERSIZED` 判定，且 500 行仍不需登记，红因不可能落到棘轮上）；KK 在根里另抄一份
`collect_live_check_report` → 链路入口唯一性报红，同时条目上限也报红（第二处定义本身也是一个新根条目，
两条门禁重叠而非冲突）；LL 删掉 `pub(crate) use scheduler::*;` 一行 → 配对挂载报红；MM 把
`crates/qx-cli/src/main.rs: 1884` 写回预算表 → "已退出登记集"报红。

本轮另有一处**门禁脚本自身的缺陷**值得记下：首次整跑把 `line_budgets.yaml` 的变异备份做在 `--snapshot`
**之前**，MM 的 `cp` 还原因此把上一轮（Phase 4p）的旧表写回工作树 —— 表现为 `RESTORED_MM_ARCH_EXIT=1`、
棘轮差异 `entries 40 → 40 / sum +0`、而末尾 `MUTATION_RESIDUE=yes` 仍是假绿（它比对的就是那份陈旧备份）。
修法是在快照段末尾重抓一次备份，然后**整跑重跑换新日志**（上方数字全部出自重跑那一份），不拼接。

行数账：登记条数 40 → 39、39 条求和 55,337 → 53,453（−1,884），唯一变化就是 `main.rs` 整项退出登记列表。
连续三轮净降（4o −2,861、4p −1,719、4q −1,884）。当前最大项依次是 `crates/qx-runtime/src/lib.rs` 3,660、
`qx-storage/src/lib.rs` 3,536、`qx-api/src/lib.rs` 3,137、`qx-xingban/src/backtest.rs` 3,024、
`qx-storage/src/sqlite.rs` 2,565、`qx-adapter/src/binance.rs` 2,307；`qx-cli` 的 crate 根已完全不在列表内。
代价与 Phase 4p 同性质：十二个文件合计 3,706 行，比 Phase 4p 末的 3,667 行多 39 行模块头与挂载开销，
收敛的是"单文件长度"口径而非功能体积。剩余同量级回收点 `ashare.rs` 1,978 / `backtests.rs` 1,379 /
`crates/qx-execution/src/tests.rs` 1,088 需要按真实职责重写才能拆 —— "纯搬家"这一类到此确实见底。

## v0.0.1

Initial Qianxing V5 architecture release candidate.

### Added

- Event-driven kernel architecture
- Domain model layer
- Market data abstraction
- Trading lifecycle
- Matching engine foundation
- Risk rule engine
- State replay foundation
- Plugin extension framework
- SDK boundary
- CLI and CI validation framework

### Validation

- Architecture checks
- E2E pipeline specification
- Deterministic replay verification
- Benchmark validation framework
