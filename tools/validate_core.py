#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
牵星 Qianxing — 核心语义验证原型（Python）

用途有二：
  1. 在与 Rust 实现**相同的算法语义**下独立复算，交叉验证设计正确性
     （因果队列排序、确定性重放、point-in-time 可见性、撮合模型）。
  2. 作为未来 Python 控制面（PyO3 绑定之上）的语义骨架。

注意：本文件只验证**语义**，不代表性能。性能必须以 Rust 实测为准。

运行： python3 tools/validate_core.py
"""

from __future__ import annotations

import heapq
import sys
from dataclasses import dataclass, field
from typing import Callable, List, Optional, Tuple

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")

SCALE = 1_000_000_000

# ---------------------------------------------------------------- 定点数值


def dec(s: str) -> int:
    """十进制字符串 -> 定点原始值。"""
    neg = s.startswith("-")
    body = s[1:] if neg else s
    if "." in body:
        i, frac = body.split(".")
        assert len(frac) <= 9, "超过 9 位小数"
        frac = frac.ljust(9, "0")
    else:
        i, frac = body, "0" * 9
    v = int(i or "0") * SCALE + int(frac)
    return -v if neg else v


def show(x: int) -> str:
    neg = x < 0
    x = abs(x)
    i, frac = divmod(x, SCALE)
    s = f"{frac:09d}".rstrip("0")
    return f"{'-' if neg else ''}{i}" + (f".{s}" if s else "")


# ---------------------------------------------------------------- 因果队列

PRIO_TIMER, PRIO_FEEDBACK, PRIO_MARKET, PRIO_COMMAND, PRIO_MATCH, PRIO_APPLY, PRIO_POST = (
    0, 1, 2, 3, 4, 5, 9,
)


@dataclass(order=False)
class Event:
    seq: int
    ts: int
    prio: int
    kind: str

    def key(self) -> Tuple[int, int, int]:
        return (self.ts, self.prio, self.seq)


class CausalQueue:
    """按 (ts, prio, seq) 全序出队。"""

    def __init__(self):
        self._heap: List[Tuple[Tuple[int, int, int], int, Event]] = []
        self._n = 0

    def push(self, e: Event) -> None:
        heapq.heappush(self._heap, (e.key(), self._n, e))
        self._n += 1

    def pop(self) -> Optional[Event]:
        return heapq.heappop(self._heap)[2] if self._heap else None

    def __len__(self) -> int:
        return len(self._heap)


# ---------------------------------------------------------------- 摘要


def fnv1a(data: bytes) -> int:
    h = 0xCBF29CE484222325
    for b in data:
        h ^= b
        h = (h * 0x0100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def digest_events(events: List[Event]) -> int:
    buf = bytearray()
    buf += len(events).to_bytes(8, "little")
    for e in events:
        buf += e.seq.to_bytes(8, "little")
        buf += e.ts.to_bytes(8, "little")
        buf += e.prio.to_bytes(8, "little")
        buf += e.kind.encode()
        buf += b"\x1f"
    return fnv1a(bytes(buf))


# ---------------------------------------------------------------- 数据视图


@dataclass
class Bar:
    ts: int
    open: int
    high: int
    low: int
    close: int
    volume: int


class DataView:
    """point-in-time 可见性的唯一入口。"""

    def __init__(self, bars: List[Bar]):
        if any(left.ts >= right.ts for left, right in zip(bars, bars[1:])):
            raise ValueError("DataView requires strictly increasing timestamps")
        self.bars = list(bars)

    def as_of(self, ts: int) -> List[Bar]:
        """只返回 ts 及之前的数据——前视偏差在数据层封禁。"""
        return [b for b in self.bars if b.ts <= ts]


# ---------------------------------------------------------------- 质量门


def quality_check(bars: List[Bar]) -> List[str]:
    issues = []
    for i, b in enumerate(bars):
        if min(b.open, b.high, b.low, b.close, b.volume) < 0:
            issues.append(f"NegativePrice@{i}")
        if b.high < b.low:
            issues.append(f"HighLessThanLow@{i}")
        if not (b.low <= b.close <= b.high):
            issues.append(f"CloseOutOfRange@{i}")
        if b.volume == 0:
            issues.append(f"ZeroVolume@{i}")
        if i > 0 and b.ts <= bars[i - 1].ts:
            issues.append(f"NonMonotonic@{i}")
    return issues


# ---------------------------------------------------------------- 确定性 RNG


class DeterministicRng:
    """xorshift64* —— 与 Rust 实现同构。"""

    def __init__(self, seed: int):
        self.s = seed if seed else 0x9E3779B97F4A7C15

    def next_u64(self) -> int:
        x = self.s
        x ^= x >> 12
        x &= 0xFFFFFFFFFFFFFFFF
        x ^= (x << 25) & 0xFFFFFFFFFFFFFFFF
        x ^= x >> 27
        self.s = x
        return (x * 0x2545F4914F6CDD1D) & 0xFFFFFFFFFFFFFFFF

    def next_prob(self) -> int:
        return self.next_u64() % SCALE


# ---------------------------------------------------------------- 撮合模型

BUY, SELL = "BUY", "SELL"


def limit_ok(side: str, limit: Optional[int], px: int) -> bool:
    if limit is None:
        return True
    return px <= limit if side == BUY else px >= limit


class NextBarOpenFillModel:
    """下一根 bar 开盘价成交——结构上杜绝同 bar 作弊。"""

    name = "NextBarOpen"
    tier = "Bar"
    assumption = "假设下一根bar开盘可全部成交；不建模排队与容量"

    def fill(self, side, qty, limit, bar: Bar, rng) -> Optional[Tuple[int, int]]:
        if limit_ok(side, limit, bar.open):
            return bar.open, qty
        return None


class BestPriceFillModel:
    name = "BestPrice"
    tier = "Bar"
    assumption = "假设最优价无限流动性——乐观上界，不可用于容量评估"

    def fill(self, side, qty, limit, bar: Bar, rng) -> Optional[Tuple[int, int]]:
        if limit_ok(side, limit, bar.close):
            return bar.close, qty
        return None


class OneTickSlippageFillModel:
    name = "OneTickSlippage"
    tier = "Bar"
    assumption = "所有订单固定滑点一档——保守上界"

    def __init__(self, tick: int):
        self.tick = tick

    def fill(self, side, qty, limit, bar: Bar, rng) -> Optional[Tuple[int, int]]:
        px = bar.close + self.tick if side == BUY else bar.close - self.tick
        return (px, qty) if limit_ok(side, limit, px) else None


class ProbabilisticFillModel:
    name = "Probabilistic"
    tier = "L1"
    assumption = "触及限价按概率成交——建模L1下的不确定性"

    def __init__(self, prob_fill_on_limit: int, tick: int):
        self.p = prob_fill_on_limit
        self.tick = tick

    def fill(self, side, qty, limit, bar: Bar, rng) -> Optional[Tuple[int, int]]:
        if rng.next_prob() >= self.p:
            return None
        px = bar.close + self.tick if side == BUY else bar.close - self.tick
        return (px, qty) if limit_ok(side, limit, px) else None


class VolumeSensitiveFillModel:
    name = "VolumeSensitive"
    tier = "L2L3"
    assumption = "最优价容量=成交量×比例；需L2/L3支撑"

    def __init__(self, frac_bp: int):
        self.frac_bp = frac_bp

    def fill(self, side, qty, limit, bar: Bar, rng) -> Optional[Tuple[int, int]]:
        capacity = (bar.volume * self.frac_bp) // 10_000
        if capacity <= 0:
            return None
        q = min(qty, capacity)
        return (bar.close, q) if limit_ok(side, limit, bar.close) else None


# ---------------------------------------------------------------- 成本


class ZeroFee:
    def commission(self, qty, price, is_maker) -> int:
        return 0


class MakerTakerFee:
    def __init__(self, maker_bp: int, taker_bp: int):
        self.maker_bp, self.taker_bp = maker_bp, taker_bp

    def commission(self, qty, price, is_maker) -> int:
        notional = (qty * price) // SCALE
        bp = self.maker_bp if is_maker else self.taker_bp
        return (notional * bp) // 10_000


# ---------------------------------------------------------------- 回测


@dataclass
class Outcome:
    hash: int
    n_fills: int
    total_fee: int
    total_return: int
    max_drawdown: int
    final_equity: int


def max_drawdown(equity: List[int]) -> int:
    peak, mdd = 0, 0
    for e in equity:
        peak = max(peak, e)
        if peak > 0:
            mdd = max(mdd, (peak - e) * SCALE // peak)
    return mdd


def total_return(equity: List[int]) -> int:
    if not equity or equity[0] == 0:
        return 0
    return (equity[-1] - equity[0]) * SCALE // equity[0]


def gen_bars(n: int, seed: int) -> List[Bar]:
    rng = DeterministicRng(seed)
    bars, px = [], dec("100")
    for i in range(n):
        drift = (rng.next_u64() % 2001) - 1000
        px = max(px + drift * 1_000_000, SCALE)
        open_ = px
        close = px + ((rng.next_u64() % 1001) - 500) * 1_000_000
        high = max(open_, close) + (rng.next_u64() % 501) * 1_000_000
        low = max(min(open_, close) - (rng.next_u64() % 501) * 1_000_000, SCALE)
        volume = 1_000 + rng.next_u64() % 5_000
        bars.append(Bar((i + 1) * SCALE, open_, high, low, close, volume))
    return bars


def run_backtest(
    bars: List[Bar],
    seed: int,
    fast: int = 5,
    slow: int = 20,
    fill_model=None,
    fee_model=None,
) -> Outcome:
    fill_model = fill_model or NextBarOpenFillModel()
    fee_model = fee_model or MakerTakerFee(2, 5)

    view = DataView(bars)
    rng = DeterministicRng(seed)
    cash = dec("100000")
    pos = 0
    equity: List[int] = []
    pending: List[Tuple[str, int, Optional[int]]] = []  # (side, qty, limit)
    events: List[Event] = []
    seq = 0
    n_fills = 0
    total_fee = 0

    def ma(window: List[Bar]) -> int:
        return sum(b.close for b in window) // len(window)

    for i in range(slow + 1, len(bars)):
        hist = view.as_of(bars[i - 1].ts)  # 只用上一根及之前
        if len(hist) > slow:
            f_now, s_now = ma(hist[-fast:]), ma(hist[-slow:])
            f_prev, s_prev = ma(hist[-fast - 1:-1]), ma(hist[-slow - 1:-1])
            golden = f_prev <= s_prev and f_now > s_now
            death = f_prev >= s_prev and f_now < s_now
            if golden and pos == 0:
                pending.append((BUY, dec("10"), None))
            elif death and pos > 0:
                pending.append((SELL, dec("10"), None))

        # 本根 bar 撮合上一根提交的挂单
        still = []
        for (side, qty, limit) in pending:
            res = fill_model.fill(side, qty, limit, bars[i], rng)
            if res is None:
                still.append((side, qty, limit))
                continue
            px, q = res
            fee = fee_model.commission(q, px, limit is not None)
            notional = (q * px) // SCALE
            if side == BUY:
                cash -= notional + fee
                pos += q
            else:
                cash += notional - fee
                pos -= q
            n_fills += 1
            total_fee += fee
            events.append(Event(seq, bars[i].ts, PRIO_APPLY, f"FILL:{side}:{px}:{q}"))
            seq += 1
        pending = still
        equity.append(cash + (pos * bars[i].close) // SCALE)

    return Outcome(
        hash=digest_events(events),
        n_fills=n_fills,
        total_fee=total_fee,
        total_return=total_return(equity),
        max_drawdown=max_drawdown(equity),
        final_equity=equity[-1] if equity else 0,
    )


# ---------------------------------------------------------------- 自校验


def pct(x: int) -> str:
    return f"{x / SCALE * 100:.2f}%"


def main() -> None:
    print("牵星 Qianxing — 核心语义验证原型\n")

    bars = gen_bars(400, 20260910)
    issues = quality_check(bars)
    print(f"[观星 · 质量门] bars={len(bars)} 问题数={len(issues)}")
    assert not issues, issues

    a = run_backtest(bars, 42, 5, 20)
    b = run_backtest(bars, 42, 5, 20)
    c = run_backtest(bars, 42, 6, 20)
    d = run_backtest(bars, 42, 5, 20, fill_model=BestPriceFillModel())

    print(
        f"\n[星板 · 回测 A] 成交={a.n_fills} 手续费={show(a.total_fee)} "
        f"总收益={pct(a.total_return)} 最大回撤={pct(a.max_drawdown)} 终值={show(a.final_equity)}"
    )

    print("\n[更路 · 重放校验]")
    print(f"  ① 同输入两次运行哈希一致     : {a.hash == b.hash}")
    print(f"  ② 改参数(fast 5->6)哈希变化   : {a.hash != c.hash}")
    print(f"  ③ 改撮合模型哈希变化          : {a.hash != d.hash}")
    assert a.hash == b.hash, "相同输入必须产生相同结果"
    assert a.hash != c.hash, "改参数必须改变结果"
    assert a.hash != d.hash, "改模型必须改变结果"

    # 前视偏差验证：as_of 绝不返回未来数据
    view = DataView(bars)
    assert len(view.as_of(bars[0].ts)) == 1
    assert len(view.as_of(bars[9].ts)) == 10
    print("\n[观星 · PIT] as_of 只返回历史数据: ✓")

    # 因果优先级验证
    q = CausalQueue()
    q.push(Event(1, 100, PRIO_POST, "post"))
    q.push(Event(2, 100, PRIO_MARKET, "market"))
    q.push(Event(3, 50, PRIO_COMMAND, "cmd"))
    order = [q.pop().kind for _ in range(3)]
    print(f"[牵星 · 因果队列] 出队顺序={order}")
    assert order == ["cmd", "market", "post"]

    # 撮合模型语义
    bar = Bar(1, dec("100"), dec("110"), dec("90"), dec("105"), 1_000)
    r = DeterministicRng(1)
    assert NextBarOpenFillModel().fill(BUY, dec("10"), None, bar, r)[0] == dec("100")
    assert VolumeSensitiveFillModel(1000).fill(BUY, 10_000, None, bar, r)[1] == 100
    assert ProbabilisticFillModel(0, 0).fill(BUY, 1, None, bar, r) is None
    print("[星板 · 撮合模型] NextBarOpen/VolumeSensitive/Probabilistic: ✓")

    print("\n全部自校验通过 ✓")


if __name__ == "__main__":
    main()
