"""远端客户端层：连一台运行中的 `qx-api` 服务（方案 §6.2 接口分层）。

这一层只有一件事：把远端 Run 的完整生命周期摆出来——提交、看状态、取消、等、取结果。
[`AppClient`][qianxing.client.AppClient] 与本地用例层共享**同一份**请求/结果契约：
`qianxing.client` 发出去的 `BacktestSpec` 与 `qianxing.core.run_backtest` 收的那一份
是同一个 dataclass，服务端跑的也是同一个 Rust 用例。跨语言同一 Run 这件事在
`maturity/language_api_contract.yaml` 里逐格登记。

[`RemoteRunHandle`][qianxing.client.RemoteRunHandle] 的 `wait()` **缺省有限**（30 s，
与进程内句柄同值），方案 §9 要的就是这个：无限等待只能由调用方显式请求。

**这一层刻意不导出**本地自由函数（`run_backtest` / `validate_dataset` 那一族）：
它们在进程内跑，与服务无关；混在一起会让"这条调用到底打没打网络"变得要靠读实现才知道。

服务端执行面（`PaperClient` / `LiveClient`）也不在这里——它们依赖服务端的执行 worker
与 operator 身份，见 `qianxing.paper` 与 `qianxing.live`。
"""

from qianxing.app import AppClient, RemoteRunHandle

__all__ = [
    "AppClient",
    "RemoteRunHandle",
]
