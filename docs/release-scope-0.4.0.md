# 0.4.0 回测 / Paper 发布范围

牵星 0.4.0 面向本机研究、历史数据回测和 Paper 闭环。发布通过表示对应的安装、回测、报告和本地 Paper 验收通过；它不表示交易所账户已认证，不表示交易所沙盒下单或实盘交易已经获批。

## 已包含

- Rust CLI 和 Python wheel 的回测 / Paper 安装路径。
- 用户提供的 BarFrame 回测；示例项目可用 `qx-cli quickstart <目录> --strategy macd --html` 演示完整流程。
- 回测产物校验、离线 HTML/SVG 报告和本地策略 Paper 主链。
- OKX 与 Bitget 的真实公开行情 quickstart；公共行情验证只读，不请求账户私有接口、不提交订单。

## 必须在产品中显示的边界

- 内置 quickstart 生成的是示例 BarFrame。示例收益不是市场历史表现，也不是投资回报承诺。
- Paper 按 L1 最优报价和显示数量模拟触价成交，按 client order id 顺序处理。它没有隐藏流动性、队列位置、交易所撮合延迟和市场冲击模型，也不与 Bar/L1/L2 回测撮合等价。
- 0.4.0 不宣称 OKX、Binance 或 Bitget 的认证沙盒下单、账户状态同步、真实成交回报、完整对账或生产运行批准。连接器的离线契约和匿名公开行情验收不能替代这些证据。
- Binance 公共行情验收在 GitHub hosted runner 上可能因地区限制返回 HTTP 451。0.4.0 发布门禁会保留 OKX 和 Bitget 的真实行情验收；Binance 检查在 PR / 手动完整验收流程中继续运行。被跳过或地区受限的结果不得记为成功。

## 运维与分析限制

- EventLog 和持久审计记录按追加事实保留，不提供自动删除或压缩。文件型 Paper 账户日志大小会在 `paper-check --json` 输出，并按默认 1 GiB（可由 `QIANXING_EVENT_LOG_WARN_BYTES` 调整）提示；SQLite/PostgreSQL 不由该报告测算。当前仍没有后台自动容量告警或轮转，长时间运行前应规划磁盘 / 数据库容量、备份与封段归档。
- Paper 每笔订单的规范风控裁决（放行/拒绝、规则版本、拒绝原因与可用投影字段）会以不可变事实写入账户 EventLog，并由 `paper-check --json` 只读汇总最近 100 条；它尚未汇总到与回测相同的研究报告读模型。Paper 失败仍应检查对应命令审计和 EventLog；不得把简化状态摘要当成完整组合风险报告。
- 多币种曲线可各自查看，但当前比较面不会按 FX 转换成统一组合净值。不同结算币种的结果不能直接相加或排名。

## 发布门禁

发布工作流仍要求版本一致、核心 CI、Linux / Windows / macOS 构件、Python wheel、报告与供应链产物。公共行情 quickstart 使用真实行情，但它不代表下单验收；Binance 的地区限制单独记录。发布后仍不得将 0.4.0 描述为已获生产批准的实盘系统。
