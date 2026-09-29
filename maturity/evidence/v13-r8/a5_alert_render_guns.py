"""V13 第 8 轮 A5 的变异取证：四颗枪 + 一颗隔离对照，各自带合计行。

这一批只管 A5 那四颗（渲染器改名 / 窗口错位 / 分隔符回到字面 \n / 探针指标只在注释里）。
A6 那七颗住在 `a6_report_readout_guns.py`——上一版一份脚本连放两批，跑到 A5 时锚点在门禁
那一份里撞到 2 次，整批在尾巴上抛异常退出，那份日志因此没有合计行。

那颗撞 2 次的锚点是这一份要交代的形状：`text = production_text(path.read_text(...))` 今天住在
两处（`account_money_field_registry_check` 与 `prometheus_exposition_check`），对照那颗要还原的是
后者——所以锚点带上自己那两行上下文，并在这里现读现证：三行块恰好一次、单行写法命中两次、
块所属的 def 名逐字对上。任何一条不对就整批不放枪。

两条读法分开记：`cargo` 量"用例还有没有牙齿"（每颗腿前重链一次，避开过期 binary 守卫替产品报红），
`门禁` 量"形状钉子还在不在"。A5 的注入全都不改行为，所以 cargo 必须照旧绿——红只可能来自门禁。

从仓根跑：`python -X utf8 maturity/evidence/v13-r8/a5_alert_render_guns.py`。
"""

import hashlib
import pathlib
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8")
ROOT = pathlib.Path(".")

PIPELINE = "crates/qx-cli/src/event_pipeline.rs"
ALERTS = "deploy/prometheus/qianxing-alerts.yml"
GATEFILE = "tools/check_architecture.py"

GATE = ["python", "-X", "utf-8", "tools/check_architecture.py"]
BUILD_NATS = ["cargo", "build", "-p", "qx-cli", "--features", "nats"]
CARGO_NATS = ["cargo", "test", "-p", "qx-cli", "--features", "nats", "--bin", "qx-cli"]

T_BODIES = "两格各住在自己的渲染器体内"
T_SHAPE = "的 render 用真换行分隔每一格"
T_OLD_RENDER = "Prometheus 告警规则点名的指标都有生产渲染器"

BACKSLASH = chr(92)
RELAY_PARKED = ['qx_outbox_relay_parked{{worker=\\"{worker}\\"}} {}\\n",']
CONSUMER_DEAD = ['qx_event_consumer_dead_lettered_total{{worker=\\"{worker}\\"}} {}\\n\\']
PARKED_AS_CONT = [RELAY_PARKED[0][:-2] + BACKSLASH]
DEAD_AS_TAIL = [CONSUMER_DEAD[0][:-1] + '",']
RELAY_APPLY = ["    fn apply(&mut self, report: &qx_storage::OutboxRelayReport) {"]
ALERT_TAIL = [
    '          description: "worker={{ $labels.worker }} has {{ $value }} events parked past the '
    'retry budget; the relay head stays blocked until an operator resolves them."',
]
PROBE_NAME = "qx_comment_only_metric_total"
PROBE_RULE = [
    "      - alert: QianxingCommentOnlyProbe",
    f"        expr: {PROBE_NAME} > 0",
    "        for: 1m",
    "        labels:",
    "          severity: warning",
    "          service: qianxing",
    "        annotations:",
    '          summary: "probe: a metric named only inside a comment"',
    '          description: "probe"',
]
PROBE_COMMENT = [f"    // 探针：{PROBE_NAME} 只出现在这一行注释里，没有任何渲染器写出它。"]
GATE_RAW_ANCHOR = [
    "        if not production_rust_source(path):",
    "            continue",
    '        text = production_text(path.read_text(encoding="utf-8"))',
]
GATE_RAW_REVERT = [
    "        if not production_rust_source(path):",
    "            continue",
    '        text = path.read_text(encoding="utf-8")',
]
ANCHOR_OWNER = "prometheus_exposition_check"

GUNS = [
    (
        "A2",
        "渲染器里改名：qx_outbox_relay_parked → _paused（规则点到一格空名）",
        [(PIPELINE, RELAY_PARKED, [RELAY_PARKED[0].replace("_parked", "_paused")])],
        T_BODIES,
    ),
    (
        "A3",
        "parked 整格挪进 Consumer 的 render、dead_lettered 挪回 Relay（名字全在场、窗口错位）",
        [(PIPELINE, RELAY_PARKED, DEAD_AS_TAIL), (PIPELINE, CONSUMER_DEAD, PARKED_AS_CONT)],
        T_BODIES,
    ),
    (
        "A4",
        "parked 那一格的分隔符改回两字符的字面反斜杠加 n（R7-i 的同族形状）",
        [
            (
                PIPELINE,
                RELAY_PARKED,
                ['qx_outbox_relay_parked{{worker=\\"{worker}\\"}} {}\\\\n",'],
            )
        ],
        T_SHAPE,
    ),
    (
        "A5",
        "探针：规则点名的指标只出现在注释里（验 production_text 那一行有没有牙齿）",
        [
            (ALERTS, ALERT_TAIL, ALERT_TAIL + [""] + PROBE_RULE),
            (PIPELINE, RELAY_APPLY, PROBE_COMMENT + RELAY_APPLY),
        ],
        T_OLD_RENDER,
    ),
]


def load(rel: str) -> tuple[str, str]:
    text = (ROOT / rel).read_bytes().decode("utf-8")
    return text, ("\r\n" if "\r\n" in text else "\n")


def digest(rel: str) -> str:
    return hashlib.sha256((ROOT / rel).read_bytes()).hexdigest()[:16]


def hits(rel: str, lines: list[str]) -> int:
    text, eol = load(rel)
    return text.count(eol.join(lines) + eol)


def owner_of(rel: str, lines: list[str]) -> str:
    """锚点块所属的 def 名——用块自己的行号往上找，不看任何文档口径。"""
    text, eol = load(rel)
    needle = eol.join(lines)
    start = text.count("\n", 0, text.index(needle)) + 1
    body = text.split("\n")
    for n in range(start - 1, 0, -1):
        m = re.match(r"^def (\w+)\(", body[n - 1].rstrip("\r"))
        if m:
            return m.group(1)
    return "<顶层>"


def apply_edit(rel: str, old_lines: list[str], new_lines: list[str]) -> None:
    text, eol = load(rel)
    old = eol.join(old_lines) + eol
    found = text.count(old)
    assert found == 1, f"{rel} 锚点出现 {found} 次：{old_lines[0].strip()[:44]!r}"
    new = (eol.join(new_lines) + eol) if new_lines else ""
    assert old != new, f"{rel} 新块与旧块相同=空枪"
    (ROOT / rel).write_bytes(text.replace(old, new, 1).encode("utf-8"))


def run(argv: list[str]) -> tuple[int, str]:
    done = subprocess.run(
        argv,
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=1200,
    )
    return done.returncode, done.stdout + done.stderr


def cargo_verdict(log: str) -> tuple[str, str]:
    results = [line for line in log.splitlines() if line.startswith("test result:")]
    if not results:
        if "error[E" in log or "error: could not compile" in log:
            return "compile", next(line for line in log.splitlines() if "error" in line)[:80]
        return "noresult", log.strip()[-160:]
    passed = failed = 0
    for line in results:
        counts = re.findall(r"(\d+) (?:passed|failed|ignored)", line)
        if len(counts) < 2:
            return "noline", line[:80]
        passed += int(counts[0])
        failed += int(counts[1])
    stale = log.count("比 ") + log.count("请先 cargo build")
    return (
        "red" if failed else ("green" if passed else "empty"),
        f"{len(results)} 腿 / 通过 {passed} / 失败 {failed} / 过期守卫命中 {stale}",
    )


def gate_verdict(log: str) -> tuple[str, list[str]]:
    fails = [
        line.split(chr(0x2717), 1)[1].strip() for line in log.splitlines() if chr(0x2717) in line
    ]
    passed = [line for line in log.splitlines() if "[PASS]" in line]
    if "架构不变量自检全部通过" in log and not fails:
        return f"green/{len(passed)}", []
    return f"red/{len(fails)}", fails


print("== 分类器自检")
assert cargo_verdict("test result: FAILED. 13 passed; 1 failed")[0] == "red"
assert cargo_verdict("test result: ok. 259 passed; 0 failed")[0] == "green"
assert cargo_verdict("test result: ok. 0 passed; 0 failed")[0] == "empty"
assert cargo_verdict("error[E0308]: mismatched types")[0] == "compile"
assert cargo_verdict("请先 cargo build 再跑一次\n")[0] == "noresult"
assert gate_verdict("架构不变量自检全部通过 ✓\n[PASS] x\n")[0] == "green/1"
assert gate_verdict("  " + chr(0x2717) + " 某条判据\n")[1] == ["某条判据"]
assert DEAD_AS_TAIL[0].endswith(BACKSLASH + "n" + '",')
assert PARKED_AS_CONT[0].endswith(BACKSLASH + "n" + BACKSLASH)
print("   五条 cargo 串 + 两条门禁串 + A3 拼接，判定与预期一致")

print("\n== stage-0 锚点普查（含对照那颗的归属自证）")
one_line = hits(GATEFILE, GATE_RAW_ANCHOR[2:])
three = hits(GATEFILE, GATE_RAW_ANCHOR)
print(f"   {GATEFILE}：单行读法 ×{one_line}、三行上下文 ×{three}")
if three != 1:
    print("ANCHOR_BAD：对照那颗的锚点不是恰好一次，整批按未放枪记账")
    raise SystemExit(2)
owner = owner_of(GATEFILE, GATE_RAW_ANCHOR)
print(f"   三行块属于 def {owner}()（要 {ANCHOR_OWNER}）；还原块在场 ×{hits(GATEFILE, GATE_RAW_REVERT)}（要 0）")
if owner != ANCHOR_OWNER or hits(GATEFILE, GATE_RAW_REVERT) != 0:
    print("ANCHOR_BAD：还原的不是「有生产渲染器」那一颗的读者")
    raise SystemExit(2)
if one_line < 2:
    print(f"ANCHOR_BAD：单行读法今天只命中 {one_line} 次，那份『两处同名读者』的登记要改写")
    raise SystemExit(2)

viable = {}
for gun, _, edits, _ in GUNS:
    ok = True
    for rel, old, new in edits:
        found = hits(rel, old)
        noop = old == new
        print(f"  {gun} {pathlib.Path(rel).name} ×{found}{' 空枪' if noop else ''} ← {old[0].strip()[:44]!r}")
        ok = ok and found == 1 and not noop
    viable[gun] = ok

FILES = [PIPELINE, ALERTS, GATEFILE]
base = {rel: digest(rel) for rel in FILES}
print(f"\n== 基线 sha（{len(base)} 份）")
for rel, sha in base.items():
    print(f"   {sha} {rel}")

rc, log = run(GATE)
gv, fails = gate_verdict(log)
print(f"\n== 基线门禁 {gv} rc={rc}")
if not gv.startswith("green"):
    print("   失败面：" + str([f[:40] for f in fails][:6]))
    print("BASELINE_NOT_GREEN：基线不绿就一颗枪都不放")
    raise SystemExit(2)

print("\n== 干净树的 nats 整跑基线（先重链，再整跑）")
rc_build, build_log = run(BUILD_NATS)
print(f"   build rc={rc_build}")
rc, log = run(CARGO_NATS)
state, detail = cargo_verdict(log)
guard = [line for line in log.splitlines() if "比 " in line and "旧" in line]
print(f"   基线 cargo {state} rc={rc} {detail} 过期守卫行={len(guard)}")
if state != "green" or guard:
    print("BASELINE_NOT_GREEN：干净树上的 nats 整跑不绿，后面的『照旧绿』不能作数")
    raise SystemExit(2)

tally = {"KILLED": 0, "SURVIVED": 0, "未放枪": 0}
rows = []
fails_of: dict[str, list[str]] = {}
control = False
for gun, note, edits, want_gate in GUNS:
    if not viable[gun]:
        tally["未放枪"] += 1
        rows.append((gun, "未放枪", "锚点普查没过", "-"))
        print(f"{gun} 未放枪（锚点不是恰好一次，或是空枪）")
        continue
    saved = {rel: (ROOT / rel).read_bytes() for rel, _, _ in edits}
    for rel, old, new in edits:
        apply_edit(rel, old, new)
    rc_build, build_log = run(BUILD_NATS)
    rc, log = run(CARGO_NATS)
    state, detail = cargo_verdict(log)
    rcg, glog = run(GATE)
    gv, fails = gate_verdict(glog)
    fails_of[gun] = list(fails)
    hit = [f for f in fails if want_gate in f]
    killed = gv.startswith("red") and bool(hit) and state == "green"
    verdict = "KILLED" if killed else "SURVIVED"
    tally[verdict] += 1
    rows.append(
        (
            gun,
            verdict,
            f"cargo={state} rc={rc} {detail[:44]} build_rc={rc_build}",
            f"{gv} 命中={bool(hit)} 其余红={len(fails) - len(hit)}",
        )
    )
    print(
        f"{gun} {verdict} 「{want_gate}」| cargo {state} rc={rc} {detail} | 门禁 {gv} rc={rcg} "
        f"命中={hit[0][:56] if hit else (fails[:1] or '无红')}"
    )
    # 「其余红」不能只是一个数字：把每一颗红行的标题逐颗印出来，读者才知道这一发越界红了谁。
    others = [f[:40] for f in fails if f not in hit]
    print(f"   {gun} 的其余红行 {len(others)} 颗：{others}")
    for rel, blob in saved.items():
        (ROOT / rel).write_bytes(blob)
        assert digest(rel) == base[rel], f"{gun} 之后 {rel} 没回到基线"

print("\n== 隔离对照 A5-C：同一发注入 + 门禁那一行还原成整份文本读法，「有生产渲染器」必须不红")
print(
    "   对照的判据是集合相等：对照红的面 = A5 那一发红的面 减掉「有生产渲染器」自己，"
    "多一颗少一颗都不算通过"
)
saved = {rel: (ROOT / rel).read_bytes() for rel in FILES}
a5_fails = fails_of.get("A5")
a5_others = None if a5_fails is None else [f for f in a5_fails if T_OLD_RENDER not in f]
if a5_fails is None or len(a5_fails) == len(a5_others):
    print("   A5 那一发没在「有生产渲染器」上留下红行，对照没有可比的面")
    control = False
else:
    try:
        apply_edit(ALERTS, ALERT_TAIL, ALERT_TAIL + [""] + PROBE_RULE)
        apply_edit(PIPELINE, RELAY_APPLY, PROBE_COMMENT + RELAY_APPLY)
        apply_edit(GATEFILE, GATE_RAW_ANCHOR, GATE_RAW_REVERT)
        rc, log = run(GATE)
        gv, fails = gate_verdict(log)
        old_face = [f for f in fails if T_OLD_RENDER in f]
        others = [f for f in fails if T_OLD_RENDER not in f]
        print(f"   对照：门禁 {gv} rc={rc}｜「有生产渲染器」红={bool(old_face)}（期望 False）")
        print(f"   对照红 {len(others)} 颗 vs A5 那发的其余红 {len(a5_others)} 颗")
        for face in sorted(set(others) ^ set(a5_others)):
            print(f"     差集：{face[:52]}")
        control = not old_face and others == a5_others
    finally:
        for rel, blob in saved.items():
            (ROOT / rel).write_bytes(blob)
            assert digest(rel) == base[rel], f"对照之后 {rel} 没回到基线"

print("\n== 合计")
for gun, verdict, cargo_line, gate_line in rows:
    print(f"  {gun} {verdict:9s} {cargo_line} | 门禁 {gate_line}")
names = [row[0] for row in rows]
assert len(names) == len(GUNS) == len(set(names)), f"判定行数与枪数不符：{names}"
assert (
    tally["KILLED"] + tally["SURVIVED"] + tally["未放枪"] == len(GUNS)
), f"分档算术不符：{tally}"
print("   ", tally, " 共", len(GUNS), "颗；A5-C 通过 =", control)
assert all(digest(rel) == sha for rel, sha in base.items()), "收尾 sha 不一致"
print("   收尾 sha 复核通过（三份文件回到基线）")
if tally["SURVIVED"] or tally["未放枪"] or not control:
    print("A5_BATTERY_NOT_CLEAN")
    raise SystemExit(1)
print("A5_BATTERY_EXIT=0")
