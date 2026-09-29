"""第 8 轮 A4 变异取证：七颗枪，逐颗放—跑—按 sha 还原。

两条读法分开记：`cargo` 量"用例还有没有牙齿"，`门禁` 量"形状钉子还在不在"。
G3/G4/G5 三颗的预期是"用例照旧绿而门禁红"——那正是 A4 登记的缺口形状：夹具一退回预算值，
CAST/::numeric 与增量计数同时失去唯一读者，而测试自己不会喊。
"""

import hashlib
import pathlib
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8")
ROOT = pathlib.Path(".")
TEST = "crates/qx-storage/tests/outbox_backend_semantics.rs"
SQLITE = "crates/qx-storage/src/sqlite.rs"
CARGO = ["cargo", "test", "-p", "qx-storage", "--features", "sqlite", "--test",
         "outbox_backend_semantics", "--", "--nocapture"]
GATE = ["python", "-X", "utf-8", "tools/check_architecture.py"]

CHECK1 = "人工出口那颗 helper 在三本后端各自的用例里各被调用一次"
CHECK2 = "停摆夹具跨数位边界"
CHECK3 = "停摆条数按增量问库里的状态量"

POSTGRES_CALL = b'    assert_parked_outbox_has_an_operator_exit(&store, "postgres-park");\n'
SQLITE_CALL = b'    assert_parked_outbox_has_an_operator_exit(&store, "sqlite-park");\n'
STRADDLE_LOOP = b"(2, OUTBOX_MAX_ATTEMPTS + 6)"
STRADDLE_PAGE = b"attempts: OUTBOX_MAX_ATTEMPTS + 6,"
BEFORE_LINE = b"    let before = store.count_parked_outbox().unwrap();\n"
ORDER_CAST = b"ORDER BY CAST(e.attempts AS INTEGER) >= CAST(?2 AS INTEGER),"
COUNT_CAST = b"WHERE CAST(attempts AS INTEGER) >= CAST(?1 AS INTEGER)"

GUNS = [
    # (编号, 说明, [(文件, 旧, 新)], 期望：cargo / 门禁 谁红)
    ("G1", "删掉 postgres 用例里的 -park 调用点（A4 补的那颗执行者）",
     [(TEST, POSTGRES_CALL, b"")], (None, CHECK1)),
    ("G2", "把 -park 的 postgres 调用点挪进 sqlite 用例（文本在场、归属错位）",
     [(TEST, POSTGRES_CALL, b""), (TEST, SQLITE_CALL,
       SQLITE_CALL + POSTGRES_CALL)], (None, CHECK1)),
    ("G3", "park 夹具退回预算值（8 与 8，字典序与数字同一个答案）",
     [(TEST, STRADDLE_LOOP, b"(2, OUTBOX_MAX_ATTEMPTS)")], ("green", CHECK2)),
    ("G4", "page 夹具退回预算值（同上，退到页尾那一面失去分岔）",
     [(TEST, STRADDLE_PAGE, b"attempts: OUTBOX_MAX_ATTEMPTS,")], ("green", CHECK2)),
    ("G5", "把 `before` 写死成 0（独占库/目录时成立，共用 DSN 时先撞红）",
     [(TEST, BEFORE_LINE, b"    let before = 0;\n")], ("green", CHECK3)),
    ("M1", "摘掉 sqlite available() 的 CAST（退回按文本比）",
     [(SQLITE, ORDER_CAST, b"ORDER BY e.attempts >= ?2,")], ("red", None)),
    ("M2", "摘掉 sqlite count_parked() 的 CAST（退回按文本比）",
     [(SQLITE, COUNT_CAST, b"WHERE attempts >= ?1")], ("red", None)),
]


def digest(rel: str) -> str:
    return hashlib.sha256((ROOT / rel).read_bytes()).hexdigest()[:16]


def snapshot(rel: str) -> bytes:
    return (ROOT / rel).read_bytes()


def restore(rel: str, blob: bytes) -> None:
    (ROOT / rel).write_bytes(blob)
    assert digest(rel) == hashlib.sha256(blob).hexdigest()[:16], f"{rel} 还原失败"


def run(argv: list[str]) -> tuple[int, str]:
    done = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True,
                          encoding="utf-8", errors="replace")
    return done.returncode, done.stdout + done.stderr


def cargo_verdict(log: str) -> tuple[str, str]:
    for line in log.splitlines():
        if line.startswith("test result:"):
            # 只按 `N failed` 的计数定性：`1 failed; 0 ignored` 里同时有 "failed; 0" 这段子串，
            # 拿子串当"零失败"的判据会把红念成绿。
            counts = re.findall(r"(\d+) (?:passed|failed|ignored)", line)
            failed = int(counts[1]) if len(counts) >= 2 else -1
            return ("red" if failed else "green"), line.strip()
    if "error[" in log or "error:" in log:
        return "compile", [l for l in log.splitlines() if "error" in l][:2][0]
    return "noresult", log.strip()[-200:]


def gate_verdict(log: str) -> tuple[str, list[str]]:
    fails = [line.split(chr(0x2717), 1)[1].strip() for line in log.splitlines()
             if chr(0x2717) in line]
    pass_lines = [line for line in log.splitlines() if "[PASS]" in line]
    if "架构不变量自检全部通过" in log and not fails:
        return f"green/{len(pass_lines)}", []
    return f"red/{len(fails)}", fails


print("stage-0 锚点普查（每颗 needle 在目标文件里必须恰好出现一次）")
# 分类器自检：这两条串各来自一次真实输出，念错就会把红记成绿（本轮第一遍正是这样空的）。
assert cargo_verdict("test result: FAILED. 13 passed; 1 failed; 0 ignored; 0 measured")[0] == "red"
assert cargo_verdict("test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured")[0] == "green"
assert gate_verdict("  " + chr(0x2717) + " 某条判据\n")[1] == ["某条判据"]
assert gate_verdict("架构不变量自检全部通过 ✓（796 项）\n[PASS] a\n")[0] == "green/1"
census = {}
for gun, _, edits, _ in GUNS:
    for rel, old, _ in edits:
        blob = (ROOT / rel).read_bytes()
        hits = blob.count(old)
        census[(gun, rel, old[:28])] = hits
        print(f"  {gun} {rel.split('/')[-1]} …{old[:28]!r} → {hits} 次")
assert all(v == 1 for v in census.values()), f"有锚点不是恰好一次：{census}"
base = {rel: digest(rel) for rel in (TEST, SQLITE)}
print(f"基线 sha：{base}")

tally = {"KILLED": 0, "SURVIVED": 0, "未放枪": 0}
rows = []
for gun, note, edits, (want_cargo, want_gate) in GUNS:
    saved = {rel: snapshot(rel) for rel, _, _ in edits}
    for rel, old, new in edits:
        blob = snapshot(rel)
        assert blob.count(old) == 1, f"{gun} 锚点错位"
        (ROOT / rel).write_bytes(blob.replace(old, new, 1))
    cargo_line = gate_line = "-"
    if want_cargo is not None or want_gate is not None:
        rc, log = run(CARGO)
        cargo_verdict_, cargo_detail = cargo_verdict(log)
        names = [line.split()[1] for line in log.splitlines()
                 if line.startswith("test ") and "... FAILED" in line]
        cargo_line = f"{cargo_verdict_} rc={rc} {cargo_detail[:44]} 红腿={names[:3]}"
    if want_gate is not None:
        rc, log = run(GATE)
        gv, fails = gate_verdict(log)
        hit = [f for f in fails if want_gate in f]
        gate_line = f"{gv} rc={rc} 命中={bool(hit)} {hit[0][:60] if hit else (fails[:1] or '')}"
        killed = bool(hit) and gv.startswith("red")
    elif want_cargo is not None:
        killed = cargo_verdict_ == "red" if want_cargo == "red" else cargo_verdict_ == "green"
    else:
        killed = False
    # 用例侧的期望：M1/M2 要红，G3/G4/G5 要"照旧绿"（这才是缺口的证据）
    if want_cargo == "red":
        killed = killed and cargo_verdict_ == "red"
    if want_cargo == "green":
        killed = killed and cargo_verdict_ == "green"
    verdict = "KILLED" if killed else "SURVIVED"
    tally[verdict] += 1
    rows.append((gun, verdict, note, cargo_line, gate_line))
    for rel, blob in saved.items():
        restore(rel, blob)
    assert digest(rel) == base[rel], f"{gun} 之后 {rel} 没回到基线"
    print(f"{gun} {verdict} 期望({want_cargo},{want_gate}) | cargo: {cargo_line} | 门禁: {gate_line}")

print("\n合计：", tally, " 共", len(GUNS), "颗")
assert all(digest(rel) == sha for rel, sha in base.items()), "收尾 sha 不一致"
print("收尾 sha 复核通过（两份文件都回到基线）")
