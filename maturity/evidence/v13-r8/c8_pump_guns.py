r"""C8 的变异枪：响应泵的三格形状 + 那七颗常驻用例的两格归属（V13 第 8 轮 C8，任务 #214）。

C8 的形状是一句话的三个部分——"worker 不守规矩时，父进程不替它兜账"：队列有界、单行有上限、
迟到答复在写入前排空。修复落在三格（`strategy_host.rs` 的通道与 JSONL 支、`request` 里的排空、
`tests/mod.rs` 的挂载），钉它的判据是门禁的两颗 `strategy_pump_bounds_check`，跑它行为的是
`crates/qx-cli/src/tests/strategy_pump_bounds.rs` 的七颗常驻用例。两个 oracle 各自放、各自读：

  门禁 oracle = `python -X utf8 tools/check_architecture.py`（纯文本判据，42 s/轮）
  用例 oracle = `cargo test -p qx-cli --bins strategy_pump_bounds`（行为判据，7 颗/轮）

G1 通道还原成无界 `mpsc::channel()` → 门禁必须红（两格同时瞎）。
G2 把有界常量换成字面量 64（数字对、名字不对）→ 门禁必须红：钉的是"取那颗常量"，不是"有个数"。
G3 JSONL 支退回 `BufRead::lines()` → 门禁必须红。注入本身不编译，所以用例 oracle 记编译层红。
G4 单行上限从共享函数改成现场字面量 `usize::MAX` → 门禁必须红：上限必须只有一个来源。
G5 排空调用被掏空（`as_ref()` 不再 map）→ 门禁必须红（排空不在写入之前）。用例 oracle 必须绿：
   这一格在 Rust 面没有行为反例——`drain_stale_responses` 的行为另有 G6/T5 钉，这里钉的是位置。
G6 排空整句挪到写入之后（两行换位，行数不变）→ 门禁必须红（顺序判据）。
G7 把通道那行整体注释掉，针尖只活在注释里 → 门禁必须红。这一颗专门验 `production_text` 真的
   剥注释：若它绿，说明针尖能被一行注释冒充，判据当场作废。
G8 挂载行 `mod strategy_pump_bounds;` 注释掉 → 门禁必须红（归属格），而用例 oracle 必须报
   `running 0 tests`：没挂进模块的文件既不入编译面也不入执行面，七颗用例对执行面贡献 0 颗。
   这正是"名册格杀 0 颗"的形状——牙齿只在门禁一侧，所以门禁必须有名册格。
G9 把一颗用例改名（针尖名不在）→ 门禁必须红（逐颗点名），用例 oracle 必须绿且仍是 7 颗：
   改名不影响执行，证明"数总数"式判据会放过这一发，逐颗点名才不会。
T1 上限常量 +1 → 用例必须红（`jsonl_line_cap_reuses_the_frame_budget`），门禁必须绿。
T2 上限比较改成 `cap * 8` → 用例必须红（超长行被接受），门禁必须绿。
T3 行终止判据强制为真 → 用例必须红（无换行尾行被截掉一颗字节 + 跨窗口拼行被提前收尾），门禁必须绿。
T4 CR 剔除认错了字节 → 用例必须红（`\r` 漏进文本），门禁必须绿。
T5 排空读到第一颗就返回 → 用例必须红（清 0 颗 ≠ 2 颗），门禁必须绿。
C0/C10 不放枪对照：两个 oracle 都必须绿，证明前面各发的红来自注入本身。

判定形状：门禁的红必须含目标那两颗标签之一，别的判据替它说话不算咬到；用例的红必须有
`test result:` 行（编译层红不算行为牙齿，但会照实记录在案）。每发按 sha 基线还原并复核。

用法：
    python -X utf8 maturity/evidence/v13-r8/c8_pump_guns.py > maturity/evidence/v13-r8/c8_pump_guns.txt

中途被打断会留下注入：按本脚本的 sha 基线还原 HOST/CASES/MOUNT/GATE 四份（`git status` 里
GATE 与文档同时有本轮别的改动，不能 `git checkout --`）。
"""

import hashlib
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]  # maturity/evidence/v13-r8/ → 仓根
HOST = "crates/qx-cli/src/strategy_host.rs"
CASES = "crates/qx-cli/src/tests/strategy_pump_bounds.rs"
MOUNT = "crates/qx-cli/src/tests/mod.rs"
GATE = "tools/check_architecture.py"
FILES = (HOST, CASES, MOUNT, GATE)

ARM1 = "策略 worker 响应泵：队列有界、单行有上限、迟到答复在写入前排空"
ARM2 = "C8 的七颗泵用例逐颗在位，且那份文件真被 tests 目录模块挂载"

BASE: dict[str, bytes] = {}
EOLS: dict[str, bytes] = {}
for rel in FILES:
    raw = (ROOT / rel).read_bytes()
    eol = b"\r\n" if b"\r\n" in raw else b"\n"
    assert b"\n" not in raw.replace(eol, b""), f"{rel} 混用换行，先修 EOL"
    BASE[rel] = raw
    EOLS[rel] = eol
    print(f"  EOL {rel}: {eol!r}（{len(raw.split(eol)) - 1} 行）")

FAIL_RE = re.compile(r"^\s*✗(.*)$", re.M)
PASS_RE = re.compile(r"^\[PASS\]", re.M)
RAN_RE = re.compile(r"^running (\d+) tests?$", re.M)
RESULT_RE = re.compile(r"^test result: (\w+)\. (\d+) passed; (\d+) failed;", re.M)
ROW_RE = re.compile(r"^test (\S+) \.\.\. (\w+)$", re.M)


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:12]


def lines_of(rel: str) -> list[bytes]:
    return BASE[rel].split(EOLS[rel])


def gate() -> tuple[int, int, list[str]]:
    r = subprocess.run(
        [sys.executable, "-X", "utf8", str(ROOT / GATE)],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    out = r.stdout + r.stderr
    return r.returncode, len(PASS_RE.findall(out)), [m.strip() for m in FAIL_RE.findall(out)]


def rows() -> tuple[int, int, int, int, list[str], str]:
    """跑那七颗常驻用例：返回 (退出码, running 颗数, passed, failed, FAILED 名册, 编译层诊断)。"""
    r = subprocess.run(
        ["cargo", "test", "-p", "qx-cli", "--bins", "strategy_pump_bounds"],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    out = r.stdout + r.stderr
    ran = RAN_RE.findall(out)
    res = RESULT_RE.findall(out)
    if not res:
        diag = "COMPILE_RED" if re.search(r"^error(\[\w+\])?:", out, re.M) else "读不出 test result 行"
        return r.returncode, -1, -1, -1, [], diag
    passed, failed = int(res[0][1]), int(res[0][2])
    bad = sorted(name for name, verdict in ROW_RE.findall(out) if verdict == "FAILED")
    return r.returncode, int(ran[0]) if ran else -1, passed, failed, bad, res[0][0]


# (编号, 说明, 目标文件, 目标行号(1 基), 该行期望字节文本, 替换文本, 门禁目标标签, 用例期望)
GUNS = [
    (
        "G1",
        "通道还原成无界 mpsc::channel()",
        HOST,
        225,
        b"            let (sender, responses) = mpsc::sync_channel(STRATEGY_PUMP_BACKLOG);",
        b"            let (sender, responses) = mpsc::channel();",
        ARM1,
        "GREEN",
    ),
    (
        "G2",
        "有界常量换成字面量 64（数字对、名字不对）",
        HOST,
        225,
        b"            let (sender, responses) = mpsc::sync_channel(STRATEGY_PUMP_BACKLOG);",
        b"            let (sender, responses) = mpsc::sync_channel(64);",
        ARM1,
        "GREEN",
    ),
    (
        "G3",
        "JSONL 支退回 BufRead::lines()",
        HOST,
        230,
        b"                        match read_jsonl_line_within(&mut reader, jsonl_line_cap_bytes()) {",
        b"                        match read_jsonl_line_within(&mut reader.lines(), jsonl_line_cap_bytes()) {",
        ARM1,
        "COMPILE",
    ),
    (
        "G4",
        "单行上限从共享函数改成现场字面量 usize::MAX",
        HOST,
        230,
        b"                        match read_jsonl_line_within(&mut reader, jsonl_line_cap_bytes()) {",
        b"                        match read_jsonl_line_within(&mut reader, usize::MAX) {",
        ARM1,
        "GREEN",
    ),
    (
        "G5",
        "排空调用被掏空（不再 map 到 helper）",
        HOST,
        389,
        b"                let _ = self.responses.as_ref().map(drain_stale_responses);",
        b"                let _ = self.responses.as_ref();",
        ARM1,
        "GREEN",
    ),
    (
        "G6",
        "排空整句挪到写入之后（两行换位，行数不变）",
        HOST,
        None,  # 特殊：389 与 412 交换
        None,
        None,
        ARM1,
        "COMPILE",
    ),
    (
        "G7",
        "通道那行整体注释掉：针尖只活在注释里",
        HOST,
        225,
        b"            let (sender, responses) = mpsc::sync_channel(STRATEGY_PUMP_BACKLOG);",
        b"            // mpsc::sync_channel(STRATEGY_PUMP_BACKLOG);",
        ARM1,
        "COMPILE",
    ),
    (
        "G8",
        "tests 目录模块挂载行注释掉",
        MOUNT,
        426,
        b"mod strategy_pump_bounds;",
        b"// mod strategy_pump_bounds;",
        ARM2,
        "ZERO",
    ),
    (
        "G9",
        "一颗用例改名（名册针尖不在）",
        CASES,
        124,
        b"fn drain_clears_queued_responses_and_reports_the_count() {",
        b"fn drain_clears_queued_responses_and_reports_the_count_renamed() {",
        ARM2,
        "GREEN",
    ),
    (
        "T1",
        "上限常量 +1（不再等于分帧预算）",
        HOST,
        819,
        b"    DEFAULT_MAX_FRAME_BYTES",
        b"    DEFAULT_MAX_FRAME_BYTES + 1",
        None,
        "RED",
    ),
    (
        "T2",
        "上限比较放宽成 cap * 8",
        HOST,
        850,
        b"        if collected.len() > cap {",
        b"        if collected.len() > cap * 8 {",
        None,
        "RED",
    ),
    (
        "T3",
        "行终止判据强制为真",
        HOST,
        847,
        b'        let ended_with_newline = window[..end].ends_with(b"\\n");',
        b"        let ended_with_newline = true;",
        None,
        "RED",
    ),
    (
        "T4",
        "CR 剔除认错了字节",
        HOST,
        857,
        b"            if collected.last() == Some(&b'\\r') {",
        b"            if collected.last() == Some(&b'!') {",
        None,
        "RED",
    ),
    (
        "T5",
        "排空读到第一颗就返回",
        HOST,
        876,
        b"            Ok(_) => dropped += 1,",
        b"            Ok(_) => return dropped,",
        None,
        "RED",
    ),
]


def apply(gid: str) -> None:
    for num, label, rel, lineno, want, new, arm, expect in GUNS:
        if num != gid:
            continue
        if gid == "G6":
            book = lines_of(HOST)
            a, b = book[388], book[411]
            assert a == b"                let _ = self.responses.as_ref().map(drain_stale_responses);", "G6 锚点 A 不符"
            assert b == b"                let received = self", "G6 锚点 B 不符"
            book[388], book[411] = b, a
            (ROOT / HOST).write_bytes(EOLS[HOST].join(book))
            return
        assert rel is not None and lineno is not None and want is not None and new is not None
        book = lines_of(rel)
        assert book[lineno - 1] == want, f"{gid} 第 {lineno} 行与期望锚点不符：{book[lineno - 1]!r}"
        assert book.count(want) == 1, f"{gid} 锚点命中 {book.count(want)} 次（期望 1）"
        assert want != new, f"{gid} 空枪"
        book[lineno - 1] = new
        (ROOT / rel).write_bytes(EOLS[rel].join(book))
        return
    raise AssertionError(gid)


print("\n== stage-0：锚点普查（先数枪，不放枪）")
viable = {}
for num, label, rel, lineno, want, new, arm, expect in GUNS:
    if num == "G6":
        book = lines_of(HOST)
        ok = book[388] == b"                let _ = self.responses.as_ref().map(drain_stale_responses);" and book[411] == b"                let received = self"
        print(f"  {num} 换位锚点 389/412 {'可放' if ok else '不符 → 未放枪'}")
        viable[num] = ok
        continue
    book = lines_of(rel)
    hits = book.count(want)
    ok = book[lineno - 1] == want and hits == 1 and want != new
    viable[num] = ok
    print(f"  {num} {pathlib.Path(rel).name:<24} 行 {lineno} 命中 {hits} {'可放' if ok else '不通过 → 未放枪'}　{label}")

print("\n== C0 基线（放枪前两个 oracle 都必须绿）")
grc, gpass, gfails = gate()
crc, cran, cpass, cfailed, cnames, cdiag = rows()
print(f"  GATE_EXIT={grc} [PASS]={gpass} ✗={len(gfails)}")
for f in gfails:
    print(f"    ✗ {f}")
print(f"  ROWS_EXIT={crc} running={cran} passed={cpass} failed={cfailed} {cdiag}")
tally = {}
baseline_ok = grc == 0 and not gfails and crc == 0 and cran == 7 and cpass == 7 and cfailed == 0
assert baseline_ok, "基线不绿，放枪会把别处的红算到变异头上"
tally["C0"] = "GREEN"

for num, label, rel, lineno, want, new, arm, expect in GUNS:
    print(f"\n== {num} {label}")
    if not viable[num]:
        tally[num] = "未放枪"
        print("  锚点不过，跳过（不把 0 命中读成通过）")
        continue
    try:
        apply(num)
        grc, gpass, gfails = gate()
        crc, cran, cpass, cfailed, cnames, cdiag = rows()
        target = [f for f in gfails if arm and arm in f]
        other = [f for f in gfails if not (arm and arm in f)]
        gate_killed = bool(target)
        gate_green = grc == 0 and not gfails
        if expect == "RED":
            case_hit = crc != 0 and cfailed >= 1 and bool(cnames)
            killed = case_hit and gate_green
            verdict = "KILLED" if killed else "SURVIVED（那颗用例不咬，或门禁连带瞎了）"
        elif expect == "GREEN":
            if not gate_killed:
                verdict = "SURVIVED（门禁没咬）"
            elif crc != 0 or cfailed != 0:
                verdict = "红错了格（用例连带变了，注入面不是纯形状）"
            else:
                verdict = "KILLED"
        elif expect == "COMPILE":
            if not gate_killed:
                verdict = "SURVIVED（门禁没咬）"
            elif cdiag == "COMPILE_RED":
                verdict = "KILLED（用例 oracle 到编译层为止）"
            else:
                verdict = "用例没有编到红"
        elif expect == "ZERO":
            killed = gate_killed and cran == 0
            verdict = "KILLED" if killed else "SURVIVED"
        else:
            verdict = "未放枪"
        print(f"  GATE_EXIT={grc} [PASS]={gpass} ✗={len(gfails)} → 目标红 {len(target)} / 附带红 {len(other)}")
        for f in target:
            print(f"    目标红：{f[:150]}")
        for f in other:
            print(f"    附带红：{f[:150]}")
        print(f"  ROWS_EXIT={crc} running={cran} passed={cpass} failed={cfailed} 诊断={cdiag}")
        for n in cnames:
            print(f"    FAILED {n}")
        print(f"  判定：{verdict}")
        tally[num] = verdict
    finally:
        (ROOT / HOST).write_bytes(BASE[HOST])
        (ROOT / CASES).write_bytes(BASE[CASES])
        (ROOT / MOUNT).write_bytes(BASE[MOUNT])
        for check_rel in (HOST, CASES, MOUNT):
            got = sha((ROOT / check_rel).read_bytes())
            assert got == sha(BASE[check_rel]), f"还原后字节不符：{check_rel}"
        print(f"  还原 HOST/CASES/MOUNT sha={sha(BASE[HOST])}/{sha(BASE[CASES])}/{sha(BASE[MOUNT])} 复核通过")

print("\n== C10 不放枪对照：还原之后两个 oracle 必须回到基线")
grc, gpass, gfails = gate()
crc, cran, cpass, cfailed, cnames, cdiag = rows()
print(f"  GATE_EXIT={grc} [PASS]={gpass} ✗={len(gfails)}")
for f in gfails:
    print(f"    ✗ {f}")
print(f"  ROWS_EXIT={crc} running={cran} passed={cpass} failed={cfailed} {cdiag}")
tally["C10"] = "GREEN" if (grc == 0 and not gfails and crc == 0 and cran == 7 and cpass == 7) else "SURVIVED"
assert tally["C10"] == "GREEN", "对照发不绿，前面各发的红就不可信"

print("\n== 收尾 sha 复核（四份文件逐一对回基线）")
for rel in FILES:
    got, want = sha((ROOT / rel).read_bytes()), sha(BASE[rel])
    print(f"  {'OK ' if got == want else 'NO '} {rel} {got}")
    assert got == want

print("\n判定：", tally)
# 分档按前缀而不是全等：三发的判定带"（用例 oracle 到编译层为止）"后缀，按全等比就把咬到的算成没咬。
killed = sum(1 for v in tally.values() if v.startswith("KILLED"))
green = sum(1 for v in tally.values() if v == "GREEN")
uns = sum(1 for v in tally.values() if v == "未放枪")
other = {k: v for k, v in tally.items() if not v.startswith("KILLED") and v != "GREEN" and v != "未放枪"}
surv = len(other)
print(f"合计 {len(tally)} 发：KILLED {killed} / GREEN（对照）{green} / SURVIVED 或红错了格 {surv} / 未放枪 {uns}")
for k, v in sorted(other.items()):
    print(f"  未咬：{k} {v}")
assert killed + green + uns + surv == len(tally), f"分档算术不符：{killed}+{green}+{uns}+{surv} != {len(tally)}"
assert uns == 0 and surv == 0, "有枪没放出去或没咬，不能算收口"
# 收口标记由脚本自己打，不靠启动它的那条命令补 echo：换了启动方式也不会出现"跑完了而日志没记到"。
print("BATTERY_EXIT=0")
