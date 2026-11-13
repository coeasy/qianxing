# 本地 A 股与数字货币回测验证

执行入口：`tools/local_backtest_validation.py`。脚本使用仓库内置的 A 股和数字货币示例数据，调用当前构建的 `qx-cli` 完成回测与报告验证，不自行计算收益、手续费或资金曲线。

```powershell
$env:CARGO_BUILD_JOBS = '2'
cargo build -p qx-cli --bin qx-cli
python tools/local_backtest_validation.py --binary target/debug/qx-cli.exe --work-dir D:/workspace/qx-check-new --report-dir docs/backtest-check-new
```

`--work-dir` 和 `--report-dir` 都必须是尚不存在的目录。脚本保留已有验证记录，不删除或覆盖旧目录。Windows 请使用较短的数据目录，避免内容寻址产物的长文件名触发系统路径长度限制；目录中的空格受支持。

每次执行包含：

1. 创建两份数字货币项目和两份 A 股项目。
2. 每份项目连续回测两次，验证同目录产物逐字节一致。
3. 比较独立目录的结果身份和归一化产物；资金曲线、成交文件必须逐字节一致。
4. 通过 Rust 报告读取链路校验输入指纹、RunRecord 和回放事实，并生成 HTML 报告。
5. 修改输入与成交文件，确认报告读取拒绝被篡改的内容；在 `finally` 中恢复原文件并再次验证。
6. 输出 HTML、SVG、JSON、Markdown 汇总、逐步骤日志和原始输入/产物 ZIP。

`validation.json` 包含二进制摘要、结果身份、重复执行证据和预期拒绝检查。正向步骤失败会记录 `failed` 并返回非零退出码；篡改检查的非零退出码属于预期验证结果。ZIP 中的摘要保留原执行路径，换目录后应重新运行脚本生成匹配的新产物。

2026-10-28 本地执行记录：

- [验证汇总](backtest-validation-2026-10-28-final/README.md)
- [A 股 HTML 回测报告](backtest-validation-2026-10-28-final/ashare-backtest.html)
- [数字货币 HTML 回测报告](backtest-validation-2026-10-28-final/crypto-backtest.html)
- [原始输入与引擎产物](backtest-validation-2026-10-28-final/input-and-artifacts.zip)
- [含空格目录的完整验证](backtest-validation-2026-10-28-spaces/README.md)

本次示例为 `000001.SZSE` 的 6 根 Bar 和 `BTCUSDT.BINANCE` 的 70 根 Bar，各产生 1 笔成交。它们验证示例回测、成交记账、重复性和报告完整性，不能用来评估策略盈利能力，也不代表真实行情数据质量、完整框架或实盘部署已经验收。
