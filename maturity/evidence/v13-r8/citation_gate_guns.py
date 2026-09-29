"""B4 落盘后的复射：两颗新判据各自配「会红」与「等价写法必须绿」的对照枪（任务 #216）。

八发都只动一行字节，放枪前量 baseline、每发按 sha 快照还原、收尾再量一次全绿。
判定形状：红必须是目标那颗判据红，别的判据替它说话不算咬到。

  裸名收窄（hint 只认整颗 crate 提及）
    L1 裸名 291 + 同行别颗锚点带路径 → 歧义红（原来是"越界到错文件"那一格替它说话）
    L2 裸名 241 + 同上             → 现在必须红：这一发正是修复前全绿的盲区
    L3 裸名 291 + 同行无提示        → 歧义红（对照：提示从来不是判据的依据）
    L4 裸名 241 + 同行无提示        → 歧义红（与 L2 只差提示一颗）
  逗号并列（尾巴第一次进判据）
    B1 尾巴 166 改成 2999           → 越界红
    B2 尾巴 166 改成 97（真空行）    → 空行红
    B3 名字 2 颗 / 行号 3 颗（含 1 颗逗号尾巴）→ 错位红
    B4 名字 2 颗 / 行号 2 颗（斜杠并列，无逗号）→ 必须绿：证明 B3 的红来自逗号而不是别的

用法（整跑约十余分钟，每发都要重跑一遍全门禁）：
    python -X utf8 maturity/evidence/v13-r8/citation_gate_guns.py > maturity/evidence/v13-r8/citation_gate_guns.txt
它只改 `docs/自研量化框架审计与重构方案-V13.md` 的 205/207/278 三行，每发结束立即按 sha 还原；
中途被打断会留下注入，先跑一次 `git diff --stat docs/自研量化框架审计与重构方案-V13.md` 确认干净。
"""
import hashlib
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]  # maturity/evidence/v13-r8/ → 仓根
V13 = "docs/自研量化框架审计与重构方案-V13.md"

L207 = '   `equity_for_with_spec_and_fx`）+ `qx-core/src/trading.rs:291` + `qx-xingban/src/backtest.rs:1072`。'
L205 = '5. **权益口径 8 个入口**：`qx-core/src/ledger/query.rs:119,123,156,165,205,260`（`equity`、'
L278 = '   （`crates/qx-api/src/lib.rs:58,82,102,107,118,131,159,166`）。在 HTTP 处理线程里这是"一个中毒 = 一个请求线程没了"的形状。'

# (编号, 说明, 行号, 原行, 新行, 期望, 目标红的 needles)
LEGS = [
    ("L1", "裸名 291 + 同行带路径的别颗锚点", 207, L207,
     '   `equity_for_with_spec_and_fx`）+ `trading.rs:291` + `qx-xingban/src/backtest.rs:1072`。',
     "RED", ["份同名候选"]),
    ("L2", "裸名 241 + 同行带路径的别颗锚点（修复前全绿的那一发）", 207, L207,
     '   `equity_for_with_spec_and_fx`）+ `trading.rs:241` + `qx-xingban/src/backtest.rs:1072`。',
     "RED", ["份同名候选"]),
    ("L3", "裸名 291 + 同行再无 crate 提及", 207, L207,
     '   `equity_for_with_spec_and_fx`）+ `trading.rs:291` + 星班回测的结算那一格。',
     "RED", ["份同名候选"]),
    ("L4", "裸名 241 + 同行再无 crate 提及", 207, L207,
     '   `equity_for_with_spec_and_fx`）+ `trading.rs:241` + 星班回测的结算那一格。',
     "RED", ["份同名候选"]),
    ("B1", "逗号尾巴改成越界行号", 278, L278,
     L278.replace("131,159,166", "131,159,2999"), "RED", ["越出 2566 行"]),
    ("B2", "逗号尾巴改成真空行", 278, L278,
     L278.replace("131,159,166", "131,159,97"), "RED", ["指过去的那一行不是空行"]),
    ("B3", "2 个名字对 3 颗行号（第三颗走逗号）", 205, L205,
     '5. **权益口径 8 个入口**：`equity`/`equity_for`（`qx-core/src/ledger/query.rs:119/156,165`（`equity`、',
     "RED", ["一颗对一颗"]),
    ("B4", "2 个名字对 2 颗行号（全走斜杠，无逗号）", 205, L205,
     '5. **权益口径 8 个入口**：`equity`/`equity_for`（`qx-core/src/ledger/query.rs:119/156`（`equity`、',
     "GREEN", []),
]

FAIL_RE = re.compile(r"^\s*✗(.*)$", re.M)
PASS_RE = re.compile(r"^\[PASS\]", re.M)


def read(rel: str):
    p = ROOT / rel
    raw = p.read_bytes()
    eol = b"\r\n" if b"\r\n" in raw else b"\n"
    return p, raw, eol, raw.split(eol)


def gate():
    r = subprocess.run([sys.executable, "-X", "utf8", str(ROOT / "tools/check_architecture.py")],
                       cwd=str(ROOT), capture_output=True, text=True, encoding="utf-8", errors="replace")
    out = r.stdout + r.stderr
    return r.returncode, len(PASS_RE.findall(out)), [m.strip() for m in FAIL_RE.findall(out)]


p, raw, eol, lines = read(V13)
SNAP = {V13: raw}
SHA = {V13: hashlib.sha256(raw).hexdigest()}

print("== 基线")
rc, npass, fails = gate()
print(f"  GATE_EXIT={rc} [PASS]={npass} ✗={len(fails)}")
assert rc == 0 and not fails, "基线不绿"

print("== 分类器自检")
assert len(FAIL_RE.findall("  ✗ 一颗\n     ✗ 两颗\n[PASS] 正常\n")) == 2, "分类器漏收"
assert not FAIL_RE.search("[PASS] 正常"), "分类器误收"
print("  OK")

print("== stage-0 锚点普查")
for tag, _, number, old, new, _, _ in LEGS:
    hit = lines[number - 1] == old.encode("utf-8")
    noop = old == new
    count = sum(1 for ln in lines if ln == old.encode("utf-8"))
    print(f"  {tag} 第 {number} 行 命中={hit} 全份唯一={count == 1}{' 空枪' if noop else ''}")
    assert hit and count == 1 and not noop, f"{tag} 锚点不过，不放枪"

tally = {}
for tag, desc, number, old, new, want, needles in LEGS:
    print(f"\n== {tag} {desc}（期望 {want}）")
    try:
        lines2 = list(lines)
        lines2[number - 1] = new.encode("utf-8")
        p.write_bytes(eol.join(lines2))
        rc, npass, fails = gate()
        tagged = [f for f in fails if any(n in f for n in needles)]
        other = [f for f in fails if f not in tagged]
        if want == "GREEN":
            tally[tag] = "GREEN（等价写法确实不该红）" if rc == 0 and not fails else "RED（对照枪多咬了）"
        else:
            tally[tag] = "KILLED" if tagged else "SURVIVED（没咬）"
        print(f"  GATE_EXIT={rc} [PASS]={npass} ✗={len(fails)} → {tally[tag]}")
        for f in tagged:
            print(f"    目标红：{f[:230]}")
        for f in other:
            print(f"    附带红：{f[:230]}")
    finally:
        p.write_bytes(SNAP[V13])
        now = hashlib.sha256(p.read_bytes()).hexdigest()
        assert now == SHA[V13], f"还原后 sha 不符：{now[:16]} != {SHA[V13][:16]}"
        print("  还原 sha 复核通过")

print("\n== 收尾复射")
rc, npass, fails = gate()
print(f"  GATE_EXIT={rc} [PASS]={npass} ✗={len(fails)}")
assert rc == 0 and not fails, "收尾门禁不绿"

print("\n== 判定汇总")
for tag, *_rest in LEGS:
    print(f"  {tag}: {tally[tag]}")
