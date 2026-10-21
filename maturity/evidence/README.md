# 证据目录与导入格式（M0）

本目录保存可持久复核的**本地性能/运行证据**与**外部交易所验收证据**。仓库根目录的 JSON/Prometheus
记录用于复核本机基准和 Paper/worker 观测；只有 `testnet/`、`production/` 下经核验的外部验收记录
才能翻转 `sandbox_tested` / `production_approved`。

`maturity/capabilities.yaml` 的 `implementation` / `code_tested` 由源码、门禁和本地执行证据支撑；
外部两档必须由对应验收流程产生的证据支撑，本地 Paper 或基准数据不得替代。

> **两条轨，别混读（P0-1）**：`testnet/` 与 `production/` 只保存需要真实交易所凭据的验收证据；
> 本仓的主用法仍是无需交易所凭据的回测与 Paper 闭环；币圈示例与 A 股示例验收分别记录在
> `maturity/backtest_acceptance.yaml` 和 `maturity/ashare_example_backtest_acceptance.yaml`
>（由 `tools/backtest_acceptance.py` 生成）。A 股记录使用仓内示例数据，不代表真实行情或完整 profile 测量。
> 根目录的 Paper worker scrape 可以证明本地指标路径可读，
> 但不能翻转 `sandbox_tested` / `production_approved`，也不代表持续运行或生产行情质量。

## 四档证据

| 档位 | 含义 | 谁来翻真 |
|---|---|---|
| `implementation` | 代码/配置已存在 | 源码在盘 |
| `code_tested` | 本地测试、契约测试或 Paper E2E 已通过 | 门禁 + `cargo test` |
| `sandbox_tested` | 真实供应商沙盒/模拟账户已通过 | 本目录的沙盒证据记录 |
| `production_approved` | 真实生产凭据、故障演练与运维审批已通过 | 本目录的生产证据记录 + 运维审批 |

## 硬约束

1. **没有证据不得翻真。** `tools/check_architecture.py` 的
   `capabilities_check` 已常驻核对「未拿到外部沙盒记录前 `sandbox_tested` 全为 false」——
   在证据目录出现**通过**记录之前，能力矩阵里任何 `sandbox_tested: true` 都会当场变红。
2. **跳过不算通过。** 缺凭据的验收必须以 `outcome: "skipped"` 落盘（退出码 3），
   `skipped` **不能**用于翻真。
3. **证据必须是脱敏的。** 记录里不得出现 API key / secret / 账户号明文；凭据只从环境变量读取。
4. **重跑不重复下单。** 验收脚本必须自带幂等（`client_order_id` / 去重键），
   重跑同一条 acceptance 不得产生第二笔真实订单。

## 目录约定

```
maturity/evidence/
  testnet/<UTC 时间戳>-<标签>/          # 沙盒证据（Binance testnet 等）
    result.json                         # 机器可读结论（含 outcome / 分阶段记录）
    <venue>.acceptance.json             # 交易所侧原始回报（脱敏）
  production/<UTC 时间戳>-<标签>/        # 生产证据（真实凭据 + 故障演练 + 审批）
    result.json
    approval.json                       # 运维审批记录
```

## `result.json` 最小字段

```json
{
  "venue": "binance-spot-testnet",
  "outcome": "passed | failed | skipped",
  "started_at": "2026-10-06T00:58:56Z",
  "stages": [
    { "name": "runtime-check", "status": "passed" },
    { "name": "live-check", "status": "passed" },
    { "name": "submit-order", "status": "passed", "idempotent_rerun": true },
    { "name": "reconcile", "status": "passed" }
  ],
  "submit_hash": "<提交请求的稳定摘要>",
  "report_sequence": ["accepted", "fill", "reconciled"],
  "failure_classes": [],
  "redacted": true
}
```

`outcome` 只认 `passed` / `failed` / `skipped` 三值；只有 `passed` 才允许把对应能力的
`sandbox_tested` 翻真，且必须由**当轮**证据支持（证据带时间戳，不随能力矩阵长期有效）。

## 现状（2026-10-10）

> **机读边界登记（T0-3）**：实盘轨的五段、翻转规则的唯一措辞、证据根与当轮现状登记在
> `maturity/external_chain.yaml`，由 `tools/check_architecture.py` 的 `external_chain_check`
> 七颗逐格对账（五段与验收方案 §2 那张表逐行同名同序 / 规则在登记面、方案 §4 与脚本三处同源 /
> 脚本的**代码**真实现了那条规则 / 现状与盘上逐份 `result.json` 一致 / 没有 `outcome=pass` 记录时
> 两档必须全为 false / 前两档的翻真依据里不出现证据记录）。本节是**人读**版；外部验收状态以登记面
> 与盘上的脱敏 `result.json` 为准，不依赖历史门禁数量或快照。

- **实盘轨**：`maturity/evidence/testnet/20260920T005856Z-dryrun/result.json` 的 `outcome` 是 `"skipped"`。
  因此能力矩阵里 `sandbox_tested: true` **0 条**、`production_approved: true` **0 条**。
  这一档的卡点是**真实交易所凭据**（`QX_BINANCE_TESTNET_API_KEY` / `_SECRET`），不是代码。
- **回测轨**：币圈示例 `maturity/backtest_acceptance.yaml` 与 A 股示例
  `maturity/ashare_example_backtest_acceptance.yaml` 的 `outcome` 均为 `"passed"`，
  自述 `credentials_required: false` / `external_venues: none` / `network_accessed: false` /
  `orders_sent: false`。两条示例各自证明的是**无凭据也能重复验收**：同目录重跑逐格相等（含产物文件名与
  `config_fingerprint`），两个独立目录的 `result_hash` / `data_fingerprint` / `replay_verdict`
  与**路径归一化后**的产物内容相等（`equity.csv` / `fills.csv` 逐字节相等）。
- 本目录 `maturity/evidence/README.md` 与 `testnet/20260920T005856Z-dryrun/` 下那两份记录
  **是仓库资产（已跟踪）**——`git ls-files maturity/` 列得到它们。写在这里以免下一位读者
  按「证据目录不进版本控制」的老口径去找 `.gitignore`。
- **规则口径后续修正**：历史记录 `testnet/20260920T005856Z-dryrun/result.json` 保留当时的规则原文。
  2026-10-16 复核成功路径发现重复 `request_id` 本来就必须以非零退出码被拒，旧的总阶段判定却要求
  它退出码为 0，导致完整验收无法通过。当前登记规则允许的唯一非零阶段是带明确幂等拒绝事实的重复
  提交；`live-check` 需证明 `network_accessed=false` 与 `orders_sent=false`，其它必需阶段须成功。

## Venue EvidenceBundle 生产（WP-5.2，2026-10-16）

仓内 `qx-cli evidence pack-venue-acceptance <验收目录> --operator <操作人>` 可将一份真实验收
窗口聚合为 `venue.evidence.json`。它只接受 Binance testnet / OKX sandbox 的
`outcome=pass`、`credentials_present=true`、`send_orders=true` 结果，并核对运行检查、提交及
幂等重复提交、worker 恢复、对账和终态状态阶段。普通 dry-run、`skipped`、失败、Paper 与本地
回测均不能生成 L3 bundle。验收产物记录窗口 epoch 毫秒起止时间；bundle 只保存配置脱敏摘要、
来源文件摘要与相对日志文件名，不复制可能包含敏感诊断的阶段输出。

`pack-venue-acceptance` 负责从本仓验收产物生产候选 bundle；之后仍需由受信操作人执行
`qx-cli evidence sign`，接收方使用独立可信公钥执行 `verify`。签名证明内容完整性，不替代对
真实订单、交易所账户记录和验收环境的人工/外部核验。没有实际通过的 testnet/sandbox 窗口与签名
记录时，能力登记仍保持 `sandbox_tested: false`；L4 mainnet producer 仍未实现。

## 第三方策略构建材料（WP-5.2，2026-10-16）

`evidence export-run` 可用 `--build-attestation <PATH>` 附加第三方策略的构建材料。必须同时提供
`--runtime-config`；证明中的 `strategy_id` 与目标 RunManifest 策略一致，`artifact_sha256` 与运行时配置
声明的策略工件摘要一致。`schema_version=1` 证明至少列出 `source`、`dependency_lock`、
`build_recipe`、`toolchain` 四类材料；每项以证明文件所在目录为根，使用目录内相对路径和 SHA-256。
源码修订字段必须是 40 或 64 位十六进制提交标识。导出时逐件核对并搬运材料，包内的
`build/attestation.json` 和每个材料文件都进入 EvidenceBundle 工件摘要；`verify-run` 会再次核对
材料名册与配置中的策略身份/工件摘要。

此 attestation 记录并绑定第三方提供的构建声明，不会自行重建其二进制，也不证明源码能产生该二进制。
完整的第三方可复现构建、OS 级断网复算和 L4 mainnet 证据仍未完成。
