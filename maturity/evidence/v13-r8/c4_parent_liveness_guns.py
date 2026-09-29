"""C4 那三格各自的变异枪：写端、轮询守卫、用例名册（V13 第 8 轮 C4，任务 #218）。

C4 的形状是"ring 传输带不来『父进程已经不在了』"。修复占三格，每格都可能被单独改回去而
另外两格仍然完好，所以逐颗放枪。两颗 oracle 各自跑：门禁 `tools/check_architecture.py`
与 Python 用例 `python/tests/test_strategy_contract.py`（整包 discover）。

  G1 `while not parent_gone.is_set():` → `while True:`（删掉轮询循环对存活事件的读取）
     → 用例必须红（关掉 stdin 后 worker 不自收摊，5s 预算内 wait 超时）**且**门禁必须红。
     这一发是行为牙齿：它证明那颗用例真会拒绝孤儿 worker。
  G2 把 `strategy_host.rs` 的三处 stdin 写法整体还原成修复前的形状（共享模式 Stdio::null()
     + 丢弃写端 + `!shared &&` 前置）→ 门禁必须红。用例这一面**必须绿**，且这不是漏放：
     Rust 侧没有任何常驻用例以共享传输起过 Python worker（下面 stage-0 实测），这一格的
     牙齿只有门禁，作为遗留限制登记。
  G3 `if parent_gone.is_set():` → `if True:`（等价变异：能走出那个循环只有这一条路）
     → 门禁必须红、用例必须绿。这一颗钉的是"补删只在存活事件那一格"这个形状，
     不是行为差异——正常析构路径上父进程自己删，子进程抢删会伤到父进程的环文件。
     G1 与 G3 各自咬门禁里不同的一颗（"循环读事件"与"补删绑事件"是两条判据，
     不是同一条条件写两遍），所以两发的目标红不会互相替说话。
  G4 把前向对照用例改名为 `_test_…`（unittest 不再收集）→ 门禁必须红（名册按名字核对）。
     用例这一面绿、只少一颗，说明"仍在服务"那格一旦消失，靠的是门禁名册而不是那条红。
  G5 删掉 runner 里那颗调用行 → `gate_self_honesty_check` 必须红（判据定义在却没人执行）。
  G6 不放枪的对照：两颗 oracle 都必须绿，证明前面五发的红来自注入本身。

判定形状：红必须是**目标那颗**判据红，别的判据替它说话不算咬到；每发按 sha 快照还原并复核，
收尾再量一次全绿。用法（每发都重跑门禁 + 整包用例）：

    python -X utf8 maturity/evidence/v13-r8/c4_parent_liveness_guns.py > maturity/evidence/v13-r8/c4_parent_liveness_guns.txt

中途被打断会留下注入：`git diff --stat crates/qx-cli/src/strategy_host.rs python/qianxing_strategy/worker.py
python/tests/test_strategy_contract.py tools/check_architecture.py` 确认后按 `git checkout --` 还原前三份，
第四份（门禁）正在被本轮别的改动使用，只能按本脚本的 sha 基线还原。
"""
import hashlib
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]  # maturity/evidence/v13-r8/ → 仓根
HOST = "crates/qx-cli/src/strategy_host.rs"
WORKER = "python/qianxing_strategy/worker.py"
CASES = "python/tests/test_strategy_contract.py"
GATE = "tools/check_architecture.py"
FILES = (HOST, WORKER, CASES, GATE)

BASE = {}
EOLS = {}
for rel in FILES:
    raw = (ROOT / rel).read_bytes()
    eol = b"\r\n" if b"\r\n" in raw else b"\n"
    assert b"\n" not in raw.replace(eol, b""), f"{rel} 混用换行，先修 EOL"
    BASE[rel] = raw
    EOLS[rel] = eol
    print(f"  EOL {rel}: {eol!r}（{len(raw.split(eol)) - 1} 行）")

FAIL_RE = re.compile(r"^\s*✗(.*)$", re.M)
PASS_RE = re.compile(r"^\[PASS\]", re.M)
RAN_RE = re.compile(r"^Ran (\d+) tests?.*$", re.M)
RESULT_RE = re.compile(r"^(OK|FAILED)(.*)$", re.M)


def raw_now(rel: str) -> bytes:
    return (ROOT / rel).read_bytes()


def sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()[:12]


def hit_count(rel: str, line: str) -> int:
    want = line.encode("utf-8")
    return sum(1 for l in raw_now(rel).split(EOLS[rel]) if l == want)


def splice(rel: str, old: str, new: str) -> None:
    """整行字节替换：命中必须恰好 1 次，且 old != new（空枪不算放过）。"""
    assert old != new, f"空枪：{rel} ← {old[:60]!r}"
    ob, nb = old.encode("utf-8"), new.encode("utf-8")
    lines = raw_now(rel).split(EOLS[rel])
    hits = [i for i, l in enumerate(lines) if l == ob]
    assert len(hits) == 1, f"锚点命中 {len(hits)} 次（期望 1）：{rel} ← {old[:70]!r}"
    lines[hits[0]] = nb
    (ROOT / rel).write_bytes(EOLS[rel].join(lines))


def splice_block(rel: str, olds: tuple[str, ...], news: tuple[str, ...]) -> None:
    """连续多行整体替换：这段 run 在文件里必须只出现一次。"""
    ob = [s.encode("utf-8") for s in olds]
    nb = [s.encode("utf-8") for s in news]
    lines = raw_now(rel).split(EOLS[rel])
    starts = [i for i in range(len(lines) - len(ob) + 1) if lines[i : i + len(ob)] == ob]
    assert len(starts) == 1, f"块锚点命中 {len(starts)} 次（期望 1）：{rel} ← {olds[0][:60]!r}"
    lines[starts[0] : starts[0] + len(ob)] = nb
    (ROOT / rel).write_bytes(EOLS[rel].join(lines))


def drop_line(rel: str, old: str) -> None:
    lines = raw_now(rel).split(EOLS[rel])
    hits = [i for i, l in enumerate(lines) if l == old.encode("utf-8")]
    assert len(hits) == 1, f"锚点命中 {len(hits)} 次（期望 1）：删行 {rel} ← {old[:70]!r}"
    del lines[hits[0]]
    (ROOT / rel).write_bytes(EOLS[rel].join(lines))


def restore(rel: str) -> None:
    (ROOT / rel).write_bytes(BASE[rel])
    assert raw_now(rel) == BASE[rel], f"还原后字节不符：{rel}"


def gate() -> tuple[int, int, list[str]]:
    r = subprocess.run(
        [sys.executable, "-X", "utf8", str(ROOT / GATE)],
        cwd=str(ROOT), capture_output=True, text=True, encoding="utf-8", errors="replace",
    )
    out = r.stdout + r.stderr
    return r.returncode, len(PASS_RE.findall(out)), [m.strip() for m in FAIL_RE.findall(out)]


def cases() -> tuple[int, int, str, list[str]]:
    """整包 Python 用例：返回 (退出码, Ran 的颗数, 收尾行, 失败/错误名册)。"""
    r = subprocess.run(
        [sys.executable, "-X", "utf8", "-m", "unittest", "discover", "-s", "python/tests", "-q"],
        cwd=str(ROOT), capture_output=True, text=True, encoding="utf-8", errors="replace",
    )
    out = r.stdout + r.stderr
    ran = RAN_RE.search(out)
    res = RESULT_RE.search(out)
    names = sorted({m for m in re.findall(r"^(?:FAIL|ERROR):\s+\S+", out, re.M)})
    return r.returncode, (int(ran.group(1)) if ran else -1), (res.group(0) if res else "？"), names


# (编号, 说明, 门禁目标红的 needle, 期望用例, 用例期望性质)
GUNS = [
    ("G1", "轮询循环不再读父进程存活事件", "worker 侧的轮询循环读父进程存活事件", "RED", "行为牙齿"),
    ("G2", "Rust 侧写端还原成修复前", "共享内存模式也保住子进程 stdin", "GREEN", "Rust 面无用例牙齿（见 stage-0）"),
    ("G3", "补删的守卫换成恒真（等价变异）", "补删环文件只走", "GREEN", "形状钉，非行为差异"),
    ("G4", "前向对照用例改名不再被收集", "两格常驻用例都在位", "GREEN", "名册钉"),
    ("G5", "删掉 runner 里那颗调用行", "判据", "GREEN", "门禁自检钉"),
]

HOST_OLD = (
    "            // 共享内存模式也留着这根 stdin 管道，并且不把写端丢掉：句柄活在 `self.stdin` 里，",
    "            // 父进程一退出（包括 `std::process::exit` 跳过 Drop 那几条路）OS 就关掉写端，",
    "            // 子进程读到 EOF 即自收摊。此前这里是 Stdio::null()，worker 只能干等 ring 里永远",
    "            // 不再来的下一颗请求，变成 1 kHz 永久自转的孤儿并留下两份 ring 文件（V13 C4）。",
    "            .stdin(Stdio::piped())",
    '        let stdin = child.stdin.take();',
    "        if stdin.is_none() || (!shared && stdout.is_none()) {",
)
HOST_NEW = (
    "            .stdin(if shared {",
    "                Stdio::null()",
    "            } else {",
    "                Stdio::piped()",
    "            })",
    "        let stdin = if shared { None } else { child.stdin.take() };",
    "        if !shared && (stdin.is_none() || stdout.is_none()) {",
)

W_LOOP = "        while not parent_gone.is_set():"
W_UNLINK_GUARD = "    if parent_gone.is_set():"
C_CONTROL_CASE = "    def test_shared_ring_worker_keeps_serving_while_parent_stdin_stays_open(self):"
G_CALL = "    strategy_ring_parent_liveness_check()"


def apply_gun(gid: str) -> None:
    if gid == "G1":
        splice(WORKER, W_LOOP, "        while True:")
    elif gid == "G2":
        for old in HOST_OLD:
            assert hit_count(HOST, old) == 1, f"G2 锚点在 {HOST} 命中异常：{old[:60]!r}"
        splice_block(HOST, HOST_OLD[:5], HOST_NEW[:5])
        splice(HOST, HOST_OLD[5], HOST_NEW[5])
        splice(HOST, HOST_OLD[6], HOST_NEW[6])
    elif gid == "G3":
        splice(WORKER, W_UNLINK_GUARD, "    if True:")
    elif gid == "G4":
        splice(CASES, C_CONTROL_CASE, C_CONTROL_CASE.replace("def test_", "def _test_"))
    elif gid == "G5":
        drop_line(GATE, G_CALL)
    else:
        raise AssertionError(gid)


def touched(gid: str) -> tuple[str, ...]:
    return {
        "G1": (WORKER,), "G2": (HOST,), "G3": (WORKER,), "G4": (CASES,), "G5": (GATE,),
    }[gid]


print("\n== stage-0-A：Rust 侧到底有没有以共享传输起过 Python worker 的用例（决定 G2 的定性）")
shared_arm = spawn_arm = 0
for path in sorted(ROOT.glob("crates/**/*.rs")):
    text = path.read_text(encoding="utf-8", errors="replace")
    under_test = "tests" in path.parts or path.name == "tests.rs"
    if under_test:
        shared_arm += len(re.findall(r"SharedMemory(?:Json|Columnar)", text))
    spawn_arm += len(re.findall(r'Command::new\("(?:python|python3|py)"\)|"-m", "qianxing_strategy', text))
print(f"  crates/**/tests 里出现 SharedMemoryJson/Columnar：{shared_arm} 处")
print(f"  crates/** 里以 argv 直接起 Python worker 的写法：{spawn_arm} 处")
assert shared_arm == 0 and spawn_arm == 0, "Rust 面出现了共享传输的用例，G2 的定性要改"
print("  → 实测 0：Rust 那一格只有门禁牙齿，用例面走不到（记入遗留限制）")

print("\n== stage-0-B：锚点普查（先数枪，不放枪）")
anchors = [
    ("G1", WORKER, [W_LOOP]),
    ("G2", HOST, list(HOST_OLD)),
    ("G3", WORKER, [W_UNLINK_GUARD]),
    ("G4", CASES, [C_CONTROL_CASE]),
    ("G5", GATE, [G_CALL]),
]
viable = {}
for gid, rel, lines in anchors:
    counts = [hit_count(rel, l) for l in lines]
    ok = all(c == 1 for c in counts)
    viable[gid] = ok
    print(f"  {gid} {pathlib.Path(rel).name:<24} 锚点 {len(lines)} 颗 ×命中 {counts} {'可放' if ok else '不通过 → 未放枪'}")

print("\n== 基线（放枪前两颗 oracle 都必须绿）")
rc, npass, fails = gate()
crc, cran, cres, cnames = cases()
print(f"  GATE_EXIT={rc} [PASS]={npass} ✗={len(fails)}")
for f in fails:
    print(f"    ✗ {f}")
print(f"  CASE_EXIT={crc} Ran={cran} {cres}")
for n in cnames:
    print(f"    {n}")
assert rc == 0 and not fails and crc == 0 and cran == 62, "基线不绿，放枪会把别处的红算到变异头上"
BASELINE_PASS, BASELINE_CASES = npass, cran

tally = {}
for gid, label, needle, expect_case, nature in GUNS:
    if not viable[gid]:
        tally[gid] = "未放枪"
        print(f"\n== {gid} {label}\n  锚点不过，跳过（不把 0 命中读成通过）")
        continue
    print(f"\n== {gid} {label}（{nature}）")
    try:
        apply_gun(gid)
        grc, gnp, gfails = gate()
        crc, cran, cres, cnames = cases()
        target = [f for f in gfails if needle in f]
        other = [f for f in gfails if needle not in f]
        gate_killed = grc != 0 and bool(target)
        case_red = crc != 0
        if expect_case == "RED":
            killed = gate_killed and case_red
        else:
            killed = gate_killed and not case_red
        tally[gid] = "KILLED" if killed else ("SURVIVED" if not gate_killed else "红错了格")
        print(f"  GATE_EXIT={grc} [PASS]={gnp}（基线 {BASELINE_PASS}）✗={len(gfails)} → 目标红 {len(target)} 颗 / 附带红 {len(other)} 颗")
        for f in target:
            print(f"    目标红：{f[:170]}")
        for f in other:
            print(f"    附带红：{f[:170]}")
        print(f"  CASE_EXIT={crc} Ran={cran}（基线 {BASELINE_CASES}）{cres}")
        for n in cnames:
            print(f"    {n}")
        if expect_case == "RED" and not case_red:
            print("    用例没有变红 —— 那颗常驻用例根本不咬孤儿 worker，G1 只是形状")
        if expect_case == "GREEN" and case_red:
            print(f"    用例如外变红：{cnames}")
        if not gate_killed:
            print(f"    没有一颗门禁红含「{needle}」—— 这一枪没咬")
        print(f"  判定：{tally[gid]}")
    finally:
        for rel in touched(gid):
            restore(rel)
            print(f"  还原 {pathlib.Path(rel).name} sha={sha(BASE[rel])} 复核通过")

print("\n== G6 不放枪对照：还原之后两颗 oracle 必须回到基线")
rc, npass, fails = gate()
crc, cran, cres, cnames = cases()
print(f"  GATE_EXIT={rc} [PASS]={npass} ✗={len(fails)} / CASE_EXIT={crc} Ran={cran} {cres}")
for f in fails:
    print(f"    ✗ {f}")
tally["G6"] = "GREEN" if (rc == 0 and not fails and crc == 0 and cran == BASELINE_CASES) else "SURVIVED"
assert tally["G6"] == "GREEN", "对照发不绿，前面五发的红就不可信"

print("\n== 收尾 sha 复核")
for rel in FILES:
    got, want = sha(raw_now(rel)), sha(BASE[rel])
    print(f"  {'OK ' if got == want else 'NO '} {rel} {got}")
    assert got == want

print("\n判定：", tally)
killed = sum(1 for v in tally.values() if v == "KILLED")
green = sum(1 for v in tally.values() if v == "GREEN")
uns = sum(1 for v in tally.values() if v == "未放枪")
surv = len(tally) - killed - green - uns
print(f"合计 {len(tally)} 发：KILLED {killed} / GREEN（对照）{green} / SURVIVED 或红错了格 {surv} / 未放枪 {uns}")
assert uns == 0 and surv == 0, "有枪没放出去或没咬，不能算收口"
