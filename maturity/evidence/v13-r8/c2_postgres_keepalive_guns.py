"""C2 那六格各自的变异枪：调用点、三颗默认值、词表、两处守卫、用例在场（V13 第 8 轮 C2，任务 #214）。

C2 的形状是"持锁者停在 socket read 上时，取锁有界只救了排队的那一侧"。修复落在
`dsn_with_socket_defaults` 与其调用点，判据占六格，每格都可能被单独改回去而另外几格完好，
所以逐颗放枪。两颗 oracle 各自跑：门禁 `tools/check_architecture.py` 与
`cargo test -p qx-storage --features sqlite,postgres,nats --lib`（CI 里那条门后用例腿，实测 36 颗）。

  M1 idle 抬回驱动默认的两小时 → 目标红在"判死上界排在预算之前"那一格，用例红在字面量上。
     这一发同时咬两颗 oracle，是唯一一发行为牙齿：它证明那颗用例真会拒绝把持锁者放回无限远。
  M2 词表里的一颗键名打错 → 目标红在"三颗键逐字停在词表上"。这一发的形状是"编译得过、
     DSN 当场解析失败"，仓内没有真连接跑不了，所以用例那面红的是它自己抄的那三颗字面量。
  M3 去掉"写过就不补"的守卫 → 目标红在"调用方写过的保活键一字不动"，用例红在负例那一格。
  M4 去掉 fragment 的 pass-through → 目标红在"带 fragment 的 URL 原样交回"，用例红在等值那一格。
  M5 把常驻用例改名 → 目标红在"只有一份实现、且有一条常驻用例问它"。用例这一面**必须绿**、
     颗数一颗不少：改名后的那颗照样被 `#[test]` 收集、照样过。这一格的牙齿只有门禁——
     它认的是名字，而 cargo 认的是属性，两者错位正是"用例仍在服务"这句话需要单独钉的原因。
  M6 把用例里那句通约关系换成恒真 → 目标红回到"判死上界排在预算之前"那一格的**第二条款**
     （三颗常量从源码读出来仍然成立，红只能来自"用例不再问这件事"）。用例绿。
     M1 与 M6 打的是同一颗判据的两个条款，不是同一颗条件写两遍：M1 量的是值，M6 量的是读者。
  M7 把调用点还原成只补 connect_timeout → 目标红在 V11 O8 那一格（它的 needle 本轮刚改成新写法）。
     用例这一面**必须全绿**：36 颗一颗不少一颗不多。这一发是本轮最贵的一颗——它证明"helper 被
     用例直接调用"不等于"生产链路用上了它"，而把这件事钉住的是门禁那一格而不是用例。
  M8 不放枪的对照（只动注释）：两颗 oracle 都必须绿，证明前面七发的红来自注入本身。

判定形状：红必须是**目标那颗**判据红，别的判据替它说话不算咬到；用例面同理——期望变红的
四发（M1–M4）红的必须正是 `a_postgres_dsn_without_socket_keepalive_gets_the_repo_bounds`
那一颗，名册逐颗核对。cargo 的 `test result:` 行一颗都没有时算"这一腿压根没跑起来"而不是红，
所以判据从 `test result:` 那行的 passed/failed 取值，不按退出码猜；同一腿的 `FAILED` 名册颗数
要与那颗结果行相等，不相等就当场拒绝（上一版把两份读数相加，一颗红印成两颗，票面数字是假的）。
每发按 sha 快照还原并复核，收尾再量一次全绿。用法：

    python -X utf8 maturity/evidence/v13-r8/c2_postgres_keepalive_guns.py \
      > maturity/evidence/v13-r8/c2_postgres_keepalive_guns.txt

中途被打断会留下注入：`git diff --stat crates/qx-storage/src/postgres.rs tools/check_architecture.py`
确认后按本脚本的 sha 基线还原（两份都是本轮正在改的文件，不能 `git checkout --`）。
"""
import hashlib
import pathlib
import re
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]  # maturity/evidence/v13-r8/ → 仓根
PG = "crates/qx-storage/src/postgres.rs"
GATE = "tools/check_architecture.py"
FILES = (PG, GATE)
CARGO = shutil.which("cargo") or "cargo"
FEATURES = ["--features", "sqlite,postgres,nats"]
LEG = 36  # cargo test -p qx-storage --features sqlite,postgres,nats --lib 实测颗数
BASELINE_GATE_PASS = 813

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
RESULT_ROW_RE = re.compile(r"^test result: \w+\.\s+(\d+) passed;\s+(\d+) failed", re.M)
NAME_RE = re.compile(r"^test (\S+) \.\.\. ok$", re.M)
FAILED_RE = re.compile(r"^test (\S+) \.\.\. FAILED$", re.M)


def raw_now(rel: str) -> bytes:
    return (ROOT / rel).read_bytes()


def sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()[:12]


def splice(rel: str, old: str, new: str) -> None:
    """整行字节替换：命中必须恰好 1 次，且 old != new（空枪不算放过）。"""
    assert old != new, f"空枪：{rel} ← {old[:60]!r}"
    ob, nb = old.encode("utf-8"), new.encode("utf-8")
    lines = raw_now(rel).split(EOLS[rel])
    hits = [i for i, l in enumerate(lines) if l == ob]
    assert len(hits) == 1, f"锚点命中 {len(hits)} 次（期望 1）：{rel} ← {old[:70]!r}"
    lines[hits[0]] = nb
    (ROOT / rel).write_bytes(EOLS[rel].join(lines))


def run_hits(rel: str, run: tuple[str, ...]) -> list[int]:
    """连续多行整块在文件里出现的位置（单行锚点也走这里，一视同仁）。"""
    ob = [s.encode("utf-8") for s in run]
    lines = raw_now(rel).split(EOLS[rel])
    return [i for i in range(len(lines) - len(ob) + 1) if lines[i : i + len(ob)] == ob]


def block_start(rel: str, olds: tuple[str, ...]) -> int:
    starts = run_hits(rel, olds)
    assert len(starts) == 1, f"块锚点命中 {len(starts)} 次（期望 1）：{rel} ← {olds[0][:60]!r}"
    return starts[0]


def replace_block(rel: str, olds: tuple[str, ...], news: tuple[str, ...]) -> None:
    lines = raw_now(rel).split(EOLS[rel])
    start = block_start(rel, olds)
    lines[start : start + len(olds)] = [s.encode("utf-8") for s in news]
    (ROOT / rel).write_bytes(EOLS[rel].join(lines))


def drop_block(rel: str, olds: tuple[str, ...]) -> None:
    lines = raw_now(rel).split(EOLS[rel])
    start = block_start(rel, olds)
    del lines[start : start + len(olds)]
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


def cases() -> tuple[int, int, int, list[str], list[str], bool]:
    """门后那条 cargo 用例腿：返回 (退出码, passed 合计, failed 合计, 通过名册, 失败名册, 没有结果行)。"""
    r = subprocess.run(
        [CARGO, "test", "-p", "qx-storage", *FEATURES, "--lib"],
        cwd=str(ROOT), capture_output=True, text=True, encoding="utf-8", errors="replace",
        timeout=900,
    )
    out = r.stdout + r.stderr
    rows = RESULT_ROW_RE.findall(out)
    bad = sorted(set(FAILED_RE.findall(out)))
    passed = sum(int(a) for a, _ in rows)
    # 红的颗数只从 `test result:` 那一行取，不再把名册加第二遍：上一版把两份读数相加，
    # 于是一颗红印成两颗，判定没受影响而票面数字是假的。两份读数不等时当场拒绝，不让
    # "名册数到 1、结果行说 2"这种错位混进取证日志。
    if rows:
        failed = sum(int(b) for _, b in rows)
        assert failed == len(bad), f"结果行报红 {failed} 颗，FAILED 名册数到 {len(bad)} 颗：{bad}"
    else:
        failed = len(bad)
    names = sorted(NAME_RE.findall(out))
    # 结果行一颗都没有 = cargo 压根没跑起来（编译失败）；跑起来了就是红的颗数与名字说话。
    no_result_row = not rows
    return r.returncode, passed, failed, names, bad, no_result_row


# 六颗门禁判据（目标红的 needle 取自判据名本身，不取自被注入的代码文本）
A1 = "PostgreSQL 连接槽取锁有界、连接阶段的界在仓内落地"
A2A = "DSN 里 socket 保活的判死上界排在连接槽等待预算之前"
A2B = "补齐的三颗键逐字停在驱动的 DSN 词表上"
A3A = "调用方写过的保活键一字不动"
A3B = "带 fragment 的 URL 不是该由仓库重写的形状"
A4 = "socket 层的界只有一份实现、且有一条常驻用例问它"

IDLE_OLD = "const KEEPALIVE_IDLE_SECONDS: u64 = 10;"
IDLE_NEW = "const KEEPALIVE_IDLE_SECONDS: u64 = 7200;"
VOCAB_OLD = '    "keepalives_retries",'
VOCAB_NEW = '    "keepalive_retries",'
GUARD_BLOCK = ("        if lowered.contains(key) {", "            continue;", "        }")
FRAGMENT_BLOCK = ("        if rest.contains('#') {", "            return base;", "        }")
CASE_OLD = "    fn a_postgres_dsn_without_socket_keepalive_gets_the_repo_bounds() {"
CASE_NEW = "    fn _gun_renamed_a_postgres_dsn_without_socket_keepalive() {"
# 用例面变红时，红的那一颗必须正是这条常驻用例——别颗用例替它说话不算这一档有牙齿。
DSN_CASE = "postgres::tests::a_postgres_dsn_without_socket_keepalive_gets_the_repo_bounds"
CALL_OLD = "        let dsn = dsn_with_socket_defaults(dsn, CONNECT_TIMEOUT_SECONDS);"
CALL_NEW = "        let dsn = dsn_with_connect_timeout(dsn, CONNECT_TIMEOUT_SECONDS);"
RELATION_OLD = (
    "        assert!(",
    "            Duration::from_secs(",
    "                KEEPALIVE_IDLE_SECONDS + KEEPALIVE_INTERVAL_SECONDS * KEEPALIVE_RETRIES",
    "            ) < SLOT_WAIT_BUDGET,",
    '            "socket 层的界要收在槽位等待预算之内，否则补了默认值也救不了持锁者"',
    "        );",
)
RELATION_NEW = (
    "        assert!(",
    "            SLOT_WAIT_BUDGET > Duration::from_secs(0),",
    '            "socket 层的界要收在槽位等待预算之内，否则补了默认值也救不了持锁者"',
    "        );",
)
CTRL_OLD = "/// 首个探测之后每隔几秒重发一次。"
CTRL_NEW = "/// 首个探测之后每隔几秒重发一次（对照发：只动注释，两颗 oracle 都必须绿）。"

# (编号, 说明, 门禁目标 needle, 期望用例, 用例期望性质)
GUNS = [
    ("M1", "idle 抬回驱动默认的两小时", A2A, "RED", "行为牙齿：值与关系一起塌"),
    ("M2", "词表里的一颗键名打错", A2B, "RED", "用例红在自己抄的三颗字面量上"),
    ("M3", "去掉「写过就不补」的守卫", A3A, "RED", "负例那一格"),
    ("M4", "去掉 fragment 的 pass-through", A3B, "RED", "等值那一格"),
    ("M5", "把常驻用例改名", A4, "GREEN", "名册钉：这颗只有门禁牙齿"),
    ("M6", "用例里那句通约关系换成恒真", A2A, "GREEN", "读者钉：红只能来自第二条款"),
    ("M7", "调用点还原成只补 connect_timeout", A1, "GREEN", "接线钉：helper 被用例直调≠生产用上它"),
    ("M8", "对照发：只动一行注释", None, "GREEN", "不放枪，证明前七发的红来自注入"),
]


def apply_gun(gid: str) -> None:
    if gid == "M1":
        splice(PG, IDLE_OLD, IDLE_NEW)
    elif gid == "M2":
        splice(PG, VOCAB_OLD, VOCAB_NEW)
    elif gid == "M3":
        drop_block(PG, GUARD_BLOCK)
    elif gid == "M4":
        drop_block(PG, FRAGMENT_BLOCK)
    elif gid == "M5":
        splice(PG, CASE_OLD, CASE_NEW)
    elif gid == "M6":
        replace_block(PG, RELATION_OLD, RELATION_NEW)
    elif gid == "M7":
        splice(PG, CALL_OLD, CALL_NEW)
    elif gid == "M8":
        splice(PG, CTRL_OLD, CTRL_NEW)
    else:
        raise AssertionError(gid)


print("\n== stage-0-A：这一档到底有几面读者（决定 M5/M6/M7 的定性）")
helper_defs = case_defs = 0
for path in sorted(ROOT.glob("crates/**/*.rs")):
    text = path.read_text(encoding="utf-8", errors="replace")
    helper_defs += text.count("fn dsn_with_socket_defaults(")
    case_defs += text.count("fn a_postgres_dsn_without_socket_keepalive")
# 生产面与用例面的调用数分开量：M7 打的正是"只有生产那一颗调用点"，把两份混在一起数
# 就读不出这一格到底有没有别的读者。
pg_text = (ROOT / PG).read_text(encoding="utf-8")
cut = pg_text.index("#[cfg(test)]")
call_re = re.compile(r"(?<!fn )\bdsn_with_socket_defaults\(")
prod_calls = len(call_re.findall(pg_text[:cut]))
test_calls = len(call_re.findall(pg_text[cut:]))
print(f"  dsn_with_socket_defaults 的定义：{helper_defs} 颗")
print(f"  那条常驻用例的定义：{case_defs} 颗")
print(f"  生产面（`#[cfg(test)]` 之前）的调用：{prod_calls} 颗")
print(f"  用例面（`#[cfg(test)]` 之后）的调用：{test_calls} 颗")
assert helper_defs == 1 and case_defs == 1, "定义或在场的用例数变了，M5/M7 的定性要改"
assert prod_calls == 1 and test_calls >= 1, "调用点数变了，M7 这一发不再唯一指认接线那一格"
print("  → 用例直调 helper、不经过 connect_with_pool_size：M7 的用用例面必须绿，接线只有门禁牙齿")

print("\n== stage-0-B：锚点普查（先数枪，不放枪）")
# 每颗枪的锚点按"整块 run 只出现一次"来量，不逐行数：`        }` 这类行在文件里到处是，
# 逐行命中数既没有信息也会把可放的枪误判成不通过。
anchors = {
    "M1": [(PG, (IDLE_OLD,))],
    "M2": [(PG, (VOCAB_OLD,))],
    "M3": [(PG, GUARD_BLOCK)],
    "M4": [(PG, FRAGMENT_BLOCK)],
    "M5": [(PG, (CASE_OLD,))],
    "M6": [(PG, RELATION_OLD)],
    "M7": [(PG, (CALL_OLD,))],
    "M8": [(PG, (CTRL_OLD,))],
}
viable = {}
for gid, groups in anchors.items():
    counts = [len(run_hits(rel, run)) for rel, run in groups]
    ok = all(c == 1 for c in counts)
    viable[gid] = ok
    print(f"  {gid} 块锚点 {len(counts)} 组 ×命中 {counts} {'可放' if ok else '不通过 → 未放枪'}")
assert all(viable.values()), "有锚点不唯一，先把普查里的枪处理掉再谈变异"

print("\n== 基线（放枪前两颗 oracle 都必须绿）")
grc, gnp, gfails = gate()
crc, cpass, cfailed, cnames, cbad, cnorow = cases()
print(f"  GATE_EXIT={grc} [PASS]={gnp} ✗={len(gfails)}")
for f in gfails:
    print(f"    ✗ {f}")
print(f"  CARGO_EXIT={crc} passed={cpass} failed={cfailed} 名册={len(cnames)} 没有结果行={cnorow}")
for n in cbad:
    print(f"    FAILED {n}")
assert grc == 0 and not gfails and crc == 0 and cfailed == 0 and cpass == LEG, "基线不绿，放枪会把别处的红算到变异头上"

tally = {}
for gid, label, needle, expect_case, nature in GUNS:
    print(f"\n== {gid} {label}（{nature}）")
    try:
        apply_gun(gid)
        grc, gnp, gfails = gate()
        crc, cpass, cfailed, cnames, cbad, cnorow = cases()
        target = [f for f in gfails if needle and needle in f]
        other = [f for f in gfails if not needle or needle not in f]
        gate_killed = grc != 0 and bool(target)
        case_red = crc != 0
        if gid == "M8":
            killed = grc == 0 and not gfails and crc == 0 and cfailed == 0 and cpass == LEG
            tally[gid] = "GREEN" if killed else "对照发不绿"
        else:
            if expect_case == "RED":
                # 用例面要红，而且红的必须是那条常驻用例自己：别颗红不算这一档有牙齿。
                killed = gate_killed and case_red and not cnorow and cbad == [DSN_CASE]
            else:
                killed = gate_killed and not case_red
            tally[gid] = "KILLED" if killed else ("SURVIVED" if not gate_killed else "红错了格")
        print(f"  GATE_EXIT={grc} [PASS]={gnp}（基线 {BASELINE_GATE_PASS}）✗={len(gfails)} → 目标红 {len(target)} 颗 / 附带红 {len(other)} 颗")
        for f in target:
            print(f"    目标红：{f[:190]}")
        for f in other:
            print(f"    附带红：{f[:190]}")
        print(f"  CARGO_EXIT={crc} passed={cpass} failed={cfailed} 名册={len(cnames)} 没有结果行={cnorow}")
        for n in cbad:
            print(f"    FAILED {n}")
        if expect_case == "RED" and not case_red:
            print("    用例没有变红 —— 那颗常驻用例根本不咬这一档")
        if expect_case == "GREEN" and case_red:
            print(f"    用例如外变红：{cbad}")
        if gid != "M8" and not gate_killed:
            print(f"    没有一颗门禁红含「{needle}」—— 这一枪没咬")
        print(f"  判定：{tally[gid]}")
    finally:
        for rel in FILES:
            restore(rel)
        for rel in FILES:
            print(f"  还原 {pathlib.Path(rel).name} sha={sha(BASE[rel])} 复核通过")

print("\n== 收尾：还原之后两颗 oracle 必须回到基线")
grc, gnp, gfails = gate()
crc, cpass, cfailed, cnames, cbad, cnorow = cases()
print(f"  GATE_EXIT={grc} [PASS]={gnp} ✗={len(gfails)} / CARGO_EXIT={crc} passed={cpass} failed={cfailed}")
for f in gfails:
    print(f"    ✗ {f}")
assert grc == 0 and not gfails and crc == 0 and cfailed == 0 and cpass == LEG, "收尾不绿，前面七发的红就不可信"
print("\n== 收尾 sha 复核")
for rel in FILES:
    got, want = sha(raw_now(rel)), sha(BASE[rel])
    print(f"  {rel}: {got} == {want}")
    assert got == want

killed = [g for g, v in tally.items() if v == "KILLED"]
green = [g for g, v in tally.items() if v == "GREEN"]
survived = [g for g, v in tally.items() if v not in ("KILLED", "GREEN")]
uns = [g for g, _, _, _, _ in GUNS if not viable[g]]
print(f"\n== 合计：KILLED {len(killed)} {killed} / GREEN（对照）{len(green)} {green} / SURVIVED {len(survived)} {survived} / 未放枪 {len(uns)} {uns}")
assert len(uns) == 0 and len(survived) == 0 and len(killed) == 7 and len(green) == 1
