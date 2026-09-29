"""V13 第 8 轮 A6 的变异取证：七颗枪，逐颗放—跑—按 sha 还原。

这一批只放 A6 那七颗（排版侧 P1–P6 加命令侧跨面那颗 A1）。A5 那四颗与它的隔离对照住在
`a5_alert_render_guns.py`，两份日志不再混在同一份里——上一版一份脚本连放两批，A5 的锚点在
门禁那一份里撞到 2 次，整批在尾巴上抛异常退出，读者要自己去分哪几行属于哪一颗票面。

两条读法分开记：`cargo` 量"用例还有没有牙齿"，`门禁` 量"形状钉子还在不在"。A6 的两格写侧一直
在场、正文不念，所以删掉排版那行不会让别的断言跟着红——这正是"只有门禁看得见"的形状。

从仓根跑：`python -X utf8 maturity/evidence/v13-r8/a6_report_readout_guns.py`。
"""

import hashlib
import pathlib
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8")
ROOT = pathlib.Path(".")

READOUT = "crates/qx-cli/src/report_readout.rs"
UNIT = "crates/qx-cli/src/tests/report_readout.rs"
CLITEST = "crates/qx-cli/tests/report_readout_honesty.rs"
COMMANDS = "crates/qx-cli/src/config_commands.rs"
ARTIFACTS = "crates/qx-cli/src/backtests/artifacts.rs"
ALERTS = "deploy/prometheus/qianxing-alerts.yml"
GATEFILE = "tools/check_architecture.py"

GATE = ["python", "-X", "utf-8", "tools/check_architecture.py"]
CARGO_UNIT = ["cargo", "test", "-p", "qx-cli", "--bin", "qx-cli", "readout"]
CARGO_CLI = ["cargo", "test", "-p", "qx-cli", "--test", "report_readout_honesty"]
CARGO_COST = ["cargo", "test", "-p", "qx-cli", "--bin", "qx-cli", "backtest_cost_provenance"]

# 判据标题（用作"哪一颗红了"的归属），与门禁里的一致。
T_LAYOUT = "三格溯源由读侧一处印出"
T_WRITER = "写侧三键各只落一处"
T_TESTS = "排版侧与命令行侧各有一颗用例钉住这一行"
T_COMMANDS = "命令侧不再拼这三格"
T_RULES = "两条 R7 计数在告警规则里各占一条 expr"

# —— 锚点（按行写，脚本按各文件自身的行结束符拼接）——
PROV_CODE = [
    "        format!(",
    '            "  matching_kernel={} cost_source={} rejected_orders={}",',
    '            render_text(summary_text(summary, "/matching_kernel")),',
    '            render_text(summary_text(summary, "/execution_costs/source")),',
    '            render_number(summary_number(summary, "/rejected_orders"))',
    "        ),",
]
COMMENTED_PROV = ["        // " + line for line in PROV_CODE]
COST_WRITE = ['        "execution_costs": { "source": input.cost_source },']
UNIT_NAME = ["fn provenance_line_prints_kernel_cost_source_and_rejection_count_together() {"]
SUMMARY_PRINT = ['    println!("[Report] summary={}", summary_path.display());']
# A1 那一发的锚点不誊写：按告警名在规则文件里现找出整条规则，再自证它点的是哪一格计数。
API_ALERT = "QianxingApiConnectionRejections"
API_ALERT_METRIC = "qx_api_connections_rejected_total"


def load(rel: str) -> tuple[str, str]:
    text = (ROOT / rel).read_bytes().decode("utf-8")
    return text, ("\r\n" if "\r\n" in text else "\n")


def digest(rel: str) -> str:
    return hashlib.sha256((ROOT / rel).read_bytes()).hexdigest()[:16]


def alert_rule_lines(rel: str, alert: str, metric: str) -> list[str]:
    """返回 `alert:` 那一颗规则的完整块（含其后的空行），并钉住它点名的是 `metric`。"""
    text, eol = load(rel)
    lines = text.split(eol)
    starts = [n for n, line in enumerate(lines) if line.strip() == f"- alert: {alert}"]
    if len(starts) != 1:
        raise AssertionError(f"{rel} 里名为 {alert} 的规则命中 {len(starts)} 次")
    i = starts[0]
    block = [lines[i]]
    for line in lines[i + 1 :]:
        if line.strip() == "":
            block.append(line)
            break
        block.append(line)
    joined = "\n".join(block)
    if metric not in joined or f"- alert: {alert}" not in joined:
        raise AssertionError(f"{alert} 这条规则没有点名 {metric}")
    if sum(1 for line in block if "expr:" in line) != 1:
        raise AssertionError(f"{alert} 的块里 expr 行数不是 1：{block}")
    return block


GUNS: list[tuple[str, str, list[tuple[str, list[str], list[str]]], list[list[str]], str, str]] = [
    # (编号, 说明, 编辑列表, cargo 腿, 期望门禁标题, 期望 cargo)
    (
        "P1",
        "删掉 report_readout_lines 里那行三格溯源（正文少一栏）",
        [(READOUT, PROV_CODE, [])],
        [CARGO_UNIT, CARGO_CLI],
        T_LAYOUT,
        "red",
    ),
    (
        "P2",
        "把同一行代码原样搬进注释（文本在场、生产里没有）",
        [(READOUT, PROV_CODE, COMMENTED_PROV)],
        [CARGO_UNIT, CARGO_CLI],
        T_LAYOUT,
        "red",
    ),
    (
        "P3",
        "读法指针写错一格：/execution_costs/source → /execution_costs/sources",
        [
            (
                READOUT,
                ['            render_text(summary_text(summary, "/execution_costs/source")),'],
                ['            render_text(summary_text(summary, "/execution_costs/sources")),'],
            )
        ],
        [CARGO_UNIT, CARGO_CLI],
        T_LAYOUT,
        "red",
    ),
    (
        "P4",
        "写侧多落一颗同名的 execution_costs 键（读侧念的是后写的那一份）",
        [(ARTIFACTS, COST_WRITE, COST_WRITE + COST_WRITE)],
        [CARGO_COST, CARGO_UNIT, CARGO_CLI],
        T_WRITER,
        "green",
    ),
    (
        "P5",
        "改名排版侧那颗用例（用例照跑照过，名册读不到它）",
        [
            (
                UNIT,
                UNIT_NAME,
                [UNIT_NAME[0].replace("provenance_line_prints", "provenance_row_prints")],
            )
        ],
        [CARGO_UNIT],
        T_TESTS,
        "green",
    ),
    (
        "P6",
        "命令侧自己再拼一次 matching_kernel 那句话（第二份真值）",
        [
            (
                COMMANDS,
                SUMMARY_PRINT,
                ['    println!("[Report] summary={} matching_kernel", summary_path.display());'],
            )
        ],
        [CARGO_UNIT, CARGO_CLI],
        T_COMMANDS,
        "green",
    ),
    (
        "A1",
        "删掉点名 qx_api_connections_rejected_total 的那条告警规则（计数照样逐格写出，只是没有规则会响）",
        [(ALERTS, alert_rule_lines(ALERTS, API_ALERT, API_ALERT_METRIC), [])],
        [CARGO_UNIT],
        T_RULES,
        "green",
    ),
]


def apply_edit(rel: str, old_lines: list[str], new_lines: list[str]) -> None:
    text, eol = load(rel)
    old = eol.join(old_lines) + eol
    hits = text.count(old)
    assert hits == 1, f"{rel} 锚点出现 {hits} 次：{old_lines[0].strip()[:40]!r}"
    assert old != (eol.join(new_lines) + eol if new_lines else ""), f"{rel} 新块与旧块相同=空枪"
    new = (eol.join(new_lines) + eol) if new_lines else ""
    (ROOT / rel).write_bytes(text.replace(old, new, 1).encode("utf-8"))


def run(argv: list[str]) -> tuple[int, str]:
    done = subprocess.run(
        argv, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=900
    )
    return done.returncode, done.stdout + done.stderr


def cargo_verdict(log: str) -> tuple[str, str]:
    results = [line for line in log.splitlines() if line.startswith("test result:")]
    if not results:
        if "error[E" in log or "error: could not compile" in log:
            return "compile", next(line for line in log.splitlines() if "error" in line)[:80]
        return "noresult", log.strip()[-200:]
    failed = 0
    passed = 0
    for line in results:
        counts = re.findall(r"(\d+) (?:passed|failed|ignored)", line)
        if len(counts) < 2:
            return "noline", line[:80]
        passed += int(counts[0])
        failed += int(counts[1])
    state = "red" if failed else ("empty" if not passed else "green")
    return state, f"{len(results)} 腿 / 通过 {passed} / 失败 {failed}"


def gate_verdict(log: str) -> tuple[str, list[str]]:
    fails = [
        line.split(chr(0x2717), 1)[1].strip()
        for line in log.splitlines()
        if chr(0x2717) in line
    ]
    passed = [line for line in log.splitlines() if "[PASS]" in line]
    if "架构不变量自检全部通过" in log and not fails:
        return f"green/{len(passed)}", []
    return f"red/{len(fails)}", fails


print("== 分类器自检（真串来自本轮的实测输出）")
assert cargo_verdict("test result: FAILED. 13 passed; 1 failed; 0 ignored; 0 measured")[0] == "red"
assert cargo_verdict("test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured")[0] == "green"
assert cargo_verdict("test result: ok. 0 passed; 0 failed; 0 ignored")[0] == "empty"
assert cargo_verdict("error[E0061]: this function takes 3 arguments\ngenerated 1 error")[0] == "compile"
assert cargo_verdict("   Compiling qx-cli\ntest result: ok. 8 passed; 0 failed")[0] == "green"
assert gate_verdict("  " + chr(0x2717) + " 某条判据\n")[1] == ["某条判据"]
assert gate_verdict("架构不变量自检全部通过 ✓（815 项）\n[PASS] a\n")[0] == "green/1"
# P5 那颗的改名必须动到被钉的前缀，又不能撞上别的同名用例。
RENAMED = UNIT_NAME[0].replace("provenance_line_prints", "provenance_row_prints")
assert RENAMED != UNIT_NAME[0] and "provenance_line_prints" not in RENAMED
print("   五条 cargo 串 + 两条门禁串 + 改名自测，判定与预期一致")

FILES = sorted({rel for _, _, edits, _, _, _ in GUNS for rel, _, _ in edits} | {GATEFILE})
print("\n== stage-0 锚点普查")
viable = {}
for gun, _, edits, _, _, _ in GUNS:
    ok = True
    for rel, old, new in edits:
        text, eol = load(rel)
        hits = text.count(eol.join(old) + eol)
        noop = old == new
        print(f"  {gun} {pathlib.Path(rel).name} ×{hits}{' 空枪' if noop else ''} ← {old[0].strip()[:44]!r}")
        if hits != 1 or noop:
            ok = False
    viable[gun] = ok
base = {rel: digest(rel) for rel in FILES}
print(f"\n== 基线 sha（{len(base)} 份）")
for rel, sha in base.items():
    print(f"   {sha} {rel}")

rc, log = run(GATE)
gv, fails = gate_verdict(log)
print(f"\n== 基线门禁 {gv} rc={rc}")
if not gv.startswith("green"):
    print("   失败面：" + str([f[:40] for f in fails][:6]))
    print("BASELINE_NOT_GREEN：基线不绿就一颗枪都不放，整批按未放枪记账")
    raise SystemExit(2)

tally = {"KILLED": 0, "SURVIVED": 0, "未放枪": 0}
rows = []
for gun, note, edits, legs, want_gate, want_cargo in GUNS:
    if not viable[gun]:
        tally["未放枪"] += 1
        rows.append((gun, "未放枪", note, "锚点普查没过", "-"))
        print(f"{gun} 未放枪（锚点不是恰好一次，或是空枪）")
        continue
    saved = {rel: (ROOT / rel).read_bytes() for rel, _, _ in edits}
    for rel, old, new in edits:
        apply_edit(rel, old, new)
    cargo_line, cargo_state = "n/a", "green"
    if legs:
        states = []
        for argv in legs:
            rc, log = run(argv)
            state, detail = cargo_verdict(log)
            states.append(state)
            cargo_line = f"{state} rc={rc} {detail[:44]}"
            if state != "green":
                break
        cargo_state = "green" if all(s == "green" for s in states) else "red"
    rc, log = run(GATE)
    gv, fails = gate_verdict(log)
    hit = [f for f in fails if want_gate in f]
    killed = gv.startswith("red") and bool(hit)
    if want_cargo == "red":
        killed = killed and cargo_state == "red"
    if want_cargo == "green":
        killed = killed and cargo_state == "green"
    verdict = "KILLED" if killed else "SURVIVED"
    tally[verdict] += 1
    rows.append((gun, verdict, note, cargo_line, f"{gv} 命中={bool(hit)} 其余红={len(fails) - len(hit)}"))
    print(
        f"{gun} {verdict} 期望(cargo={want_cargo}, 门禁「{want_gate}」) | cargo: {cargo_line} | "
        f"门禁: {gv} rc={rc} 命中={hit[0][:58] if hit else (fails[:1] or '无红')}"
    )
    # 「其余红」不能只是一个数字：把每一颗红行的标题逐颗印出来，读者才知道这一发越界红了谁。
    others = [f[:40] for f in fails if f not in hit]
    print(f"   {gun} 的其余红行 {len(others)} 颗：{others}")
    for rel, blob in saved.items():
        (ROOT / rel).write_bytes(blob)
        assert digest(rel) == base[rel], f"{gun} 之后 {rel} 没回到基线"

print("\n== 合计")
for gun, verdict, note, cargo_line, gate_line in rows:
    print(f"  {gun} {verdict:9s} cargo={cargo_line:36s} 门禁={gate_line}")
names = [row[0] for row in rows]
assert len(names) == len(GUNS) == len(set(names)), f"判定行数与枪数不符：{names}"
assert (
    tally["KILLED"] + tally["SURVIVED"] + tally["未放枪"] == len(GUNS)
), f"分档算术不符：{tally}"
print("   ", tally, " 共", len(GUNS), "颗")
assert all(digest(rel) == sha for rel, sha in base.items()), "收尾 sha 不一致"
print("   收尾 sha 复核通过（全部文件回到基线）")
if tally["SURVIVED"] or tally["未放枪"]:
    print("A6_BATTERY_NOT_CLEAN")
    raise SystemExit(1)
print("A6_BATTERY_EXIT=0")
