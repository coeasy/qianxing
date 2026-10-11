"""服务端执行层：Paper 轨（方案 §6.2 接口分层）。

**这一层明确依赖服务端执行**——方案 §6.2 的原话。`PaperClient` 把版本化的
`TradeControlSpec` 提交给一台运行中的 `qx-api`：Rust 侧认证 operator、在 `qx-app` 里校验、
经既有 ControlPlane 持久化并入队，再由配置好的 Paper worker 执行。

Python 侧不撮合、不下单、不记账。`PaperVenue` 是仓库自己的撮合实现，不需要任何交易所凭据——
它是"不碰真钱也能把订单闭环跑完"的那条轨。

实盘面见 `qianxing.live`。
"""

from qianxing.app import PaperClient, TradeCommand, TradeWorkflowClient, TradingInstrumentSpec

__all__ = [
    "PaperClient",
    "TradeCommand",
    "TradeWorkflowClient",
    "TradingInstrumentSpec",
]
