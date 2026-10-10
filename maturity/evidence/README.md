# 证据目录与导入格式（M0）

本目录承载**外部证据**——本地测试证明不了的档位（`sandbox_tested` / `production_approved`）。
`maturity/capabilities.yaml` 的四档状态里，前两档由 `tools/check_architecture.py` 与 `cargo test`
自证；后两档**只能**由本目录下的证据记录翻真。

> **两条轨，别混读（P0-1）**：本目录只管**实盘轨**——需要真实交易所凭据的那条路。
> 本仓的**主用法是回测与 Paper 闭环**，它一条交易所凭据都不用，它的验收边界**不在本目录**，
> 而在 `maturity/backtest_acceptance.yaml`（由 `tools/backtest_acceptance.py` 生成）。
> 对那条轨而言，`sandbox_tested` / `production_approved` 是**不适用**，不是「待补」。
> 把两条轨混在一张表里读，会让「实盘还差外部证据」被误读成「整个仓未落地」。

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
> 两档必须全为 false / 前两档的翻真依据里不出现证据记录）。本节是**人读**版，数字以那份登记面与
> `maturity/gate_snapshot.json` 为准。

- **实盘轨**：`maturity/evidence/testnet/20260920T005856Z-dryrun/result.json` 的 `outcome` 是 `"skipped"`。
  因此能力矩阵里 `sandbox_tested: true` **0 条**、`production_approved: true` **0 条**。
  这一档的卡点是**真实交易所凭据**（`QX_BINANCE_TESTNET_API_KEY` / `_SECRET`），不是代码。
- **回测轨**：`maturity/backtest_acceptance.yaml` 的 `outcome` 是 `"passed"`，
  自述 `credentials_required: false` / `external_venues: none` / `network_accessed: false` /
  `orders_sent: false`。它证明的是**无凭据也能完整验收**：同目录重跑逐格相等（含产物文件名与
  `config_fingerprint`），两个独立目录的 `result_hash` / `data_fingerprint` / `replay_verdict`
  与**路径归一化后**的产物内容相等（`equity.csv` / `fills.csv` 逐字节相等）。
- 本目录 `maturity/evidence/README.md` 与 `testnet/20260920T005856Z-dryrun/` 下那两份记录
  **是仓库资产（已跟踪）**——`git ls-files maturity/` 列得到它们。写在这里以免下一位读者
  按「证据目录不进版本控制」的老口径去找 `.gitignore`。
- **本轮（T0-3）改正了一处口径漂移**：验收脚本写进 `result.json` 的规则原文是
  「全部阶段退出码为 0」，而方案 §4 与脚本的**代码**（`tools/binance_testnet_acceptance.py`
  在 `outcome` 判定前先断言「非 `live-check` 阶段一律为 0」）都是
  「除 `live-check` 之外全部阶段退出码为 0」。已把脚本那句**文字**改成与代码、方案一致；
  `testnet/20260920T005856Z-dryrun/result.json` 是**历史产物**，保留原文字不改。
