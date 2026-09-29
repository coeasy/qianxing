"""V13 第 8 轮 C7 的变异取证：四颗枪 + 一颗等价写法对照，各自带合计行。

这一批只管 C7 那四颗（摘掉 `timeout=` / 补一条无截止的第二调用 / 预算改成 0 / 超时按 skip 收）。
上一份落在 `c7_acceptance_timeout_guns.txt` 的字节不是这一批：那是 `scratch_r8/w98_guns_r8.py` 的
11 颗混编批次日志（C7 + A3 + B 同场），它自己的合计写着 `KILLED 10 / SURVIVED 1 / 未放枪 0`——
把 C7 那 4 行从别人的日志里数出来当本票取证，读的是行而不是批次，所以整份存档另放
`c7_acceptance_timeout_guns_run1_mixed-w98-dump.txt` 并在头注里写清作废理由。

三条读法各自界住一格，所以每一发都要单独点名：
  T_CALLS  量"唯一那条子进程调用带整体截止"——`len(calls) == 1` 与 `timeout=` 在场是同一颗判据的两张脸，
           所以 C7-1（摘掉截止）与 C7-2（补一条没截止的腿）各咬一次；
  T_BUDGET 量"预算是正数且超时被接住"；
  T_EXIT   量"挂住那一腿按失败退出码收口、阶段记录里留了那一格"。

对照那颗不还原缺陷，而是做一次**语义等价的写法改动**（把 `timeout=LEG_BUDGET_SECONDS` 挪到参数表首位）：
门禁必须照旧全绿。它证明 T_CALLS 读的是"这条调用带不带截止"，不是"`timeout=` 恰好排在最后一行"——
少了这一发，把判据换成整行字面匹配也能绿，而这正是这一票要防的形状。

注入体先过 `ast.parse` 再放枪：语法坏了门禁也会红，那一红不能记在判据牙齿上。
每条腿只跑门禁（不改行为的两颗红只可能来自门禁，改行为的两颗由阶段记录与退出码自己作保）。

从仓根跑：`python -X utf8 maturity/evidence/v13-r8/c7_acceptance_timeout_guns.py`。
"""

from __future__ import annotations

import ast
import hashlib
import pathlib
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8")
ROOT = pathlib.Path(__file__).resolve().parents[3]

ACC = "tools/binance_testnet_acceptance.py"
GATE = [sys.executable, "-X", "utf-8", "tools/check_architecture.py"]

T_CALLS = "验收脚本唯一那条子进程调用带整体截止"
T_BUDGET = "挂住那一腿按正数预算超时"
T_EXIT = "超时按失败退出码收口并先写进阶段记录"

RUN_BLOCK = [
    "        result = subprocess.run(",
    "            command,",
    "            capture_output=True,",
    "            text=True,",
    "            encoding=\"utf-8\",",
    "            timeout=LEG_BUDGET_SECONDS,",
    "        )",
]
RUN_BLOCK_REORDERED = [
    "        result = subprocess.run(",
    "            command,",
    "            timeout=LEG_BUDGET_SECONDS,",
    "            capture_output=True,",
    "            text=True,",
    "            encoding=\"utf-8\",",
    "        )",
]
FAIL_DEF = ["def fail(step: str, result: subprocess.CompletedProcess[str]) -> None:"]
SECOND_LEG = [
    "def _probe_leg(payload: Path) -> subprocess.CompletedProcess[str]:",
    '    """第二张没有截止的腿（取证注入，不在生产路径上）。"""',
    "    return subprocess.run([str(payload), \"--help\"], capture_output=True, text=True)",
    "",
    "",
] + FAIL_DEF

GUNS: list[tuple[str, str, list[str], list[str], str]] = [
    ("C7-1", "摘掉唯一那条调用的整体截止", ACC, ["            timeout=LEG_BUDGET_SECONDS,"], [], T_CALLS),
    (
        "C7-2",
        "补一条没有截止的第二调用（同一颗判据的第二张脸）",
        ACC,
        FAIL_DEF,
        SECOND_LEG,
        T_CALLS,
    ),
    ("C7-3", "预算从 180 改成 0（每一腿当场超时）", ACC, ["LEG_BUDGET_SECONDS = 180"], ["LEG_BUDGET_SECONDS = 0"], T_BUDGET),
    (
        "C7-4",
        "挂住那一腿的退出码从 failure 换成 skipped",
        ACC,
        ["            sys.exit(EXIT_FAILURE)"],
        ["            sys.exit(EXIT_SKIPPED)"],
        T_EXIT,
    ),
]
CONTROL = ("C7-C", "等价写法：timeout= 挪到参数表首位，门禁必须照旧全绿", ACC, RUN_BLOCK, RUN_BLOCK_REORDERED)


def load(rel: str) -> tuple[str, str]:
    text = (ROOT / rel).read_bytes().decode("utf-8")
    return text, ("\r\n" if "\r\n" in text else "\n")


def digest(rel: str) -> str:
    return hashlib.sha256((ROOT / rel).read_bytes()).hexdigest()[:16]


def hits(rel: str, lines: list[str]) -> int:
    if not lines:
        return (ROOT / rel).read_bytes().decode("utf-8").count("\n")
    text, eol = load(rel)
    return text.count(eol.join(lines) + eol)


def apply_edit(rel: str, old_lines: list[str], new_lines: list[str]) -> None:
    text, eol = load(rel)
    old = eol.join(old_lines) + eol
    found = text.count(old)
    assert found == 1, f"{rel} 锚点出现 {found} 次：{old_lines[0].strip()[:48]!r}"
    new = (eol.join(new_lines) + eol) if new_lines else ""
    assert old != new, f"{rel} 新块与旧块相同=空枪"
    (ROOT / rel).write_bytes(text.replace(old, new, 1).encode("utf-8"))


def parses(rel: str) -> bool:
    try:
        ast.parse((ROOT / rel).read_bytes().decode("utf-8"))
    except SyntaxError as exc:
        print(f"   注入后 {rel} 语法坏了：{exc.msg}")
        return False
    return True


def run_gate() -> tuple[int, list[str], int]:
    done = subprocess.run(GATE, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace")
    body = done.stdout + done.stderr
    titles = [line.strip()[2:].strip() for line in body.splitlines() if line.strip().startswith("✗")]
    passes = len([line for line in body.splitlines() if line.startswith("[PASS]")])
    return done.returncode, titles, passes


shots = [
    *[(tag, note, rel, old, new, "咬", want) for tag, note, rel, old, new, want in GUNS],
    (CONTROL[0], CONTROL[1], CONTROL[2], CONTROL[3], CONTROL[4], "对照", ""),
]

print("== 第 0 遍：整张枪表的锚点普查（任一颗不合格就一颗都不放）")
viable = {}
for tag, note, rel, old, new, kind, want in shots:
    anchor = hits(rel, old)
    new_hits = hits(rel, new) if new else 0
    noop = old == new
    print(f"  {tag} {kind} {pathlib.Path(rel).name} 旧块×{anchor} 新块×{new_hits}"
          f"{' 空枪' if noop else ''} ← {old[0].strip()[:48]!r}")
    viable[tag] = anchor == 1 and not noop and (kind == "对照" or bool(want))
    if not viable[tag]:
        continue
    saved = (ROOT / rel).read_bytes()
    try:
        apply_edit(rel, old, new)
        viable[tag] = parses(rel)
    finally:
        (ROOT / rel).write_bytes(saved)
if not all(viable.values()):
    print(f"BAD：{[t for t, ok in viable.items() if not ok]} 没通过锚点/语法普查，整批按未放枪停手")
    raise SystemExit(2)

base = {rel: digest(rel) for _t, _n, rel, _o, _w, _k, _e in shots}
print(f"\n== 基线 sha（{len(base)} 份）")
for rel, sha in base.items():
    print(f"   {sha} {rel}")

rc, titles, passes = run_gate()
print(f"\n== 基线门禁 rc={rc} [PASS]={passes} ✗={len(titles)} {titles[:3]}")
if rc != 0 or titles:
    print("BASELINE_NOT_GREEN：基线不绿就一颗枪都不放")
    raise SystemExit(2)

tally = {"KILLED": 0, "GREEN": 0, "SURVIVED": 0, "未放枪": 0}
rows: list[tuple[str, str, str, str]] = []
for tag, note, rel, old, new, kind, want in shots:
    saved = (ROOT / rel).read_bytes()
    apply_edit(rel, old, new)
    try:
        rc, titles, passes = run_gate()
    finally:
        (ROOT / rel).write_bytes(saved)
        assert digest(rel) == base[rel], f"{tag} 之后 {rel} 没回到基线"
    if kind == "对照":
        verdict = "GREEN" if rc == 0 and not titles else "SURVIVED"
        detail = f"rc={rc} [PASS]={passes} ✗={len(titles)} 等价写法后门禁{'全绿' if not titles else titles[:2]}"
    else:
        hit = [t for t in titles if want in t]
        others = [t for t in titles if t not in hit]
        verdict = "KILLED" if rc != 0 and hit else "SURVIVED"
        detail = f"rc={rc} [PASS]={passes} ✗={len(titles)} 命中={bool(hit)} 其余红={len(others)}"
        for face in others:
            print(f"   {tag} 的其余红行：{face[:72]}")
    tally[verdict] += 1
    rows.append((tag, verdict, note, detail))
    print(f"{tag} {verdict} 「{want or '等价写法对照'}」| {detail}")

print("\n== 合计")
for tag, verdict, note, detail in rows:
    print(f"  {tag} {verdict:9s} {note} | {detail}")
names = [row[0] for row in rows]
assert len(names) == len(shots) == len(set(names)), f"判定行数与发数不符：{names}"
fired = len(rows)
assert tally["KILLED"] + tally["GREEN"] + tally["SURVIVED"] + tally["未放枪"] == fired, f"分档算术不符：{tally}"
assert fired == len(GUNS) + 1, f"发数 {fired} 与枪表 {len(GUNS)} + 1 颗对照不符"
assert tally["GREEN"] == 1, f"对照那颗没绿：{tally}"
print(
    f"合计 {fired} 颗：KILLED {tally['KILLED']} / GREEN（对照）{tally['GREEN']} / "
    f"SURVIVED {tally['SURVIVED']} / 未放枪 {tally['未放枪']}"
)
assert all(digest(rel) == sha for rel, sha in base.items()), "收尾 sha 不一致"
print("   收尾 sha 复核通过（注入过的文件回到基线）")
if tally["SURVIVED"] or tally["未放枪"]:
    print("C7_BATTERY_NOT_CLEAN")
    raise SystemExit(1)
print("C7_BATTERY_EXIT=0")
