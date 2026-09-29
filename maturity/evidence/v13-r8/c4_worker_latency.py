"""量 C4 这颗用例的两个真实时延：启动到答上话、关掉 stdin 到自收摊（V13 第 8 轮 C4，任务 #218）。

`python/tests/test_strategy_contract.py` 里那两颗常驻用例带两颗预算
（`SHARED_WORKER_REPLY_BUDGET_SECONDS`、`SHARED_WORKER_EXIT_BUDGET_SECONDS`）。预算不该手挑一个
数：它要大到容得下慢机器，又要小到"worker 变成孤儿"那颗回归不会把 CI 挂半小时。落盘前实测：

  1. 起真 worker（同一份 argv，`stdin` 是管道）→ 压一颗真请求 → 等应答，量"启动到答上话"；
  2. 关掉父进程这一侧的 stdin 写端 → 等进程退出，量"关 stdin 到退出"，并复核两份环文件已被补删。

跑 7 轮取中位数与最大值；预算按最大值留出量级余量（回复档留到秒级、退出档留到 5 s），
不是"看着顺眼"。在仓根执行：

    python -X utf8 maturity/evidence/v13-r8/c4_worker_latency.py > maturity/evidence/v13-r8/c4_worker_latency.txt
"""
import io
import json
import pathlib
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[3]  # maturity/evidence/v13-r8/ → 仓根
PY = ROOT / "python"
sys.path.insert(0, str(PY))
from qianxing_strategy.frame import REQUEST, RESPONSE, encode_frame, read_frame  # noqa: E402
from qianxing_strategy.ring import RingEmpty, SharedMemoryRing  # noqa: E402
from qianxing_strategy import StrategyBars, StrategyInput  # noqa: E402

CAP, SLOT = 8, 4096
ROUNDS = 7
MODULE = PY / "tests" / "fixtures" / "strategy_target.py"

REQUEST_OBJ = StrategyInput(
    request_id="request-1",
    strategy_id="strategy-1",
    strategy_version="v1",
    data_fingerprint="bars-1",
    as_of=10,
    instrument="BTCUSDT.BINANCE",
    positions={"BTCUSDT.BINANCE": 2},
    cash={"USDT": 100},
    available_margin_raw=90,
    risk_state="verified",
    research_targets={"BTCUSDT.BINANCE": 3},
    bars=StrategyBars("snapshot-1", (9, 10), (1, 2), (2, 3), (1, 2), (2, 3), (10, 11)),
)


def run_once() -> tuple[float, float, int, bool]:
    with tempfile.TemporaryDirectory() as directory:
        inp = pathlib.Path(directory) / "input.ring"
        outp = pathlib.Path(directory) / "output.ring"
        SharedMemoryRing.create(inp, CAP, SLOT).close()
        SharedMemoryRing.create(outp, CAP, SLOT).close()
        proc = subprocess.Popen(
            [
                sys.executable, "-m", "qianxing_strategy.worker",
                "--module", str(MODULE), "--protocol", "shared_memory_json",
                "--input-ring", str(inp), "--output-ring", str(outp),
                "--ring-capacity", str(CAP), "--ring-slot-bytes", str(SLOT),
            ],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=str(PY),
        )
        spawn = time.monotonic()
        frame = encode_frame(REQUEST, 1, REQUEST_OBJ.to_json().encode("utf-8"))
        with SharedMemoryRing(inp, CAP, SLOT) as writer:
            writer.push_wait(frame, time.monotonic() + 30)
        deadline = time.monotonic() + 30
        while True:
            with SharedMemoryRing(outp, CAP, SLOT) as reader:
                try:
                    encoded = reader.try_pop()
                    break
                except RingEmpty:
                    if time.monotonic() >= deadline:
                        raise AssertionError("worker 在 30 s 内没答这一颗请求")
                time.sleep(0.002)
        replied = time.monotonic()
        kind, seq, payload = read_frame(io.BytesIO(encoded))
        assert (kind, seq) == (RESPONSE, 1)
        reply = json.loads(payload)
        assert reply["ok"] and reply["output"]["target_qty"] == 3, reply
        proc.stdin.close()
        closed = time.monotonic()
        rc = proc.wait(timeout=60)
        exited = time.monotonic()
        gone = (not inp.exists()) and (not outp.exists())
        for stream in (proc.stdout, proc.stderr):
            stream.close()
        return replied - spawn, exited - closed, rc, gone


rows = [run_once() for _ in range(ROUNDS)]
print(f"轮数：{ROUNDS}（capacity={CAP}, slot_bytes={SLOT}，argv 与用例里那颗 helper 同一份）")
print("启动→答上话 (s):", [round(r[0], 3) for r in rows])
print("关 stdin→退出 (s):", [round(r[1], 3) for r in rows])
print("退出码:", sorted({r[2] for r in rows}), "两份环文件都被补删:", sorted({r[3] for r in rows}))
print(f"中位数  启动={statistics.median(r[0] for r in rows):.3f}  退出={statistics.median(r[1] for r in rows):.3f}")
print(f"最大值  启动={max(r[0] for r in rows):.3f}  退出={max(r[1] for r in rows):.3f}")
print("用例预算：SHARED_WORKER_REPLY_BUDGET_SECONDS=15、SHARED_WORKER_EXIT_BUDGET_SECONDS=5")
