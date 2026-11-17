# 回测报告布局与明细改进

本次修复的是单市场 HTML 报告的生成代码，重新生成了 [A 股报告](backtest-report-v2-2026-10-28/ashare-backtest.html) 和 [数字货币报告](backtest-report-v2-2026-10-28/crypto-backtest.html)。原先分享的报告也已刷新；完整新验证记录位于 [新版报告汇总](backtest-report-v2-2026-10-28/README.md)。

## 已修复的问题

- 成交表有 16 列，执行明细也有多列，但 `fill-scroll` / `table-wrap` 没有对应滚动约束，表格会撑宽整个页面。现在宽表格仅在所属区域内滚动，表头随区域滚动保持可见。
- 原来的 820px 页面、300px 最小网格列和不换行表头不能很好适应窄屏。现在网格允许收缩，窄屏自动变为单列或双列，长路径、哈希和 JSON 可以换行。
- 所有金额卡片统一标注人民币符号。现在优先展示摘要中的精确金额和实际币种，包含多币种手续费；币种缺失时明确标为缺失，不推测为人民币。
- 报告面板宽度变化时，联动图只监听窗口 resize。现在同时监听图表容器的尺寸变化。

## 新增明细

运行起止时间与原始毫秒时间戳、策略参数与未使用参数说明、精确账户数据、逐点权益/持仓（最近 500 条）、月度表格、完整摘要展开及 JSON 下载、页内导航。现有成交筛选、执行记录、回放追溯和模型可信度信息继续保留。

这些内容来自已验证的摘要、权益和成交产物；页面不增加新的收益或风险计算口径，也不从当前配置文件推断历史运行参数。权益表中的金额是 raw 的兼容投影，精确账户值单独展示。

## 指标定义与跨格式口径

回测引擎与 `qx-app` 是指标事实的唯一计算端，HTML、JSON 和 XLSX 只读取同一份已验证摘要。摘要中的 `metrics_definition` 使用独立的 `schema_version: 1`：Sharpe、Sortino、Calmar、胜率和收益因子按每个已记录样本周期计算，不年化；基准收益与主动收益只对严格相同的时间戳观测计算，也不推断日历或年化基数。展示层分别标明每周期口径和基准价格口径。

精确手续费由 Ledger fee entries 按币种汇总并写入 `metrics_exact.fees_by_currency`。`metrics.fees_raw` 只保留为历史兼容投影，不能跨币种相加或替代精确手续费。XLSX Overview 会同时导出原始投影、精确按币种手续费、精确换手/币种、精确初末权益和指标定义版本；HTML 从同一摘要展示这些字段。对应的版本化口径合同为 [`backtest-metrics-definition-v1.json`](../schemas/backtest-metrics-definition-v1.json)。

多市场 Bar 与多市场深度 CLI 报告也投影同一版本的 `metrics_definition`，并在 JSON、HTML 与 XLSX Overview 显示周期/年化及费用定义。两类报告均从经过事件重放的账簿 Fee entries 按币种精确汇总手续费；深度报告继续对账已验证摘要里的 `metrics_exact.fees_by_currency`。收益、回撤和手续费都在 HTML/XLSX 与 JSON 投影间逐项核对，展示层不重新计算这些事实。

多市场 Bar 与深度报告支持 `--benchmark-bars`，可选 `--benchmark-calendar`，并可用 `--benchmark-csv` 导出逐周期组合、基准与主动收益。分析只接受严格时间戳对齐，不做价格复权或隐式年化；多轮 `--against` 比较会在 JSON、HTML、XLSX Benchmark 工作表及 CSV 中分别保留每轮结果，CSV 增加 `run_id`。Bar 报告将同一时间戳内的权益变化合并为该时点最后一笔已验证权益；深度报告基于 qx-app 完整复核的权益曲线计算，HTML/XLSX 预览采样不会截断分析序列。

## 项目级报告入口

报告命令会先生成并检查同次请求的 HTML、SVG、XLSX 与 CSV 文件，再经共享 bundle 发布器统一切换。`--evidence` 生成的 EvidenceBundle 也进入同一事务。发布器在每个输出目录记录事务日志、暂存文件和旧文件备份；普通发布失败会回滚，进程在切换期间退出后，下次报告发布会根据事务是否完整提交恢复旧文件集或保留新文件集。已有证据包内容相同时允许重复生成，内容不同时在发布前拒绝覆盖。

```sh
qianxing project report ./my-project PROJECT_RUN_ID
qianxing project report ./my-project PROJECT_RUN_ID -o reports/run.html
qianxing project report ./my-project PROJECT_RUN_ID --cli /path/to/qx-cli   # 显式指定渲染器
```

默认输出到该运行的 `.qianxing/runs/PROJECT_RUN_ID/report.html`。命令先核对已登记产物的字节摘要，再调用 Rust 进行报告校验和生成，保留独立的 `report.log` / `report-status.json`。生成报告失败不修改原回测终态。渲染器按「`--cli` > wheel 内置的 `qianxing/_bin/qx-cli` > PATH 上的 `qx-cli`」单链选中即用，不做静默替换；三个落点都不存在时报告阶段以 `report_renderer_missing` 失败并给出 `--cli PATH` 的修法，回测本身的状态不变。发布 wheel 前由构建脚本把宿主平台 `qx-cli` 装进包内，所以干净 venv 只装 wheel 也能出报告（见 [SDK 项目运行](strategy-project-runs.md) 的报告一节）。不支持的 outcome 应使用其专用报告入口。

## 验证与边界

26 项报告 HTML 定向测试通过，其中包括报告 bundle 失败后保留旧文件和带 `--evidence` 的 JSON 报告导出；Rust 发布层 10 项测试通过，其中覆盖暂存态重启恢复旧文件集、提交态重启保留新文件集及安装失败回滚。`cargo check -p qx-cli` 与 `cargo build -p qx-cli` 通过。新版 A 股和数字货币报告又完成了双目录、重复回测、篡改拒绝和报告重新生成验证。内联 JavaScript 语法、页内链接、UTF-8 与离线资源已核对。

**浏览器视觉复核尚未完成。** Chrome 工具无法加载请求头策略，内置浏览器也不可用；没有把源代码和结构检查冒充为桌面/手机实际截图验收。完整方案中的服务端部署管理、实盘就绪、原子提交恢复和跨宿主验收仍未完成。
