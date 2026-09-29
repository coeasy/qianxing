"""V13 第 8 轮 C5 的变异取证：WebSocket 单条消息读取的整体截止与帧数上界（11 颗）。

站点行号按当前文件现取，不手抄；每颗先断言锚点恰好命中一次、新文本一颗不在。
腿：rust = `cargo test -p qx-adapter --lib -- websocket`（名字过滤），gate = 整份门禁。
两颗界都是"用例摸不到"的形状（值、接线、位置、用例名），所以每颗都同时放 rust 与 gate
两条腿，红的到底是哪一条要如实分开记。还原按 sha 校验，校验不上整批停手。
"""

import hashlib
import pathlib
import subprocess
import sys
import time

sys.stdout.reconfigure(encoding="utf-8")
ROOT = pathlib.Path(".")
ADAPTER = "crates/qx-adapter/src/lib.rs"
SEC = "docs/SECURITY.md"
BUDGET = "maturity/line_budgets.yaml"
RUST_TIMEOUT = 300
GATE_TIMEOUT = 600
# 摘掉墙钟那颗：滴帧用例要一路撞到 8 192 帧上界才收口，实测约 123 s，截止必须比它宽，
# 否则报的是"腿超时"而不是"断言红"，那颗名就白测了。
C5_2_EXPECTED_SECONDS = 130


def read(rel: str) -> bytes:
    return (ROOT / rel).read_bytes()


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:16]


EOL = {}


def eol(rel: str) -> str:
    if rel not in EOL:
        EOL[rel] = "\r\n" if b"\r\n" in read(rel) else "\n"
    return EOL[rel]


def body(rel: str) -> list[str]:
    return read(rel).decode("utf-8").replace(eol(rel), "\n").split("\n")


def to_bytes(rel: str, text: str) -> bytes:
    return text.replace("\n", eol(rel)).encode("utf-8")


def seg(rel: str, first: int, last: int) -> str:
    lines = body(rel)
    out = "\n".join(lines[first - 1 : last])
    assert out.strip(), f"{rel}:{first}-{last} 取到空段"
    return out


OLD_BLOCK = seg(ADAPTER, 555, 559)
DEADLINE_BLOCK = seg(ADAPTER, 560, 564)
READ_STATEMENT = seg(ADAPTER, 565, 572)
FRAMES_STEP = seg(ADAPTER, 573, 573)
WRAP_CALL = seg(ADAPTER, 537, 541)
HEADER_DEADLINE_ANCHOR = seg(ADAPTER, 354, 355)
FRAME_TEST_FN = seg(ADAPTER, 999, 999)

GUNS = [
    (
        "C5-1 摘掉帧数上界那道界",
        ADAPTER,
        seg(ADAPTER, 554, 559),
        "    loop {",
        ["rust", "gate"],
        "滴帧用例的『帧数』断言 + 门禁的 const/圈数计数",
    ),
    (
        "C5-2 摘掉墙钟那道界",
        ADAPTER,
        seg(ADAPTER, 555, 564),
        OLD_BLOCK,
        ["rust-hang", "gate"],
        "墙钟用例的『整体截止』断言（它要撞到 8 192 帧才收口）+ 门禁",
    ),
    (
        "C5-3 预算值从 120 s 漂成 10 s",
        ADAPTER,
        seg(ADAPTER, 460, 460),
        "const WEBSOCKET_MESSAGE_BUDGET: Duration = Duration::from_secs(10);",
        ["rust", "gate"],
        "只有门禁（用例交的是自己的窗口，读不到这颗常量）",
    ),
    (
        "C5-4 帧数上界从 8 192 漂成 8 193",
        ADAPTER,
        seg(ADAPTER, 465, 465),
        "const MAX_WEBSOCKET_FRAMES_PER_MESSAGE: usize = 8_193;",
        ["rust", "gate"],
        "只有门禁（同上）",
    ),
    (
        "C5-5 唯一入口把两颗界接成字面量",
        ADAPTER,
        WRAP_CALL,
        "    read_websocket_message_within(stream, Duration::from_secs(10), 100)",
        ["rust", "gate"],
        "门禁的接线钉（flat 形状）",
    ),
    (
        "C5-6 两道界都挪到真正的读之后",
        ADAPTER,
        seg(ADAPTER, 554, 573),
        "    loop {\n"
        + READ_STATEMENT
        + "\n"
        + OLD_BLOCK
        + "\n"
        + DEADLINE_BLOCK
        + "\n"
        + FRAMES_STEP,
        ["rust", "gate"],
        "位置钉 + 帧数用例的 socket.index（第一遍实测：挪位后那颗用例红在 index 13≠12，"
        "墙钟那道用例只看得到返回，只有位置钉看得到）",
    ),
    (
        "C5-7 摘掉滴帧用例的名（改名去掉中段词）",
        ADAPTER,
        FRAME_TEST_FN,
        FRAME_TEST_FN.replace("websocket_frame_dribble_stops", "websocket_frame_stops"),
        ["rust", "gate"],
        "门禁的用例名钉",
    ),
    (
        "C5-7b 改名但保留门禁钉的整个前缀",
        ADAPTER,
        FRAME_TEST_FN,
        FRAME_TEST_FN.replace("_ceiling() {", "_ceiling_v2() {"),
        ["rust", "gate"],
        "补钉之后应由用例名钉杀掉（第一遍实测 SURVIVED：钉没带 `(`，前缀改名吃得下）",
    ),
    (
        "C5-8 文档把 120 s 那颗指到隔壁一行",
        SEC,
        "`crates/qx-adapter/src/lib.rs:460/465`",
        "`crates/qx-adapter/src/lib.rs:461/465`",
        ["gate"],
        "安全白皮书点名某颗东西时，被引那一格里就是那颗",
    ),
    (
        "C5-9 行数预算往下压一颗",
        BUDGET,
        "crates/qx-adapter/src/lib.rs: 1147\n",
        "crates/qx-adapter/src/lib.rs: 1146\n",
        ["gate"],
        "单文件行数预算只降不升",
    ),
    (
        "C5-10 握手块的截止接成固定 3600 s（测 O7 改按函数体数后有没有牙）",
        ADAPTER,
        HEADER_DEADLINE_ANCHOR,
        HEADER_DEADLINE_ANCHOR.split("\n")[0]
        + "\n    let deadline = Instant::now() + Duration::from_secs(3_600);",
        ["rust", "gate"],
        "握手块用例的『整体截止』断言 + 门禁的 header_body 计数",
    ),
]

FILES = sorted({gun[1] for gun in GUNS})
# 摘块那两颗的"新文本"就是被保留的那半截（`loop {` / 帧数块），本来就在场：
# 它们只需锚点唯一，不需新文本缺席。
NEW_MAY_EXIST = ("C5-1 ", "C5-2 ")
print("=== 第 0 遍：锚点普查 ===")
plan = []
bad = 0
for name, rel, old_text, new_text, legs, expect in GUNS:
    raw = read(rel)
    old, new = to_bytes(rel, old_text), to_bytes(rel, new_text)
    hits = raw.count(old)
    already = raw.count(new)
    need_absent = not name.startswith(NEW_MAY_EXIST)
    print(f"{name}: 锚点 {hits} 次 / 新文本已在场 {already} 次{'（须缺席）' if need_absent else '（允许在场）'} / 腿 {legs}")
    if hits != 1 or (need_absent and already != 0):
        bad += 1
    plan.append((name, rel, old, new, legs, expect))
if bad:
    print(f"BAD 普查有 {bad} 颗不对，整批停手")
    sys.exit(1)
if __import__("os").environ.get("QX_CENSUS_ONLY") == "1":
    print(f"仅普查：{len(plan)} 颗锚点全部命中一次，未放枪")
    sys.exit(0)

ORIG = {rel: read(rel) for rel in FILES}
for rel in FILES:
    print(f"原件 {rel} sha {sha(ORIG[rel])}")


def restore() -> None:
    for rel in FILES:
        (ROOT / rel).write_bytes(ORIG[rel])
    for rel in FILES:
        if sha(read(rel)) != sha(ORIG[rel]):
            print(f"还原失败 {rel}，整批停手")
            sys.exit(1)


def run_leg(cmd: list[str], timeout: int) -> tuple[object, float, str]:
    started = time.time()
    proc = subprocess.Popen(
        cmd,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    try:
        out = proc.communicate(timeout=timeout)[0] or ""
        return proc.returncode, time.time() - started, out
    except subprocess.TimeoutExpired:
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(proc.pid)], capture_output=True, check=False)
        out = proc.communicate()[0] or ""
        return "TIMEOUT", time.time() - started, out


def rust_leg(timeout: int = RUST_TIMEOUT) -> tuple[object, float, str]:
    return run_leg(["cargo", "test", "-p", "qx-adapter", "--lib", "--", "websocket"], timeout)


def gate_leg() -> tuple[object, float, str]:
    return run_leg([sys.executable, "-X", "utf8", "tools/check_architecture.py"], GATE_TIMEOUT)


def summarize(leg: str, rc: object, text: str) -> str:
    lines = text.splitlines()
    if rc == "TIMEOUT":
        return "整腿超时"
    if leg.startswith("rust"):
        failed = [
            line.split("...")[0].replace("test ", "").strip()
            for line in lines
            if line.startswith("test ") and line.rstrip().endswith("FAILED")
        ]
        result = [line for line in lines if line.startswith("test result:")]
        compile_err = [line for line in lines if line.startswith("error[")]
        if compile_err:
            return f"编译红 {len(compile_err)} 条 / {compile_err[0][:90]}"
        return f"rc={rc} {result[-1][:60] if result else '无 test result 行'} 红 [{', '.join(failed) or '无'}]"
    rows = [line for line in lines if line.startswith("[FAIL]")]
    detail = [line for line in lines if "✗" in line]
    return f"rc={rc} 红 {len(rows)} 条 " + " | ".join(row[:150] for row in detail[:3])


print("\n=== 基线（未放枪）===")
restore()
rc, seconds, text = rust_leg()
print(f"rust 腿 rc={rc} {seconds:.1f}s {summarize('rust', rc, text)}")
if rc != 0:
    print("基线不绿，整批停手")
    sys.exit(1)
rc, gate_seconds, text = gate_leg()
print(f"gate 腿 rc={rc} {gate_seconds:.1f}s {summarize('gate', rc, text)}")
if rc != 0:
    print("基线门禁不绿，整批停手")
    sys.exit(1)
HANG_TIMEOUT = int(max(C5_2_EXPECTED_SECONDS + 120, seconds * 3 + 120))
print(f"挂死腿截止 {HANG_TIMEOUT}s（基线 rust 腿 {seconds:.1f}s，C5-2 预计 ~{C5_2_EXPECTED_SECONDS}s）")

killed = survived = notfired = 0
print("\n=== 逐颗放枪 ===")
for name, rel, old, new, legs, expect in plan:
    raw = read(rel)
    if raw.count(old) != 1:
        print(f"[未放枪] {name} 注入时锚点 {raw.count(old)} 次")
        notfired += 1
        continue
    (ROOT / rel).write_bytes(raw.replace(old, new, 1))
    results = []
    red = False
    for leg in legs:
        if leg == "rust-hang":
            rc, elapsed, text = rust_leg(HANG_TIMEOUT)
        elif leg == "rust":
            rc, elapsed, text = rust_leg()
        else:
            rc, elapsed, text = gate_leg()
        if rc == "TIMEOUT" or rc != 0:
            red = True
        results.append(f"    {leg}: {'红' if rc == 'TIMEOUT' or rc != 0 else '绿'} {elapsed:.1f}s {summarize(leg, rc, text)}")
    verdict = "KILLED" if red else "SURVIVED"
    killed += verdict == "KILLED"
    survived += verdict == "SURVIVED"
    print(f"[{verdict}] {name} 预期『{expect}』")
    for line in results:
        print(line)
    restore()

print("\n=== 收枪复跑 ===")
rc, seconds, text = rust_leg()
print(f"rust 腿 rc={rc} {seconds:.1f}s {summarize('rust', rc, text)}")
if rc != 0:
    print("BAD 收枪 rust 不绿")
rc, seconds, text = gate_leg()
print(f"gate 腿 rc={rc} {seconds:.1f}s {summarize('gate', rc, text)}")
if rc != 0:
    print("BAD 收枪 gate 不绿")
for rel in FILES:
    print(f"残留检查 {rel} sha {sha(read(rel))} 原件 {sha(ORIG[rel])}")
print(f"\n合计 {len(plan)} 颗：KILLED {killed} / SURVIVED {survived} / 未放枪 {notfired}")
