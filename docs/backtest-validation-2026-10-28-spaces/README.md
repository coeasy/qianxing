# 本地 A 股与数字货币回测验证

执行时间：2026-10-28T14:39:46.519217+08:00

结果：**passed**。数据范围为仓库示例数据，不代表真实历史数据或实盘验收。

| 市场 | 标的 | 策略 | Bar 数 | 成交数 | 输入校验 | 回放校验 | 报告 |
|---|---|---|---:|---:|---|---|---|
| crypto | BTCUSDT.BINANCE | strategy-macd | 70 | 1 | verified | verified | [HTML](crypto-backtest.html) |
| ashare | 000001.SZSE | strategy-ashare | 6 | 1 | verified | verified | [HTML](ashare-backtest.html) |

验证包含：同目录重复执行产物一致性、独立目录结果一致性、Rust 输入与回放校验、修改输入/成交文件时拒绝生成报告，以及恢复文件后的再次验证。

负向检查日志中的报错是预期拒绝，不是正常回测失败。正向步骤失败会以 failed 记录并返回非零退出码。

报告中的收益来自短小示例数据，仅用于功能验证。手续费、资金曲线和成交事实均由 Rust 引擎产生，验证脚本不计算交易收益。

运行数据目录：`D:\workspace\qv spaces28`。详细证据见 [validation.json](validation.json)，命令输出在 `logs/`。

全方案交付状态：尚未完成。完整服务端部署、实盘就绪判定、原子提交恢复和跨宿主验收不在本次回测通过结论内。

原始输入和引擎产物：[input-and-artifacts.zip](input-and-artifacts.zip)。ZIP 内的摘要保留原执行路径；换目录后请重新执行验证脚本生成匹配的新产物。
