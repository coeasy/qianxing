# Changelog

## Unreleased — V11 D/E/F/G/H/I/J/K/L/M/N/O/P/Q/R4/R5/R6/R7 十八轮：判据只看得到这一次调用，看不见"崩溃之后""没人读过""另一半没跟上"与"另一个进程写的""根本没人在刷"，也看不见"这条路从来没人走""这颗循环不回来""这条重试没有上界""这份血缘背不起"，更看不见"这段等待没有上界""这份声明没人兑现""这一格只有用例守着"，P 轮看见**文档替不存在的能力作保**，Q 轮看见**名字相等不等于点之后有人接、写侧印的不等于读侧认的、而判据自己也有盲区**，R4 轮把那把尺子**调头量尺子自己**，R5 轮把两颗「等一个决定」的裁决变成实现、并把用例搬出主文件时**暴露门禁认文本的两处盲区**：台账里的行号引用有没有还指着今天的那一行、门禁脚本自己有没有孤儿、以及"验证手段"本身是不是也被验证，R6 轮看见**一份旧订单可以替这一轮作保、一个 202 可以只说完半句话、一颗没人写的状态可以替一整个词表作保，而上一轮写下的"后果"本身也要复测**（2026-09-24 起，O / P 两轮 2026-09-25，Q / R4 两轮 2026-09-25，R5 / R6 两轮 2026-09-26）

R/S/T 与两次合流之后，扫描面从"字段有没有被念出来"推到"链的另一半有没有人走"。这七轮的缺陷是同一个形状：
**D 轮**五颗在钱与锁上——孤儿锁让一次崩溃永久阻断四条链、手续费币种从未参与记账、多腿把两条不同账本的钱相加、
`report` 把"没写过"念成 0、D2 的第一版又把这些费用一律拒掉；**E 轮**（E1–E7）在"判据自己不会说话"的地方——
`environment` 只有"非空"一条判定因而 14 处 production 风控可以整体静默失效、`--json` 收了旗标却不产出 JSON、
外部验收脚本指向命令面上不存在的入口、过期 binary 守卫手抄清单、调度作业 owner 无人领取仍打印
`READY processed=0`、两处零消费者死代码；**F 轮**两颗是"链只修了一半"——`/stream` 背后那个只有测试会走的
写入口，和 `dataset-ingest` 只写不读；**G 轮**一颗是"同一份读模型两半各自说话"——事件每 250 毫秒重投影，
账户快照只在服务装配那一刻装载一次；**H 轮**两颗在同一根缝上，只是这次隔了进程——执行 worker 在**另一个
进程**把 `Executed`/`Failed` 追加进 store，`/control/audit` 却念服务装配时装进 `ApiState` 的那一份，
每一笔命令都被念成 `Accepted`；链上那个"把进程内那份整段推回去"的第二写入者（`sync_control`，三份后端
实现）零生产调用，随修随删。**I 轮**两颗仍在这条时间轴上，只是这次空的是"到底有没有人在刷"：
投影桥的三条启动失败路径与线程退出都不在任何读面上，`/ready` 因而能把一份冻结在 boot 的账户读模型念成
健康（I1）；`QueryPort` 剩下三格读的是被 HTTP 端点回填过的副本，"现读"要等有人先走过端点（I2）。
**J 轮**两颗不在读模型这条时间轴上，而在链路的边上：**J1** 把公开 API 表面那一大片零调用入口分成两类——
"同一能力的多余写法"删（24 处：11 颗 `serve_*` 转发壳、零调用的 `control_port`、无 fencing 的 
`ack_job` 四份、Postgres 的 8 颗 `from_storage`），"唯一实现的孤儿"留，但每一颗都要在文档里
说出"零调用是因为调用方在别处"。**J2** 是**当时可见范围内**最后一条读不到停机令牌的循环——API 的 accept 循环，
而 `run_runtime_api` 主线程 `join` 等的正是它，令牌翻起来也停不掉。**K 轮**证明那句话当时说满了：
进程监督者 `supervise_workers` 那颗 `loop` 同样一次都不读，而它是所有 worker 的父进程。K1–K5 收的是
另一族——**没有上界的行为**：复活没有预算、出站重试没有次数上限、崩溃后的锁被别人删掉、
命令行参数被静默丢弃。**L / M 两轮**收第 1 轮三扫剩下的九颗，其中**三颗（L1 / M1 / M2）逐条量下来不是
缺陷**：摘要的 `account` 块与 `clock_start/clock_end` 都有读者（`qx report --json` 整份外销、溯源判倒流
并写进哈希），NATS 那"三处无超时"有两处本就带库内给的界、第三处只是把界传下去，`/events/live` 那条 WS
循环不读令牌但也不阻断停机（连接线程是 detach 的，被 join 的是 accept 循环）。剩下六颗修掉：**L2** 血缘
三格从"空串外销"改成"没得说就不上 wire"，**L3** 把只写了半条的投影链登记补成读写两侧点名 + 七条行号锁，
**L4** 让 `shutdown_timeout_ms` 第一次真的接成上界并把体检的常量时钟换成真实时钟，**M3** 给 CI 补上
实盘文档教的那颗 `nats,postgres,sqlite` 组合并让矩阵由代码与文档自己生成要求，**M4** 按 J1 那把尺子
分完最后一批零调用公开项（十删九留 + 一期待裁决）。**N 轮**是第 2 轮三扫（同一把尺子把 A 连通性 /
B 孤儿与悬空配置 / C 终止性各重扫一遍）报上来的十一颗，其中七颗落在同一根轴上：**一份声明写在配置里、
执行平面没人兑现**（N2 的超时收口、N3 的窗口与日历、N4 的触发声明、N5 的 `strategies[]` 成本规则、
N9 的封顶半边），或者**一段等待没有上界**（N1 的 deadline 单位、N8 的管道写、N10 的静默、N11 的分片累计）。
D…M 那十轮的形状没有再报上来；这一轮的十一颗全部发生在**长跑**的时候，短程冒烟一颗也撞不到。
**O 轮**（O1–O9）是第 3 轮三扫的九颗，八颗落在同一根轴的**剩余格子**上：N5 只钉了 `cost_rules_path`，
剩下六格照旧静默（O2）；K1 只钉"整轮全败"，混合失败照旧无上界（O5）；M1 只钉"解析之后那一跳"，
解析本身照旧没界（O9）——另有一颗不在产品代码上：**常驻用例自己的首行读没有超时**，一次回归能把
整颗 CI 作业挂住而不是跑红（O6）。**P 轮**（P1–P3）第一次把刀落在**口径与承诺**上：没有执行者的控制
命令在受理处当场拒（P1）、三份从未接线的死实现连同替它们背书的四处文档口径一起删改（P2）、
唯一实现但没接线的那颗留在原地并逐颗登记锚点（P3）。**Q 轮**（Q1–Q4）不再问"这条链有没有人走"，
问的是**通告与证据隔了多远**：clap 命令表 58 条声明里有 18 条在 qx-cli 全部用例里一次都没出现过——
三处名字相等只证明"help 印得出"，不证明"点之后有人接"（Q1）；`deploy` 里两份 bar-frame 示例各带一格
没人读的键，而 Rust 读侧静默丢掉它、Python 读侧当场拒收，同一份文档两侧口径相反（Q2）；21 颗
"零生产调用 + 仓内唯一写法"按 J1 那把尺子留在原地登记，被删的只有"同一份工作的第二种写法"那三颗
（Q3）；README 替卯眼的两条走不到的能力作保，而**这条判据自己也被变异抓到过一次盲区**——全文范围
"提没提过未接线"接上一颗也不红、硬抄的行号只证明门禁没写错而不证明文档没漂，两次都由取证当场逼出来
（Q4）。
**R4 轮**把那把尺子调头量尺子自己：门禁脚本自己的 12 项孤儿常量清零、补一颗"审计者不得给自己留孤儿"的常驻
自检，顺带抓到 R4-3 那两颗"校验通过却什么都不约束"的字段与两处内存无界增长。**R5 轮**把 §52.13 里两颗「等一个
决定」的裁决变成实现（控制面按终态退场 + 有界审计窗口、哈希审计链补上生产读侧），并把四份超限 Rust 文件的用例
搬成兄弟文件——搬家之后门禁认文本的两处盲区同时现形。**R6 轮**（R6-1–R6-6）是第 4 轮三扫的六颗，三颗落在"一份
证据替另一份作保"上：paper 主链路验收拿 `orders()[0]` 那笔旧订单替本轮作保（R6-2）、一个 202 只说了"受理成了"
却没说"交给执行者了"（R6-3）、一颗从未被构造过的 `Rejected` 状态连带一格永远为 0 的计数照常外销（R6-1）；另有
同一判断的第二种写法（R6-4）、一次搬家带出的第三处门禁盲区（R6-6），以及一颗**上一轮把后果记错了**的缺口——
它不是无限重投而是一次整店停机，本轮改口并只报不修（R6-5）。
**R7 轮**（R7-1–R7-10c）是第 5 轮三扫的十颗，其中四颗是"同一件事有两种算法"：死信重放按窗口取一页再筛，
点名的事件于是可以"找不到"（R7-1）；TEXT 存的 u64 列在三处排序上各按各的口径，同毫秒的命令念反执行顺序
（R7-2）；快照键表的行内 id 从不进摘要，改过它仍通过校验（R7-3）；文件锁的年龄由判据交出一遍、循环又自己
量一遍（R7-9）。一颗是作业声明里三格只有写侧没有读侧、却仍是作业 JSON 的必填项（R7-4），一颗量下来不是缺陷
——`fetch_remote_snapshot` 的 `allOrders` 分页被限频预算界住了（R7-5），两颗把"不留第二种写法"推到摘要输入与读侧同源
（R7-7 / R7-8），最后一批是本轮改动自己的波及面：30 份 `.rs` 与一次 fmt 之后，四份文本 156 处行号重钉（R7-10 系列）。
逐颗落点、常驻判据原文与变异三级取证（断言红 > 仅门禁红 > SURVIVED）见
[docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §40、§41、§42、§43、§44、§45、§46、§47、§48、§49、§50、§51、§52、§53、§54、§55。

### Fixed

- **孤儿文件锁不再永久断链**（D1）：新增 `crates/qx-core/src/file_lock.rs` 作为写锁的唯一内核，
  `decide_lock(age, stale_after, takeover_used)` 是纯判据；`registry` / 多腿订单簿 / 作业队列 claim /
  存储信封四处各改一行委托，`FileJobQueue` 不再把"持有者早已不存在"报成 `LeaseHeld`。
  "同一场竞争只接管一次"住在判据里而不是循环里，是因为变异当场证明写在循环里的那一份咬不住。
- **成交手续费币种真正参与记账**（D2 + D5 是同一颗的两半）：`crates/qx-core/src/ledger/fill.rs` 的
  `base_asset_of` + `fee_in_settlement_raw` 让费用腿一律记折算后的数；现货基准资产费用用**该笔成交自己的
  价格**定点折算（不引入外部汇率、只对乘数为 1 的口径开放），折不了算的当场 `ReconcileRequired`——
  而不是像第一版那样把 Binance 现货每天正常发生的 `commissionAsset=BTC` 挡在链外。
- **多腿组合收益不再跨币种相加**（D3）：`multi_leg_spec_guard` 在任何腿级撮合之前比两条腿的记账币种，
  币种只问 `backtests/mod.rs:29` 的 `backtest_settlement_currency`（引擎真正落账那一份），
  缺规格那条腿按它实际落账的默认币种参与比较，而不是抄 spec 字段得到空串。
- **`report` / `status` 不再把缺席念成 0**（D4）：`report_format.rs` 的 `NOT_DECLARED` + `summary_number`
  是缺失口径的唯一出处，两个出口共用；整块缺失除逐格 `not_declared` 外还单独说一句。
- **`environment` 词表闭合、production 判据只有一处出口**（E1）：`RUNTIME_ENVIRONMENTS` 单点声明、
  词表外 fail closed、`is_production()` 唯一出口且只问词表常量。原先那条只禁字面量写法的门禁被
  E1-M2 证伪过一次，现在的判据问的是"出口引用的是常量还是自己抄的字面量"。
- **`--json` 收了旗标就得出 JSON**（E4）：四条 config 处理器的 `as_json` 形参位由门禁逐条核对；
  外部验收脚本点名的入口与 worker 现在必须在 clap 命令表、help 与验收配置里同时存在（E5）；
  过期 binary 守卫不再手抄被测文件清单（E6）。
- **调度作业 owner 无人领取时 fail closed**（E7）：通配 owner 收成 `JOB_OWNER_ANY` 一个常数，
  领取判据 `claimable_by` 单点，装配处 `validate_job_owners` 在新建与载入两条路径都问一遍。
  仓库自带示例里 5/8 套拓扑原本是"作业永远留在队列里、`JobRun` 却已标成 Running"的形状。
- **删掉两处零生产调用的入口**（F1、E2/E3）：`ApiState::publish_event` 与无账户键的 `project_event_log`
  被删，事件与实时游标只住在账户投影里，唯一写入者是 `project_account_event_log`；
  `qx-core/src/fenye.rs`（464 行、自身文件之外零消费者）与 `qx-zhenlu/src/portfolio/` 三处平行实现同批删除。
- **`dataset-ingest` 有了读侧**（F2）：`verify_dataset_registry_declarations` 由 `dataset-bundle` 与策略回测链
  各调一次，两侧都把核对条数印成 `registry_checked=N/M`，没登记过的组件说成"未核对"而不是悄悄算过。
- **账户快照跟着事件投影一起刷新**（G1）：读模型的事件半边每 250 毫秒重投影、快照半边此前只在
  `build_configured_api_service` 装载一次，于是服务进程按天跑时 `/account/snapshot|positions|balances|orders`
  把启动那一刻的权益一路念下去。装载规则（含 R12 的默认账户判据）收进唯一出口
  `publish_api_account_snapshots`，boot 与投影桥都只经它，而不是在桥里重抄一遍默认账户规则。
- **`/control/audit` 现读 worker 写的那本 store**（H1）：控制面读侧此前念 `ApiState.control`——服务装配时
  装进来的那一份，此后只会被本进程的 POST 换掉，于是隔了一个进程的执行回写永远看不见，每笔命令都被念成
  `Accepted`。新增 `ControlProvider` / `with_control_provider`，`fn control_plane()` 成为唯一现读出口
  （HTTP 路由与 `QueryPort::control_audit()` 都走它，现读后回填那份 `pub` 字段），装配侧把 provider 装在
  **与提交侧同一个** `configured_control_store` 结果上。与 G1/S3 同族，只是这次隔了进程。
- **删掉审计链上的第二个写入者**（H2）：`sync_control(plane)` 在文件 / SQLite / Postgres 三本后端各一份
  近似实现（23 + 20 + 20 行），全仓唯一调用者是 `qx-storage` 自己那条用例。H1 之后 store 才是权威，
  "把进程内那份整段推回链上"绕开 worker 的 `transact` 顺序；按 F1/E3 先例删除，用例改走 `append`，
  审计链的 append-only、幂等与防篡改覆盖一条没少。

- **投影桥没起来时 `/ready` 念出停摆与原因**（I1）：新增 `ProjectionRefresher { Unreported, Running,
  Stopped(String) }`，写入口唯一（`report_projection_refresher`，`crates/qx-api/src/lib.rs:1082`）；桥的三条
  提前返回路径各自报 `Stopped(原因)`（拓扑里没有账户级 EventLog 那一条此前完全静默），启动成功先报
  `Running`；线程退出（含 panic）由自带的 `ProjectionRefresherGuard` 在 `Drop` 里报停摆。`/ready` 只在读模型
  里确有投影时判定，且判定排在健康扫描之前——boot 那份投影永远"健康"，因为它就是被成功
  装载出来的。默认值是 `Unreported` 而不是 `Stopped("")`：否则每个不带桥的进程内装配会一启动就
  自判 503。
- **`QueryPort` 的三格与端点问同一个出口**（I2）：`job_runs()` / `ledger_entries()` /
  `reconcile_reports()` 此前直接 `state.*.clone()`，那份副本只在有人调过 `query_models()` 之后才前进。新增
  `fn query_models_last_known()`（`:1186`）作为唯一现读出口，只有 `Err` 才退回最后已知副本——trait 签名没有
  `Result`，"这一次没读到"不由它编造成空表，与 R10 的口径同一条。
- **零调用公开入口按"是不是唯一实现"分层处置**（J1）：删掉的是同一能力的多余写法——`crates/qx-api/src/lib.rs`
  那 11 颗 `serve_*` 转发壳（每颗都手抄了一份"怎么把一条连接交给同一套 handler"）与零调用的
  `control_port()`（文件 3362 → 3176 行，净减 186），`crates/qx-storage` 无 fencing token 的 `ack_job`
  （trait 声明 + 三本后端）与 Postgres 的 8 颗 `from_storage`（后者绕开 `connect_with_pool_size` 那条
  唯一构造路）。留下的九颗孤儿各自登记在 `maturity/capabilities.yaml`：`to_qifi()`、`PostgresStorage::migrate`
  与 `health_check`、`ApiState::projection_keys()`、`with_registration_correlation()`、
  `consume_batch_with_projection`、耕禄的 `sharpe_ratio()` 等——分界是"先找能力再找名字"，
  删掉唯一实现会把部署手册里那条命令变成没有对应实现的话。
- **API 的 accept 循环读停机令牌**（J2）：`serve` 此前是 `for stream in listener.incoming()`，阻塞迭代器，
  令牌翻起来也不回来。新增 `fn accept_polling`（`crates/qx-api/src/lib.rs:1911`）作为两条长驻入口
  （明文 `serve` 与 `serve_tls_mtls_with_stores`）共用的那颗循环：监听套接字转非阻塞、每轮先问令牌、
  `WouldBlock` 时按 20 ms 节拍睡、被接出来的流显式转回阻塞再设读超时。两处生产装配传
  `|| context.should_stop()`，与其余 worker 共用监督器那一枚令牌；参数是回调而不是 `Arc<AtomicBool>`，
  为的是不在 qx-api 与 qx-runtime 之间添依赖边（本轮零新依赖）。
- **CCXT 复活有界：退避 + 死轮预算**（K1）：`crates/qx-cli/src/venue_runtime/ccxt_market_worker.rs` 的
  `ccxt_respawn_delay`（500 ms 起指数翻倍、封顶 10 秒，委托 `qx_core::retry::Backoff` 而不是自己写位移）、
  `ccxt_dead_cycle_ledger`（只有整轮全失败才算死轮，任何一次成功清零，`attempted == 0` 不算）与
  `CCXT_DEAD_CYCLE_BUDGET = 5` 让"子进程持续不可用"从每秒两次的空转变成有界退避后 `Failed` 退出；
  用户流侧 `ccxt_execution.rs:333` 读同一颗预算。R3 是同一族，这次两条腿都在 CCXT 侧。
- **Outbox 的毒事件不再无限阻断链尾**（K2）：`OUTBOX_MAX_ATTEMPTS` + `outbox_exhausted` 是出站侧唯一的
  尝试判据（三本后端不自己数次数），relay 在 `claim_outbox` **之前**跳过用尽的事件并计入 `report.parked`；
  `parked` 一路到 Prometheus `qx_outbox_relay_parked` 与 relay worker 的自报行——数了却读不到，等于把
  "有条事件发不出去"重新咽回去。消费者侧的死信早就有这套，出站侧缺的是另一半。
- **文件锁的 Drop 只删自己那一把**（K3）：D1 打开年龄接管的口子之后，无条件 `remove_file` 让旧持有者在
  收尾时删掉**新持有者**的锁，第三个写者于是与第二个同时进临界区。现在锁文件登记
  `pid-serial-nanos` 令牌、Drop 先比对（读不到按"不是我的"留下）、接管动手前复读年龄、争用消息带
  `holder=`。
- **进程监督循环读停机令牌**（K4）：`supervise_workers` 新增 `stop: impl Fn() -> bool` 参数并在循环头
  每轮先问，命中即共用 `stop_managed_children` 的 kill+wait 有序收工——不需要父→子的信号通道，零新依赖，
  与 J2 同一形状。判定排在 `try_wait` 之前，否则子进程秒退时那一轮先被读成故障。用例一正一反搬进新壳
  `crates/qx-orchestrator/tests/process_supervisor_stop_token.rs`。
- **`backtest` 外层位置参数不再被子命令静默丢弃**（K5）：`qx-cli backtest a.json b.json spec --report latest`
  此前收下那三份路径却不参与回测，使用者以为换了数据实际没有；现在子命令在场且外层位置参数非空时当场
  退出 2 并说清两种合法形状，正向用例同时钉住**两种形状仍能到达各自的处理链**（只钉拒收的话，判据写宽
  成"带位置参数就拒"也能绿）。
- **血缘三格不再外销一份背不起的血缘**（L2）：`ProjectionLineage` 的 `dataset_version` /
  `manifest_digest` / `source_digest` 由裸 `String` 改成带 `skip_serializing_if` 的 `Option<String>`，
  没得说的格子不出现在 JSON 里；快照侧取那份被投影的 `EventLog::digest()`，事件侧退回整体缺席
  （进程内总线没有可发布的整体摘要，把总线名填进 `*_digest` 是外销一个不是摘要的摘要），无键读路
  拿不到镜像投影时"宁可不给信封"而不是编一个 0。§37.5 第 2 条（S9）随之关闭。
- **原子投影链的登记补上读侧那一半**（L3）：J1 只登记了写侧三颗，L3 复核时把读侧 `load_projection`
  （`lib.rs:797` 与三本后端各一颗）与那条实际走的不带投影的 `consume_batch` 一起点名——那份落盘的投影
  至今只有 `outbox_backend_semantics.rs` 的两条契约用例读过，**没有生产读者**。登记改为七条行号锁，
  漂了就红：漂掉的登记会把读者支到另一段代码上。
- **`shutdown_timeout_ms` 第一次真的接成上界**（L4）：托管收尾从 supervisor 的 `child.wait()`
  （会一直堵到进程真的消失，而那格预算当时什么都不约束）搬成 `crates/qx-orchestrator/src/reap.rs` 里
  唯一的 `reap_round` + `stop_managed_children`：步长塞进剩余预算、先判收工再判超时、超时消息点名每条
  还没收工的子进程。同一轮把 `runtime-check` 里 `snapshot(0, …)` 那格恒为假的过期判定换成
  `runtime_timestamp_ms()`，并新增 `health_observed` 把这块如实声明成"配置派生的花名册、没有心跳事实"。
- **CI 现在编译实盘文档教的那颗组合**（M3）：`sqlite,nats` 同时点亮才有的一段代码（`qx-cli` main.rs 的
  `Sqlite*Store` 导入与 `event_pipeline` 的 SQLite 分支）此前没有任何作业走过；矩阵补
  `nats,postgres,sqlite`，并新增两项判据让"代码里的每颗 `cfg` 闸门 + 部署文档里的每颗 `--features`"
  都必须被矩阵覆盖、且每颗条目单独跑 `clippy --all-targets -D warnings`。
- **最后一批零生产调用的公开项按 J1 那把尺子分完**（M4）：有替代写法的十颗删（`open_postgres`、
  `register_control_order`、`save_control_state`、`start_run_with_manifest`、`venue` / `venue_mut`、
  `advance_by` / `NANOS_PER_DAY`、`append_raw` / `raw_records`），判据同时钉"原名不许回来"与
  "替代那颗仍被生产走到"——后者红就说明收口变成了断链；唯一能力的九颗与四颗账户维度对账判据留并登记。
  来源关联号从"只钉在门禁文本上"抬到断言红：两条控制面入口各写 `control:{command_id}`、非控制面提交
  保持 `{worker}:submit:{client_id}`、补偿腿登记点名 worker / 组 / 腿。
- **调度链的四格：单位、超时收口、唯一派发判据、派不动的声明当场拒**（N1–N4）：
  `start_run_at`（`crates/qx-scheduler/src/lib.rs:506`）把 `timeout_seconds` 当毫秒加，一份 30 秒超时的作业
  deadline 落在启动后 30 **毫秒**，与 `crates/qx-cli/src/workers.rs:119` 的 `now.saturating_add(60_000)` 分叉；
  卡死的 `Running` 永久占着并发键（`is_timed_out` / `mark_timed_out` 两颗零生产读者），新增 `sweep_timed_out(now)`
  作唯一收口、生产派发每轮先扫；窗口与交易日历的判据只住在 `due_jobs_with_calendar`，生产却走 `due_jobs`，
  于是 `JobWindow` 与 `TradingCalendar` 两份声明在运行时静默不生效；注册表派不动的 `Retry` / `Event` / `Manual`
  躺在 jobs 文件里既不报错也不入队，新增纯判据 `undispatchable_by_registry`（`job_spec.rs:73`）+ 装配处
  `validate_job_triggers`，载入与新建两条路径各问一遍（与 E7 的 owner 判据同一条形状）。E7 之后这是第二条
  "调度配置能写成一句永远不生效的话"的路。
- **库内执行面去重，随之而来的十颗零调用公开项按 J1 登记**（N4b）：`run_cron_tick*` / `due_jobs` /
  `start_run` / `finish_run` 在库内与 CLI 两侧各写了一份同样的四步且会各自漂移，本轮把库内收成唯一执行体。
  代价是那批入口从此没有生产调用者，逐颗登记在 `zero_caller_library_surface_kept_by_design`；留下
  `run_event_with_manifest` / `run_manual_with_manifest` 的理由是它们是 `Trigger::Event` / `Manual` 在仓内**唯一**
  的派发者——删掉等于把两条触发声明变成只能被装配处当场拒绝，这正是 N4 那颗守卫必须同时存在的原因。
- **`strategies[i].cost_rules_path` 从"静默丢掉"改成"当场拒"**（N5）：`config validate` 逐条校验并打印 PASS、
  回测链会投影进 `.strategy` 后读取，而 Paper / Execution worker 只读顶层 `config.strategy.cost_rules_path`。
  `execution_cost_binding_from_config`（`crates/qx-cli/src/runtime_wiring.rs:184`）先问
  `unapplied_cost_rules_declaration`（`runtime_check.rs:175`），报法直接点名那一格；`config validate` 与装配走
  同一颗判据，"校验说没问题、装配跑不动"不可能再分裂。本轮**没有**把绑定做完（执行平面接受多份 binding
  是另一件事），处置是 R11 那条口径的第二次应用：不支持就说不支持。
- **两颗零引用重复写法删除，M4 那把尺子改认可达性**（N6–N7）：`submit_order_via_gateway_with_risk` 只是
  转发壳、`complete_reconcile` 零引用而 `reconcile_snapshot` 自己会把状态推到 `Live`（留着等于允许跳过真实
  比对直接改状态）；`m4_zero_caller_surface_check` 的"活替代仍被生产走到"原先只问符号有没有出现在文件文本里，
  于是替代那颗其实住在 `#[cfg(test)]` 之后也能绿——新增 `m4_production_text()`（`tools/check_architecture.py:3336`）
  把正文收成"第一个测试模块之前"。J1 已登记过"认名字不认能力"，这次是同族的另一格，且这格能修就修了。
- **子进程 stdin 的写第一次有预算**（N8）：新增 `crates/qx-adapter/src/io_budget.rs` 的 `write_all_within`，
  把写交给一颗命名线程、用带截止时间的 channel 收它，预算内没收回来就 `on_timeout`（生产里是杀子进程，
  管道读端随之关闭，卡住的写线程自己以 `BrokenPipe` 收尾）。三处生产调用点（CCXT Worker、策略 worker、事件
  consumer handler）的"响应超时"判定原先都排在写**之后**——`timeout_ms` 只保护了读、没保护写。成功时把 writer
  原样还回来复用同一条 stdin，失败不带回 writer、调用方置空句柄走既有"stdin 不可用"出口。零新依赖。
- **Venue 订单缓存封顶，且只退终态**（N9）：新增 `crates/qx-adapter/src/venue_cache.rs` 作两个 venue 共用的
  唯一判据（`MAX_CACHED_ORDERS = 8_192`、退到 `8_192 - 1_024` 的迟滞线、越限才扫表），每处订单写入都过封顶
  （binance 3 处、ccxt 2 处），退场时级联清掉 `remote_ids` / `seen_trade_ids` / `cumulative_costs` /
  `seen_fill_keys` / `venue_order_ids` 五份派生索引。**两条限制是设计的一部分**：活跃订单永不退场（去掉
  `is_terminal` 那层筛选就是拿丢活跃单换内存），以及被退场的单迟到回报仍按"未知本地订单"升级对账而不是静默。
- **一条薄行情不再能带走整台节点**（N10）：静默原先在读侧被当故障返回 `Err`、会话层把它读成"这条流完了"
  直接 `return`，而 `crates/qx-orchestrator/src/lib.rs:282-299` 那颗监督器是**进程粒度**的（任一托管 worker
  退出即 `Err("managed worker {id} exited…")`，仓内没有线程级重启）。修法是三态而不是多包一层重试：
  `WebSocketRead::{Message, Idle, Closed}` → `BinanceStreamRead` 同名三态 → 共用引擎 `run_binance_stream`；
  `Idle` **只**在帧边界成立（`filled == 0` 且 `WouldBlock | TimedOut`），既不切断会话也不复位预算（否则又回到
  R3 那颗的反面），EOF 落在帧边界仍是 `Err`；全仓只有一处三态映射点 `read_or_forward`。行情 worker 改走同一颗
  引擎，两条流的预算都是 `BinanceStreamRetryPolicy::new(10, 1s, 30s)`；订阅回执那一格故意保持严格。
- **WS 长度预算两处都问**（N11）：`MAX_WEBSOCKET_MESSAGE_BYTES = 16 MiB` 原先只量单帧，分片每片单独合法、
  累计无界。现在帧头声明一超预算就**在读取载荷之前**失败（不为收不下的消息先分配那 16 MiB），分片累计超预算
  报"累计"。

- **`error_code` 那一格只装"这一次为什么失败"**（O1）：`Scheduler::finish_run*` 成功一侧原本把调用方
  递进来的结果摘要原样落进这一格，而 `/scheduler/runs` 原样外销它、重试判据又拿它匹配
  `retryable_codes`——一条摘要在读面上就是一个从没发生过的错误码。写入处改成 `if success { None } else { … }`，
  worker 侧成功分支同步交 `None`；反向那半（失败带码必须照常留码）一并钉住，否则"这一格永远写不进东西"
  这种退化也能绿。
- **`strategies[]` 剩下六格声明从"静默走内核默认"改成"当场拒"**（O2）：N5 只给 `cost_rules_path` 记过
  那道断层，本轮把同一颗判据铺到 `fill_model` / `initial_cash_raw` / `product` / `risk_rules` /
  `margin_mode` / `allow_short`——命令行回测四处读点只取顶层 `strategy.<字段>`，而 `config validate`
  逐块体检把实例也算在内，于是"只在实例里写了撮合口径"的配置过得了校验、跑起来不按它走。唯一实现
  `unapplied_strategy_declaration`（`crates/qx-cli/src/backtests/config_declarations.rs:43`）文案同时
  交代后果与"顶层当前生效的是 X"。**判据的边界是"这条入口读不读这一格"**：`strategy backtest` 那条链
  逐块装配，实例声明在那儿真生效，所以门禁额外钉了一条负向项——`config validate` 不得接管这颗判据。
- **调度作业自检改问派发器问的那三个问题**（O3）：`ecosystem-smoke` 原先只数 `ready_jobs` 的长度，
  N4 那类"注册面上根本派发不到"的声明在夹具里照样报 `ready=1`；现在与 `config validate` 共用
  `undispatchable_by_registry`，再加依赖满足与 `due_jobs_with_calendar` 到点，三问齐了才算过。
- **停机自报里必须有重连计数**（O4）：`reconnects` 只在 `run_binance_stream` 内部推进，那两行
  `ServiceStatus::Stopped` 的 message 是它通向监督器的唯一出口。少了它，一条整轮都在重连的流与一条
  从头连到尾的流停起来一模一样，事后查不回来。
- **CCXT 复活预算改成按标的记账**（O5）：两份标的里固定坏一份时，K1 那两半**同时**失效——好的一份每轮
  复位连胜、坏的一份每轮清零"整轮全败"，复活于是没有上界、退避永远停在第一档。新增
  `respawn_strikes: HashMap<String, u32>`（键 `ticker:<instrument>` / `ohlcv:<instrument>`），两颗失败点
  各自问一次 `ccxt_respawn_allowed(strikes, CCXT_RESPAWN_STRIKE_BUDGET=3)`，成功点逐点复位。
- **常驻用例的首行读自己带上界**（O6）：J2 那颗停机用例的 5 秒上界排在 `read_line` **之后**，管不到
  这一次等待——"接客但不吐 HTTP 首行"的回归不会让用例红，而是让它挂住并带走同一颗作业后面的所有用例。
  测试客户端改成先 `set_read_timeout(5s)` 再读，超时以断言红收掉。
- **WebSocket 握手块有整体截止**（O7）：逐字节读意味着"单次读有界"不等于"整块有界"——socket 超时只管
  一次 `read_exact`，一个每 9 秒吐一个字节的对端能把一次握手占住 65536 × 超时，而 64 KiB 上限只在累计
  之后才问。新增 `read_header_block(reader, budget)`（`qx-adapter/src/lib.rs:509`），每一字节之前问一次墙钟。
- **PostgreSQL 取锁与连接阶段第一次真的有界**（O8）：两颗取锁入口原本裸 `Mutex::lock()`，一条被黑洞掉的
  连接能让持有者永远停在 socket read、后来者跟着无限排队（停机令牌问不到、`/ready` 读不到新东西）；
  新增 `lock_slot`（`try_lock` 轮询 + 60 秒预算，`Busy` 文案点名"持有者卡在往返里，不是慢"）。M1 那颗
  "连接阶段的界推给驱动与 DSN"当时只存在于注释里——`dsn_with_connect_timeout` 只在调用方没写时补
  `connect_timeout=10`，`key=value` 与 URI 两种 DSN 形状各按各的连接符补，显式给过的一字不动。
- **三处地址解析共用一颗带预算的入口**（O9）：`to_socket_addrs()` 走系统解析器且代码侧原本一个界都没有，
  一台黑洞掉的 DNS 能让 worker 停在 `send` 里几十分钟，既读不到停机令牌也回不到循环头，而
  `connect_timeout` 只界住解析**之后**那一跳。新增 `within_budget` + `resolve_socket_address`
  （`qx-adapter/src/lib.rs:469`、`:488`），WSS / TLS HTTP / plain HTTP 三处共用；裸 `to_socket_addrs()`
  全仓只剩实现内部那一处。**代价如实记下**：预算管的是调用方最多等多久，不取消那颗解析线程。
- **没有执行者的控制命令在受理处当场拒**（P1）：八类 `CommandKind` 里只有 `SubmitOrder`（venue 执行
  worker 领）与 `PauseStrategy` / `ResumeStrategy`（Strategy worker 领）有人领取，其余五类过去拿到 202
  与一条 `Accepted` 审计然后永远停在 pending。判据长在唯一的受理处 `ControlPlane::submit_validated`，
  在写入 `Accepted` 与发审计**之前**返回 `ControlError::Invalid`，HTTP 侧因此回 400。三条边界各自钉住：
  不落进结构性 `validate`（修复前落盘的历史命令要能读回来处置）、八类变体一个都不删（历史审计要能
  反序列化）、判据位置必须排在审计之前（写在之后就成了"先记账再说不干"）。
- **内核里三份从未接线的死实现删掉，四处替它们背书的口径同时改口**（P2）：`qx-core/src/engine.rs`
  （`Engine` / `EngineCtx` / `EngineRunOptions` / `EngineRunReport` / `Handler`，5 颗自测）、
  `qx-core/src/queue.rs`（`CausalQueue`，2 颗自测）与 `clock.rs` 里的 `TestClock`（1 颗自测）全仓零生产
  调用，而 README 设计底线、`qx-core` crate 文档、`Cargo.toml` description 与 `tools/validate_core.py`
  都在为"确定性时钟""因果事件队列""不用系统时间"作保。`clock.rs` 不能整份删（`pub type Ts = u64;` 被
  event/order/sourcing 使用），收成那一格单位口径；口径改到两条**真实推进路径**——回测按 bar 序列推进
  （`qx-xingban`）、实盘按到达顺序把交易所/进程戳推进 `EventLog`（`qx-runtime/src/pipeline.rs`），
  因果优先级 `Priority` 留在 `event.rs:126` 由 `(ts, prio)` 单调落盘判据执行，不再假设有乱序调度队列。
  `validate_core.py` 把 `CausalQueue` / `TestClock` 标成本文件自用的参考形状，同时保留一条真判据：
  `PRIO_*` 数值必须与 `event.rs` 的 `prio` 逐项相同（由门禁钉住）。
- **有实现、没接线的那颗留在原地并登记**（P3）：`crates/qx-runtime/src/data_binding.rs` 的三颗公开面
  （`:13` / `:78` / `:165`）是"研究快照必须绑到一份不可变数据集与为该数据集编译的那份因子计划"的唯一
  实现，而 `DatasetResolver` 由谁提供没有裁决过。按 J1 那把尺子的"唯一实现的孤儿"处置：不删、不为它
  编一个调用方、门禁 `M4_KEPT_SURFACES` 三行活行号 + `maturity/capabilities.yaml` 逐颗点名锚与未决原因。
- **clap 命令表里 18 颗"通告了但零用例证据"归零**（Q1）：`ecosystem` / `doctor` / `runtime-check` /
  `live-check` / `paper-check`≡`paper-e2e` / `strategy list` / `config explain` 七条此前只在 help、命令表
  与分派分支三处名字相等，没有任何用例启动过它们；补的是**子进程按名字点**的一批（`crates/qx-cli/tests/
  cli_dispatch.rs` 5 → 12 颗），判据是各分支自己打出的第一句结论或最后一句失败，不是 clap 的用法错误。
  过程中先拆掉一处**测手法而不是测入口**的用例：把 Paper 模板复制进临时目录会连带 `instrument_spec_path`
  / `jobs_path` / `target_snapshot_path` 三颗相对 `deploy/` 的引用一起搬家，验收链以 `processed=0` 退 2，
  于是删掉那个隔离助手、改为在仓库根目录零参数跑打包模板（`data/qianxing-paper` 已被 `.gitignore` 挡住）。
- **同一份 bar-frame 两侧口径相反**（Q2）：`deploy` 里两份 pairs 示例各带 `frequency` / `quality` 两格
  没有任何读侧认得的键——Rust 静默丢掉、Python 严格模式当场拒。收法是**收这一类而不是收这一颗**：
  两格示例删掉，Rust 读侧对 `schema_version>=1` 的文档按名单拒未知键（名单三方相等：读侧认的 = 写侧印的
  = Python 允许的，共 9 格），未知键那一步只走 `serde_json::Map` 问键名、不把值过一遍 `Value`，因为
  serde 的 `Content` 没有 i128 档、会把超过 u64 的定点价量塌成浮点。旧文档（无版本号）仍走兼容分支，
  `crates/qx-datastruct/tests/frame_contract.rs` 3 → 5 颗正反用例各钉一次。
- **21 颗"零生产调用 + 仓内唯一写法"留在原地登记，删掉的只有第二种写法**（Q3）：按 J1 那把尺子重量
  剩下的公开面，因子 / 账本 / 回测 / 适配器 / 插件 / 更路 / API / 文件状态八侧共 21 颗一颗不删（删掉
  就等于把某条能力在仓内的唯一一份实现连根拔掉），删的是更路 `Metrics` / `BasicAnalyser` 与星板
  `run_batch` 三颗——那份绩效汇总与那条批量入口在别处已经有活的写法。逐颗锚点与"接还是删"那句话一起
  登记在 `maturity/capabilities.yaml`，代价由既有的行号判据付（`M4_DEAD_SURFACES` 与"替代写法仍被生产
  走到"两项的文案同时扩到 Q3 这批名字，门禁项数不变）。
- **README 替卯眼两条走不到的能力作保，判据自己也被抓到两次盲区**（Q4）：`sign_ed25519`、
  `Profile::assemble`、`bootstrap_plan` 在全部 crate 的生产文本里零调用点，而 crate 表那一行把"Ed25519
  签名 + 静态装配计划"念成能力；README 因此在两处改口（速查行、"三条容易被读过头的边界"那一节），并
  明确分开**接到的**那条 C ABI detached Ed25519。第一次收紧（全文出现过 marker）被对照变异证伪：接上三颗
  里任意一颗、剩下两颗仍没接，那句话既不消失也不红；第二次改成逐颗比较"生产文本里有没有调用点"与
  "README 引用它的那一行有没有说它走不到"，速查行按能力族比较；第三次是把行号判据改成**从 README 抠出
  它自己印的数字**再回源码核对——原先那版拿门禁里硬抄的 (file, line, symbol) 比源码，实测改 README 里的
  `:259` 为 `:261` 不红，整个"文档漂移"类都不在射程内。
- **台账里那些 `path:NNN` 行号引用第一次有了判据**（R4-1）：起点是 Q3 删完之后 `sharpe_ratio` 那一行仍指着
  已被删掉的写法、V11 的 J1 保留表同样没跟上，但真正收口的是判据。新增 `capabilities_citation_check`
  （`tools/check_architecture.py:5921`）六颗：被引文件在场、并列的名字与并列的行号一颗对一颗、行号不越出
  被引文件长度、指过去那一行不是空行、点名某颗东西时被引那一格里就是那颗、引用密度地板
  `CAP_CITATION_FLOOR = 90` 与带名绑定地板 `CAP_BOUND_NAME_FLOOR = 20`。台账现有引用 194 处 / 带名绑定
  63 处，前五颗各 0 命中。**`docs/` 明确不在扫描面内**：方案文档按轮次追加、引用的是历史时刻的形状，挂上
  "行号必须指向今天的代码"只会让人在下一轮开头先把判据放宽。7 跑变异全部 KILLED。
- **门禁脚本自己也被审**（R4-2）：仓库各处都有"零调用点"判据，唯独这份八千余行的脚本里没有。新增
  `gate_self_honesty_check`（:6845）三颗走 `tokenize` 而不是正则——常量名出现在注释或字符串里不算有人用
  它。实测模块级常量 356 颗、孤儿 0，`*_check` 判据 71 颗、未被 runner 调用 0；第一颗清掉的孤儿是
  `BINANCE_STREAM_FILE`（N10 之后就没再走过）。地板排在孤儿之前是故意的：**判据被改瞎时孤儿列表天然为空、
  看着像通过**，所以必须先红在地板。
- **插件的两颗生命周期超时进了口径与判据**（R4-3）：`healthcheck_timeout_ms`
  （`crates/qx-plugin/src/lib.rs:122`）与 `shutdown_timeout_ms`（:123）被 `validate`（:169）问非 0、被
  `canonical_hash`（:240）计入指纹——两件事都不构成等待，仓内没有一处把它们换算成 `Duration`，是 L4 的
  同族漏网。README 两处（crate 表速查行 :38、"三条容易被读过头的边界" :307）改到与实际可达性同口径；
  `plugin_claim_honesty_check`（:6730）带一颗**反向排除**，正则把 `config.shutdown_timeout_ms` 那处合法的
  真接线排除掉，撤掉它门禁当场把真接线读成违规。
- **三处契约面同时补齐，且契约只有一份**（R4-4）：`on_order_update` 除适配器自身之外仓内零派发，判据把
  这份零派发钉成在册状态而不是让它伪装成一条活链；`schemas/strategy_api_v1.schema.json` 此前**零读者且与
  读侧矛盾**（顶层漏了写侧恒印的 `instrument` / `target_qty`、意图层漏了多腿那三格政策、
  `additionalProperties: false` 让每份真实载荷都过不了它），现在有三个常驻读者并由判据逐颗点名（:4908），
  另加"多抄一份 `*strategy*.json` 即红"；账户快照契约补齐 `header.trading_day`、`positions` 少的那一格必填
  与 `orders` 键本身的形状。新壳 `crates/qx-runtime/tests/strategy_api_schema_contract.rs`（275 行、4 颗）。
- **`strategy.leverage` / `position_mode` 在回测链当场拒**（R4-5）：O2 / N5 同族的最后两格——两条 Bar
  内置回测链把这两颗读进配置、既不接线也不吭声，回测以 1x 全仓静默跑完并给出一个能看的收益数字。落点判据
  只有一处实现：`PRODUCT_POLICY_FIELDS`（`crates/qx-cli/src/backtests/config_declarations.rs:76`，字段表与
  后果文案同住），四条内置策略链各有接线；`config validate` **不接管**——那两格在实盘、Paper 与跨语言回测
  上都是真生效的，写成全局校验会误杀三条能走的链。新壳 `crates/qx-cli/src/tests/backtest_product_policy.rs`
  （218 行、3 颗）。
- **`/account/snapshot/diff` 的历史表从只进不出变成有界容器**（R4-6）：投影桥每 250 ms 重装一次账户快照，
  历史表过去只进不出。新增 `crates/qx-api/src/snapshot_history.rs`（128 行）：`MAX_SNAPSHOT_HISTORY = 1_024`
  （:19），`insert`（:31）先把这次写入的摘要**挪回队尾**、再从队首退到界内（:37）。"挪回队尾"是关键一步：
  安静账户里那份唯一的历史基线会一直停在队首，不挪就会被下一轮波动连同客户端正在用的 `base` 一起退掉，
  而这条端点对退掉的 base 只有 409。全局那一格与每份带 `account_id`/`venue_id` 的投影各持一份
  （`crates/qx-api/src/lib.rs:507` / :603）——只封全局等于没封。
- **针路 OMS 的订单表刻意不共用 venue 那把封顶**（R4-8）：`Oms { orders }`（`crates/qx-zhenlu/src/oms.rs:14`）
  被 `pipeline.rs:444` 长期持有，照抄 `MAX_CACHED_ORDERS` + `evict_stale_terminal_orders`
  （`crates/qx-adapter/src/venue_cache.rs:18` / :25）是错的——那条退场规则的前提"被退场的订单再有回报就走
  `ReconcileRequired`"在实盘 ingest 一侧成立（`pipeline.rs:1390`），在冷启动回填一侧不成立：
  `rebuild_runtime_indexes`（:1224）按事件顺序把整条 EventLog 重放进 OMS，其中 :1244 对
  `apply_fill(fill)?` 直接打问号上抛，于是退掉一笔早期终态订单就等于把它之后每一笔成交都变成"订单不存在"。
  **照抄封顶的净效果不是省内存，是把代价从内存换成一份再也打不开的长日志。** 本轮登记"刻意的不同"与两头
  代价各一颗判据（:4213 / :4231）。
- **API 自己的 HTTP 读侧有了整体截止**（R4-9）：O7 收的是 WSS 握手块，这颗是同族的另一侧。`serve_stream_as`
  （`crates/qx-api/src/lib.rs:1811`）在 :1820 把 `HTTP_REQUEST_BUDGET`（:2186，5 秒）交给 `read_request`，
  截止排在循环里**每一轮 read 之前**；此前只有单次 read 的 100 ms 套接字超时——一个每次挤 1 字节的客户端
  可以把一条连接无限占住，而 `/ready` 那侧一切正常。两颗常驻用例
  （`a_dribbling_client_hits_the_overall_budget_not_the_size_cap` :3378 与
  `a_header_split_across_reads_ends_the_request_at_its_own_length` :3406）。
- **两条补散度恢复循环不再以 100 ms 重生 venue**（R4-10）：`worker_entry.rs` 里 Binance（:199）与
  CCXT（:444）两条恢复循环过去把"每 100 ms 重建一次 venue"当空闲轮询，而 CCXT 那侧重建 venue 就是重启一个
  Python 子进程——停滞期间以 10 Hz 反复 spawn。新增共用的 `spread_recovery_poll_interval(stalled_rounds)`
  （`crates/qx-cli/src/spread.rs:240`）：停滞 0 轮回到 100 ms 常规轮询，此后取 K1 那份指数退避
  （`ccxt_respawn_delay`）封顶 10 秒。

- **控制面从「只追加」换成「终态退场 + 有界审计窗口 + 累计摘要」**（R5-1，即 R4-7 的裁决落地）：
  `ControlPlane` 过去只有 `Vec::push`，`commands` / `requests` / `audit` 三本都只长不收，一条已执行完的命令
  连同它的身份会被背到重启。现在命令一进终态就经唯一的 `retire` 离开主表（另一条只在 `from_json`，收敛旧
  状态文件的存量），`execute` 不自己 `remove`；审计流水留在 `AUDIT_WINDOW_RECORDS = 1_000` 条的窗口里，
  只由 `trim_audit_window` 一处裁剪、一次裁到配额的一半，且**成对裁**（`Accepted` 与终态记录同进同出，
  留半对等于写一份自家 `from_json` 会拒收的状态）；窗口之外只剩 `RetirementSummary` 的累计计数，
  `/control/audit` 一次交出 `records` 与 `retirement` 两格。"什么算终态"只有一颗
  `CommandStatus::is_final`（qx-control/src/lib.rs:131），退场、裁剪、恢复期配对三处都问它。
- **哈希审计链的两半都接上了**（R5-2，即 H3 的裁决落地；本轮先把定性改口：此前说的"写侧有、读侧无人验"
  是错的，当时**写侧与读侧都不存在**）：写侧 `chain_audit`（qx-storage/src/lib.rs:1538）是三本后端唯一写入者，
  各自在已经持有锁/事务的 `transact_control` 里接一次（file/state.rs:82、sqlite.rs:1046、postgres.rs:1511），
  `AuditChainWriter` 的 `tail` / `drop_from` / `append_entries` 三个动作都不越出那次事务；顺序是**链先落、
  状态后落**，崩在中间留下的残尾由下一笔事务按检查点截掉（`truncate_from_unlocked`，只认 65_536 字节尾部窗口），
  检查点比对失败直接失败关闭——**写不进链的命令也不会写进状态**。读侧 `verify_audit_chain`（:1491）判两件事：
  链自身逐环连续，且状态里的检查点正指链尾；生产读者是 doctor 新增的 `audit_chain` 检查
  （crates/qx-cli/src/doctor_report.rs:318），文件与 SQLite 两本逐环核对，PostgreSQL 那本按"不触网"的口径
  报**未扫描**而不是通过。
- **内置策略的轮内序号不再被当成全店身份**（R5-3）：`BuiltinStrategy::new` 每轮把 `next_intent_id` 置 1
  （qx-strategy/src/builtin.rs:227）而 worker 每轮新建一份，契约侧过去把 `intent.intent_id` 直接当 `client_id`，
  后者又同时是 `command_id` 与 `request_id`（strategy_contract.rs:712-713）——第二次产生订单必然撞
  `DuplicateRequest`，digest 不同时在 spread.rs:460 硬错。改法是换身份来源而不是让策略记数：
  `strategy_order_identity(round_scope, intent_id)`（:443）把"哪一轮"与"轮内第几条"折成 `client_id`，
  `round_scope` 取契约输入的 `request_id`；同轮重放幂等、换轮得新身份，`trace.intent_id` 保留原值不影响归因。
  与 R5-1 是耦合的：控制面一旦开始退场，"身份按轮稳定"就从洁癖变成前提。
- **词表里不再有没人写的状态，摘要里不再有永远为 0 的格**（R6-1）：`CommandStatus` 曾带着第四颗 `Rejected`，
  而全文件没有一处构造它——五处出现全是读侧模式；P1 把"没有执行者的命令"改成在写下第一颗审计记录之前当场拒，
  "受理期拒绝"这一格在生产里根本不会留下记录。R5-1 顺手加进 `RetirementSummary` 的 `rejected` 那一格随之
  结构性为 0，却和其余三格一起外销给 `/control/audit`（L2 同族）。两颗一起删：词表只剩 `Accepted` / `Executed` /
  `Failed`，`is_final`（crates/qx-control/src/lib.rs:132）认领两颗终态，摘要剩 `retired_total` / `executed` /
  `failed` / `last_retired_ts`。新增一颗**闭合性**判据（tools/check_architecture.py:3408）：声明的变体、有生产者
  的变体、`is_final` 认领的集合三者必须互相推出。
- **paper-check / paper-e2e 只认本轮新增的事实**（R6-2）：`run_paper_pipeline_once`
  （crates/qx-cli/src/venue_runtime/paper_worker.rs:287）过去拿 `pipeline.orders()[0]` 的 `Executed` 审计作保
  ——那是 OMS 里**最旧**的一条，而 `data_dir` 是持久的，第二轮在上一轮的订单上照样打印 ✓。现在开跑前先取基线
  （`baseline_orders` :332 / `baseline_ledger` :337，且必须早于 `run_scheduler_worker`），"本轮"是差集而不是
  存在集（:373 的基线过滤），每颗新增各自要有一颗 `Executed`（:407-410：停在 `Accepted` 的也算有一条审计），
  账本既不能缩短（:376）也不能在本轮下过单之后原地不动（:417）；本轮没有新增**且**基线也没有订单才是"无事可做"
  （:381），验收行把 `new_orders={}` 念出来（:424）。`paper_e2e_entrypoint_runs_scheduler_strategy_execution_and_ledger` 跑两轮并断言订单总数仍是 1。
- **入队失败不再被念成已交给执行者**（R6-3）：`POST /control/commands` 把命令写进 store 之后
  `let _ = enqueuer(...)` 吞掉队列失败仍回 202，而 202 那句话的内容正是"已经交给执行者"。现在失败当场 503 +
  错误体点名 `control_command_not_queued`（crates/qx-api/src/lib.rs:1754），受理那一半不回滚（记录仍在审计里等补投），
  装配处的接线格子 `with_command_enqueuer`（crates/qx-cli/src/api_service.rs:103）把入队错误原样上抛；常驻用例
  `a_command_that_cannot_be_queued_answers_503_not_202`（crates/qx-api/src/tests.rs:323）两面都判——换成一写得进
  的队列必须照常 202，否则判据就退化成"什么都不受理"也照样绿。
- **"什么算终态"在跨进程那一侧也不再各写一遍**（R6-4）：worker 重放审计时四处各自 `matches!` 过状态，现在
  `command_is_final`（crates/qx-cli/src/spread.rs:481）是 qx-cli 侧唯一入口，四个调用者
  （binance_submit.rs:295 / ccxt_execution.rs:118 / paper_worker.rs:149 / workers.rs:376）只是把控制面那颗
  `is_final` 搬到 `latest_audit` 上；门禁 :3391 随之从"三处"改口成"四处"，并加上 `.is_final(` 调用数下界。
- **死信重放不再是"取一页再筛"**（R7-1）：`replay_dead_letter_from_store`（`crates/qx-cli/src/event_pipeline.rs:665`）
  此前按窗口取最旧的一页、再在那一页里筛点名的 `event_id`，于是一份确实躺在死信表里的事件被念成"找不到"——
  危害是假阴性而不是慢：运维按这一句决定不用管，那笔已经放弃自动重试的投递就静默留在原地。现在 trait 只留点查
  一颗入口 `dead_letter`（`crates/qx-storage/src/lib.rs:707`），三份后端各自取 `attempts` 最大的那一行（重放会追加
  新行，最大次数才是最后一次入账）：`max_by_key`（`crates/qx-storage/src/file/consumers.rs:192`）、
  `ORDER BY attempts DESC LIMIT 1`（`crates/qx-storage/src/sqlite.rs:337`）、同型的 `dead_letter`（`crates/qx-storage/src/postgres.rs:504`）；
  契约用例共用 `assert_dead_letter_point_query`（`crates/qx-storage/tests/outbox_backend_semantics.rs:56`），夹具刻意跨过
  2 与 10 的数位边界——那同时是"取最大"对"取最新"、以及 R7-2 那颗字典序的分界。
- **TEXT 存的 u64 列在三处排序上收进同一口径**（R7-2）：`created_ts`、`enqueued_ts`、`command_id` 在文件后端是数字、
  在 SQL 后端是 TEXT 里存着的数字，于是 `ORDER BY` 给的是字符串序。三处排序各有一份 file/sqlite/postgres 实现，收的是
  同一条口径：sqlite 侧 `ORDER BY CAST(e.created_ts AS INTEGER)`（`crates/qx-storage/src/sqlite.rs:597`）与
  `ORDER BY CAST(c.enqueued_ts AS INTEGER)`（`crates/qx-storage/src/sqlite.rs:1177`），Rust 侧
  `jobs.sort_by_key`（`crates/qx-storage/src/sqlite.rs:1664`）、`events.sort_by_key`（`crates/qx-storage/src/file/outbox.rs:58`）、
  `commands.sort_by_key`（`crates/qx-storage/src/lib.rs:1220`）；Postgres 不把判据搬回应用层重排，而是换表达式索引：
  `qx_outbox_pending_numeric_idx`（`crates/qx-storage/src/postgres.rs:315`）替掉旧的裸列索引那一句
  （`crates/qx-storage/src/postgres.rs:314`）——只改 `ORDER BY`、留着旧索引，等于让规划器继续按一串没人按的键做写放大。
- **快照键表的行内自指 id 由约束兜住**（R7-3）：`state_hash` 只吃 map 的键，行内那一份 `order_id`/`fill_id`/`transfer_id`
  从不进摘要，所以"写侧恰好一致"不是契约——改过 `fill_id` 的快照照样通过校验。四张表共用的一条纪律收进
  `check_keys`（`crates/qx-protocol/src/lib.rs:203`），反例用例是
  `key_tables_reject_rows_whose_inline_id_disagrees_with_their_key`（`crates/qx-protocol/tests/snapshot_single_source.rs:775`）。
- **作业声明里的三格无人格删掉，读侧继续容忍**（R7-4）：`JobSpec`（`crates/qx-scheduler/src/job_spec.rs:98`）此前的
  `input_refs`/`output_refs`/`permission_scope` 没有任何读者，却是作业 JSON 的**必填**格——运行者手抄三份没人执行的声明，
  其中 `permission_scope` 这个名字还替一种并不存在的授权作保（权限判定在控制面那边）。删之后不改 `deny_unknown_fields`：
  升级前写下的调度状态与队列信封还带着那三格，读崩旧状态不是收口，
  `retired_job_spec_keys_are_ignored_by_the_loader`（`crates/qx-scheduler/src/lib.rs:830`）钉的就是这个选择。
- **`/control/audit` 的流水与摘要出自同一次现读**（R7-8）：H1 收的是"念 boot 那一份"，本轮收另一半——trait 侧
  `control_retirement`（`crates/qx-api/src/lib.rs:2146`）此前分两次读，两次读会把"这一页窗口"和"窗口外累计"念成
  两个时刻的账。降级从此只住在唯一出口里：`control_plane_last_known`（`crates/qx-api/src/lib.rs:1209`），
  `control_audit`（`crates/qx-api/src/lib.rs:2139`）与摘要各取自己那一格、共用同一份现读结果。
- **锁的年龄只由判据交出**（R7-9）：`decide_lock`（`crates/qx-core/src/file_lock.rs:51`）已经收了"同一场竞争只接管一次"，
  本轮收的是抢锁循环在 `Takeover` 支里自己再 `age_of` 一次——同一个判断的两种写法，删不删取决于两次读之间的抖动。
  现在两条支路只按判据带回来的年龄说话（`crates/qx-core/src/file_lock.rs:148`、`crates/qx-core/src/file_lock.rs:162`）。
- **审计链的摘要输入形状由金色记录钉住**（R7-7）：词表钉子
  `audit_chain_status_vocabulary_is_pinned_by_literal_words`（`crates/qx-storage/src/tests.rs:478`）只钉住名字，本轮补上
  整张输入的摆法：`audit_chain_digest_inputs_are_pinned_by_a_golden_record`（`crates/qx-storage/src/tests.rs:515`）——
  改输入形状的人要在用例里改口，而不是在别人的机器上看到断链。

### Added（常驻用例与门禁）

- 架构不变量 **328 项 → 439 项全绿**（本条记到 L/M 为止；422 是 K 轮末的数，L/M 那 17 项列在同一括号里
  却没跟着改总数，本轮按实测改口为 439；N 轮的 +10 见下一条）（G1 那 3 项：装载出口唯一、调用点在轮询循环之内、用例在起桥前后
  各写一笔；H1 那 3 项：现读出口唯一、读写从同一本 store 解析、用例在装配服务前后各走完一次命令生命周期；
  H2 那 1 项：审计链每本存储只留 `append` 一个写入入口；I 轮那 5 项：写入口唯一且每条失败路径都报停摆、
  判定顺序与"确有投影"前提、端到端用例真的 stop/join 并复读、三格共用同一个现读出口且用例先于
  请求、逐格服务三台且兜底不得退化成空表；
  J1 那 3 项：`pub fn serve*` 名单闭合到四颗、队列与构造路只留那几条（含 `control_port` 不回来）、留下的两颗网络入口确有生产调用点；
  J2 那 4 项：循环体每轮问令牌且两侧复位非阻塞、两条入口都交给同一颗 `accept_polling`、两处生产调用点各问一次 `should_stop()`、限时用例在位且上界是 5 秒；K 轮那 14 项：共用退避与一颗预算且固定 500 ms 不回潮、
  用户流那道闸门按**整条条件**计数（存在性判据被 K1-M3 当场证伪）、预算判据出口唯一且三本后端不自己数次数、
  跳过排在 claim 之前并单独计数且不占名额、`parked` 一路到 Prometheus 与自报行、Drop 先比对令牌再删、
  接管动手前复读年龄、监督循环每轮先读令牌且排在 `try_wait` 之前、装配处交的是真令牌而不是常量判据、
  K1/K2/K4/K5 的用例各有正反对照；**L / M 那 17 项**：血缘三格可缺席且未声明的格子不上 wire、快照摘要
  取自那份事件源、事件信封不再声称背不起的血缘、L2 用例两侧四颗在位（L2 4 项）；投影链登记行号逐条指到
  定义本身、登记写侧读侧两侧点名、三本后端的 `load_projection` 仍无生产读者（L3 3 项）；托管收尾只有一处
  实现且按预算轮询、轮询有界性是可判定的算术、体检用真实时钟并把健康块声明成花名册、L4 用例在位（L4 4 项）；
  矩阵点亮代码里每一颗特性闸门与文档教出的每一颗构建、每颗条目单独跑 clippy（M3 2 项）；M4 删掉的十颗不许
  以原名回来、替代写法仍被生产走到、留下的公开面与活替代行号逐条对得上、两条登记在位（M4 4 项））；
  整树 `cargo test --workspace`
  **846 passed / 0 failed / 0 ignored / 86 个 `test result:` 壳**
  （壳的口径：23 个 unit 目标 + 42 个集成测试目标 + 21 个 doc-test 目标，比 K 轮多的两颗壳就是 L2 新起的
  `qx-protocol/tests/projection_lineage.rs` 与 `qx-api/tests/snapshot_envelope_lineage.rs`；
  `--all-features` 同形状
  850 passed / 0 failed / 4 ignored / 86 壳）；
  `CLI_TEST_FLOOR` 210 → **242**（磁盘 `^#\[test\]$` 重测：`src/tests` 189 + `crates/qx-cli/tests` 53，
  多出的一颗是 L4 的 `runtime_check_roster.rs`）。
  `cargo fmt` / `clippy -D warnings` 均为 0（K 轮重跑：三种基线 + `sqlite,postgres` 共六种特征组合，九次全 0；
M3 之后新进矩阵的 `nats,postgres,sqlite` 单独实测 EXIT=0，本轮没有新增任何 `#![allow]` 与新依赖），
  零新依赖、零新增 `#![allow]`——clippy 在收口前抓到一处 `clippy::assertions_on_constants`（K2 的预算边界用例里
  `assert!(OUTBOX_MAX_ATTEMPTS > 1)` 那一格），改法不是加 `#[allow]` 而是把边界改成**由谓词扫出来**，
  顺手让这条用例在预算改成任何别的数字时仍说真话；Python 46 OK（2 skip）、`tools/validate_core.py` 全过、
  `tools/binance_testnet_acceptance.py --allow-skip` 仍按缺凭据走 `outcome=skipped`。
  逐轮拆分未取证，本轮数字见 §47.10。
- 架构不变量 **439 项 → 449 项全绿**（N 轮那 10 项，全部追加在 `termination_budget_check()` 内，该函数现共
  27 项）：**N8 2 项**——三处子进程 stdin 写入都走 `write_all_within`、没有一处退回裸 `write_all`，且
  `io_budget.rs` 三颗用例（正向不误伤 / 预算兜住写并打断管道 / 普通写错误不走超时分支）逐条在位；
  **N9 3 项**——封顶只挑终态、越限才扫表、一次退到迟滞线，两个 venue 的 growth 点数量与派生索引级联逐颗点名，
  已退场订单的回报仍按分歧升级对账；**N10 4 项**——传输层三态且静默只在帧边界、两条流共用同一颗重连引擎且
  静默既不切断会话也不充抵预算、三态映射全仓只有一份 `read_or_forward`、行情 worker 经共用车间重连不再自己
  持有 socket；**N11 1 项**——长度预算按"整条消息"与"单帧声明"两处同时问。**N1–N7 没有新增门禁项**：那七颗
  的性质由常驻用例直接问得出，加门禁等于把同一句话抄第二遍。整树 `cargo test --workspace` **869 passed /
  0 failed / 0 ignored / 86 个 `test result:` 壳**（+23 颗全部落在既有壳里，壳数不变），`--all-features`
  **873 passed / 0 failed / 4 ignored / 86 壳**；`CLI_TEST_FLOOR` 常量 **242 未变**（磁盘重测 `src/tests` 190 +
  `crates/qx-cli/tests` 53 = 243，多出的一颗是 N4 的 `undispatchable_trigger_shape_fails_closed_at_assembly`，
  地板常量本轮没有理由上调）；`cargo fmt --all -- --check` rc=0，`clippy --workspace --all-targets`
  `-- -D warnings` 在 default / sqlite / postgres / nats / `postgres,nats` / `nats,postgres` /
  `nats,postgres,sqlite` 七种组合上全部 rc=0；Python 46 OK（2 skip）、`tools/validate_core.py` 全过、
  `binance_testnet_acceptance.py --allow-skip` 仍按缺凭据走 `outcome=skipped`。N1–N7 之外零新依赖、
  零新增 `#![allow]`，逐颗数字见 §48.12。
- 行数棘轮：H2 删的三份实现让 `qx-storage` 三本后端各下调；`crates/qx-api/src/lib.rs` 到此为止是全表唯一
  一处上调——H1 的 provider 管道 3230 → 3271（+41），I 轮的枚举与写入口、`projection_readiness` 与
  `query_models_last_known` 再加 69 行，补测 clippy 时把 I1 用例改写成 struct-update 又撑出 2 行，落到
  **3362**。上调走的是快照头自己写的出口（"确需增长则重新生成并审阅 diff"），逐行 diff 已阅并把理由
  写进 §44.8——把这份读面搬去另一个文件只是挪行数，不算收口。**J 轮把这格掉头**：J1 净减 186 行
  （3362 → 3176），J2 那颗共用循环加回 50 行（→ **3226**，净比 I 轮低 136），J1 删掉的 `ack_job` 与
  `from_storage` 让 `qx-storage` 三本再各下调（今日快照 2380 / 2554 / 2038）。本轮唯一一处上调是
  `crates/qx-cli/src/strategy_contract.rs` 840 → **841**，+1 是 mTLS 那颗多出来的 `stop` 实参行。
  K 轮四处上调、一处出表（§46.9）；**L / M 两轮无新增上调**——`maturity/line_budgets.yaml` 与 §46.9 同值
  （L2 改的是字段可缺席、L4 的 `reap.rs` 与 M4 的删减都落在门槛之下），ratchet 只降不升复核通过。
  **N 轮三处上调并按出口重打快照**（`--snapshot`，今日 39 个超 500 行文件在册）：`binance.rs 2217 → 2465`、
  `ccxt.rs 1068 → 1159`、`qx-adapter/src/lib.rs 1758 → 2013`，另有 13 个文件下降。三处增长都是 N9/N10 的
  常驻用例与共用引擎本身，逐行 diff 已阅；把用例挪去新文件只是挪行数，不算收口。
- 新用例族：`crates/qx-cli/src/tests/scheduler_owner_routing.rs`（5 条）+
  `crates/qx-scheduler/tests/job_owner_routing.rs`（3 条）、`crates/qx-cli/tests/dataset_registry_readside.rs`（3 条）、
  `crates/qx-cli/src/tests/api_snapshot_republish.rs`（2 条：纯重装载 + 真起桥端到端）、
  `crates/qx-cli/src/tests/api_control_audit_live.rs`（1 条：控制面审计现读，装配前后各走完一次命令生命周期，
  四向比对 HTTP 体 / trait 读点 / 回填字段）、
  `crates/qx-core` 里把"只接管一次"搬进纯判据的 `second_stale_sighting_in_one_competition_waits_instead_of_deleting`、
  多腿缺规格两端的 `leg_without_a_spec_is_compared_by_the_currency_it_actually_books_in` 与
  `missing_spec_reports_the_default_booking_currency_instead_of_an_empty_one`、
  `crates/qx-cli/src/tests/api_projection_refresher_ready.rs`（1 条：真起桥 → 桥在跑时 `/ready` 不含停摆 →
  stop + join 之后 503 并念出守卫那句原因）、`api_query_models_live.rs` 的逐格与兜底两条（三台各读一格的
  服务；provider 由成功翻成失败之后仍读到最后已知那一份），外加 `crates/qx-api` 内
  `readiness_blames_the_refresher_only_when_the_read_model_holds_projections`，外加
  `crates/qx-cli/src/tests/api_serve_stop_token.rs`（1 条：真起 worker、真发一次 `GET /ready`，再
  `request_shutdown()` 并在最多 5 秒的上界内断言 `serve` 已返回——卡住要红在断言上，不能把测试壳挂到 CI 超时）。

- 架构不变量 **449 项 → 470 项全绿**（O 轮 +9、P 轮 +12；逐项见 §49.10 与 §50.4）：**O2 那 4 项**里含
  一条**负向钉**（`config validate` 不得接管"声明了没人应用"这颗判据，因为实例声明在 `strategy backtest`
  那条链上真生效）；**O5 / O4 / O7 / O9 / O8 各 1 项**全部追加在 `termination_budget_check()` 内，数的是
  接线形状（两颗失败点各问一次预算、成功一侧逐点复位、两行自报同时带两个计数、握手签名带 `budget` 且
  调用点交的是 socket 超时、三处解析共用一颗入口且裸 `to_socket_addrs()` 只剩一处、两颗取锁入口各走一次
  有界等待且 DSN 默认值在落地处）；**O6 是收紧**既有 J2 那一项（首行读必须排在 `set_read_timeout` 之后），
  总数不变；**O1 / O3 没有新增项**，性质由常驻用例与既有唯一判据问得出。**P1 那 6 项**：`has_executor()`
  只判一次且逐格点名三类、受理处拒在写入 `Accepted` 之前且理由点名"没人执行"、判据不侵入结构性校验与
  恢复读侧、八类变体仍全部留在契约里、五类拒与三类照常受理两份常驻用例、旧记录读回来处置与 HTTP 侧 400
  各一颗。**P2 那 6 项**：原名不得回到 `crates/`、`pub type Ts = u64;` 全仓唯一且在 `clock.rs`、三份口径
  来源不再承诺那三件、设计底线改口到真实推进路径、优先级数值的唯一真相在 Rust 侧而 Python 逐项跟随、
  `(ts, prio)` 单调落盘判据只有写入处那一份且在正文不在用例里。**P3 不增加项数**（三行登记数据 + capabilities 一行）。
- 本轮新增用例 **12 颗**：O 轮 9 颗（`a_successful_finish_refuses_an_error_code`、
  `crates/qx-cli/src/tests/strategy_declaration_scope.rs` 3 颗、
  `a_symbol_that_never_recovers_stops_buying_resurrections`、
  `websocket_handshake_header_block_stops_at_its_own_deadline`、
  `address_resolution_returns_to_its_caller_at_the_budget`、`postgres.rs` 的槽位与 DSN 各 1 颗）+
  P1 的 3 颗（`kinds_without_an_executor_are_refused_at_acceptance`、
  `kinds_with_an_executor_stay_acceptable`、`persisted_commands_without_an_executor_still_restore`，另加
  既有 HTTP 用例改断 400）；**P2 随三份死实现删掉 8 颗自测**（`engine.rs` 5、`queue.rs` 2、`clock.rs` 1）。
  整树 `cargo test --workspace` **872 passed / 0 failed / 0 ignored / 86 个 `test result:` 壳**（EXIT=0），
  五组合特性矩阵全绿（`sqlite` / `postgres` / `nats` / `postgres,nats` / `nats,postgres,sqlite` 各自
  clippy rc=0 + `main.rs` 用例 191 / 190 / 193 / 193 / 194），`cargo fmt --all -- --check` rc=0，
  零新依赖、零新增 `#![allow]`；Python 46 OK（2 skip）、`tools/validate_core.py` 全过。
  **一处不吻合如实记下**：把 O +9 与 P −5 加到 §48.12 的 869 上得 873，比实测多一颗——N 轮末那份整树
  日志没有保留，那颗追不回来；本轮以 872 为唯一现状数，不回头改当轮的 869 记录。
- 变异取证 **O 轮 14 跑 + P 轮 10 跑 = 24 跑**（`target/o_mutate.py` / `target/p_mutate.py`，全部前台执行、
  逐字节 sha256 还原）：非基线 SURVIVED 0，三级分级与逐颗明细见 §49.11、§50.5。
- 行数预算 **一处上调**：`crates/qx-api/src/lib.rs 3303 → 3304`（P1 的常驻用例被 rustfmt 折出一行，按
  §46 / §47 的先例走 `--snapshot`，快照 diff 里只有这一行）；`maturity/line_budgets.yaml` 现共 40 条登记。
- 架构不变量 **470 项 → 478 项全绿**（Q1 +2、Q2 +2、Q3 +0、Q4 +4，逐项见 §51.5）：**Q1 那 2 项**是
  "命令表解析出的命令名不少于实测 49 颗"的地板与"每一颗都被 qx-cli 用例点过名"（54 份用例、免检登记
  0 颗），前者挡大批删除、后者挡"新加通告却零用例"；**Q2 那 2 项**是顶层键名单三方同为一串（Rust 读侧
  认的 = 写侧印的 = Python 严格模式允许的）与 `deploy` 里 5 份 bar-frame 示例的每一格顶层键都有读者；
  **Q3 不增加项数**（`M4_DEAD_SURFACES` / 替代判据 / capabilities 三处按既有形状扩名字）；**Q4 那 4 项**
  是逐颗对齐（README 说它走不到 ⟺ 生产文本零调用点）、速查行按能力族、`fnv1a` 与 `C ABI` 两条口径不得混读、
  README 自己印出的三处行号仍指到定义本身。
- Q 轮新增用例 **9 颗**（Q1 的 7 颗子进程入口用例 + Q2 的 2 颗帧契约正反用例），整树
  `cargo test --workspace` **881 passed / 0 failed / 86 个 `test result:` 壳**（EXIT=0，
  `target/q_round_tests.log`；872 + 9 与本条相减吻合）。五组合特性矩阵全绿（clippy 各自 rc=0 + 单元
  191 / 191 / 194 / 194 / 194），`cargo fmt --all -- --check` rc=0，零新依赖、零新增 `#![allow]`；
  Python 46 OK（2 skip）、`tools/validate_core.py` 全过。**与 §50.5 的 191 / 190 / 193 / 193 / 194 差
  一颗，本轮没有追到那颗具体是哪一颗用例**——实测只到"`--list` 里 sqlite 与 postgres 两份清单逐颗相同
  （各 251），nats 多三颗 `tests::worker_observability::*`"，故只记当轮数字不改上一轮记录。
- Q 轮变异取证 **18 跑**（Q1 4 + Q2 4 + Q3 4 + Q4 6，`target/q1_mutate.py` / `target/q2_mutate.py` /
  `target/q3_mutate.py` / `target/q4_mutate.py`，全部前台执行、逐字节 sha256 还原）：非基线 SURVIVED 0，
  其中 **Q4 的 6 跑含一颗故意留下的对照组**（C1：拿加固前那版判据跑同一颗变异，实测 SURVIVED，用来交代
  为什么要改成逐颗/逐族）。**本条第一版漏掉了 Q3 那 4 跑**（写成 14 = Q1 4 + Q2 4 + Q4 6），把
  `target/q3_mutate.py` 按最终基线 478 复跑之后才对齐：Q3-M1/M2/M3 仅门禁红、Q3-M4 断言红，与当轮
  470 基线下的分档一致。Q1 / Q2 两份取证同样在 478 基线下复跑过一次（首跑分别是 472 / 474）。
- 行数预算 Q 轮 **一处上调**：`crates/qx-datastruct/src/lib.rs 702 → 733`（Q2 的未知键拒 + 那份三方共用
  名单 + 注释），登记总数仍是 40 条。
- 架构不变量 **478 项 → 516 项全绿**（R4 轮 +38）：新增的 38 颗里 **15 颗在标签上点名 V11 R4-x**
  （R4-3 1、R4-4 1、R4-5 5、R4-6 2、R4-8 2、R4-9 2、R4-10 2，按本轮门禁日志的 `[PASS]` 行逐条数出），
  **9 颗以判据名自证**（R4-1 的六颗 + R4-2 的三颗，它们的标签里不写轮次），**其余 14 颗是对既有判据加格**、
  本轮没有逐颗回溯对号——这一句是留给下一轮的口子，不是笔误。
- R4 轮常驻用例 **+16 颗、`test result:` 壳 86 → 87**：唯一那颗新壳是
  `crates/qx-runtime/tests/strategy_api_schema_contract.rs`。整树 `cargo test --workspace`
  **897 passed / 0 failed / 0 ignored / 87 个壳**（EXIT=0）。16 颗逐颗对号是按**用例名**对号的，不是按
  `git diff` 计数——HEAD 早于 D 轮，`+#[test]` 会把十五轮混在一起：R4-6 `snapshot_history.rs` 3 + R4-4 新壳
  4 + R4-4 的嵌套层三颗（`nested_rows_declare_exactly_the_keys_the_writer_prints` /
  `nested_required_lists_cover_every_key_the_reader_cannot_default` /
  `key_tables_declare_the_shape_the_reader_enforces`）+ R4-5 `backtest_product_policy.rs` 3 + R4-9
  `crates/qx-api/src/lib.rs` 2（:3378 / :3406）+ R4-10 <!-- 史 -->
  `ccxt_respawn_budget.rs::a_stalled_recovery_round_buys_no_more_venues` 1 = 16。**独立算术复核**：
  qx-cli 的单元目标在本轮矩阵里是 195 颗（§51.6 同口径 191），差的 4 颗正好是 R4-5 的 3 + R4-10 的 1。
- 五组合特性矩阵全绿、failed 0：`sqlite` 255 / `postgres` 254 / `nats` 257 / `postgres,nats` 257 /
  `nats,postgres,sqlite` 258（`target/r4_feature_matrix.log`）。那颗 255 已按明细逐行对号：
  `src\main.rs` 单元 195 + 11 份集成测试壳相加 60（`target/r4_combo_sqlite_detail.log`）。
  **本轮把汇总口径改成"该组合下所有 `test result:` 行相加"，与 §51.6 的"`main.rs` 用例"口径不同，
  这两组数字不可直接相减**。`CLI_TEST_FLOOR` 仍是 254，磁盘重测 `src/tests` 198 + `crates/qx-cli/tests`
  60 = 258，地板本轮没有理由上调。
- `maturity/capabilities.yaml` 566 → **581 行**（566 → 574 是 R4-4 / R4-6 / R4-8 / R4-10 的登记，
  574 → 581 是收口时补上的 R4-3 / R4-5 / R4-9 三族各两颗证据 + R4-9 一颗限制）：台账引用 183 → **194 处**、
  带名绑定 58 → **63 处**，越界 / 空行 / 错位 / 对不上 / 不在场各 0 命中。
- 变异取证 **R4 轮 49 跑**（七份脚本，日志 `target/r4_1.log` / `r4_3.log` / `r4_4.log` /
  `r4_5a|b|c.log` / `r4_5_m2.log` / `r4_68.log` / `r4_9.log` / `r4_10.log`，全部前台执行、逐颗 sha256
  还原）：**断言红 23 / 仅门禁红 23 / SURVIVED 3**——三颗 SURVIVED 全是刻意摆出来的负对照，非基线 SURVIVED
  0。**事故前那份记录写的是 51 跑（23 / 22 / 6），本轮无法复现多出来的那两颗行**：七份现存脚本可复现的总数
  就是 49，§52 按 49 记账，不回头把两个数并成一个。
- 行数预算 R4 轮 **0 处上调、登记总数仍是 40 条**：新建的 `crates/qx-api/src/snapshot_history.rs` 128 行
  未过 `OVERSIZED = 500` 门槛，不进棘轮。`cargo fmt --all -- --check` 干净、
  `cargo clippy --workspace --all-targets -- -D warnings` 干净（本轮自己新增的代码带进来两颗违规，收口前
  修掉）、`python3 tools/validate_core.py` 全部通过、
  `python -m unittest discover -s python/tests -q` **47 OK（skipped=2）**（`target/r4_close_verify.log`）；
  `crates/qx-core` 仍是 21 份 `.rs` / 5,654 行（本轮没碰内核）。零新依赖、零新增 `#![allow]`；唯一一处
  非判据的源码改动是 `crates/qx-protocol/src/wire.rs`（218 → 220 行）的文档口径修正——那颗
  `From<&Order> for OrderSnapshot` 的注释此前漏了 `policy` 一格、并把 `order_id == client_order_id`
  归因给"EventLog 恢复路径"，实际原因是 `qx_core::Order` 本身只有 `client_id`。

- R5 轮新增 **6 格常驻判据**：`control_retirement_check`（tools/check_architecture.py:3355）五颗——退场路唯一、
  裁剪出口唯一且配额由常量推出、终态判据唯一、两格一起外销、五颗常驻用例点名到形状；
  `api_control_audit_check` 的读侧那颗——"审计链有生产读者：doctor 在文件与 sqlite 两本上逐环核对并把
  检查点对到链尾"。门禁总数本轮实测 **524 项全绿**。
- R5 轮新增常驻用例（走真实生产接缝，不起 mock）：qx-control 15 颗（含五颗退场形状：成对退场、窗口内仍认
  身份、长跑有界、旧状态收敛、半对历史拒收）；qx-storage 文件后端 4 颗链用例（事务写入 + 篡改可见、残尾截断、
  检查点回退、并发事务串行）+ 1 颗负向断言（检查点落后链尾必须报错）；SQLite 后端 1 颗链用例；
  qx-cli `builtin_command_identity` 4 颗（跨轮不撞、跨 intent 不撞、同轮重放幂等、同轮多单各自身份）。
  本轮末 `cargo test --workspace --all-features -j 4`：**87 个目标 / 915 passed / 0 failed / 4 ignored / 0 warnings**。
- **把四份超限 Rust 文件拉回行数棘轮之内，没有删任何一条断言**：沿用仓库自己的"外置 `#[cfg(test)] mod tests` +
  兄弟文件"先例。`qx-control/src/lib.rs` 939 → 599、`qx-storage/src/lib.rs` 2419 → 1692、
  `qx-storage/src/sqlite.rs` 2554 → 2352、`qx-cli/src/config_commands.rs` 528 → 407（doctor 的报告体与三颗
  存储侧检查搬进新模块 `doctor_report.rs`，470 行）。`maturity/line_budgets.yaml` 随之重生成。
- 取证 **14 跑 / 12 颗 KILLED / 1 颗刻意 SURVIVED（只改注释的负对照）/ 还原异常 0**，明细与两条按取证改掉的
  判据写在 §53.7。
- R6 轮新增 **8 格常驻判据**（门禁 **524 项 → 532 项全绿**）：`control_retirement_check`
  （tools/check_architecture.py:3355）里的词表闭合那颗（:3408 / :3412）与 `is_final` 那颗加上的调用数下界（:3391），
  `paper_acceptance_attribution_check`（:3456）四颗（:3466 基线定序 / :3478 语句级条件 / :3487 自报新增数 /
  :3494 两轮用例），`command_queue_honesty_check`（:5975）三颗（:5988 503 与错误名 / :5998 用例两面 / :6006 装配
  不吞错）。常驻用例新增 qx-api `a_command_that_cannot_be_queued_answers_503_not_202`（一条判两面），qx-cli
  `paper_e2e_entrypoint_runs_scheduler_strategy_execution_and_ledger` 从跑一轮改成跑两轮。本轮末 `cargo test --workspace --all-features -j 2`：**87 个目标 /
  916 passed / 0 failed / 4 ignored / 0 warnings**。取证 **14 颗变异 + 1 颗负对照 / 25 跑 / 还原异常 0**，
  明细写在 §54.7。
- **一次搬家动了"只降不升"的棘轮，按实测披露**：`crates/qx-api/src/lib.rs` 超限之后把用例搬成兄弟文件
  `src/tests.rs`（预算 1064 行），`maturity/line_budgets.yaml` 随之重拍——本轮唯一上抬的一格就是这个新文件
  （0 → 1064），`lib.rs` 重拍后 2445 行，一条断言都没删。同一场搬家带出第三处"门禁认文本"盲区：四颗判"用例在
  不在"的判据（:2473 / :2967 / :4545 / :5977）仍按路径只读 `lib.rs`，其中 :5977 正是 R6-3 新写的那颗——四处改成
  `source_with_tests` 成对读（§54.6）。
- **四份文本的 `path:line` 第一次由同一颗判据逐颗核对**（R6-7 / R6-8 / R6-9）：起点是白皮书、变更日志与
  方案书里的行号没人量过（R6-7 逐颗复位），落点是把台账那套 `capabilities_citation_check`（`tools/check_architecture.py:6443`）泛化成
  一颗共用扫描器 `citation_audit`（`tools/check_architecture.py:6316`）加文档侧薄壳 `doc_citation_check`（`tools/check_architecture.py:6457`），
  常驻门禁 532 项 → **550 项全绿**（三份长文档各六颗；地板逐份钉在 `DOC_CITATION_TARGETS`（`tools/check_architecture.py:6261`）里：
  方案书 580 引用 / 50 带名、变更日志 220/25、白皮书 410/60，台账沿用 90/20）。搬过来第一份取证就是它自己
  把 5 处文档锚点打红——门禁长一行，指进门禁的行号就集体下移一次；那不是判据过敏，是"文档按行号引用代码"
  这件事本身的代价。三口径（名字右边界、破折号区间远端、裸引用位置化归属）之外，R6-9 再把裸引用的归属扩到
  "同一行只点了文件名、没带行号"那一颗：`CAP_BARE_PATH`（`tools/check_architecture.py:6241`）一次多量到 61 颗、当场红 12 颗，
  其中一颗是"数字恰好落在误认文件长度之内"的假通过。取证 6 颗变异、杀 6、存活 0，每颗在三种判据形状下逐列
  回放（明细在 §54.9–§54.12）。修完仍剩 349 颗无人核对的裸行号，按限制登记在 `maturity/capabilities.yaml`（R7 轮按同一条规则复测仍是 349，逐份拆分见 §55.11）。
- **常驻门禁 550 项 → 553 项全绿**（本轮新增三颗）：`JobSpec 字段名单可解析，且不含三格只有写侧的退役键`
  （`tools/check_architecture.py:6973`）、`deploy 里 3 份作业清单示例的每一格顶层键都在 JobSpec 名单里`
  （`tools/check_architecture.py:6985`，只删代码不删示例，下一个人会照示例抄回来）、
  `窗口与累计摘要在两个读面上同源`（`tools/check_architecture.py:3439`）。锁那一颗（`tools/check_architecture.py:7598`）
  不是新增而是加严：它现在同时要求"计数喂回判据"与"循环只按判据交出的年龄说话"。
- **R7-6 的一批"仓内唯一写法、零生产调用"进保留表**：15 颗登记在 `tools/check_architecture.py:3759` 起的那一段，
  批次理由与"留还是删"的分界写在 `tools/check_architecture.py:3755`；同批里的 `DataSchemaVersion` 走了相反的方向——
  它是 `DATA_SCHEMA_VERSION`（`crates/qx-data/src/schema.rs:3`）那一格事实的第二种写法，按 Q3 口径直接删。
- **点名表改为派生，不再手抄第二份行号**（R7-10b）：`m4_unnamed_citations`（`tools/check_architecture.py:3788`）过去拿
  一份手抄清单，R7-10 把文档里一颗从旧号改到新号之后，手抄的那份仍按旧号要求——同一个判断留两种写法，
  改对的那一半反而把判据弄红。现在它由保留表派生。
- **本轮基线的用例数按实测记下**（是当轮日志里的数，不是增量）：outbox 后端语义 12、队列后端语义 2、
  快照单一来源 13、qx-scheduler 18 + 3、qx-storage `--lib` 22、qx-core file_lock 9；取证脚本与日志这轮起落盘在
  `maturity/evidence/v11-r7/`，上一轮被方案书 §54 点名的四份材料（两份快照、两颗探针与变异器）也在这轮补落进
  `maturity/evidence/v11-r6/`——临时工作目录从此不再是任何一句登记的去处。
- **迁进来的取证脚本自己也得跑得动**（R7-11）：三份 R6 脚本的仓根是按它们原先住在临时目录里反推的，
  迁进 `maturity/evidence/v11-r6/` 之后方案书那句"可复跑"就成了假话。逐颗按新落点改口后复跑：探针给同版
  快照 §54 之后 51 颗、全文件 293 颗，探针与两种判据形状（含 `--legacy`）各自 rc=0；"四份文本同一版快照
  合计 400 颗"改口成"当时的观测"——另三份同版快照没进证据目录，它今天不可复跑。
- **§54.11 那颗"349 颗无人核对的裸行号"按同一条规则复测过**（R7-10）：总数不变仍是 349，逐份拆分是
  方案书 242→241、变更日志 101→102、台账 5→5、白皮书 1→1。两颗不对称都有出处，明细在 §55.11。

### 本轮踩到

- **文档替一次还没发生的登记作保**：§49.13 与 §50.5 那两行的 capabilities.yaml 数字先写成了「537 行、
  就地扩写、没有增加行数」——那是登记尚未落地时写下的**预期**，不是实测。O / P 两轮的能力登记补完之后
  重测是 **537 → 560 行**（23 行 = 11 条 evidence + 12 条 limitations，全部挂在 7 个既有条目下、没有新开
  能力条目），两行按实测改口并在原处记下"这一格先前写的是预期"。同一族的老毛病见 §47.10 那句"数字全部
  取自当轮门禁日志"：写文档时最容易混进正文的，就是那种"我知道它会是多少"的数字。
- **一颗变异让门禁**崩**而不是红一项**：P1-M1 摘掉受理处那三行拒之后，门禁项里那句
  `submit.index("has_executor")` 抛 `ValueError`，整条门禁以 traceback 收场。崩掉的门禁不是一次判定——
  它既不红也不绿，而 CI 只看退出码时"红一项"与"脚本坏了"是两种完全不同的信号。改写成 `find()` +
  `0 <= refusal_at < audit_at` 之后重跑，才得到"恰好这一项红"的干净结论。
- **一颗门禁详情文案被自己的变异证伪并当场改掉**：O7 那一项原文写"把 deadline 检查挪出循环，全树用例
  仍绿（真 socket 进不了测试）"，而 O7-M1 实测是**断言红**——那颗用例用的是假 `Read`（每读一次吐一个字节），
  真 socket 根本不进这一格。这句话会教下一个人"这一格只有门禁能守"，而实测正好相反。同族的口径修正
  在 §49.10 末与 §49.11 第 3 条。
- **一颗"门禁红"其实不是那颗修复的牙齿**：O1-M1 摘掉四行让 `qx-scheduler` 里 M4 登记的**行号**集体漂移，
  红的是"行号逐条仍指到定义本身"那一项，与 O1 的口径无关。为了把两件事分开，补了一颗等行数的 O1-M2，
  它只红用例。读 §49.11 时按"仅断言红"理解 O1 的牙齿。
- **八颗"仅门禁红"说的是同一件事**：worker 与真 socket / 真连接 / 真解析器 / 真 Python 子进程进不了用例，
  于是"机制有没有被接到调用点上"这一格只有接线判据数得到（O4-M1、O5-M1、O5-M2、O6-M1、O8-M1、O8-M2、
  O9-M1，加 P2-M5 / P2-M6 / P2-M7）。把它们升级成断言红需要可注入的 socket / 子进程抽象，那是另一颗
  形状改动，本轮没有为它先加一层。
- **`validate_core.py` 在 P2-M6 之下仍然绿**：把 Python 侧 `PRIO_POST` 改成 8，红的是门禁那项数值比对，
  而 `validate_core.py` 自己不读 Rust 侧——它现在明确自称"参考形状"，这条限制就不再是一句空话而是一处
  需要读者知道的边界（§50.5 末）。
- **O 轮没有留下一份独立整树测试日志**：那次尝试跑的是 `tools/run_tests.py`，这个文件在仓内不存在，
  日志只剩一行报错。本轮不冒领一个数字，整树总数以 P 轮那份 `target/p_round_tests.log` 为准（§49.13、
  §50.5）。
- **文档替不存在的能力作保，是这一轮里最贵的一课**（P2）：`engine.rs` / `queue.rs` / `TestClock` 三份
  实现自 V10 布局起就没人调用，而 README、crate 文档、`Cargo.toml` description 与 `validate_core.py`
  四处同时引用它们——读这四份的人会以为自己有一份确定性时钟与一条乱序调度队列可用。删的时候还撞到
  一处不能照字面执行的事：`pub type Ts = u64;` 住在 `clock.rs`，被 event/order/sourcing 使用，而门禁的
  M4 那项无条件读这个文件。教训不是"文档要常新"，而是**口径来源超过一份，删除就不是一个文件的事**；
  所以本轮把四处来源一起改口，并让门禁逐份钉住"不许再承诺"。
- **两颗 SURVIVED 都在说"用例的名字比用例的内容管得更宽"**：`takeover_happens_at_most_once_per_competition`
  永远走不到"同一场竞争第二次见到孤儿锁"那一支，多腿币种闸门在缺规格那一端没有任何用例。
  两条都在本轮当场补掉（不是写进遗留），处置分别是"把规则搬进纯函数"和"给兜底那一份补行为用例"。
- **五颗变异只有门禁红**（L3b / D2-M2 / E7-M7 / F1-N4 / F2-P4）：共同形状是改坏之后单进程、单 worker 的
  端到端观察不到差别。分级比断言红低一等，逐条写在 §41.7，不混进"全绿"里说。
- **G1 的第一版修法是 SURVIVED 的**：端到端用例当时只写"起桥之前那一笔"，于是"把装载挪到轮询循环之外、
  桥启动时多装一次"这种半修法用例全绿——只有补上起桥之后的第二笔才把它推到断言红。反向的那半也要说清：
  "把第二笔删掉"这种削测试的手法没有任何断言挡得住，挡住它的是门禁那条静态判据（N1 / N1b 两行取证，
  登记在 §41.7 第 9 条）。
- **H1 的门禁判据第一版被自己的围栏柱咬了一次**：那一项把 provider 块取成"从 `.with_control_provider(`
  起往后 200 个字符"，而 `move || store.load()` 的起点正好是第 200 格——差一被切在窗口外面，基线当场红。
  修法不是把窗口放大到刚好包住，而是改成"取到下一个 builder 调用为止"并加 `-1 < provider_at <
  submitter_at` 定序：窗口跟着代码长，装配顺序也一并钉住（§43.7 第 1 行）。
- **`cargo fmt --all` 会把它碰到的 CRLF 文件改写成 LF**：H2 删完三份实现后跑 fmt，`qx-storage` 三本
  后端整体翻成 LF。按开局 sha 逐本改回 CRLF、重跑 fmt / clippy / 整树用例才收口。规则与"取锚点前先跑一遍
  格式化"是同一条：**改动过的文件要按它原本的 EOL 落盘，不能让格式化工具顺手把整本换行改掉**。
- **I2-M1 是真活下来的那颗（第一版用例上只有门禁红）**：`query_models()` 成功时会回填同一份副本，端到端
  用例先读 `reconcile_reports()` 就等于替 `ledger_entries()` 答了题。修法不是放宽断言，而是加一层"每台
  服务只读自己那一格"的逐格用例，并把这条顺序理由同时写进门禁第 5 项与用例的文档注释（§44.6 说明 1）。
- **H 轮那句"clippy 0 / 0"没能覆盖之后落地的代码**：本轮补跑 `--workspace --all-targets` 时 I1 用例里
  的 `clippy::field_reassign_with_default` 当场红。跨轮引用一个门禁数字之前要重跑它——数字只在被测量的那
  一刻为真。rustfmt 把这条改写撑成 4 行，于是行数棘轮跟着 `--snapshot` 一次（3360 → 3362）。
- **J1 把自己写进门禁详情的那半句话证伪了**：原文是"原名回来红在这里，改名回来不红"。跑 J1-N1
  （把那颗壳改叫 `serve_with_store_as` 请回来）却照样红——判据认名字不认能力，凡带 `serve` 前缀的第五颗
  一律红。文本按实测改，并把这个真实边界登记成 `zero_caller_entry_gate_recognises_names_not_capabilities`；
  同族的 J1-N2（无 fencing 的确认改叫 `ack_job_unfenced` 请回来）是真 SURVIVED，没有靠再加一条正则糊过去。
- **J2-M3 证伪了写在代码注释里的一句平台判断**：注释说"Windows 会把非阻塞模式传播到被 accept 出来的流，
  不复位则 `set_read_timeout` 以 WSAEINVAL 失败"。拿掉那句复位，用例全绿——本机（Windows + rustc 1.98）
  不传播。注释与门禁详情都改成"各平台口径不一致，本机不传播"，这一行按防御性复位保留（§45.7 说明 1）。
- **J2-N1 是收紧判据才从 SURVIVED 变成有牙的**：第一版第 2 项只数 `stop: impl Fn() -> bool` 与遗留
  `listener.incoming()`，于是"mTLS 收了令牌却不问"（`&|| false`）两边都不红；改成数那句字面委托
  `self.accept_polling(listener, &stop,` == 2 之后它红在门禁。记下来的是方法论那一条：判据能咬到哪一层，
  取决于变异跑到哪一层才写判据。
- **L3-M1 的第一版是一颗无效变异，不是漏判**：登记行里 `load_projection` 只出现一次，把 `::load_projection`
  换成别的名字之后行内仍留着裸词，而判据问的是"这一行点没点名那颗函数"——全绿是**判据正确**。改成把这一行
  里所有出现一并换掉，才问出真问题（它红在门禁）。**无效变异比漏判更危险**：它给出的那个绿看起来和
  "有牙"的结论一模一样（§47.8 说明 2）。
- **L2-M2 一度被夹具的过滤文案报成"本轮 0 项"**：门禁红那半是拿一份"本轮新增判据"的关键词列表去数命中的，
  而 L2-M2 咬住的那一项不在列表里，于是真命中被吞成没咬住。修法不是给判据补前缀，而是改成逐颗点名
  "这一颗要咬住哪一项"，并把"该红没红"计为夹具失败而不是通过（同一处还修了 `-q` 下变红的用例名要从
  `failures:` 段里捞）。

- **N8-4 那颗变异没有"红"，它是挂住**：把带预算的写退回调用线程上的阻塞写（修复前的形状）之后，
  用例不是失败而是不返回——只有门禁 / CI 超时看得见。这是三级口径之外的一格，也正是 §48.9 第 1 行
  存在的理由：这一族性质**不可能**只靠用例收口。
- **一颗真 flaky 用例是在整树 `cargo test` 里红起来的，不是被 CI 发现的**：
  `a_draining_pipe_writes_through_and_gives_the_writer_back` 第一版用 1 ms 预算 + `unreachable!()`，整树并发
  时把"写线程还没起来"念成"管道不读"（那次复测 rc=101）。改成 500 ms + `panic!("对端在读时不该走超时分支")`
  之后连跑三遍绿；顺带 N8-1（预算改 0 ms）现在会同时打红这一格——正向对照本来就该在预算为 0 时红。
- **变异夹具自己崩在一颗已经写进源码的变异上**：`mut_n10.py` 缺 `import re`，死在 `restore()` 之前，
  `lib.rs` 当时带着 m1 的改动。手工按锚点逆向还原、核对 sha 之后补了 `atexit.register(restore)` 与
  `write_robust()`（写后按 sha 复核、失败退避重试）；随后本机杀软在 124 KB 的 `lib.rs` 上又抛过一次
  `OSError [Errno 22]`（§47.8 第 4 条那个老对手），这次被兜住，七颗全部还原为开局哈希。**取证工具的还原
  路径必须比取证本身更可靠**，否则一轮变异留下的是被改坏的生产源码，而没人会去 diff 它。
- **文档回写自己被门禁咬了一次**：capabilities.yaml 补完之后 449 掉成红一项——`两条 M4 登记各占一行` 数的
  是 slug 出现的**行数**，而我新写的那条 limitation 为了交代出处，把上一条的 slug 逐字抄进了正文，于是那
  一格被数成 2 行。引用改成人话后回到 449。登记名的唯一性本身就是判据，"顺手引用"恰好是最容易破坏它的形状。
- **两颗只有门禁红**（N9-6 上限乘十、N10-m8 行情 worker 自带一份退避上限）：没有任何用例断言"封顶就是
  8_192"这一格数量，worker 那一格也没有可注入的真 socket。**与 N9-2 / N9-3 恰好相反**——退场顺序与迟滞
  两条门禁是**绿的**，只有用例在守；改数量不改字面就没人红。一条性质住在用例还是住进门禁，取决于它能否
  被写成静态判据，而不是取决于它重不重要。
- **一颗变异把判据自己的盲区抓出来两次（Q4）**：第一版判据问"README 全文有没有出现过『Ed25519 未接线』"，
  C1 对照实测——只删 crate 表那一处仍绿（边界那一节还带着那句话），接上三颗里任意一颗也仍绿；第二版把
  `(file, line, symbol)` 抄进门禁去核源码，于是 M3（把 README 里的 `:259` 改成 `:261`）**SURVIVED**：
  那份判据证明的是门禁没写错，不是文档没漂。两条都写进了判据注释。可推广的那一句是：**写"文档 ↔ 代码"
  这一类判据时，比较的两边都得从现场取**，任何一边是手抄的，那一半就永远红不了。
- **补测试的活儿先拆掉一处"测手法而不是测入口"的旧用例**（Q1）：Paper 那两颗入口此前被复制进临时目录跑，
  而配置里除 `data_dir` 外还有 `instrument_spec_path` / `jobs_path` / `target_snapshot_path` 三颗相对
  `deploy/` 的引用，搬家之后验收以 `processed=0` 退 2——守的一直是隔离手法。改成在仓库根目录零参数跑
  打包模板之后，两次调用共用 `data/qianxing-paper`（`.gitignore` 的 `/data/` 已挡住）并留在同一颗用例里
  顺序跑，避免两个进程写同一份运行时状态。
- **一条收紧差点把超过 u64 的定点价量塌成浮点**（Q2）：读侧拒未知键最省事的写法是 `#[serde(flatten)]`
  或先过一遍 `serde_json::Value`，但 serde 的 `Content` 没有 i128 档，值走那一趟就没法还原成 `i128`。
  最后只从 `Map<String, Value>` 问键名、一个值都不过。收口"读侧收紧"之前要先问一句这条路上有没有类型
  装不下要保护的东西。
- **CHANGELOG 自己有一份重复块**：本轮在"本轮踩到"一节里发现 O / P 那六条教训被同一轮写了两遍（后一份
  是前一份的旧措辞子集），删掉后一份、保留信息更全的那一份。这正是 §50.5 P2 那一课在同一棵树上的复现：
  **同一个口径写两处，被删掉的总是没人读的那一处**。
- **外部沙盒本轮仍不可跑**：`cargo test -p qx-storage --features sqlite,postgres,nats -- --ignored` 的
  四颗（1 颗 nats + 3 颗 postgres）全部缺 `QX_TEST_NATS_*` 与 PG DSN，nats 那颗以
  `QX_TEST_NATS_SUBJECT_PREFIX must be set` panic 收场（`target/q_ignored_storage.log`）。这不是新缺口，
  是 §50.6 那条遗留这一轮的实测复现，仍按"未拿到外部沙盒记录前 `sandbox_tested` 全为 false"处理。
- **一份被截成 0 字节的日志一度被当作"clippy 跑过"的证据**（R4）：`target/r4_clippy_full.log` 是 0 字节，
  一次被中断的重定向留下的。本轮之后所有 clippy 结论只认能打印 `Finished` / `error` 的现场输出——绿色文件
  存在 ≠ 那一次跑真的发生过。
- **四份受版本控制的文件被写成 0 字节**（R4）：根因是一条早已记过的写法
  `open(p, "wb").write(open(p, "rb").read().replace(...))`——目标在读取之前就被截断。踩它的是**一份被上一版
  替代却没有删掉的变异脚本**：它在修正版已经存在的情况下被再次执行，清零了 `tools/check_architecture.py`、
  `crates/qx-zhenlu/src/oms.rs`、`crates/qx-runtime/src/pipeline.rs`、`crates/qx-api/src/snapshot_history.rs`。
  三份 `.rs` 从修正版脚本自己的 `target/recover/pre_mut/` 快照字节级写回，sha 后来由那颗脚本在基线阶段
  重新打印并逐颗比对通过；门禁脚本从恢复副本重建、再回放截断之后仅有的两处 Edit，得到
  **438,776 B / 8,075 行 / CR 0 / sha `95875e1cb13f`**，与 R4-3 取证脚本自己记录的基线 `gate: 95875e1c`
  **独立吻合**。事故之后本轮**七份取证脚本全部重跑**，§52 的每张表都出自那批重跑日志。能留下的只有两句：
  **被替代的脚本必须当场删除**，以及 **file-history 是本轮唯一的 undo**——四份文件回得来靠的是恰好存在一份
  快照目录，不是靠任何机制。
- **一条只在"文档改了、判据没跟上"时才红的判据，红的那一格往往不是出问题的那一格**（R4-3-M5）：那一跑
  本格红 0、只有 4 颗 collateral 红——这不是基线外幸存，而是"本格根本不看这里"，所以按未覆盖面登记而不是
  写成通过。§51.7 的 Q4 教训在另一颗判据上重现。
- **一颗变异第一次跑出来是编译红，不是任何一级取证结论**（R4-5-M2）：Windows 上 `os error 32`
  （另一个程序占用 `qx-cli.exe`）打断了链接。单颗复跑才拿到断言红。**按本轮口径，编译红不作取证、必修好
  重跑**——把"这一跑没拿到结论"记成"这一颗是断言红"就是替一件没发生的事作保。
- **一颗 SURVIVED 要补一把文本锁才咬得住**（R4-10-M-b）：把停滞账改成每轮从 1 重来，用例与门禁两边都不红；
  补上门禁里 `recovery.count("stalled_rounds + 1") == 2` 之后才红在门禁。这正是 K1 那一轮"预算终身累计不
  复位"的教训形状，所以本轮把它当成一次重现而不是新事。
- **一次"复跑之后总数变了"没有回头改上一轮的记录**（R4）：事故前的取证记录写的是 51 跑（23 / 22 / 6），
  还原后的树上现存七份脚本可复现的总数是 49。差异全部落在 R4-5 那一族（脚本现在恰好定义 10 颗）。本轮
  按 49 记账并把这句差异写在 §52.12 里，而不是把两个数并成一个。

- **搬家暴露的两处门禁盲区比产品缺陷更值钱**：`split("#[cfg(test)]")[0]` 这类"生产半"切法会把外置的
  `src/tests.rs` 整份读成生产代码（那份文件里没有 `#[cfg(test)]` 标记），于是"唯一写入者""零生产调用"这类判据
  把测试读成调用点；反过来，"常驻用例里必须出现某颗名字"那三处只读单颗文件，用例一外置就读不到。
  修法是**按路径认生产源码**（`production_rust_source`，替掉三处各写一遍的内联判断）与**成对读**
  （`source_with_tests`：`lib.rs` ↔ `tests.rs`、`x.rs` ↔ `x/tests.rs`），而不是给测试文件补一个标记。
- **一条"常量还在函数里"的判据看不见"配额是抄来的"**：R5-A2 把 `AUDIT_WINDOW_RECORDS / 2` 换成字面量 `500`
  （行为等价）第一次跑出来是 SURVIVED——那颗判据只查常量是否出现在 `trim_audit_window` 体内，而函数开头那句
  `if self.audit.len() <= AUDIT_WINDOW_RECORDS` 让常量一直都在。判据改成点名到写法。这与 §52.7 那颗"认写法
  不认能力"的判据是同族的反面：写法代理太松，等价改法就从缝里漏出去。
- **读侧那半没有牙齿，就先补牙齿再上取证台**：`verify_audit_chain` 的"检查点对齐"那一半当时用例与门禁两头都
  没咬着（用例只在"对得上"的方向 `.unwrap()`）。直接摆上去只会得到一条"本轮修的东西没被钉住"，所以先在
  `audit_file_chain_rewinds_to_the_state_checkpoint` 里补那颗负向断言，再跑 R5-B2——它现在只红用例、门禁 0 格。
- **台账里的 `path:NNN` 与门禁登记表是一根绳上的两半**：R5-B1 在 `file/state.rs` 插一行，把台账引用的
  `:99/103` 顶成空行，连带红在"指过去的那一行不是空行"；改 `tools/check_architecture.py` 自己加两行，
  `plugin_claim_honesty_check` 的定义行从 6878 漂到 6880，台账第 52 行跟着红。引用门禁脚本自身的行号是
  最容易漂的一格，本轮按实测改口，并把"改完就复跑门禁"记成流程而不是判据。
- **一条行为等价的改写让子串锚点在两栏同时瞎**：R6-2-M2b 把 `ledger_entries < baseline_ledger` 写成
  `ledger_entries < baseline_ledger - baseline_ledger`（永远为假），用例层与门禁层**都绿**——因为那颗判据查的是
  这段子串在不在，而它确实还在。补法是把四句条件都扩成语句级锚点（连 `if` 与结尾的 `{` 一起认），重跑才红。同一
  颗判据的另一格还遇到过无效探针：`for client_id in []` 直接 `error[E0282]`，编译红不是一次判定，换成
  `.iter().take(0)` 与 `&new_client_ids[..0]` 两颗能编译的写法才拿到证据（§54.7）。
- **一条扫描结论里"后果"那一半也要量**：R6-5 报上来的时候写作"worker 拿不到 ack 会无限重投"，量下来不是——
  `QueuedControlCommand`（crates/qx-storage/src/lib.rs:1100）没有重试计数，但 worker 拿到 `Err(UnknownCommand)`
  就以非零码退出，`supervise_workers`（crates/qx-orchestrator/src/lib.rs:224）的 `loop` 在子进程退出时
  `return Err(...)`（:298）停掉其余 worker：**一次整店停机**而不是空转。本轮按实测改口才写进 §54.5，没有把那句
  抄进文档；同一族更早的一次是 §53.1 里那句"写侧有、读侧无人验"（当时写侧也不存在）。
- **门禁脚本自己的行号又漂了一格**：R6-2 的说明文字让 `plugin_claim_honesty_check` 定义行从 6994 漂到 6996，
- **两处"没点名的锚点"是悄悄假通过的**：白皮书 §10 与 §12 曾指着 `capabilities_citation_check`
  （`tools/check_architecture.py:6409`），而那一格早已换成 `not blank,`。 <!-- 史 -->
  名字与引用被折行拆开，旧边界绑不上名字，判据就只量了"行号在那份文件里、且不是空行"。同族形状 R6-9 又照出
  一颗更糟的：方案书里四颗门禁行号被就近误认成 `crates/qx-api/src/lib.rs`，数字恰好落在那份文件 2445 行以内，
  于是"对得上"。两处的修法都不是再加判据，而是**把名字请回同一行**——点名只对点了名的引用有效。
  这一格今天仍然成立（跨行折行就丢点名），已写进白皮书 §10 的 G-6 边界。
  台账第 52 行跟着红——与 §53.7 那两条同一根绳，本轮按实测改口。
- **区间远端可以"在界内却指错东西"，判据看不见**：`CAP_RANGE_END`（`tools/check_architecture.py:6253`）只量越界与空行，
  不参与名字对齐。变更日志里 `write_optional_money`（`crates/qx-protocol/src/lib.rs:267`）那颗此前写作一个区间，两端写成
  `crates/qx-protocol/src/lib.rs:287-288`，而 287 那一格其实是 `scalar_money_raw` <!-- 史 -->。两个数字都在这份 877 行的
  文件里、都不是空行，判据一路绿；它是 R7-10 人工逐颗复核时抓到的，不是门禁报的。把区间拆成具名引用之后，
  那颗假通过才变成三颗要核对的裸引用（§55.11）。
- **fmt 是行号漂移的第三只手，本轮打偏到取证脚本自己身上**：`cargo fmt` 把 `check_keys` 的两处调用摆成多行，变异脚本里
  按单行写的两条锚点当场命中 0。0 命中不是"这颗改不动"，是锚点过期——先按 sha 确认工作树没被半改，再按当前形状重做锚点。
- **一次变异存活照出的是用例自己的缺口**：R7-8-M4（把摘要面退回进程内副本）首跑 SURVIVED，因为那颗常驻用例只读了
  流水那一半。补上"摘要也要跟着现读"之后重跑才落到第一档——两份日志都留着（`maturity/evidence/v11-r7/mut_r7b_r78.txt` 与
  `maturity/evidence/v11-r7/mut_r7b_r78b.txt`）。第 2 批变异跑在文档重钉进行中，门禁基线是 7 条文档与预算红而不是 0，
  所以那批只按"新增红"的名单判档，这一格在 §55.12 里如实记下。

### 只报不修（升级路径写在 §41.7）

F1-N6 与 F2-P5 两颗变异断言与门禁都没咬住；E3 那三处删除没有常驻判据（换个名字回来不会红）；
sqlite-only 构建下 `/ready` 如实报 503 但没有 runbook；`QueryPort` 在仓内没有生产消费者（`query_port()` 只被用例调用）；
`SpreadGroupAttribution` 不携带币种字段，其安全性来自上游那颗闸门而不是类型自己；
G1 之后"有哪些账户域"仍取 boot 那份配置的 workers 列表，运行中新增 worker 要重启才进读模型，
且每轮重装仍是整本日重放（没有增量投影）。
R6 轮之后新增一条（§54.5）：**审计窗口滚出之后 `execute` 对那条命令回 `UnknownCommand`，worker 以非零码退出、
监督器随即停机**——要让控制面为窗口之外的身份作证就得留永久墓碑表，而那正好把 R5-1 收住的有界窗口重新打开；
且 `UnknownCommand` 与"状态文档被回滚成旧版本"在生产里同形，改成静默跳过等于给后者开门。登记在
`maturity/capabilities.yaml` 的 `paper_execution.limitations`。
H 轮之后新增三条：**H2 的"不得复活"钉的是名字**——`sync_control` 原名放回会红，改名
（`push_plane_audit`）放回全绿，它与 E3 那三处删除是同一条限制而不是 H2 独有的缺口；
**H1-N2 的门禁红是异常退出**——整份用例文件消失时门禁以 `FileNotFoundError` 退出码 1 收掉，
CI 会红，但那不是判据级红；**H3 整条哈希审计链没有生产写入者**——`AuditStore` 三份后端只有
`qx-storage` 自己的用例走，控制面审计的持久性实际住在 `ControlStateBackend`，本轮只把文档口径
改到与代码一致，删还是接线留给裁决（两种代价写在 §43.6 第 3 条）。
I 轮之后新增四条（逐条见 §44.7）：**投影 panic 那一支没有常驻反例**——守卫在 panic 时同样 `Drop` 并报
停摆，是构造性保证，用例只驱动了"停机 + 正常退出"，要真造一次 panic 得接管 panic hook；
**逐格用例只覆盖 provider 的成功与失败两面**，"只有报告格前进、另两格停在旧值"那一面今天由
`load_api_query_models` 整体返回一个 `Result` 兜着；`/ready` 的 worker 集合仍来自 boot 配置这条不变；
J 轮之后新增四条（逐条见 §45.8）：**停机阶梯仍然没有触发器**——出口齐了，
`RuntimeSupervisor::request_shutdown` 在生产侧依然零调用者，仓内没有信号处理依赖，
`run_runtime_api` 的 `worker.join()` 仍只能等进程被外部终止；**mTLS 侧的终止性只有静态判据**
（J2-N1 红在门禁、不红在任何用例上，补一条同形状用例要真实证书夹具）；
**`stream.set_nonblocking(false)` 这一格买不到本机证据**（J2-M3）；**J1 的判据认名字不认能力**（J1-N2）。
L / M 轮之后新增四条（逐条见 §47.9）：**L2-M3 那颗 fallback 变异实测 SURVIVED**——"有快照、没投影"这个状态
从进程外造不出来，于是 F1-N6 一条按实测**收窄而不是删除**（登记名改成
`keyless_snapshot_envelope_no_projection_fallback_still_has_no_teeth`，§41.7 第 1 条同步标注）；
**M2 的量下来不是缺陷**（WS 循环每 100 ms 被总线唤醒、客户端断开或 `resync_required` 即收），真实残留是
一条静默不关的连接可以一直占着线程与总线读者，**并发连接数无上界**，已登记为能力限制；
**L3 的读侧接不接待裁决**——那份落盘投影要不要给进程内消费方读，本轮只把登记的两侧点名补全；
**`reconcile_cash` 与 `settlement_balance_discrepancies` 的唯一真相源待裁决**（§47.6）。
N 轮之后新增七条（逐条见 §48.11）：**`TradingCalendar` 在仓内没有任何写入者**——N3 把判据接上了，日历
本体还是空的，要的是仓外数据源与更新口径；**重试重投撞 `FileJobQueue` 按 `run_id` 落盘的 `Conflict`**，
`retry_run_at` 因此仍是一颗只有用例走得到的入口；**`Oms { orders }` 没有封顶**（`qx-zhenlu/src/oms.rs:14`），
N9 那颗住在两个 venue 适配器里，要不要共用同一判据是裁决题；**`RestVenue`（`qx-adapter/src/lib.rs:968`）
与 `ProjectionEnvelope::validate()`（`qx-protocol/src/lib.rs:78`）两颗零调用**按 J1 就地登记，没有替它们
编一个调用方——`validate()` 接不接要连"读侧要不要在反序列化之后重问一遍身份字段"一起定；**静默没有活性
上界**（N10 的反面：半开、永不 tick、不 FIN 的连接会一直占着线程与 socket，每圈读阻塞 10 秒、令牌每圈问
得到，但仓内没有应用层 keepalive 期限可依据，与 M2 同族）；**16 MiB 预算是常量、不可配置**；
**`strategies[]` 的成本规则只做到"不许假装支持"**，按实例各用各的费率本轮明确没做。
R4 轮之后新增五条（逐条见 §52.13）：**R4-7 控制面只追加、没有保留策略**——本轮把它量出来了却没修：
10 Hz 心跳下 3 万条命令就把 ≈7.09 s 的预算花在"数出 0 条待执行"上（§52.10 那张表），三条路（只加窗不改语义 /
按终态退场并保留摘要 / 由部署配置决定留多久）各自都要改形状，而改哪条是产品口径而不是连通性判断。R5 轮按「终态退场 + 有界审计窗口 + 累计摘要」把它落地了（§53.2）；
**R4-3-M5 那一格散文没有自己的判据**（结构体上关于两颗超时字段的声明口径，本轮只能靠 README 两处对齐反推）；
**`docs/` 不在引用判据的扫描面内**，V11 文档里的 `path:NNN` 仍靠人工，本轮明确不扩；
**三颗"只有门禁钉得住"的形状仍未闭合**——快照容量值
`snapshot_history_cap_value_is_pinned_by_the_gate_only`、OMS 的
`oms_cap_gate_recognises_writings_not_a_cap`、恢复退避的
`spread_recovery_backoff_is_pinned_by_text_not_by_a_running_loop`（外加 R4-5 那处播报顺序），它们同属
"跑起来的循环没有常驻用例"这一族，要真闭合需要一条能在 CI 里跑的 venue 桩；**`crates/qx-control/tests/`
仍是空目录**——R4 轮没有留下一份常驻的控制面规模用例，因为留下它等于替"只追加"这个当时尚未裁决的形状
作保。R5 轮裁决已定，退场形状的五颗常驻用例落在 `crates/qx-control/src/tests.rs`（目录本身仍空，但没有
一处判据替旧形状作保）；同一轮里 H3 的两半也接上了（写侧 §53.3、读侧 §53.4）。
R5 轮之后新增四颗（逐条见 §53.8）：**Postgres 那本审计链端到端从未被扫过**——doctor 不触网，那一本只报
未扫描，`PgChainWriter` 的逐环核对今天只有编译与文本判据看过；**`PostgresAuditStore` 仍是零构造点的公开类型**，
按 J1 那把尺子删还是留需要一次裁决；**doctor 的 `audit_chain` 检查没有用例级牙齿**（R5-B4 撤掉挂载只红门禁）；
**台账引用门禁脚本自身行号的那一格没有判据可依**，只能靠复跑守住。R5-1 / R5-2 已随裁决落地，H3 与 R4-7
那两条"等一个决定"从本节的账上划掉。

R7 轮之后新增三条（逐条见 §55.13）：**R7-2 与 R7-3 没有形状判据**——再写一处 TEXT 列 `ORDER BY`、再长出一张不校验
行内 id 的键表，仓内没有任何东西会红，只有契约用例在原地挡着，用例数地板只挡"整批被删"；**R7-5 量下来不是缺陷**——
`fetch_remote_snapshot`（`crates/qx-adapter/src/binance.rs:1200`）的分页被限频预算界住，超预算是 `Err` 而不是静默截断，
要收它得先给"页数上限"这件事一个真实故障模型；**R7-6 那 15 颗是"唯一实现但没接线"**（`tools/check_architecture.py:3759`），
接不接待是裁决题，本轮只把登记的两侧口径改到与可达性一致。

## Unreleased — V11 合流轮（两次）：第一次契约对自家写侧说了谎，第二次冲突的全是文档口径（2026-09-23）

T 轮落进本地之后 `git fetch` 才看到 `origin/main` 上多了 Q70/Q71 两颗；把那颗合到全绿，远端又多出 Q72（72c347e），于是同一轮里合了两次。两侧各自跑完自己的门禁与用例都是绿的，
合流之后才发现**对外契约对它自家读模型每天印出的 `"equity_raw":null` 说了谎**：Q70 把协议侧 `equity_raw` 变成
`Option<i128>`（缺标记价的持仓不算权益，而不是拿剩余现金冒充），T3 刚把契约钉成"七格可空、权益不可空"。
本轮跟上事实，并把三处判据从"抄字段名单"换成"问写侧产物与协议类型"。逐项证据、5 颗变异与一句被当场证伪的话见
[docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §39；第二次合流的四处口径判断在同文档 §39.5。

### Fixed

- **`schemas/account-snapshot-v1.json` 的 `equity_raw` 改为 `["integer", "null"]`**，判据同时换掉三处：
  `crates/qx-protocol/tests/snapshot_schema_contract.rs` 那条用例改由写侧自己交两份产物（八项全未算 /
  八项全算得出）逐格比契约，并要求那份"全部未算"能被 `from_json` 读回 `None`；门禁
  `account_snapshot_schema_check` 第 4 项直接正则读协议源码，契约里可空的那些必须正好等于类型为
  `Option<i128>` 的那些；`python/tests/test_bridge.py` 补"契约允许 null 的键，对面 Python 读侧也收得下"。
  名单与类型任何一侧单独漂都会红。
- **门禁的一条失败文案停在旧口径**：`/account/balances` 那一项的条件里 `equity_raw` 是第三条，文案只点名
  available/margin——判据对、文案错，一样会把人往错的方向上带（M5 顺手查出来的）。
- 合流余下的三处编译错与一处夹具分叉：T3 的两份夹具赋值补 `Some(...)`（`snapshot_schema_contract.rs:279`、 <!-- 史 -->
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
收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §34。

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
- **多腿链当场拒收那一格单账户数字**（`leg_funding.rs:73-85` 的 `reject_configured_initial_cash`，
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
[docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §38。

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
[docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §35–§37。

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
[docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §33。

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
  `cli_backtest_module_check()`（`tools/check_architecture.py:723`，`>= OVERSIZED(500)` 即红，登记进预算表
  也救不了）。把入口那 11 行组级合计折叠搬进归因内核 `crates/qx-cli/src/multi_leg.rs:452-470`
  （新增 `multi_leg_group_totals`，保证金仍取各组峰值而非求和），入口只留 1 行调用；
  `maturity/line_budgets.yaml` 本轮**一个字节都没动**。

### Added（用例、产物两端与门禁）

- `crates/qx-cli/src/tests/execution_and_multi_leg.rs:304-331`
  （`multi_leg_combined_return_weights_each_leg_by_its_own_capital`）：本金 3:1、两腿 +100bp/+1000bp 时必须
  给 **325bp** 而不是等权的 550bp；本金相等时两口径重合（排除"只是换了个说法"）；亏 1000bp 的主腿压过
  持平的对冲腿给 -750bp 而不是 -500bp；全零本金必须 `Err` 且文案含"本金合计必须为正"。
- `crates/qx-cli/tests/multi_leg_attribution.rs:726-792`
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
  `execution_and_multi_leg.rs:326`，M3（产物缺两腿期末权益）红在 `:756` 的缺键 panic，M4（入口重新自己折 <!-- 史 -->
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
收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §32。

### Fixed（最后一个不可区分的钱标量）

- **读模型不再替账户宣称"压在持仓上的那一腿不值钱"**：`crates/qx-cli/src/api_service.rs:268-272` 此前写
  `equity_for(...).unwrap_or_else(|| cash_for(...))`。内核 `Ledger::equity_for`
  （`crates/qx-core/src/ledger/query.rs:156-203`）的 `None` 含义明确 —— 有任意一条持仓拿不到标记价、或估值
  溢出。折成剩余现金之后，一个只有成交、没有行情事实的账户会把现金长期报成权益，且哈希/稳定 JSON/线格式/
  diff 全链路自洽，读侧无从发现。现在这个 `None` 原样发布为 `null`。
- **协议层第一次能表达"权益没算"**：`crates/qx-protocol/src/lib.rs:102` 的 `equity_raw` 由 `i128` 变
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
  `Some(500)`，那一条"把 `None` 折成 0"的变异动不了它 —— M3 的 panic 点 `crates/qx-api/src/lib.rs:2545` <!-- 史 -->
  正是本轮新增的那一段，没有它这个发布面就没有行为证明（日志里 M3 另外三组用例全绿）。
- 本轮 `gate1` 即验收日志：fmt/clippy 在变异段之前先跑绿，是 §31.3 那条教训的落地（无作废轮次）。

## Unreleased — V11 Q69：CCXT 对账的两半发现同时进事实流、报告与健康（2026-09-23）

交易链路实测（V11 #54）排到的第三颗：交易 TX3，§28.5 第 3、4 条（在 §29.5 以第 4、5 条重挂）。
收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §31。

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
收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §30。

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
收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §29。

### Fixed（交易链路：持仓行的三个钱字段与方向）

- **内核持仓观察的 `unrealized_pnl` / `initial_margin` / `maintenance_margin` 改 `Option<Money>`**
  （`crates/qx-core/src/event.rs:187-191`，各带 `#[serde(default)]` 让老日志缺键读成未报）：改前三者是裸
  `Money`，摘要里 `h.write_i128(position.unrealized_pnl.raw())`（HEAD `:451-453`）——"这个交易所不报维护保证金"
  与"报了零"在类型、在 `Event::digest`、在线格式上是同一份状态。现在摘要逐字段先写存在性标记再写值（`:463-466`）。
- **线格式行的两列钱改 `Option<i128>`，`Default` 不再兜 0**（`crates/qx-protocol/src/wire.rs:34-35`、`:46-47`）：
  任何 `..PositionSnapshot::default()` 起步的构造不再自动"报一个零"。**价格列刻意保持不变** —— 定点价格里 `0`
  不是合法值，钱没有这个性质；这条区分写进字段文档并由门禁钉住。
- **读模型的 Ledger 回退行不再替交易所报数**（`crates/qx-cli/src/api_service.rs:350-354`）：改前是写死的
  `unrealized_pnl_raw: 0, margin_raw: 0`（HEAD `:332-333`），而 Ledger 里没有这两个量（要按冻结的 market spec
  逐标的算，读侧没有规格来源）——一个上涨 5% 的现货仓位会长期报着"没有浮亏"。
- **CCXT 持仓方向不再靠猜**（`crates/qx-cli/src/ccxt_facts.rs:110-122`）：改前
  `.unwrap_or("long")`（HEAD `:109`），而上游连接器（`python/qianxing_ccxt/__init__.py:1240`）在交易所不给方向时
  印的是字面量 `"unknown"` —— HEAD 的实际行为是数量按**正数**入账、`position_side` 记 `"unknown"`，同一份观察里
  数量与方向自相矛盾。现在只接受 `long`/`short`，其余连 symbol 带"读到什么"一起拒绝；零数量行仍在闸门**之前**跳过。
- **CCXT 三项钱读成"没报"而不是零**（`:146-154`）：改前缺键与 `null` 一律 `Ok(Money::ZERO)`（HEAD `:131-140`），
  即生产方说"没报"、消费方改口说"零"。守卫侧（`crates/qx-runtime/src/pipeline.rs:1649-1657`）改成"报了才判"，
  不报保证金的交易所不会被判非法回报。
- **折算与哈希收口到共用写法**：行哈希与账户标量共用 `write_optional_money`（定义 `crates/qx-protocol/src/lib.rs:267`、两处调用
  `:274` / `:319`），`null` 的写法全仓唯一（`:279` `money_json`），线格式没有的那一列保证金折算回 `None` 而不是 0。

### Added（用例与门禁）

- 8 条新行为用例，全部走真实入口：`crates/qx-cli/src/tests/ccxt_position_facts_honesty.rs`（146 行 3 条——方向
  fail-closed、平仓位先跳过、省略/`null`/`"0"`/`0` 四态区分）、`api_snapshot_money_fields.rs` 加 2 条行侧
  （回退行缺席、venue 报的行保留两态）、`event.rs` 的摘要两态用例、`snapshot_single_source.rs` 加 3 条
  （折算两态、行两态逐个字段跑哈希/seal/两种线格式往返/增量双向、`Default` 不报钱）；`pipeline.rs` 落盘回读用例
  加第二行，让"报了零"与"没报"同时出现在一份快照里并要求回读后不收敛。
- `tools/check_architecture.py:2125` `position_money_honesty_check()` 13 项（静态门禁 256 → **269**）：内核
  Option 形状、摘要标记唯一、行字段形状**且价格列保持 0 哨兵**、折算与槽位各只有一处写法、全仓逐行禁止给这五个
  字段兜 `0`/`Money::ZERO`、方向闸门禁抄列表、跳过点必须先于闸门、守卫的 `is_some_and` 序列、八处用例按
  `fn NAME(` 取证。`CLI_TEST_FLOOR` 173 → **178**；行数棘轮重登记 `event.rs` 674→749、
  `qx-protocol/src/lib.rs` 856→885、`pipeline.rs` 2349→2391。

### Changed（有意的破坏性后果）

- `Event::digest` 的形状变了（多一次存在性标记）：**含 venue 持仓观察的既有事件日志**会在 manifest 核对处被拒
  （`crates/qx-storage/src/lib.rs:307-309`、`crates/qx-storage/src/sqlite.rs:2104`）。这是有意的 fail-closed ——
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
[docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §28。

### Fixed（交易链路：七个汇总钱字段里六个没有生产者）

- **`available_raw` 不再是 `equity_raw` 的副本**：改前 `crates/qx-cli/src/api_service.rs:273` 就是
  `snapshot.available_raw = snapshot.equity_raw;` —— 把已经压在持仓上的那段钱说成可自由花掉。现在取本条快照记账
  的那一本结算账簿现金 `cash_for(account_id, settlement_currency())`（`:276`）。
- **`fees_raw` 与同一条快照的逐笔成交同源**：此前账户级恒 0，而同一份 JSON 的 `fills[].fee_raw` 逐笔是真数。
  现在由这些 `fee_raw` 以 `checked_add` 加出（`:296-302`），**合计溢出即拒绝发布这份快照**而不是印一个回绕过的
  数（用例把账户现金精确压到 `i128::MIN`，证明溢出只可能出现在读模型这一侧）。
- **算不出来的钱在协议上必须能缺席**：`AccountSnapshot` 的七个汇总钱字段由裸 `i128` 改 `Option<i128>`（权益仍
  是恒算得出的 `i128`）。改前 `frozen_raw` 在 `git grep` 下只命中 `crates/qx-protocol/src/lib.rs` 一个文件
  ——生产者数量为零，每条对外快照都在宣布"这个账户没有冻结资金"。`margin`/`realized_pnl`/`unrealized_pnl`/
  `funding` 同样只有读法。现在 `new()` 一律 `None`，稳定 JSON 与线格式印 `null`，`/account/balances`
  （`crates/qx-api/src/lib.rs:1459-1460`）随之从 `map(…)` 改 `and_then(…)`。
- **`None` 与 `Some(0)` 是两份状态**：`state_hash` / `scalar_hash` 共用的那一次标量写入先写存在性标记
  `write_u64(u64::from(value.is_some()))` 再写数值（`crates/qx-protocol/src/lib.rs:268`），八个钱的取法收敛到
  `scalar_money_raw()`（`:251`）唯一一处，稳定 JSON 槽位只由 `scalar_json_values()`（`:286`）填。未算状态再也
  改不出一个"看起来算过"的 0。

### Added（用例与门禁）

- `crates/qx-cli/src/tests/api_snapshot_money_fields.rs`（253 行，4 条，走真实 paper 入口与真实读模型）：结算
  账簿口径、费用与逐笔成交同源、未算缺席、费用合计溢出即拒。
- `crates/qx-protocol/tests/snapshot_single_source.rs` 新增 `uncomputed_money_is_not_the_same_state_as_computed_zero`：
  七个字段逐个跑"哈希不同 → seal/validate 自洽 → 稳定 JSON `null` vs `0` → 两种线格式往返保住区分 →
  `diff`+`apply` 两个方向都是一次真实改动"。`qx-api` 新增端点用例锁 `margin_raw: null`。
- `tools/check_architecture.py:2322` `snapshot_money_honesty_check()` 11 项（静态门禁 245 → **256**）：Option
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
[docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §27。

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
- `tools/check_architecture.py:1628` `input_provenance_check()` 9 项 + 6 条 `BACKTEST_ENTRY_OWNERS` 归属项
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
（FN6）。收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §26。

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
  `RunManifestIdentity`（`single_strategy.rs:169`、`depth.rs:182`），不落摘要的内置链把它印成
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
（FN5）。收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §25。

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
（TX1）。收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §24。

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
收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §23。

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
收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §22。

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
收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §21。

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
收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §20。

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
- **新的规格闸门 `multi_leg_spec_guard`（`backtests/leg_funding.rs:114-157`）四条判据，先于任何腿级
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
收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §19。

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
收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §18。

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
顺带关掉 §16.4 第 4 条。收口记录见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md) §17。

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

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
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

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
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

- `backtest book` 新增 `--latency-snapshots`（`cli_args.rs:479`）/ `--market-impact-bps`（`cli_args.rs:477`，
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

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
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

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
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

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
§3B 末条（死配置面）与 §6；本轮采用其 §0 的编号默认值 **E3 = 接入**（而不是删净），
收口记录见同文档 §12。上游 `77eb160`（成交归约 + 账本记账币种）在本轮断点合并进主线，
合并提交 `ef461b4`，取舍口径见 §12.1 末段。

### Added（`strategy.cost_rules_path` 从死配置变成生效配置）

- **成本规则文件有了第一个生产读者**：`ExecutionCostRules::load`（`crates/qx-xingban/src/cost_rules.rs:48`）
  此前全仓零消费者，`deploy/qianxing.costs.example.json` 是一行"宣称能配、实际没人读"的死配置。
  现在唯一读取点是 `runtime_wiring::execution_cost_binding_from_config`（`crates/qx-cli/src/runtime_wiring.rs:184`），
  它返回的 `ExecutionCostBinding`（`runtime_wiring.rs:156`）同时提供 `fee_model()` 与 `latency_model()`，
  消费方为 Bar 两条链（`backtests/single_strategy.rs:94` 与 `:278`）、多腿链
  （`backtests/multi_builtin.rs:155`，两条腿共用同一份绑定）、深度档（`backtests/depth.rs:69`）
  与三处 Paper 构造点（`venue_runtime/paper_worker.rs:48/191/244`、`paper_submit.rs:121`）——
  Q0a 解决的"两条链取同一个模型"从此不必再靠两边都写同一个常数。
- **产物记录成本来源**：回测摘要新增 `execution_costs.source`（`backtests/artifacts.rs:344`），
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
  CI 侧 `deploy/qianxing.runtime*.json` 的 for-loop（`.github/workflows/ci.yml:204-213`）
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

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
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

方案与逐阶段验收口径见 [docs/自研量化框架重构方案-V11.md](docs/自研量化框架重构方案-V11.md)
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
"Strategy worker 已关闭输出"（`crates/qx-cli/src/tests_main.rs:2812` 与 `:2856`）。 <!-- 史 -->
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
`crates/qx-storage/src/lib.rs:3521` 的 `unwrap()` 收到 `Io("拒绝访问。 (os error 5)")` —— Windows <!-- 史 -->
下跨进程令牌桶在全量并发跑时的文件共享冲突。同轮复跑把它隔离出来连跑 8 次全部 `17 passed; 0 failed`，
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
