# 策略作者进阶：Tick、OrderBook、Timer、成交回报与预热

[首跑页](first-run.md)只教最小链路：`on_bar` → `decision` / `buy` → Rust 执行。这一页只给已经跑通首跑、
要把同一个策略接到别的行情或回调上的作者；每一节只讲一个钩子族，命令都能从仓库根目录直接复制，
读数都是本机实测（2026-10-31，Windows 11 / CPython 3.13，`python -m qianxing.cli`）。

> 标注口径：**【已实现】**= 当前工作树可执行，且行为由常驻用例或实测读数钉住；
> **【设计意图，未排期】**= 还没有代码，本页不给步骤。宿主能力以
> [策略宿主能力矩阵](strategy-host-capabilities.md) 为准，本页不复述整张表。

## 0. 先认清三件事

1. **钩子只产生意图**。`buy` / `sell` / `short` / `cover` / `close_position` 返回订单意图，风控、OMS、撮合、
   费用与账本始终在 Rust 内核里；Python 不碰 Gateway，也不碰交易所。
2. **行情决定宿主，钩子决定入口**。`STRATEGY_MANIFEST.data_level` 取 `bar` / `tick` / `orderbook` / `timer`，
   它决定 Rust 用哪个本地宿主校验并投递；同一个策略类覆盖了哪个钩子，就由 BaseStrategy 的
   `on_event` 分派到哪个钩子。
3. **`warmup_bars` 只按 Bar 计数**。Tick / OrderBook / Timer 策略必须声明 `0`，自己在钩子里维护窗口。

## 1. Bar：事件钩子与经典生命周期【已实现】

两种写法二选一，本地宿主与 Runtime 类宿主的判定相同：类同时没有覆盖 `on_event` 与 `on_bar`，
就走 Backtrader 风格；否则走 `on_bar` / `on_event` 回调。

| 写法 | 覆盖的钩子 | 语义 |
|---|---|---|
| 事件式 | `on_bar(context, bar)` | 每根 Bar 调一次，返回决策 |
| 经典式 | `prenext` / `nextstart` / `next` | 预热内 `prenext`；第 N 根（N=`warmup_bars`，为 0 时按 1）走 `nextstart`；之后 `next`。`nextstart` 默认转调 `next` |

示例文件：`python/examples/strategy_file_sma.py`（`on_bar`）与 `python/examples/strategy_file_nextstyle.py`
（`prenext` / `next`，`WARMUP_BARS = 40`）。两条命令在同一份 BarFrame 上实测读数相同：

```console
python -m qianxing.cli run-strategy ./python/examples/strategy_file_sma.py ./python/examples/strategy-file-backtest.example.json
python -m qianxing.cli run-strategy ./python/examples/strategy_file_nextstyle.py ./python/examples/strategy-file-nextstyle-backtest.example.json
```

`fills=1`、`return_bps=1`、`equity_points=70`、`data_fingerprint=barframe:613fdc1dc0b779e7`（两份读数一致）。
经典写法的派发由 `python/qianxing_bridge/app.py` 的本地回调与 `python/qianxing_strategy/class_adapter.py`
的 Runtime 类宿主使用同一个分界谓词（类没有覆盖 `on_event` 与 `on_bar` 才走经典写法）；
`python/tests/test_strategy_base.py` 的预热用例同时钉住两种入口。

## 2. Tick：`on_tick`【已实现】

- `data_level = "tick"`，`warmup_bars = 0`（见 `python/examples/strategy_file_tick.py` 的注释）。
- 行情用深度快照文件：spec 里写 `depth_path`（仓库根相对）与 `tier`（`l1` / `l2`），本地宿主把它投递成 Tick。
- 若声明 `warmup_bars > 0` 又收到非 Bar 事件，桥会直接拒绝：
  `history_requirements.warmup_bars is supported only for Bar events`。

```console
python -m qianxing.cli run-strategy ./python/examples/strategy_file_tick.py ./python/examples/strategy-file-tick-backtest.example.json
```

实测：`snapshots=80`、`fills=1`、`tier=l1`、`data_fingerprint=depth:example-depth-l1-v1:c18e516d92940ffd`。
参数网格用同一份示例的 `search-strategy`：`data_level` 与 spec 形状不变，只换参数域。

## 3. OrderBook：`on_order_book`【已实现】

`data_level = "orderbook"`，行情用多档深度帧（`deploy/qianxing.depth-frame.example.json` 每边 3 档），
spec 的 `tier` 写 `l2`。仓库目前没有随包的 order-book 示例文件，下面这个最小类是**实测通过**的版本
（把 `depth_path` 指向多档帧、`tier` 改为 `l2` 后运行）：

```python
from qianxing_strategy import BaseStrategy


class BookStrategy(BaseStrategy):
    STRATEGY_ID = "book-strategy"
    STRATEGY_VERSION = "1.0"
    STRATEGY_MANIFEST = {
        "schema_version": 1,
        "strategy_id": STRATEGY_ID,
        "strategy_version": STRATEGY_VERSION,
        "parameters": {},
        "instrument_requirements": {
            "asset_classes": [], "products": [], "venues": [],
            "requires_explicit_spec": False,
        },
        "data_level": "orderbook",
        "warmup_bars": 0,
        "dependencies": [],
    }

    def __init__(self, _spec):
        self.books = 0

    def on_order_book(self, context, book):
        self.books += 1
        intents = []
        if self.books == 1:
            intents.append(self.buy(book["instrument"], qty_raw=1_000_000))
        return self.decision(context, "OrderBook", book, intents=intents)


STRATEGY_CLASS = BookStrategy


def create_strategy(spec):
    return STRATEGY_CLASS(spec)
```

实测：同一份 80 帧数据上 `fills=1`、`tier=l2`、
`data_fingerprint=depth:example-depth-l2-v1:bb98a3580a92ee38`——成交由 `on_order_book` 返回的意图驱动，
证明该钩子在本地 OrderBook 宿主上被真实调用。

## 4. Timer：只在 Runtime 类宿主上送达【已实现（宿主侧）】

本地 Bar / Tick / OrderBook 三个宿主都不投递定时器（矩阵里 `timer` 一行全是 Unsupported）；
`python/examples/strategy_file_timer.py` 面向 `qx-cli strategy-worker` 的 Runtime 类宿主，文件头已写明。
不要在 `qianxing run-strategy` 上期待 `on_timer`：本地宿主即使策略声明 `data_level = "timer"` 也不会构造定时事件。

## 5. 成交回报：`on_trade` / `on_fill_update` / `on_order`【本地支持 fill 与 order-status】

Rust 在 OMS 与账本提交之后才回调；默认投影链是：

| 宿主事件 | 作者钩子 | 说明 |
|---|---|---|
| 成交（v2） | `on_fill_update(context, update)` | `update` 带 `event_seq`，适合去重/持久化；默认投影成 v1 后走 `on_order_update` |
| 成交（v1） | `on_order_update(context, update)` → `on_trade(context, trade)` | 兼容旧代码；`trade` 是 `update["fill"]` |
| 订单状态 | `on_order_status_update(context, update)` → `on_order(context, update)` | 默认先校验身份再转调 `on_order` |

本地 Bar 实测：`on_bar` 在第二根 Bar 买入，`on_trade` 收到成交后返回平仓决策，最终 `fills=2`——
证明成交回调在本地宿主上送达，且它的返回值与行情钩子的返回值走同一条意图通道。

实测口径（本地 Bar 宿主，2026-10-31）：

- 成交回调的 `context` 键为 `account_id, as_of, available_margin_exact, available_margin_raw, cash,
  cash_exact, data, data_fingerprint, positions, positions_exact, risk_state, strategy_id,
  strategy_version, venue_id`——**不含 `instrument`**；`fill` 也只有 `venue_id`，没有 symbol。
  需要当前标的时，在 `on_bar` / `on_tick` 里记住它，或从 v1 update 的 `order.instrument` 读。
- Runtime 类宿主的适配器（`python/qianxing_strategy/class_adapter.py`）在成交/订单状态回调里会把
  `instrument` 放进 `context`。两侧这一格不一致，已登记为待裁决；本轮不改 Rust 宿主。
- `on_account` / `on_position` / `on_positions_snapshot` 在本地宿主不可用（矩阵 `account_updates` /
  `position_updates` 均为 Unsupported）；Runtime 类宿主只有在同时配置 account 与 venue 后才投递。

## 6. 预热：`history_requirements` 与 `on_warmup_complete`【已实现】

```python
def history_requirements(self):
    return {"warmup_bars": 40}
```

- 声明值来自 `history_requirements()` 或 `STRATEGY_MANIFEST.warmup_bars`；两处都有但不相等时直接失败
  （`history_requirements.warmup_bars conflicts with STRATEGY_MANIFEST.warmup_bars`）。
- 计数**按标的**；第 N 根 Bar 到达时先调用
  `on_warmup_complete(context, instrument, observed_bars)`，再派发这根 Bar 的钩子。
  `warmup_bars == 0` 时在 `on_start` 之后调用一次，`instrument=None`。
- 预热期内决策里的 `intents` 会被清空（事件式 `on_bar` 仍每根都调到，只是不能下单）；经典写法此时走 `prenext`。
- 预热没走完就结束（`on_stop` 前仍有标的 `observed < required`）会失败：
  `strategy run ended before Bar warmup completed: instrument=… observed=… required=…`。
- 历史窗口用 `self.history_decimal(context, instrument, length=…, field="close")`（精确十进制，
  按 `context["as_of"]` 截断）；`context["data"]` 是同一个只读门户。
- 只有 Bar 宿主支持预热计数；Tick / OrderBook / Timer 声明非零 `warmup_bars` 会被拒绝（见 §2）。

## 7. 多标的【本地适配器已实现】

本地 Bar / Tick / OrderBook 适配器声明 `multi_instrument`，多标的请求里 `context["instrument"]`
始终指向**当前正在处理的事件**（不是请求的主标的），`on_warmup_complete` 的上下文同理。
Runtime 类宿主当前一个 job 只处理一个标的，不声明 `multi_instrument`，声明了该要求的策略会在派发前被拒。

## 8. 错误与状态

- 策略回调抛出原生异常时，`on_error(context, error)` 与 `on_error_v1(StrategyErrorV1)` 都会运行；
  钩子自身再出错也不会替换原始失败。`StrategyErrorV1` 带 `stage`（如 `market_event`、
  `history_requirements`、`warmup_complete`、`fill_update`）、策略身份、时间、标的与有界 `message`。
- `snapshot_state()` / `restore_state()` / `on_restore()` 服务于 Runtime 宿主的检查点协议（快照上限 64 KiB）；
  一次本地 `run-strategy` 不产生持久检查点，不要依赖它跨进程续跑。
- `diagnostic_state()` 返回至多 64 KiB 的只读 JSON，只用于候选偏差诊断，不参与交易决策。

## 9. 回到首跑与能力边界

首跑承诺只有本地 `trusted_python` 后端；Tick / OrderBook 的示例命令都在本页与
[首跑页](first-run.md) 的本地链路内，不需要沙箱 SDK 或交易所凭据。远端沙箱、CCXT worker、
服务端部署与实盘就绪仍见[部署与运维](../deploy/README.md)，能力分级见
[策略宿主能力矩阵](strategy-host-capabilities.md)。
