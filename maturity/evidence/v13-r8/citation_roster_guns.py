"""V13 进名册那颗引用密度判据的变异枪（V13 第 8 轮 B 轮收尾，任务 #215）。

四发：
  G1 把 V13 第 207 行的 `qx-core/src/trading.rs:291` 还原成裸 `trading.rs:291`
     → 要红「同名候选无法核对」那一颗（歧义归属落在 V13 自己头上）。
  G2 把第 38 行的 `strategy_contract.rs:741` 改回 `config_commands.rs:469`
     → 文件在、行号越界，要红「越界」那一颗。
  G3 把 V13 的引用地板抬到**实测 + 1** → 要红「密度」那一颗。
  G4 把地板抬到**恰好等于实测** → 必须全绿：证明 G3 的红来自"越过真实颗数"，
     而不是"改门禁这一行本身把判据弄坏了"。

地板值不手抄：先用一发探测（抬到十亿）从判据自己的红行里读出真实颗数，再按读到的
数放 G3/G4。上一版把 90 写死在枪里，逗号尾巴进核后 V13 真实颗数从 69 涨到 93，
那一发就从「咬到」悄悄变成「没咬」——手抄的地板会腐烂，量出来的不会。

用法：在仓根执行 `python -X utf8 maturity/evidence/v13-r8/citation_roster_guns.py`，
输出即同目录 `citation_roster_guns.txt`（重定向 1> 落盘）。脚本会临时改写 V13 方案书与
门禁两份文件的字节，每发跑完立刻按内存里的基线字节还原并复核 sha；中途 Ctrl+C 会留下
被改过的一行，按 `git diff -- tools/check_architecture.py docs/自研量化框架审计与重构方案-V13.md`
看过后单独还原这两份。
"""
import hashlib
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]  # maturity/evidence/v13-r8/ → 仓根
DOC = "docs/自研量化框架审计与重构方案-V13.md"
GATE = "tools/check_architecture.py"
EOL = b"\r\n"
V13_TARGET = "自研量化框架审计与重构方案-V13.md"
FLOOR_RE = re.compile(r'^(    \("[^"]+", "[^"]+", )(\d+)(, \d+\).*)$')
FAIL_RE = re.compile(r"^\s*✗(.*)$", re.M)
PASS_RE = re.compile(r"^\[PASS\]", re.M)
COUNT_RE = re.compile(r"引用 (\d+) 处（地板 (\d+)），带名绑定 (\d+) 处（地板 (\d+)）")

BASE = {rel: (ROOT / rel).read_bytes() for rel in (DOC, GATE)}
for rel, raw in BASE.items():
    assert b"\n" not in raw.replace(EOL, b""), f"{rel} 混用换行，先修 EOL"


def raw_now(rel: str) -> bytes:
    return (ROOT / rel).read_bytes()


def splice(rel: str, old: str, new: str) -> None:
    """把 rel 里唯一一处 `old` 行换成 `new` 行（整行字节相等才算命中）。"""
    ob, nb = old.encode("utf-8"), new.encode("utf-8")
    assert ob != nb, f"空枪：{rel} ← {old[:50]!r}"
    lines = raw_now(rel).split(EOL)
    hits = [i for i, l in enumerate(lines) if l == ob]
    assert len(hits) == 1, f"锚点命中 {len(hits)} 次（期望 1）：{rel} ← {old[:60]!r}"
    lines[hits[0]] = nb
    (ROOT / rel).write_bytes(EOL.join(lines))


def restore(rel: str) -> None:
    (ROOT / rel).write_bytes(BASE[rel])
    got = hashlib.sha256(raw_now(rel)).hexdigest()
    want = hashlib.sha256(BASE[rel]).hexdigest()
    assert got == want, f"还原后 sha 不符：{rel}"


def gate():
    r = subprocess.run([sys.executable, "-X", "utf8", str(ROOT / GATE)],
                       cwd=str(ROOT), capture_output=True, text=True, encoding="utf-8", errors="replace")
    out = r.stdout + r.stderr
    return r.returncode, len(PASS_RE.findall(out)), [m.strip() for m in FAIL_RE.findall(out)]


def classifier_selftest() -> None:
    demo = ["  ✗ 某项（V13 方案书）", "     ✗ 另一项"]
    assert len(FAIL_RE.findall("\n".join(demo))) == 2, "分类器漏收"
    assert not FAIL_RE.search("[PASS] 一切正常"), "分类器误收 PASS 行"


def v13_roster_line() -> str:
    hits = [l.decode("utf-8") for l in raw_now(GATE).split(EOL)
            if l.startswith(b'    ("docs/') and V13_TARGET.encode() in l]
    assert len(hits) == 1, f"名册行命中 {len(hits)} 颗（期望 1）"
    return hits[0]


def floor_line(new_floor: int) -> tuple[str, str]:
    old = v13_roster_line()
    m = FLOOR_RE.match(old)
    assert m, f"名册行形状不认识，抬不动地板：{old[:70]}"
    return old, f"{m.group(1)}{new_floor}{m.group(3)}"


def report(fails, want_substr, label_red="目标红", label_other="附带红"):
    for f in fails:
        tag = label_red if want_substr in f else label_other
        print(f"    {tag}：{f[:170]}")


print("== 基线（放枪前必须全绿）")
rc, npass, fails = gate()
print(f"  GATE_EXIT={rc} [PASS]={npass} ✗={len(fails)}")
for f in fails:
    print(f"    ✗ {f}")
assert rc == 0 and not fails, "基线不绿，放枪会把别处的红算到变异头上"
classifier_selftest()
print("  分类器自检通过")

print("\n== 探测：把地板抬到十亿，从判据自己的红行里读出实测颗数（不手抄）")
try:
    probe_old, probe_new = floor_line(10 ** 9)
    print(f"  当前地板行：…{probe_old[30:78]}…")
    splice(GATE, probe_old, probe_new)
    rc, npass, fails = gate()
    dens = [f for f in fails if "引用数与带名绑定数" in f and "V13 方案书" in f]
    print(f"  GATE_EXIT={rc} [PASS]={npass} ✗={len(fails)}（期望恰好 1 颗密度红）")
    report(fails, "引用数与带名绑定数")
    assert len(dens) == 1, "密度读数不唯一，不能采信"
    m = COUNT_RE.search(dens[0])
    assert m, "红行里没有可解析的三格计数"
    measured_refs, measured_bound = int(m.group(1)), int(m.group(3))
    print(f"  实测：V13 引用 {measured_refs} 颗 / 带名绑定 {measured_bound} 颗")
finally:
    restore(GATE)
    print("  探测行已还原（sha 复核通过）")

tally = {}

DOC_GUNS = [
    (
        "G1 V13 溯源指环还原成裸名",
        "   `equity_for_with_spec_and_fx`）+ `qx-core/src/trading.rs:291` + `qx-xingban/src/backtest.rs:1072`。",
        "   `equity_for_with_spec_and_fx`）+ `trading.rs:291` + `qx-xingban/src/backtest.rs:1072`。",
        "只落在一份文件上",
    ),
    (
        "G2 V13 构造点清单还原成越界行号",
        "`strategy_contract.rs:741`、`venue_runtime/paper_worker.rs:29,122`）。",
        "`config_commands.rs:469`、`venue_runtime/paper_worker.rs:29,122`）。",
        "越出",
    ),
]

print("\n== stage-0 锚点普查（先数枪，不放枪）")
viable = {}
for label, old, new, needle in DOC_GUNS:
    hits = sum(1 for l in raw_now(DOC).split(EOL) if l == old.encode("utf-8"))
    noop = old == new
    print(f"  {label[:2]} {pathlib.Path(DOC).name} ×{hits}{' 空枪' if noop else ''} ← {old.strip()[:52]!r}")
    viable[label] = hits == 1 and not noop
g3_old, _ = floor_line(0)
print(f"  G3 {pathlib.Path(GATE).name} ×{sum(1 for l in raw_now(GATE).split(EOL) if l == g3_old.encode()):>1} ← 名册行可定位")
viable["G3"] = viable["G4"] = sum(1 for l in raw_now(GATE).split(EOL) if l == g3_old.encode()) == 1

for label, old, new, needle in DOC_GUNS:
    if not viable[label]:
        tally[label] = "未放枪"
        print(f"\n== {label}\n  锚点不过，跳过（不把 0 命中读成通过）")
        continue
    print(f"\n== {label}")
    try:
        splice(DOC, old, new)
        rc, npass, fails = gate()
        tagged = [f for f in fails if needle in f and "V13 方案书" in f]
        verdict = "KILLED" if tagged else "SURVIVED"
        tally[label] = verdict
        print(f"  GATE_EXIT={rc} [PASS]={npass} ✗={len(fails)} → {verdict}")
        report(fails, needle)
        if not tagged:
            print(f"    没有一颗红含「{needle}」且落在 V13 方案书 —— 这一枪没咬")
    finally:
        restore(DOC)
        print(f"  还原 {pathlib.Path(DOC).name} sha 复核通过")

if viable["G3"]:
    run_g3 = True
else:
    run_g3 = False
    tally["G3 地板抬到实测 + 1"] = "未放枪"
    tally["G4 地板抬到恰好等于实测（对照）"] = "未放枪"

if run_g3:
    for label, floor, expect_red in (
        ("G3 地板抬到实测 + 1", measured_refs + 1, True),
        ("G4 地板抬到恰好等于实测（对照）", measured_refs, False),
    ):
        print(f"\n== {label}")
        try:
            old, new = floor_line(floor)
            splice(GATE, old, new)
            rc, npass, fails = gate()
            dens = [f for f in fails if "引用数与带名绑定数" in f and "V13 方案书" in f]
            if expect_red:
                verdict = "KILLED" if dens else "SURVIVED"
                ok = bool(dens)
            else:
                ok = rc == 0 and not fails
                verdict = "GREEN（等价写法确实不该红）" if ok else "SURVIVED"
            tally[label] = "KILLED" if verdict == "KILLED" else ("GREEN" if ok else "SURVIVED")
            print(f"  GATE_EXIT={rc} [PASS]={npass} ✗={len(fails)} → {verdict}")
            report(fails, "引用数与带名绑定数")
            if not ok:
                print("    密度那一颗没按期望表态 —— 这一枪没咬")
        finally:
            restore(GATE)
            print("  还原 check_architecture.py sha 复核通过")

print("\n== 收尾 sha 复核")
for rel in (DOC, GATE):
    got = hashlib.sha256(raw_now(rel)).hexdigest()
    want = hashlib.sha256(BASE[rel]).hexdigest()
    print(f"  {'OK ' if got == want else 'NO '} {rel}")
    assert got == want

rc, npass, fails = gate()
print(f"\n== 还原后复跑：GATE_EXIT={rc} [PASS]={npass} ✗={len(fails)}")
assert rc == 0 and not fails, "还原后门禁没回到基线"
print("判定：", tally)
killed = sum(1 for v in tally.values() if v == "KILLED")
green = sum(1 for v in tally.values() if v == "GREEN")
uns = sum(1 for v in tally.values() if v == "未放枪")
surv = sum(1 for v in tally.values() if v == "SURVIVED")
print(f"合计 {len(tally)} 发：KILLED {killed} / GREEN（对照）{green} / SURVIVED {surv} / 未放枪 {uns}")
assert uns == 0, "有枪没放出去，不能算收口"
