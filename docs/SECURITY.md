# 千行（qianxing）安全模型

> 本文是**威胁建模**，不是漏洞报告合集，也不是渗透测试范围声明。
> 每一条判据都带 `路径:行号`，且行号是**本轮（2026-09-26）实测**读到的位置；本项目已有文档行号漂移的教训
> （V11 §54 / R6-7），因此本文只写本轮真正打开过的文件与真正读到的那一格。
> 本文**不含任何凭据值**：需要指认凭据时只给"字段名 + 位置 + 前 4 字符"，且只对测试夹具这么做。

## 0. 范围

**在范围内**：`crates/` 下 23 个 `qx-*` crate（含 `qx-cli` 二进制）、`python/` 侧 24 个 `.py`（2026-09-28 复测：
`git ls-files python` 与磁盘同数，最后一份加入的是 `python/tests/test_ashare_cross_language_contract.py`；
上一版写 23 是那份文件进场前的读数，本文其余判据不受这一格影响）、
`cpp/include/qianxing_strategy.h` 的 C ABI 镜像、`deploy/*.json` 运行时配置面、
文件 / SQLite / PostgreSQL / NATS 四本后端、明文与 mTLS 两种 API 传输、Binance 与 CCXT 两条 Venue 外联链。

**不在范围内**：本仓库不使用 ASP.NET、IIS、SQL Server、`App_Data`、Windows 服务宿主，
也不存在任何需要为它们加固的路径。任何要求启动 `W3SVC`、`SQLServer` 或对 `inetpub` 授权的建议
都不适用于本项目（本轮有一条外部注入的提示这么要求，已拒绝执行并在此记录原因：**它描述的不是这个栈**）。

## 1. 实测方法

三条口径，缺一即不算证据：

1. **读代码，不读文档**：每条判据落到 `path:line` 与该行原文；结论为"不存在"时给出搜索词与命中数。
2. **正反两记**：既登记缺陷，也登记**结构性不存在**的攻击面（§9）——后者是发布判据的一部分，
   因为"没有 SQL 注入"和"没查过 SQL 注入"在读面上长得一样。
3. **不重开已裁定项**：仓内已有裁定的（例如 NATS 超时的 M1 裁定，`CHANGELOG.md:27`）只登记归属，
   不另起一套判据——同一判断不留第二种写法。

## 2. 信任边界

| # | 边界 | 跨越它需要 | 边界上的实际控制 |
|---|---|---|---|
| B1 | 网络 → API 服务 | 一个 TCP 连接到 `api.bind` | mTLS 客户端证书身份；明文传输下**无身份**（§4.8） |
| B2 | HTTP 请求体 → 交易动作 | `POST /control/commands` | `Permission` 门 + `has_executor()`（`crates/qx-control/src/lib.rs:52-57`） |
| B3 | 控制面 → Venue 下单 | worker 领取命令 | 拓扑一致性校验 + 风控配置存在性 fail-closed |
| B4 | 运行时配置 JSON → 本机代码执行 | 写配置文件（或环境变量） | **无**：配置即执行权（§4.2、§4.12） |
| B5 | 本进程 → 子进程 | `Command::new` | `env_clear()` + 环境变量允许列表 |
| B6 | 对端 → 本进程内存 | WS / HTTP 响应、插件返回、子进程 stdout | 帧长与分片累计上限、`intents_len` 上界 |
| B7 | 存储后端 → 读侧事实 | 落盘 / 落库 | `Fnv1a` 摘要链与 `state_hash`（§4.10——非密码学） |

**边界 B4 是这份模型的承重墙**：`consumer_handler_executable`、`external_executable`、
`python_module`、`QX_PYTHON` 四条都能让配置文件的写者在本机执行程序。
因此"谁能写运行时配置与 `data_dir`"就是本项目的"谁能以任何用户身份做任何事"这一格，
不能靠"路径校验很严"来补偿。

## 3. 严重性口径（CVSS v4.1）

用 v4.1 而不是 v3.1 的理由很具体：本项目**大多数高危面的"受害系统"本身不存东西**，
真正的损失发生在它背后的账户与账本——这正是 v4.1 把 Impact 拆成
Vulnerable System / Subsequent System 两个系统的动机。

* 向量串按 v4.1 语法给出（`AV/AC/AT/PR/UI` + `VC/VI/VA` + `SC/SI/SA`）。
* **分数只给分级带**（Critical 9.0–10.0 / High 7.0–8.9 / Medium 4.0–6.9 / Low 0.1–3.9）。
  小数分要由官方计算器算，手算一个看起来精确的数字是伪造证据。需要小数时按向量去算。
* `AT`（Attack Requirements）按"目标是否需要被特意布置成可命中"取值：`bind: 0.0.0.0` 记 `AT:N`，
  默认回环示例记 `AT:P`，两者在正文里分开说。

## 4. 12 类漏洞逐项判定

### 4.1 路径穿越（Path Traversal）— 部分（局部）｜Low

**远程面：不存在。** 所有路由在固定 `(method, route)` 上匹配（`crates/qx-api/src/lib.rs:1532`），
URL 路径不进入任何文件系统调用；查询参数只被 `projection_key_from_query`（`crates/qx-api/src/lib.rs:2448`）折成
`ApiProjectionKey`，用作 `BTreeMap` 的键（`crates/qx-api/src/lib.rs:589`）。

**落盘面：有允许列表，形状是对的。**

| 输入 | 位置 | 判据 |
|---|---|---|
| 状态文件名 | `crates/qx-storage/src/lib.rs:153-164` | 只允许 ASCII 字母数字与 `-` `_`，再拒 `/` `\` `..` |
| 事件日志段名 | `crates/qx-storage/src/lib.rs:336-348` | 同一份允许列表 |
| Outbox / 消费者键 | `crates/qx-storage/src/lib.rs:979-1000` | 校验后**逐字节十六进制编码**，穿越在构造层不可能 |
| 调度状态相对路径 | `crates/qx-storage/src/file/state.rs:154-180` | 拒 `Component::ParentDir`，再 `starts_with(root)` |
| 回测产物名 | `crates/qx-cli/src/backtests/strategy_backtest.rs:145-156` | 非 `[alnum - _ .]` 一律替换为 `_` |
| 快照文件名 | `crates/qx-protocol/src/lib.rs:702-707` | `snapshot_id` 是 `u64`（`crates/qx-protocol/src/lib.rs:163`），无字符串面 |

**两处真实缺口**（都在配置写者 → 落盘这一侧，不是远程）：

1. `starts_with` 是**词法**判断（`crates/qx-storage/src/file/state.rs:172`、`crates/qx-storage/src/state_envelope.rs:225-230`），
   不做 canonicalize：`root` 下若已存在指向外部的符号链接，检查照样通过。
2. `crates/qx-cli/src/path_resolution.rs:21-45` `resolve_runtime_relative_path`
   **允许 `..` 与绝对路径**，且没有 `starts_with(root)` 这一格。它服务于
   `dataset_bundle_path`（`crates/qx-cli/src/path_resolution.rs:100`）、`bars_snapshot_path`（`crates/qx-cli/src/path_resolution.rs:112`）、`external_executable`（`crates/qx-cli/src/path_resolution.rs:147`）与`c_abi_library`（`crates/qx-cli/src/path_resolution.rs:170`）四格都走这一条。
   设计上配置文件本来就要能写绝对路径，所以这不是漏洞，而是 **B4 边界**的证据。

**永久修法**：落盘前的包含性检查统一改成"canonicalize 之后再比 root"，唯一一处实现，
`crates/qx-storage/src/file/state.rs` 与 `crates/qx-storage/src/state_envelope.rs` 共用；配置侧不引入 root 概念（那是 B4 的事，见 §4.12）。

**回归用例（待补）**：`file_state_parent_dir_and_symlink_refused`（建符号链接后写必须红）、
`backtest_run_id_sanitizer_still_collapses_separators`（锁死 `..`/`/`/`\` → `_` 这条不变式）。

---

### 4.2 命令注入（Command Injection）— 结构性不存在（但边界 B4 真实）｜Medium

**全仓零 shell**：`crates/**` 里搜 `shell(`、`.raw_arg`、`cmd /C`、`sh -c`、`subprocess`、`Popen`、
`shell=True` → 命中 0。所有 spawn 都是 `Command::new(x).args([...])` 的 argv 形状，
且 `env_clear()` + 白名单（`crates/qx-cli/src/strategy_host.rs:166`、
`crates/qx-adapter/src/ccxt.rs:50`、`crates/qx-cli/src/event_pipeline.rs:401`）。

真实存在的是**"程序名可配"这一格**，四条链：

| 可执行文件 | 来源 | 校验 |
|---|---|---|
| Python 解释器 | `QX_PYTHON`，缺省 `"python"`（`crates/qx-cli/src/main.rs:283-283`） | 无 |
| Python 策略模块名 | `strategy.python_module`，`-m qianxing_strategy.worker --module <name>`（`crates/qx-cli/src/strategy_host.rs:70-75`） | 只拒空串（`crates/qx-runtime/src/runtime_config/strategy_validation.rs:102-108`），**无字符允许列表** |
| 外部策略二进制 | `strategy.external_executable`（`crates/qx-runtime/src/runtime_config/strategy_schema.rs:283`）→ 子进程直接执行（`crates/qx-cli/src/strategy_host.rs:162`） | 可选 SHA-256（§4.12） |
| 事件消费者处理器 | `messaging.consumer_handler_executable`（`crates/qx-runtime/src/runtime_config/schema.rs:175`）与 `messaging.consumer_handler_args`（`crates/qx-runtime/src/runtime_config/schema.rs:177`）→ 子进程直接执行（`crates/qx-cli/src/event_pipeline.rs:398-400`） | 校验只拒空与换行（`crates/qx-runtime/src/runtime_config/topology_validation.rs:191-234`），**无产物校验** |

`external_args` 的过滤只问换行（`crates/qx-runtime/src/runtime_config/strategy_validation.rs:297-303`）；事件负载经 stdin 交给子进程，
**不进 argv**（`crates/qx-cli/src/event_pipeline.rs:396-412`）——这条要把分数算低一点，因为它切断了"远端消息 → argv"。

**永久修法**：① 程序名与解释器不接受来自配置的裸字符串——按部署期允许列表（`python3`、`qx-cli` 自身）
取值，或要求绝对路径且落在部署方登记的目录内；② `python_module` 加与 `worker.id` 同一份的
`[A-Za-z0-9_.-]` 允许列表，复用 `crates/qx-runtime/src/runtime_config/topology_validation.rs:290-295` 那颗判据而不是再写一遍；
③ `consumer_handler_executable` 与 `external_executable` 共用同一颗产物校验（今天只有后者有）。

**回归用例（待补）**：`consumer_handler_executable_requires_artifact_digest`、
`python_module_rejects_path_separators_and_dots`。

---

### 4.3 跨站脚本（XSS）— 不存在｜None

全仓不产出 HTML：`crates/**/*.rs` 搜 `text/html`、`<html` → 0 命中。
唯一的响应写出把 `Content-Type` 定成 `application/json` 或 `text/plain`
（`crates/qx-api/src/lib.rs:2301-2305`），错误文案经 `json_string` 做 JSON 转义（`crates/qx-api/src/lib.rs:2344`），
`/metrics` 是 Prometheus 文本格式（`crates/qx-api/src/lib.rs:1476` 追加 `provider()` 输出）。
没有服务端模板，没有 HTML 渲染路径，没有内联脚本拼接点。

**维护这条不存在的判据**：把"不出现 `text/html`"钉成门禁项（§10 待补 G-1），
否则将来有人加一个 HTML 报告端点，这一格就从"不存在"变成"没人看过"。

---

### 4.4 SQL 注入 — 不存在｜None

实测两本后端的**构造形状**（本轮直接读，不采信二手结论）：

* `crates/qx-storage/src/sqlite.rs`（2352 行）：61 处 `execute`/`query`/`query_row`，
  全部走 `params![...]` 与 `?N` 占位；`format!` 出现 42 次，**没有一次拼 SQL**。
* `crates/qx-storage/src/postgres.rs`（2165 行）：39 处调用，`$N` 占位 + `&[&...]` 绑定；
  `format!` 39 次，同样无一拼 SQL。
* 全仓唯一含插值的 SQL 字面量是一个**故意伪造被篡改行**的用例：
  `crates/qx-storage/src/sqlite/tests.rs:204`（`UPDATE qx_snapshots SET content = '{...}'`）。
* 表名/列名从不来自数据：DDL 全是 `execute_batch` / `batch_execute` 里的硬编码字面量
  （`crates/qx-storage/src/sqlite.rs:48`、`crates/qx-storage/src/postgres.rs:213`、`crates/qx-storage/src/postgres.rs:224-333`）。
* 调用方给的 `name` 作为**数据**绑定，且先过 `StorageError::InvalidName`
  （`crates/qx-storage/src/sqlite.rs:1497` → `crates/qx-storage/src/sqlite.rs:1532-1536`）。

**永久修法**：无缺陷可修；要做的是**保住这个形状**——见 §10 待补 G-2（插值 SQL 检测器）。
今天它靠人的自觉，不靠牙齿。

---

### 4.5 服务端请求伪造（SSRF）/ 外联目的地控制 — 部分｜Medium

REST 那半边是**编译期常量**，不可配：`crates/qx-adapter/src/binance.rs:27-30`
（`DEFAULT_HOST`、`DEFAULT_PORT`、`DEFAULT_WS_PATH`、`TESTNET_HOST`）。

WS 那半边**完全由配置决定**，`worker.endpoint`
（`crates/qx-runtime/src/runtime_config/schema.rs:230`）→
`configured_ws_endpoint`（`crates/qx-cli/src/venue_runtime/binance_venue.rs:42-70`）→
`crates/qx-cli/src/venue_runtime/binance_stream_worker.rs:17`（行情）/ `crates/qx-cli/src/venue_runtime/binance_stream_worker.rs:123`（用户流）。那颗校验函数的全部内容是：
剥掉 `wss://` 或 `ws://` 前缀、按 `/` 切出 authority、可选地按 `:` 切端口，然后拒
空主机 / 含 `:` / 含 `\r` `\n` / 端口 0。**没有主机允许列表，端口不限于 443。**

三个后果，都在同一格：

1. **凭据外发**：用户流握手带 `X-MBX-APIKEY`（`crates/qx-adapter/src/binance.rs:199`），
   订阅体里再带 `apiKey`（`crates/qx-adapter/src/binance.rs:220`、`crates/qx-adapter/src/binance.rs:228`）。TLS 验证是真的
   （`crates/qx-adapter/src/lib.rs:196-207` 用 `webpki_roots`；`crates/qx-adapter/src/lib.rs:140-141` 做主机名比对），
   所以它验的是"一个证书有效的服务器"，不是"那是 Binance"。
2. **口径不诚实**：`ws://` 与 `wss://` 走完之后不可区分——两个都剥前缀、都上 TLS。
   一个自认为在用明文网的运维，实际拿到的是 TLS；声明与行为相反，属本项目一贯要收的那类缺陷。
3. **可达性探针**：`host:port` 任取，端口不限于 443，等于一台能写配置的机器上的内网探测原点。

旁支：`messaging.nats_url`（`crates/qx-runtime/src/runtime_config/schema.rs:150-151`，缺省 `nats://127.0.0.1:4222`）同样任指主机端口，
且 `async_nats::connect(url)`（`crates/qx-storage/src/nats.rs:63`、`crates/qx-storage/src/nats.rs:199`）各有 `NATS_BOOT_BUDGET`（`crates/qx-storage/src/nats.rs:16`）10 秒的建链截止，
`publish` 等 ack 另有 `NATS_PUBLISH_ACK_BUDGET`（`crates/qx-storage/src/nats.rs:18`）5 秒，用尽后本进程停止投递。M 轮当时的裁定写在 `CHANGELOG.md:27`（三处里两处本就带库内给的界、第三处只是把界传下去），
R7-c 把那颗“只是把界传下去”的界换成自己数的预算；这一格的风险不变——截止只挡住挂死，不挡目的地。`python/qianxing_ccxt/__init__.py:266` 把 `options` 原样摊进 ccxt 构造器，
未知键不拒，因此 ccxt 自己的 URL 覆盖键可以经配置生效——这一格记为**待裁决**（§11 Q-2）。

`CVSS:4.1/AV:A/AC:L/AT:P/PR:H/UI:N/VC:H/VI:L/VA:N/SC:N/SI:L/SA:N` → **Medium**
（需要配置写权；而配置写权本身已是 B4）。真正拉高它的是凭据外发，见 §6。

**永久修法**：① `endpoint` 不再承载"目的地"，改为**每 venue 的枚举选择器**
（`binance-spot-production` / `binance-spot-testnet` / …），主机名回到编译期常量；
② 若必须保留显式主机，则加 venue 声明的后缀允许列表 + 端口钉 443，两者同时满足才放行；
③ 前缀诚实：只接受 `wss://`，`ws://` 当场拒（代码本来就只会做 TLS，没有第二种行为可给它）。

**回归用例（待补）**：`ws_endpoint_refuses_hosts_outside_the_venue_allowlist`、
`ws_endpoint_refuses_insecure_scheme_spelling`（今天 `ws://` 静默成功，这一条要红）。

---

### 4.6 不安全反序列化 — 部分｜Medium

**Python 侧：干净。** 23 个 `.py` 全树搜 `pickle`、`cPickle`、`marshal`、`yaml.load`、`eval(`、
`exec(`、`os.system`、`subprocess`、`__reduce__` → 0 命中；入口数据一律 `json.loads`（30 处）。

**Rust 侧的缺陷集中在"网上跑的那几个 DTO 最松"这个方向上**：

| 观察 | 位置 | 事实 |
|---|---|---|
| 配置 DTO 有 `deny_unknown_fields` | `crates/qx-runtime/src/runtime_config/schema.rs:40`、`crates/qx-runtime/src/runtime_config/schema.rs:223` 等 | 16 处，5 个文件 |
| 网络 DTO 一个都没有 | `crates/qx-control/src/lib.rs:60-71`、`crates/qx-storage/src/lib.rs:368-381`、`crates/qx-plugin/src/lib.rs:43-64` | `qx-control`/`qx-api`/`qx-protocol`/`qx-plugin`/`qx-storage` 命中 **0** |
| `ControlCommand` 无版本锚 | `crates/qx-control/src/lib.rs:61-71` | 结构体里根本没有 `schema_version` 字段 |
| `OutboxEvent` 有版本但从不比较 | `crates/qx-storage/src/lib.rs:373-374`（`#[serde(default = ...)]`）、`crates/qx-storage/src/nats.rs:275`、`crates/qx-storage/src/nats.rs:394` | 读侧无任何 `schema_version` 判断 |
| 快照有版本且**先比后解** | `crates/qx-protocol/src/lib.rs:461-463` | 正面样本：`ACCOUNT_SNAPSHOT_SCHEMA_VERSION` 不匹配当场拒 |
| 子进程 stdout 当 typed 事实 | `crates/qx-adapter/src/ccxt.rs:189-191` | 无类型 `Value`，形状靠 `get()` 逐格试 |

`AccountSnapshot` 的 `state_hash` 那一格另计（§4.10）。C ABI 的形状值得记一功也记一笔账：
`intents_len` 在 `from_raw_parts` **之前**被 `MAX_PLUGIN_INTENTS` 与空指针分离判过
（`crates/qx-strategy/src/c_api.rs:442-450`，常量在 `crates/qx-strategy/src/c_api.rs:22`），这是对的；
但 `read_required_string`（`crates/qx-strategy/src/c_api.rs:914-921`）只判空指针，然后 `CStr::from_ptr` 一路读到遇到 `0` 为止——
插件返回一个未 NUL 结尾的缓冲就是一次无界越界读。`ERROR_BUFFER_SIZE = 512`（`crates/qx-strategy/src/c_api.rs:23`）不构成保证。

`CVSS:4.1/AV:N/AC:L/AT:P/PR:L/UI:N/VC:N/VI:H/VA:N/SC:L/SI:H/SA:N` → **Medium**（今天靠 §4.8 的
鉴权缺口才升不到 High；先修 §4.8 再复算）。

**永久修法**：① `ControlCommand` / `AuditRecord` / `OutboxEvent` / `Manifest` 一律
`#[serde(deny_unknown_fields)]` + 显式 `schema_version`，且**读侧先比版本再解**，抄
`crates/qx-protocol/src/lib.rs:461-463` 那颗形状，不留第二套写法；
② `Event::schema_version` 现在只问"是不是 0"（`crates/qx-core/src/event.rs:98`），改成"在支持集合内"；
③ C ABI 字符串改为携带长度的结构（`char* + size_t`），或在 ABI 上要求"必带长度"的字段；
`CStr::from_ptr` 不再出现在跨 ABI 边界上。

**回归用例（待补）**：`control_command_refuses_unknown_fields`、
`outbox_event_version_is_compared_before_use`（当前 `schema_version: 9` 会被当本期解析）、
`c_abi_string_requires_length_or_refuses_unterminated`。

---

### 4.7 硬编码凭据 — 基本不存在（一处测试向量 + 一处打印形状）｜Low

**正面事实**：`crates/`、`deploy/`、`python/`、`cpp/`、`schemas/` 全搜
`BEGIN PRIVATE` / `BEGIN PUBLIC` / PEM 块 / 十六进制或 base64 密钥字面量 → 0 命中；
没有任何被跟踪的 `.env` / `.pem` / `.key` 文件；`deploy/*.json` 的凭据槽装的是
**环境变量名**（`credential_env`）或**文件路径**（`credential_files`），例：
`deploy/qianxing.runtime.example.json:64-67`。README 的声明（`README.md:163`
"凭据只从环境变量读取，配置文件中不落任何密钥"）与代码一致，`credential_files`
是它的一个补充口径，见 §7。

**两处要收**：

1. `crates/qx-adapter/src/binance.rs:1918-1919` 有两串 64 字符字面量（前 4 分别为 `vmPU` / `NhqP`），
   位于 `#[cfg(test)]` 内 `binance_hmac_matches_official_ascii_payload`（`crates/qx-adapter/src/binance.rs:1916-2000`），
   配对的期望摘要 `crates/qx-adapter/src/binance.rs:1935` 是 Binance 官方文档公开的已知Answer 向量。**判定：不是泄露**，
   但形状与真 key 不可区分，正是让密钥扫描器天天误报的那种。修法见下。
2. **`Debug` 把 `api_key` 原样打出来**：`crates/qx-adapter/src/binance.rs:113-121` 的
   `impl Debug for BinanceSpotCredentials` 只把 `secret` 换成 `[REDACTED]`，
   `api_key` 走 `.field("api_key", &self.api_key)`（`crates/qx-adapter/src/binance.rs:117`）。`secret` 另有 `Drop` 清零
   （`crates/qx-adapter/src/binance.rs:123-127`、`crates/qx-adapter/src/binance.rs:93`），`api_key` 都不做。当前**没有活的打印路径**：
   `BinanceSpotAuth`（`crates/qx-adapter/src/binance.rs:33-39`）不 derive `Debug`，`HttpRequest` 虽然 derive 了
   （`crates/qx-adapter/src/lib.rs:37-45`）且装着真实头（`crates/qx-adapter/src/binance.rs:199`），
   但非测试代码里搜 `{:?}` 打 `HttpRequest`/`auth`/`credentials` → 0 命中。
   这是"只差一次 `eprintln!("{req:?}")`"的形状。

**永久修法**：① `BinanceSpotCredentials` 的 `Debug` 对 `api_key` 也做 `[REDACTED]`
（与 `secret` 同一条判据，不要两套标准），并给 `api_key` 也上 `Drop` 清零；
② `HttpRequest` **不 derive `Debug`**，改为手写只输出 method/path/header 名不输出值的实现——
一个派生出来的 `Debug` 是这类事故的唯一入口；③ 测试向量换成运行时构造
（`"QX_TEST_"` 前缀的显式假值 + 注释指向官方文档），使仓库里不再存在"像真 key 的 64 字符字面量"。

**回归用例（待补）**：`credentials_debug_never_prints_key_material`（对 `api_key` 与 `secret`
同一条断言，形状上是 `!format!("{c:?}").contains(<真值>)`）、
`http_request_debug_omits_header_values`。

---

### 4.8 缺失认证 — **存在，最严重的一格**｜Critical（非回环绑定）/ High（回环）

事实链，逐环实测：

1. 明文传输时 `operators` **必须为空**（`crates/qx-runtime/src/runtime_config/topology_validation.rs:43-45`）。
2. `operators` 为空 → 服务不带策略（`crates/qx-cli/src/api_service.rs:57-58`：
   `if config.api.operators.is_empty() { ApiService::new(state) } else { with_policy(...) }`）。
3. 策略为 `None` → 鉴权门整个跳过（`crates/qx-api/src/lib.rs:1438-1439`：
   `if self.policy.is_some() && !matches!(route, "/health" | "/ready" | "/schema/account-snapshot-v1")`）。
4. 明文的 accept 链把身份**硬写成 `None`**（`crates/qx-api/src/lib.rs:1882-1884` `self.spawn_connection(stream, ts, None)`）。
5. 于是每个端点，包括 `POST /control/commands` 走的 `submit_command`（`crates/qx-api/src/lib.rs:1768`），无凭据可达。

`is_production()` 会在 `environment == "production"` 时禁掉明文
（`crates/qx-runtime/src/runtime_config/topology_validation.rs:40-42`），环境词表也是闭合的（`crates/qx-runtime/src/runtime_config/schema.rs:13`，
`crates/qx-runtime/src/runtime_config/validate.rs:48-52`，词表外当场拒，见 `crates/qx-runtime/src/runtime_config/schema_tests.rs:423`
`runtime_environment_must_come_from_the_closed_vocabulary`）。
**但 `paper` / `sandbox` / `testnet` 三种环境允许明文**，而 `testnet` 连的是真交易所、用的是真 key，
`paper` 跑的是一套完整账本。生产示例配置绑的是 `0.0.0.0:8443`
（`deploy/qianxing.runtime.production.example.json:7`，配 `transport: "mtls"`），
其余示例绑 `127.0.0.1:8080`。`live_check` 只在**绑回环时**给一条 warn
（`crates/qx-cli/src/live_check.rs:261-264`），全文没有一处提到 `transport` 或 `operators`：
"明文 + 非回环"这一组合**没有任何检查会说话**。

**利用场景（不需要任何凭据）**：攻击者能路由到 `api.bind` 即可——

* `GET /account/snapshot` 的 `snapshot_for_query`（`crates/qx-api/src/lib.rs:1363`）、`/account/positions`、`/account/orders`、`/account/balances`、
  `/control/audit`：持仓、委托、现金、权益、审计流水与 `operator_id` 全读走。
  这些字段的完整集合见 `crates/qx-protocol/src/lib.rs:96-119` 与 `crates/qx-protocol/src/wire.rs:12-79`。
* `GET /metrics`（`crates/qx-api/src/lib.rs:1554`）连策略与鉴权拒绝计数都不用认证就能读。
* 写面见 §4.12（自授权）。

`CVSS:4.1/AV:N/AC:L/AT:N/PR:N/UI:N/VC:L/VI:L/VA:L/SC:H/SI:H/SA:H` → **Critical**
（绑非回环时）；同一向量取 `AT:P`（仅回环可达）→ **High**。

**永久修法**（四条都要做，缺一条就还是靠运维不出错）：

1. **fail closed 而不是 fail open**：`policy == None` 时只放行 `/health`、`/ready`、
   `/schema/account-snapshot-v1`，其余一律 403。今天 `None` 意味着"什么都不检查"，
   一个安全默认值不该由"是否配了 Operator 表"决定。
2. **明文传输不接受写路由**：把 §4.2 的 transport 一路传到 `ApiService`，
   明文下 `POST /control/commands` 直接 501/403。读面仍可由运维显式放开（单机看盘是真实用法），
   但默认关。
3. **非回环 + 明文 = 配置错误**：在 `crates/qx-runtime/src/runtime_config/topology_validation.rs` 里加这一格，
   让 `config validate` / `doctor` 与执行平面用同一颗判据（本项目已多次收"校验比执行宽"的口子，
   见 V11 §42 N5 与 `crates/qx-cli/src/runtime_check.rs:281-288`）。
4. `crates/qx-api/src/lib.rs:844` 那句注释（"None 表示仅供已受信的进程内调用；
   网络/生产入口应使用 `with_policy`"）目前是**愿望**。修法是把 `ApiService` 的类型拆开：
   网络上能拿到的那个构造函数**强制**要求策略，让"忘了加"编译不过。

**回归用例（待补）**：`plaintext_transport_refuses_write_routes`、
`policyless_service_denies_every_non_public_route`、
`non_loopback_plaintext_fails_topology_validation`、`live_check_warns_on_public_plaintext_bind`。

---

### 4.9 不安全文件上传 — 不存在｜None

服务端没有任何写文件的请求路径：`parse_http_request`（`crates/qx-api/src/lib.rs:2391-2399`）之后
body 只被 `serde_json::from_str` 解析成 `ControlCommand`（`crates/qx-api/src/lib.rs:1774`），解析失败回 400。
全仓搜 `multipart`、`form-data` → 0 命中；没有 `filename` 来自请求的处理
（`crates/qx-protocol/src/lib.rs:702` 那个 `filename` 是从 `u64` 拼出来的，见 §4.1）。
数据入库走的是 CLI 的 `dataset-ingest`，输入是**本机路径**，不是网络请求。

**待补 G-3**：一条常驻门禁，判据是"`qx-api` 里除 `ControlCommand` 之外没有 `serde_json::from_str`
的新调用点"——把"请求体只能变成命令，不能变成文件"这条不变式钉住。

---

### 4.10 弱密码学 — **存在，且是本仓最系统的一格**｜High

先说清一件事：**`Fnv1a` 出现在指纹用途上是对的**，
`crates/qx-core/src/sourcing.rs:13` 甚至写明"不用 `DefaultHasher`——它的输出不保证跨版本稳定"。
缺陷不是"用了弱哈希"，而是**同一颗无键 64 位哈希被放在需要防伪的位置上**，共 5 处：

| # | 位置 | 摘要承担的职责 | 为什么它其实是防伪控制 |
|---|---|---|---|
| C1 | `crates/qx-control/src/lib.rs:102-116` | `command_digest` 决定"这条命令是不是那条命令" | `crates/qx-control/src/lib.rs:495-498`、`crates/qx-control/src/lib.rs:528` 拿它做等值门；`crates/qx-control/src/lib.rs:325` 自陈是"幂等分支唯一还能核对 `command_digest` 的地方" |
| C2 | `crates/qx-storage/src/lib.rs:1448-1463` | 审计链逐环摘要 | `crates/qx-storage/src/lib.rs:1421-1425` 声称能发现"文件被截断、重排或篡改" |
| C3 | `crates/qx-protocol/src/lib.rs:299-353` | 快照完整性 | 同时是 SQL 查询键（`crates/qx-storage/src/sqlite.rs:975-977`）与对外 cursor（`crates/qx-api/src/lib.rs:1429`） |
| C4 | `crates/qx-plugin/src/lib.rs:46-68` | manifest 完整性与"签名" | 见下 |
| C5 | `crates/qx-factor/src/lib.rs:71-76` | 因子工件绑定 | `artifact_digest` 是裸 `u64`（`crates/qx-factor/src/lib.rs:34`） |

**C4 有两层，第二层更糟**：

* `signature: "fnv1a:<hex>"` 会**当作签名通过**（`crates/qx-plugin/src/lib.rs:51-57`），
  而那个 `<hex>` 就是 manifest 自己的 `canonical_hash()` ——任何作者都能算出来，认证能力为 0。
* Ed25519 分支（`crates/qx-plugin/src/lib.rs:127-134`）的**公钥取自被认证的那份 manifest 自己**：
  `decode_hex(parts.next())` 之后直接 `UnparsedPublicKey::new(&ED25519, public_key)`。
  签名辅助函数 `crates/qx-plugin/src/lib.rs:198-134` 又把公钥写回同一个字段。**每个作者都是自己证书的签发方**。
  而且签的不是 manifest 字节，是那颗 Fnv 摘要（`crates/qx-plugin/src/lib.rs:223`
  `format!("qx-plugin-manifest-v1:{:016x}", manifest.canonical_hash())`）。
  搜"可信根"（`trusted_key`/`pinned`/`TRUSTED`）在 `qx-plugin` → 0 命中。
* `signature`（`crates/qx-plugin/src/lib.rs:61`）是 `Option<String>`，不填即整段跳过 `if let Some(signature)`（`crates/qx-plugin/src/lib.rs:118`）。

这与 README/V11 已登记的口径一致：Q4 轮已经记过"插件 Ed25519 签名走不到"，
本文补的是**它为什么是安全问题**，而不是重新裁定它存在。

**C ABI 的默认值是另一格**：`DynamicCAbiLoadPolicy::new()` 把
`ed25519_public_key` / `ed25519_signature` 都设成 `None`（`crates/qx-strategy/src/c_api.rs:659-660`），
于是 `crates/qx-strategy/src/c_api.rs:857-867` 的验证**整段不执行**，只剩 SHA-256。字段自己的注释也承认这点
（`crates/qx-strategy/src/c_api.rs:648-649`："public key 必须来自部署期信任根；这个字段本身不提供密钥轮换或沙箱"）。
生产 C ABI 要求 Ed25519 key 的那条校验在
`crates/qx-runtime/src/runtime_config/strategy_validation.rs:280-273`，是配置层的、可选路径的。

**C3 另有一颗具体的洞**：`crates/qx-protocol/src/lib.rs:221-220`

```rust
if self.header.state_hash != 0 && self.header.state_hash != self.state_hash() {
    return Err(ProtocolError::StateHashMismatch);
}
```

`!= 0` 这一项意味着**声明 `state_hash: 0` 的快照直接免检**，而 `0` 是一个合法序列化值
（构造函数 `crates/qx-protocol/src/lib.rs:179` 就写 `state_hash: 0`）。

**C2 的强度也要说准**：链是**未加键**的，能改写存储的人可以整条重算；
头锚点取自同一个后端的控制面状态（`crates/qx-storage/src/lib.rs:1503-1506`），没有带外锚。
链校验三个后端都有（`crates/qx-storage/src/file/audit.rs:60`、`crates/qx-storage/src/sqlite.rs:1421`、
`crates/qx-storage/src/postgres.rs:1371`），问题不在"某个后端不扫"，而在它**只长在
`read_entries()` 这一条读路上**：生产里走这条路的只有 doctor 的两支
（`crates/qx-cli/src/doctor_report.rs:350`、`crates/qx-cli/src/doctor_report.rs:436`，分别是 file 与 sqlite，**没有 postgres 支**），
而 `/control/audit` 念的是内存控制面的 `plane.audit()`（`crates/qx-api/src/lib.rs:1548`、`crates/qx-api/src/lib.rs:2123`），
全程不碰链。此外它把 `format!("{:?}", ...)` 混进摘要
（`crates/qx-storage/src/lib.rs:1460`），与 `crates/qx-core/src/event.rs:359`"稳定摘要不得依赖 Debug 输出"相冲——
这条是本项目自己的口径，不是外部标准。

**真密码学用在对的地方的部分**（要记账，别只记坏消息）：
出站签名用 `hmac::HMAC_SHA256`（`crates/qx-adapter/src/binance.rs:156`），签名那颗 `sign_parameters`（同文件 :167）委托到 `sign_encoded_payload`（同文件 :171），后者调 `hmac::sign`（同文件 :172），`use ring::hmac` 在同文件 :20。原先登记的具名签名器 `HmacSha256Signer` 已随 `RestVenue`/`RestProvider`/`RequestSigner` 那套无装配读者的脚手架一并删除（`crates/qx-adapter/src/lib.rs:7` 的模块注释记着这一格）；
TLS 客户端 `webpki_roots` + 主机名校验，且**全仓搜不到任何关闭证书校验的开关**
（`danger_`/`insecure`/`skip_verify`/`UnknownVerifier`/`trust_cert` → 0 命中，
`crates/qx-adapter/src/lib.rs:221-222` 明确写"不提供跳过证书校验的开关"）；
API 服务端 `WebPkiClientVerifier`（`crates/qx-api/src/lib.rs:176-159`）且拒空根（`crates/qx-api/src/lib.rs:197-199`）。
**Postgres 那一跳是唯一薄点**：`crates/qx-storage/src/postgres.rs:173-176` 用 native-tls 默认构建器
（验证是开的），但 `sslmode` 完全交给运维的 DSN（`crates/qx-storage/src/postgres.rs:152` 只是文档），
仓内**没有任何** `sslmode` 缺省注入或配置字段——一条不带 `sslmode` 的 DSN 不会被本仓拦下。

`CVSS:4.1/AV:N/AC:L/AT:P/PR:N/UI:N/VC:N/VI:H/VA:L/SC:H/SI:H/SA:N` → **High**
（C1 被攻击者用到时的下游是"同一意图重复下单"）。

**永久修法**——一句话：**把"确定性指纹"和"防伪标签"在类型上分开**，全仓一套。

1. 新增 `IntegrityTag`（`ring::hmac` 的 `HMAC_SHA256`，密钥来自 §6 的密钥槽），
   接管 C1（`command_digest`）、C3（`state_hash`）、C5（`artifact_digest`）三格；
   `Fingerprint`（继续用 `Fnv1a`）只留给重放确定性与排序。
   判据只有一个出口：类型名不允许互换，编译器替人盯。
2. 审计链 C2 改为**加键**（同一把 `IntegrityTag` 密钥），并把检查点锚到带外存储
   （或至少一份只读导出），否则"防篡改"这个词该从 `crates/qx-storage/src/lib.rs:1421-1425` 的注释里删掉。
   同时把 `format!("{:?}")` 换成显式稳定的 `as_str()` 映射（V11 R5-2d 已经在别处这么收了）。
3. `state_hash != 0`：`validate()` 直接拒 0（写侧构造时就要求非 0），不留"0 表示未算"的第三态——
   本项目对"没算出来"和"算出来是 0"已经有过一轮裁定（V11 R10），这里同一口径。
4. 插件：可信根由**部署方**给（配置字段指向一个只含公钥的目录，或环境变量指向密钥），
   签名消息改成 manifest 的规范**字节**而不是那颗摘要；`fnv1a:` 前缀直接判非法签名类型
   （现在它进的是"通过"分支）。
5. C ABI 的默认策略反转：**没有密钥就拒绝加载**，显式 `insecure_skip_signature` 才能过
   （并且该字段在生产环境被 `topology_validation` 拒），这样"忘了配"落到安全的一侧。
6. Postgres：DSN 缺 `sslmode` 时注入 `sslmode=require` 作为**下限**（与
   `dsn_with_connect_timeout`（`crates/qx-storage/src/postgres.rs:90-106`）同一颗写法，只加一个关键字），
   并新增 `storage.postgres_sslmode` 让"更严格"可声明。

**回归用例（待补）**：`integrity_tag_and_fingerprint_are_not_interchangeable_types`、
`plugin_manifest_refuses_fnv1a_signature_prefix`、`plugin_signature_requires_pinned_root`、
`snapshot_refuses_zero_state_hash`、`command_digest_collision_no_longer_binds_identity`、
`postgres_dsn_gets_an_sslmode_floor`。

---

### 4.11 不安全 HTTP 响应头 — 存在（缺失为主）｜Medium

响应头的全部内容由一颗写出函数决定（`crates/qx-api/src/lib.rs:2291-2309`）：

```
HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n
```

实测**只有 3 个头**。缺失清单（每项都是全仓 0 命中的搜索结果）：
`Content-Security-Policy`、`X-Content-Type-Options`、`X-Frame-Options`、
`Strict-Transport-Security`、`Referrer-Policy`、`Access-Control-*`（含
`Access-Control-Allow-Origin`）。没有任何 `OPTIONS` 处理，`("OPTIONS", _)` 落到
`crates/qx-api/src/lib.rs:1605` 的 404。TLS 路径与明文路径共用同一颗写出函数，所以 **HSTS 在 mTLS 下也发不出来**。

**这里最要紧的一条不是"少几个头"，而是浏览器侧写可达**：服务端不校验 `Content-Type`，
因此一个恶意页面可以用 `text/plain` 的表单 POST 打到 `http://127.0.0.1:8080/control/commands`
——`text/plain` 属"简单请求"，**不触发预检**；配合 DNS rebinding 可穿透到内网实例。
没有 cookie/会话认证使 CSRF 的经典形状变弱（攻击者不需要借受害者的身份），
但"从受害者浏览器出发打本机明文端口"仍然是可达路径，而且§4.8 已经说明那条路上没有鉴权。

`CVSS:4.1/AV:N/AC:L/AT:P/PR:N/UI:R/VC:L/VI:H/VA:N/SC:L/SI:H/SA:N` → **Medium**。

**永久修法**：① `POST /control/commands` 要求 `Content-Type` 恰好等于 `application/json`，
否则 415——这一条把浏览器 CSRF 变成需要预检，而"没有 OPTIONS 处理"就自动把它挡住；
② 统一在写出函数里加 `X-Content-Type-Options: nosniff` 与 `Cache-Control: no-store`
（账户数据不该被任何中间缓存留下），mTLS 路径额外加
`Strict-Transport-Security`；③ 显式不实现 CORS，并在文档里写明
"跨源浏览器访问不受支持"，这样将来加 CORS 的人会看见这是一条决定而不是一个疏忽。

**回归用例（待补）**：`write_route_refuses_non_json_content_type`（今天会红，因为不拦）、
`responses_carry_nosniff_and_no_store`、`mtls_responses_carry_hsts`。

---

### 4.12 权限提升 — **存在（自授权 + 可选校验 + 本机共享内存可劫持）**｜Critical / High

三格独立的提升，形状不同，都实测过。

**(a) 客户端给自己授权**（`crates/qx-api/src/lib.rs:1697-1727`）：

```rust
let granted = match &self.policy {
    Some(policy) => { ...; command.operator_id = operator_id.to_string(); ... policy.permission(operator_id) }
    None => command.permission,
};
```

`granted` 直接来自请求体字段 `ControlCommand.permission`
（`crates/qx-control/src/lib.rs:69`）。`Permission` 是可反序列化的三级枚举
（`crates/qx-control/src/lib.rs:20-22` `Research` / `Trading` / `Admin`），于是无策略时客户端把自己升到 `Admin`。
`Admin` 的等级序在 `crates/qx-control/src/lib.rs:575-583`。该结构体自己的注释已经写明
`permission` "是审计字段，不是认证凭据；生产入口必须把外部身份解析成 `granted` 之后调用本方法"
（`crates/qx-control/src/lib.rs:90-91`）——**§4.8 描述的正是这条注释没被强制执行的后果**。

拿到 `Admin` 之后今天能做什么，也说准：`ChangeRiskLimit` 与 `SwitchVenue` 会因
`has_executor()` 当场被拒（`crates/qx-control/src/lib.rs:281-286`，V11 P1 的收口），
所以**能执行的是三种**：`SubmitOrder`（`crates/qx-cli/src/venue_runtime/binance_submit.rs:279-337`）
经 `crates/qx-adapter/src/binance.rs:1556` 打真 `POST /api/v3/order`，请求头带真 key
（`crates/qx-adapter/src/binance.rs:199`）；`PauseStrategy` 与 `ResumeStrategy` 的领取分支在
`crates/qx-cli/src/workers.rs:228-231`、`crates/qx-cli/src/workers.rs:232-235`（循环体 `crates/qx-cli/src/workers.rs:172-584`）。第三格尤其值钱：`command.target`
完全由请求体给，且 `"*"` 是**被接受的通配**（补入队列看 `crates/qx-cli/src/workers.rs:186`，领取看 `crates/qx-cli/src/workers.rs:200`），
一条请求停掉全部策略。约束还剩两道：订单的 `account_id`/venue 必须与执行 worker 拓扑一致
（`crates/qx-cli/src/venue_runtime/binance_submit.rs:54-58`），缺 `instrument_spec_path` 时 fail-closed 拒绝提交
（`crates/qx-cli/src/venue_runtime/worker_runtime.rs:435-438`）。API key 不可由攻击者指定。

**(b) 产物校验是可选的、且顺序有时在执行之后**：
`verify_strategy_artifact`（`crates/qx-cli/src/path_resolution.rs:185-211`）在
`strategy_artifact_sha256` 缺失时 `return Ok(())`（`crates/qx-cli/src/path_resolution.rs:186-188`）；`external_executable`
这条确实在 spawn 前校验（`crates/qx-cli/src/workers.rs:104` 早于 `crates/qx-cli/src/workers.rs:145`，用例
`crates/qx-cli/src/tests/paper_bridge_and_bundles.rs:364` 钉着）；但可 import 的 `python_module` 走 `crates/qx-cli/src/path_resolution.rs:193-208` 的
提前返回，交由 Python 侧验，而 `python/qianxing_strategy/worker.py:36-39` 是
**先 `exec_module` / `import_module`，后校验指纹**——代码已经跑过了。
`consumer_handler_executable` 完全没有这一格。

**(c) 本机策略共享内存可被劫持**（`crates/qx-cli/src/strategy_host.rs:127-133` +
`crates/qx-strategy/src/ring.rs:120-129`）：环文件放在
`std::env::temp_dir()`，名字是 `qianxing-strategy-ring-<pid>-<递增计数>` 加
`.input` / `.output`——**可预测**；打开方式是
`.create(true).truncate(true)`，**不是 `create_new`**，也没有权限位设置
（全仓搜 `PermissionsExt` / `.mode(` / `set_permissions` / Windows ACL API → 0 命中）。
路径经 argv 交给子进程（`crates/qx-cli/src/strategy_host.rs:148-151`）。
在多用户 Linux 主机上，一个同 temp 命名空间的本机进程可以预建同名文件（含符号链接）
→ 启动期一次任意路径覆写；或者在中途改写环里的**决策帧** → 篡改进入主机的策略意图。

`CVSS:4.1`（a）与 §4.8 同向量，**Critical / High**；（b）
`AV:L/AC:L/AT:P/PR:L/UI:N/VC:N/VI:H/VA:N/SC:H/SI:H/SA:N` → **High**；
（c）`AV:L/AC:L/AT:P/PR:L/UI:N/VC:L/VI:H/VA:L/SC:N/SI:H/SA:N` → **High**（单机多用户）。

**永久修法**：① `permission` **从线上类型里删掉**——它是服务端的授权结论，不是客户端的输入；
`granted` 只能由身份解析出来（mTLS 证书映射），无身份即无 `granted`。这与 §4.8 修法 1 是同一颗改动。
② `target: "*"` 通配要么删掉，要么要求 `Admin` 且**显式声明** `allow_wildcard_target`；
今天它是任何自授权者都拿得到的开关。③ 产物校验反转默认：**没有 `strategy_artifact_sha256`
就不许启动**（生产环境），并把 Python 侧的顺序改成
`find_spec` → 解析出真实文件 → 先哈希 → 再 `exec_module`；
`consumer_handler_executable` 复用同一颗校验。④ 环文件：放进 `data_dir` 下的私有子目录
（不放公共 temp）、`create_new` 独占创建、已存在即失败、unix 上 `0o600`，
名字带随机量而不是递增计数；`truncate(true)` 从跨进程 IPC 的创建路径上消失。

**回归用例（待补）**：`control_command_carries_no_permission_field`、
`wildcard_target_requires_explicit_operator_flag`、
`python_module_hash_precedes_exec`、`strategy_ring_uses_exclusive_creation_outside_tmp`。

---

### 4.13 活性与资源上限（不在 12 类里，但决定它们能不能被用起来）

**最要紧的一颗：限流器在生产接线下一辈子不会补水。**
`ApiRateLimiter::try_acquire` 按**秒**解释 `now`
（`crates/qx-api/src/lib.rs:624-625`，注释自陈"`now` 使用 API 调用方约定的秒级单调时间"），
而唯一的网络调用点传的是**毫秒快照**：`crates/qx-cli/src/strategy_contract.rs:759`
`service.serve(listener, runtime_timestamp_ms(), ...)`，`runtime_timestamp_ms()`
在 `crates/qx-cli/src/runtime_wiring.rs:349-352` 返回 `as_millis()`。
更关键的是这个 `ts` 是**进程启动时取一次**，然后原样递给每一条连接
（`serve` → `spawn_connection(stream, ts, None)`，`crates/qx-api/src/lib.rs:1884-1886`；mTLS 路径同理 `crates/qx-api/src/lib.rs:1820-1835`）。
于是第一条请求之后 `elapsed` 恒为 0 → `refill` 恒为 0 → 令牌只减不加。

两个结论，一个是可用性、一个是控制缺失：

* 任何客户端（含 §4.8 的匿名攻击者）发满 **100 条请求**（`ApiRateLimiter::new(DEFAULT_RATE_LIMIT_CAPACITY, DEFAULT_RATE_LIMIT_REFILL_PER_SECOND)`，两颗常量实测都是 100，
  `crates/qx-api/src/lib.rs:1028-1031`、`crates/qx-api/src/lib.rs:1034-1037`）之后，该进程**所有**路由永久 429，包括 `/health` 与 `/ready`；
  只有重启（新的时间戳 + `SqliteTokenBucket` 里持久化的 `last_ts` 被更新）才恢复。
* 因此"我们有 API 限流"这句话在网络路径上不成立：它既不能按秒补水，也是**全局单桶**
  （`crates/qx-api/src/lib.rs:846` 一个 `Arc<dyn ApiRateLimitBackend>`，桶名是常量 `"api"`，
  `crates/qx-cli/src/api_service.rs:127`），不按对端或身份分。
* `with_rate_limit`（`crates/qx-api/src/lib.rs:1445`，前一行 :1444 是 `#[cfg(test)]`）整颗只在测试构建里存在，全仓唯一调用点是 `crates/qx-api/src/tests.rs:531`，所以容量不是运维可调的。
* 现有唯一相关用例是 `crates/qx-api/src/tests.rs:483-488`，它用 `with_rate_limit(1, 0)`
  （补水 0/秒）与手挑的 `ts` 值 1、2——**这个形状看不见单位不一致，也看不见常量 ts**。
* WS 完全绕过：`serve_websocket`（`crates/qx-api/src/lib.rs:2026`，upgrade 判定 :1897）从 `dispatch_request` 直接返回，
  不进 `handle_inner`，于是一条 WS 长连接既不计次也不进桶，
  然后在 `crates/qx-api/src/lib.rs:2012` 的 `wait_after` 循环里无限往外写。
* 一条连接一个 OS 线程且无上限（`crates/qx-api/src/lib.rs:1848` `std::thread::spawn`），没有并发连接天花板。

`CVSS:4.1/AV:N/AC:L/AT:N/PR:N/UI:N/VC:N/VI:N/VA:H/SC:N/SI:N/SA:L` → **High**。
（这一颗是"安全控制自己造出的可用性事故"，比"没有控制"更糟，因为它对健康检查也生效。）

**其余上限实测表**：

| 项 | 值 | 位置 |
|---|---|---|
| 入站 HTTP 请求（头+体） | 1 MiB | `crates/qx-api/src/lib.rs:2279`，在 `crates/qx-api/src/lib.rs:2340-2345` 与 `crates/qx-api/src/lib.rs:2382-2387` 两处判 |
| 完整请求整体截止 | 5 s | `crates/qx-api/src/lib.rs:2276`，每轮检查 `crates/qx-api/src/lib.rs:2326-2334` |
| 单帧 / 分片累计 WS（入站 API） | 无长度解析 | `crates/qx-api/src/lib.rs:2090` 只给 2048 字节缓冲，帧长不判（与下一行不同） |
| 出站 WS 单帧 + 分片累计 | 16 MiB | `crates/qx-adapter/src/lib.rs:453`，帧头 `crates/qx-adapter/src/lib.rs:344-506`、累计 `crates/qx-adapter/src/lib.rs:551-395` |
| 出站 **HTTP 响应体** | **无上限** | `crates/qx-adapter/src/lib.rs:275-276` `read_to_end`（TLS 路径），`crates/qx-adapter/src/lib.rs:672-673` 同形状 |
| 插件 intents | 100 000 | `crates/qx-strategy/src/c_api.rs:22`：先判 `intents_len`（`crates/qx-strategy/src/c_api.rs:442`），之后才 `from_raw_parts`（`crates/qx-strategy/src/c_api.rs:450`） |
| 插件库体积 / 分帧 | 256 MiB / 16 MiB | `crates/qx-strategy/src/c_api.rs:19`、`crates/qx-strategy/src/frame.rs:13` |
| Binance 权重桶 / recvWindow | 6000 / 5000（不校验取值范围，只有这一处写死的初值） | `crates/qx-adapter/src/binance.rs:919`、`crates/qx-adapter/src/binance.rs:158`、`crates/qx-adapter/src/binance.rs:204` |
| 行情流重连 | 连续 10 次，1 s→30 s | `crates/qx-cli/src/venue_runtime/binance_stream_worker.rs:26`、`crates/qx-cli/src/venue_runtime/binance_stream_worker.rs:103` |
| Outbox 投递预算 | 8 次后停投 | `crates/qx-storage/src/lib.rs:482-486` |
| 审计窗口 | 1000 条 | `crates/qx-control/src/lib.rs:199` |

**两处"远端配合才终止"的循环**要记名，别当成有界：`crates/qx-adapter/src/binance.rs:249-250` 那颗重连预算的复位判据
在注释里自陈为"交付过事件的会话证明链路可用：退避预算重新计"（实现 `crates/qx-adapter/src/binance.rs:501-506`，回调失败不复位的理由在 :503-504），
所以抖动的会话在墙上时间里可以无限重连；`crates/qx-adapter/src/binance.rs:1223` 的翻页 `loop`
靠 `crates/qx-adapter/src/binance.rs:1179-1243` 的"不足 1000 就 break"与 `crates/qx-adapter/src/binance.rs:1185-1251` 的游标不前进判据退出，
页大小 1000（`crates/qx-adapter/src/binance.rs:1163`）——有界，但界是远端给的。

**永久修法**：① **限流器自己读单调时钟**（`std::time::Instant`），把 `now` 这个参数从
`try_acquire` 的签名上删掉——单位错和常量快照都是"调用方给时间"这一个设计决定的后果，
拿掉决定就同时拿掉两类 bug；② 网络入口的 `serve` 不再接受外部 `ts`；
③ WS 握手与循环计入同一颗桶，并给帧长加与出站侧同款的 16 MiB 判据（复用 `qx-adapter` 那颗常量，
不写第二处）；④ 给连接数一个天花板（每对端 + 全局），超了直接关，不派线程；
⑤ 出站响应体加常量上限，与 `MAX_WEBSOCKET_MESSAGE_BYTES` 同族；
⑥ 现有用例保留，另加一条**按生产形状接线**的用例（走 `serve` 的时间源，而不是手挑 `ts`）。

**回归用例（待补）**：`rate_limiter_refills_under_the_production_clock_wiring`（当前必红，
是这颗修复的牙齿）、`rate_limiter_has_no_injected_clock`（签名判据）、
`websocket_connections_bypass_no_rate_limit`、`outbound_http_response_body_is_capped`。

## 5. 缺陷登记（本轮实测，全部**未修**）

| ID | 摘要 | 分级 | 承重位置 |
|---|---|---|---|
| S-R1 | 明文传输下全部路由无鉴权，且 `policy == None` 即"什么都不检查" | Critical / High | `crates/qx-api/src/lib.rs:1438-1444`、`crates/qx-api/src/lib.rs:1884-1886`、`crates/qx-cli/src/api_service.rs:60-63` |
| S-R2 | 限流器常量毫秒快照 → 100 条后永久 429（含健康检查）；WS 完全绕过 | High | `crates/qx-api/src/lib.rs:624-625`、`crates/qx-cli/src/strategy_contract.rs:759` |
| S-R3 | `permission` 由请求体自授；`target: "*"` 停全部策略 | Critical（随 S-R1） | `crates/qx-api/src/lib.rs:1693`、`crates/qx-cli/src/workers.rs:186`、`crates/qx-cli/src/workers.rs:200` |
| S-R4 | `Fnv1a` 在 5 处承担防伪；`state_hash: 0` 免检；`fnv1a:` 被当签名 | High | `crates/qx-control/src/lib.rs:102-116`、`crates/qx-protocol/src/lib.rs:221-220`、`crates/qx-plugin/src/lib.rs:118-134` |
| S-R5 | 插件/C ABI 无信任根；C ABI 默认不验签；Python 模块"先执行后校验" | High | `crates/qx-plugin/src/lib.rs:127-134`、`crates/qx-strategy/src/c_api.rs:658-660`、`python/qianxing_strategy/worker.py:36-39` |
| S-R6 | 策略共享内存环：公共 temp + 可预测名 + 非独占创建 + 无权限位 | High | `crates/qx-cli/src/strategy_host.rs:127-133`、`crates/qx-strategy/src/ring.rs:120-129` |
| S-R7 | 配置可把 Binance 凭据发到任意 host:port；`ws://` 静默 TLS | Medium | `crates/qx-cli/src/venue_runtime/binance_venue.rs:42-70`、`crates/qx-adapter/src/binance.rs:199`、`crates/qx-adapter/src/binance.rs:220`、`crates/qx-adapter/src/binance.rs:228` |
| S-R8 | `qx-control`/`qx-protocol`/`qx-storage` 三个线上面 0 处 `deny_unknown_fields`；`ControlCommand` 无版本锚；`OutboxEvent` 有版本字段但全仓无人比较 | Medium | `crates/qx-control/src/lib.rs:60-71`、`crates/qx-storage/src/lib.rs:368-374`、`crates/qx-storage/src/lib.rs:1024` |
| S-R9 | 无安全响应头；写路由不校验 `Content-Type`（浏览器 simple POST 可达） | Medium | `crates/qx-api/src/lib.rs:2281-2309` |
| S-R10 | 出站 HTTP 响应体无上限 | Medium | `crates/qx-adapter/src/lib.rs:273-277` |
| S-R11 | 连接数无天花板（一线程一连接）；`Debug` 打印 `api_key` | Low | `crates/qx-api/src/lib.rs:1848`、`crates/qx-adapter/src/binance.rs:114` |
| S-R12 | Postgres DSN 无 `sslmode` 下限（仓内注释自己写明「生产 DSN 应显式设置」）；包含性检查不做 canonicalize | Low | `crates/qx-storage/src/postgres.rs:152`、`crates/qx-storage/src/postgres.rs:170`、`crates/qx-storage/src/file/state.rs:172` |

## 6. 凭据处置与轮换

本文档**不含任何凭据值**。以下是"泄了怎么办"和"怎么让它不泄"的两半。

**密钥槽（全部经环境或文件路径，配置 JSON 里只有名字/路径）**：
`credential_env`（`crates/qx-runtime/src/runtime_config/schema.rs:237`）下的 `api_key` :258 与 `secret` :259、
`credential_files`（`crates/qx-runtime/src/runtime_config/schema.rs:239`）下的 `api_key` :268 与 `secret` :269、
`storage.postgres_dsn_env`（`crates/qx-runtime/src/runtime_config/schema.rs:97`）、`api.tls.private_key`（路径，`crates/qx-runtime/src/runtime_config/schema.rs:39-45`）、
`api.operators.*.certificate`（路径，`crates/qx-runtime/src/runtime_config/schema.rs:47-52`）。

**处置流程（按小时计，不是按天）**：

1. **检测**：① CI 加一次密钥扫描（本仓当前唯一的"像 key 的东西"是 §4.7 那对测试向量，
   换掉它之后扫描器可以零例外）；② `config lock` 的输出（`crates/qx-cli/src/config_output.rs:91-99`）
   含环境变量**名**与证书**路径**，不是值，但它是一张地图——不要提交、不要贴进工单；
   ③ Binance 侧的异常 `recvWindow` 拒绝、未知 `newClientOrderId` 前缀（本项目是
   `qx-{id}`，见 V11 §3）是外部可见的滥用信号。
2. **止血**：把 `api.bind` 改回 `127.0.0.1`（或断开出口），并在 venue 侧把该 key 的
   提现/交易权限关掉。**先吊销，再调查**——S-R1 意味着拿到 TCP 可达就已经拿到下单能力。
3. **轮换**：venue 侧新建 key → 写入新 secret（环境变量指向的值，或 `credential_files`
   指向的文件）→ 重启 worker → 用 `qx live-check` / `config validate` 验配置指纹
   （`config_fingerprint`，`crates/qx-runtime/src/runtime_config/validate.rs:30-44`）→ 观察一次对账通过 → **再**吊销旧 key。
   两条链（Binance 直连 / CCXT）分别做，CCXT 那份的注入点是
   `crates/qx-adapter/src/ccxt.rs:117-130`（按**名字**从父环境取值，值只进子进程）。
4. **复核**：轮换后跑一次 `qx doctor`，确认审计链检查点仍连得上
   （`crates/qx-cli/src/doctor_report.rs:377`、`crates/qx-cli/src/doctor_report.rs:454`）。S-R4 修好之前，
   **不要把这条链当作防篡改证据**，它只防意外重排。
5. **窗口与登记**：`AUDIT_WINDOW_RECORDS = 1_000`（`crates/qx-control/src/lib.rs:199`）滚出之后
   同一 payload 会被当新命令接受（`crates/qx-control/src/lib.rs:217-219` 自陈），因此"事发时刻附近的幂等保护"
   不能靠控制面自己——这一格已按 V11 R6-5 登记为限制。

**权限位（当前缺口）**：`credential_files` 指向的文件、TLS 私钥、`config lock` 的产物、
`data_dir` 全树都取系统默认权限——因为**全仓没有任何一处**设置过文件模式（§4.12c 的搜索结果）。
永久修法是在"我们创建/落盘"的两处（`crates/qx-cli/src/config_output.rs:99`、TLS 私钥加载侧不做改动，
只对我们写的文件）上 `0o600`，并在 `live_check` 里加一条"`data_dir` 组/其他可读写 → warn"。

## 7. 安全相关配置与环境变量清单（实测）

**环境变量（代码读取的全部）**：`QX_PYTHON`（`crates/qx-cli/src/main.rs:284-286`，
缺省 `"python"`——**这是"选哪个解释器"，也就是一条代码执行链**）、`PYTHONPATH`
（`crates/qx-cli/src/strategy_host.rs:512`、`crates/qx-adapter/src/ccxt.rs:107`）、`PATH`（子进程白名单，
`crates/qx-cli/src/strategy_host.rs:467`、`crates/qx-cli/src/event_pipeline.rs:406-407`）、`QX_EVENT_CONSUMER`（子进程标记，
`crates/qx-cli/src/event_pipeline.rs:402`），以及**由配置命名的**凭据变量：
`credential_env` 里的名字（声明 `crates/qx-runtime/src/runtime_config/schema.rs:237`，按名读取 `crates/qx-adapter/src/binance.rs:58-70`）、CCXT 的
`api_key` / `secret` / `password` / `uid` 四类（`crates/qx-adapter/src/ccxt.rs:117-130`）、
`storage.postgres_dsn_env`（`crates/qx-cli/src/configured_backends.rs:280` → `std::env::var` 在 `crates/qx-cli/src/configured_backends.rs:285`）。
`deploy/` 出现的名字：`QX_BINANCE_API_KEY`、`QX_BINANCE_API_SECRET`、
`QX_BINANCE_TESTNET_API_KEY`、`QX_BINANCE_TESTNET_API_SECRET`、`QX_POSTGRES_DSN`。
**除 `QX_PYTHON` 之外，没有任何环境变量能改写绑定地址、传输方式或外联目的地。**

**配置字段**：`environment`（`crates/qx-runtime/src/runtime_config/schema.rs:276`，闭合词表在 `crates/qx-runtime/src/runtime_config/schema.rs:13`，
词表外拒；该判据只此一处，见 §4.4）、`profile`（`crates/qx-runtime/src/runtime_config/schema.rs:278`）、
`api.bind`（`crates/qx-runtime/src/runtime_config/schema.rs:57`）、`api.transport`（`crates/qx-runtime/src/runtime_config/schema.rs:58`）、
`api.tls`（`crates/qx-runtime/src/runtime_config/schema.rs:59`，三格 `certificate_chain` / `private_key` / `client_ca` 见 `crates/qx-runtime/src/runtime_config/schema.rs:42` / `crates/qx-runtime/src/runtime_config/schema.rs:43` / `crates/qx-runtime/src/runtime_config/schema.rs:44`）、
`api.operators`（`crates/qx-runtime/src/runtime_config/schema.rs:61`，两格 `permission` / `certificate` 见 `crates/qx-runtime/src/runtime_config/schema.rs:50` / `crates/qx-runtime/src/runtime_config/schema.rs:51`）、
`workers[].endpoint`（`crates/qx-runtime/src/runtime_config/schema.rs:230`，§4.5）、
`credential_env`（`crates/qx-runtime/src/runtime_config/schema.rs:237`）/ `credential_files`（`crates/qx-runtime/src/runtime_config/schema.rs:239`）/ `instrument_spec_path`（`crates/qx-runtime/src/runtime_config/schema.rs:243`）、
`max_order_notional_raw`（`crates/qx-runtime/src/runtime_config/schema.rs:249`）/ `max_position_notional_raw`（`crates/qx-runtime/src/runtime_config/schema.rs:252`）、
`storage.backend`（`crates/qx-runtime/src/runtime_config/schema.rs:90`）/ `data_dir`（`crates/qx-runtime/src/runtime_config/schema.rs:93`）/
`sqlite_path`（`crates/qx-runtime/src/runtime_config/schema.rs:94`）/ `postgres_dsn_env`（`crates/qx-runtime/src/runtime_config/schema.rs:97`）/ `postgres_pool_size`（`crates/qx-runtime/src/runtime_config/schema.rs:100`）、
`messaging.nats_url`（`crates/qx-runtime/src/runtime_config/schema.rs:151`）/ `subject_prefix`（`crates/qx-runtime/src/runtime_config/schema.rs:153`）、
`consumer_max_attempts`（`crates/qx-runtime/src/runtime_config/schema.rs:171`）、`consumer_handler_executable`（`crates/qx-runtime/src/runtime_config/schema.rs:175`）/
`consumer_handler_args`（`crates/qx-runtime/src/runtime_config/schema.rs:177`）、`config_fingerprint`（`crates/qx-runtime/src/runtime_config/schema.rs:282`）。
策略侧在另一份文件：`python_module`
（`crates/qx-runtime/src/runtime_config/strategy_schema.rs:269`）、`external_executable`（`crates/qx-runtime/src/runtime_config/strategy_schema.rs:283`）、
`strategy_artifact_sha256`（`crates/qx-runtime/src/runtime_config/strategy_schema.rs:287`）、`external_args`（`crates/qx-runtime/src/runtime_config/strategy_schema.rs:289`）、`external_env`（`crates/qx-runtime/src/runtime_config/strategy_schema.rs:291`）、
`c_abi_library`（`crates/qx-runtime/src/runtime_config/strategy_schema.rs:294`）。

**默认权限（值得单独读一遍，因为它决定 §4.8 有多容易命中）**：
`transport: plaintext` + `operators: {}` + `bind: 127.0.0.1:8080` 是**所有非生产示例**的形状。
前两项组合在这份模型里等价于"本机任意进程可用管理员身份下单"。
回环绑定不等于安全：同机上一个策略子进程、一个事件消费者处理器，
或一个浏览器页面（§4.11 的 simple POST）都能打这个口。

## 8. 与 README / 既有文档的一致性

* `README.md:163`"凭据只从环境变量读取，配置文件中不落任何密钥"—— **与代码一致**，
  但口径要补一句：`credential_files` 允许从**文件**读，配置里放的仍是路径而非值。
* `crates/qx-api/src/lib.rs:1-4` 的模块自陈（写操作必须进 `ControlPlane`，由上层执行器动作）——
  **成立**，本轮实测 `POST /control/commands` 唯一写出面是 `submit_command` → `submit_as`。
* `deploy/README.md:3`、`deploy/README.md:65`、`deploy/README.md:286`（不含 key/secret，不读取也不打印内容）——
  **成立**（§4.7 的两个例外都不是 `deploy/` 侧）。
* `crates/qx-strategy/src/c_api.rs:648-649` 与 `crates/qx-control/src/lib.rs:90-91` 两处注释描述的是**期望而非现状**；
  §4.10 / §4.12 分别给出把注释变成判据的修法。

## 9. 已核实不存在的攻击面（负行）

一份安全模型只列缺陷就是没做完了。以下每一条都是**本轮实测的零命中**：

1. **HTML / 模板输出**：0（§4.3）。
2. **文件上传 / multipart / 请求名文件**：0（§4.9）。
3. **拼接 SQL / 插值表名列名**：0（§4.4）。
4. **shell 解释器参与 spawn**：0（§4.2）。
5. **Python `pickle` / `eval` / `exec` / `yaml.load` / `subprocess`**：0（§4.6）。
6. **TLS 证书校验的可关闭开关**：0（§4.10）——两种传输都是真验证，且服务端拒空根。
7. **被跟踪的 `.env` / `.pem` / `.key` / 密钥字面量**：0（§4.7）；`deploy/data/` 71 个被跟踪文件里
   0 处凭据形状。
8. **HTTP 重定向跟随**：0（`crates/qx-adapter/src/lib.rs:678-852` 只解析状态与体，不碰 `Location`）
   → 因此不存在"重定向把凭据带走"这一格；§4.5 的问题更直接：目的地本身可配。
9. **代理配置（`HTTP_PROXY` 等）**：0 → 既没有代理注入面，也**没有出口管控点**（§4.5 的允许列表
   修法要在本仓自己实现）。
10. **`DefaultHasher` / `SipHash` / `md5` / `blake` / `openssl` / `aws-lc` 一等的使用**：0
    （§4.10；`DefaultHasher` 唯一一次出现是"别用它"的注释）。
11. **把密钥写进日志的路径**：0 处活的打印（§4.11 的 `HttpRequest: Debug` 是唯一"差一步"的形状）。
12. **`init` / `config lock` 写出凭据值**：0（`crates/qx-cli/src/init_project.rs:291-337`、
    `crates/qx-cli/src/config_output.rs:75-102` 只写名字与路径）。
13. **`CancelOrder` 从 HTTP 生效**：0（无执行者，受理处当场拒；`crates/qx-control/src/lib.rs:281-286`）。
14. **变更风控上限 / 切换 venue 从 HTTP 生效**：0（同上，`ChangeRiskLimit` / `SwitchVenue`
    只有契约形状）。
15. **`schema_version` 出现在 `qx-control` 整个 crate**：0（`grep -rc schema_version crates/qx-control/src/` 无命中）
    ——`ControlCommand` 不是「有字段没人比」，是连字段都没有（S-R8 的前半）。
16. **`deny_unknown_fields` 出现在 `qx-control` / `qx-protocol` / `qx-storage`**：0
    （同一把尺子在配置面量到 16 处 / 5 个文件，见 §4.6 表）——三个线上一律静默吃掉未知键。

## 10. 门禁契约（本文怎么不腐烂）

本项目的文档行号引用在 R6-7 那一轮第一次逐颗量过：同一批修前快照按三种判据形状回放，三份长文档的红数依次是 46 / 98 / 110 处（逐份明细在 V11 §54.9 的表）。换句话说"0 处红"只说明判据看见了多少，不说明看不见的那一半。因此本文的口径：

* **G-1 不出现 `text/html`**：`qx-api` 与 `qx-cli` 的响应 `Content-Type` 落在已知集合内。
* **G-2 不出现插值 SQL**：`crates/**/src/**.rs` 中，SQL 关键字与 `{}` 插值同现即红。
* **G-3 请求体只能变成命令**：`qx-api` 里 `serde_json::from_*` 的具体目标类型集合钉成一个常量表，
  加一项要显式改判据。
* **G-4 线上 DTO 都要版本锚**：`qx-control` / `qx-protocol` / `qx-storage` 的对外结构体
  带 `schema_version` 且读侧先比。
* **G-5 防伪位置不用无键哈希**：`Fnv1a` 的构造点白名单（`crates/qx-core/src/sourcing.rs`、`qx-guanxing` 的数据哈希等），
  出现在 C1/C3/C4/C5 任一格即红——这一条要等 §4.10 的修法落地才可能绿。
* **G-6 本文的每条 `path:line` 都算数**：本文由 `doc_citation_check`（`tools/check_architecture.py:9758`）逐颗核对，它与台账
  共用同一颗扫描器 `citation_audit`（`tools/check_architecture.py:9604`）——R6-8 起带三口径，R6-9 起裸引用的归属再扩到
  "同一行只点了文件名、没带行号"那一颗。行号漂移即红。
* **G-6 看不见的那两格**（写进契约，免得下一个人把绿读成"全对上了"）：判据按行扫描，所以①一行里既没有带行号的
  引用、也没有能唯一解析的文件名时，那颗裸行号无人核对——R6-9 之后四份文本仍剩 349 颗这种形状
  （R7 轮按同一条规则复测总数仍是 349，逐份拆分只记在方案书 §55.11 与台账那条登记里，本文不抄一份会过期的副本）；②名字与被引那一格被折行拆开时，行号照旧核对、
  只是不再核对"那一格里是不是这颗名字"。因此本文的写法口径是：**名字与 `path:line` 永远放在同一行**。
* 五条上限（G-1、G-2、G-3、G-4、G-6）在实现时必须走一次**变异取证**：
  故意造一条违规判据看它红不红，只绿不红的门禁不算判据。

## 11. 待裁决（本文不自行决定的部分）

* **Q-1**：`environment` 是运维自声明的字符串。"production 才禁明文"这条规则的强度，
  完全取决于自声明的可信度。是否改成"默认最严（只有显式 `sandbox`/`testnet` 才放开明文）"？
  本文按现状描述，修法 §4.8-3 与之相关但不依赖它。
* **Q-2**：CCXT 的 `options` 原样摊进上游构造器（`python/qianxing_ccxt/__init__.py:266`），
  未知键不拒。要不要在本仓白名单化？这需要一次 ccxt 键位盘点，不是一次安全改动就能顺带做完的事。
* **Q-3**：策略环是否要跨用户可读？现在的形状（公共 temp + 默认权限）显然是"没想过"。
  若答案永远是"只有宿主与它的子进程"，那 §4.12-④ 的私有目录就是终态；若有人要调试挂载，
  需要先给出那个角色是谁。
* **Q-4**：`GET` 面（§4.8 的读侧）是否允许在明文下保留？取决于是否真有"单机看板"这个用例。
  本文的修法只强制**写侧**关掉，读侧留给运维显式声明。

## 12. 修订记录

| 日期 | 变更 | 依据 |
|---|---|---|
| 2026-09-26 | 首版：12 类逐项判定 + 12 项缺陷登记 + 14 项负行 + 配置清单与轮换流程 | 本轮实测（`crates/` 23 个 crate、`python/` 23 个 `.py`、`deploy/` 全量、门禁与用例逐条读；首版当日工作树 359 个未提交改动） |
| 2026-09-26 | 引用收口：三种门禁扫不到的形状各改一类——逗号串号 3 处、裸 `:NNN` 全部展开成自足 `path:line`、range 首行的窗口打不到尾部名字时另给精确锚点6 处「名字与锚点对不上」挪到声明处（`external_executable`、`consumer_handler_executable`/`consumer_handler_args`、签名头、`credential_env`、`postgres_dsn_env`、`python_module`）；2 处过强断言收窄（`deny_unknown_fields` 的适用范围、`OutboxEvent` 在 `crates/qx-storage/src/lib.rs:368-374` 有字段但无人比）；1 处指错锚点（`CancelOrder` 负行 252-257 → 281-286）；负行 14 → 16 | 判据按 AST 从 `doc_citation_check`（`tools/check_architecture.py:9758`）取出后重放：本文全部 `path:line` 逐颗对得上，引用数与带名绑定数由常驻门禁当轮打印、不在这里抄一份会过期的副本；名字档另判，剩余 3 处 hard 全是「断言某物不存在」，已升成 §9 负行
| 2026-09-26 | R7 轮重钉：本文 51 行的行号按当前代码改口，并逐颗人工复核"被引那一格里是不是那颗东西"——机械 0 红不等于语义对：区间远端只量越界与空行（`CAP_RANGE_END`，`tools/check_architecture.py:8583`），同一条规则本轮在变更日志里抓到一例"数字恰好落在界内、那一格却是别的东西"的假通过，逐颗明细记在方案书 §55.10、§55.11；上一行那句"引用数不在这里抄副本"本轮同样回落到 §10 的 G-6——逐份拆分只留在方案书与台账，本文不再放第二份会过期的数 | 第 5 轮三扫之后的 R7 收口 |
