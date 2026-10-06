# 证据目录与导入格式（M0）

本目录承载**外部证据**——本地测试证明不了的档位（`sandbox_tested` / `production_approved`）。
`maturity/capabilities.yaml` 的四档状态里，前两档由 `tools/check_architecture.py` 与 `cargo test`
自证；后两档**只能**由本目录下的证据记录翻真。

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

## 现状（2026-10-06）

- `maturity/evidence/testnet/20260920T005856Z-dryrun/result.json` 的 `outcome` 是 `"skipped"`。
- 因此能力矩阵里 `sandbox_tested: true` **0 条**、`production_approved: true` **0 条**。
- 本目录与 `/logs/` 一样**不是仓库资产**（未跟踪）：`maturity/evidence/**` 不进版本控制，
  所以 `capabilities.yaml` 里登记的能力**不依赖**本目录在盘；证据只在翻真时被引用。
