"""服务端执行层：实盘轨（方案 §6.2 接口分层）。

**这一层明确依赖服务端执行**——方案 §6.2 的原话。`LiveClient` 与 `PaperClient` 共用
同一条控制面，唯一区别是服务端要有一份**已认证的 API policy** 才会把它路由到真实 venue
worker。

这一层自己**不执行交易**：Python 侧没有交易所密钥、没有签名、没有下单路径。它只发一份
版本化命令，能不能成交由服务端与 venue 说了算。Live 仍需 testnet conformance 才能谈 GA
（见 `maturity/capability-matrix.yaml` 的 `venues` 段与 `maturity/evidence/testnet/`）。

Paper 面见 `qianxing.paper`。
"""

from qianxing.app import LiveClient, TradeCommand, TradeWorkflowClient, TradingInstrumentSpec

__all__ = [
    "LiveClient",
    "TradeCommand",
    "TradeWorkflowClient",
    "TradingInstrumentSpec",
]
