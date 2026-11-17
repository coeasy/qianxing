# 沪深300 2010年至今 SMA(20,60) 回测

真实市场回测当前为 **blocked**，没有绩效结果。工作区没有2010年至今的完整沪深300日线与独立交易日历，运行环境拒绝外部行情访问。合成序列只测试程序链路。

准备来源可说明的日线CSV（`date,open,high,low,close,volume`）和独立交易日历JSON。程序校验预热、逐日覆盖、价格范围、缺失/重复、文件散列，再运行两次Rust引擎回测，校验重放并输出HTML、XLSX及输入/产物ZIP。ZIP 同时包含冻结输入、引擎运行目录和生成的报告；`reports/archive-manifest.json` 将每个成员的大小和 SHA-256 绑定到 engine run ID、result hash、策略、runner、Rust CLI、行情和日历摘要。状态文件保存 ZIP 与清单摘要，测试验证归档可复核且相同输入生成相同 ZIP。已有目录会拒绝覆盖。

```powershell
python tools/hs300_backtest.py `
  --csv D:/market-data/hs300-daily.csv `
  --calendar D:/market-data/cn-trading-calendar.json `
  --source "数据提供方与下载批次标识" `
  --binary D:/path/to/qx-cli.exe `
  --work-dir D:/workspace/hs300-run-data `
  --report-dir docs/hs300-sma-2010-present/run-20261028 `
  --start 2010-01-01 --end 2026-10-28
```

详细口径见 [HTML报告](report.html)。指数合成单位不能代表ETF实际收益。获得真实数据后才能生成真实收益、回撤、逐笔和年度分析。

校验器测试7项、策略测试4项通过；合成数据端到端Rust回测/报告/重放复现已通过，测试数值不纳入市场结论。
