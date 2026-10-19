# Qianxing 本地项目

运行时配置：`D:\aex_work\qianxing\crates\qx-cli\.task-tmp\test-temp\qianxing-backtest-frame-ashare-18480-1791767387015034000\qianxing.runtime.json`。profile=`ashare`。当前配置为基础运行时，可通过 `qx-cli init --strategy macd --force` 绑定内置策略。

## 推荐流程

```text
qx-cli doctor D:\aex_work\qianxing\crates\qx-cli\.task-tmp\test-temp\qianxing-backtest-frame-ashare-18480-1791767387015034000\qianxing.runtime.json
qx-cli config explain D:\aex_work\qianxing\crates\qx-cli\.task-tmp\test-temp\qianxing-backtest-frame-ashare-18480-1791767387015034000\qianxing.runtime.json
qx-cli config validate D:\aex_work\qianxing\crates\qx-cli\.task-tmp\test-temp\qianxing-backtest-frame-ashare-18480-1791767387015034000\qianxing.runtime.json
qx-cli backtest D:\aex_work\qianxing\crates\qx-cli\.task-tmp\test-temp\qianxing-backtest-frame-ashare-18480-1791767387015034000\qianxing.runtime.json D:\aex_work\qianxing\crates\qx-cli\.task-tmp\test-temp\qianxing-backtest-frame-ashare-18480-1791767387015034000\qianxing.ashare.bar-frame.example.json D:\aex_work\qianxing\crates\qx-cli\.task-tmp\test-temp\qianxing-backtest-frame-ashare-18480-1791767387015034000\qianxing.ashare.spot.spec.json
qx-cli report D:\aex_work\qianxing\crates\qx-cli\.task-tmp\test-temp\qianxing-backtest-frame-ashare-18480-1791767387015034000\qianxing.runtime.json --json
```

带有可读本地数据集的项目会额外生成 `qianxing.project.json`，其中引用运行时、数据集版本和可识别的策略，结构遵循仓库的 `project-manifest-v1` schema。回测产物在 `data/qianxing/runs/`，包含摘要、权益曲线、成交明细、RunManifest 和带文件摘要的 RunRecord。

初始化生成的样例文件只用于本地回测和 Paper 验收，不包含交易密钥，也不会自动发送真实订单。CCXT/多交易所 profile 只生成公共配置和凭据引用，必须自行配置环境变量后再做 sandbox 验收。
