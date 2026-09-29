"""V13 第 8 轮 C1 的变异取证：十次 ack 的预算守卫到底会不会红（9 颗）。

两条口径：Rust 腿只跑 nats::tests（名字过滤），门禁腿整份跑。
站点类的枪按行号从文件里现取锚点，不手抄；每颗先断言锚点恰好命中一次。
还原按 sha 校验，还原不上就整批停手。
挂死那颗（C1-5）的截止用基线实测时长推出来，避免把慢编译误读成挂死。
"""

import hashlib
import pathlib
import subprocess
import sys
import time

ROOT = pathlib.Path(".")
NATS = "crates/qx-storage/src/nats.rs"
CAPS = "maturity/capabilities.yaml"
BUDGET = "maturity/line_budgets.yaml"
GATE = "tools/check_architecture.py"
RUST_TIMEOUT = 300
GATE_TIMEOUT = 600


def read(rel: str) -> bytes:
    return (ROOT / rel).read_bytes()


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:16]


CACHE = {}


def eol(rel: str) -> str:
    if rel not in CACHE:
        CACHE[rel] = "\r\n" if b"\r\n" in read(rel) else "\n"
    return CACHE[rel]


def to_bytes(rel: str, text: str) -> bytes:
    return text.replace("\n", eol(rel)).encode("utf-8")


def site_window(rel: str, line: int, height: int) -> tuple[str, str]:
    """取第 line 行往上共 height 行的原文当锚点，并给出去掉 bounded_ack 包装后的新文本。"""
    body = read(rel).decode("utf-8").replace(eol(rel), "\n")
    lines = body.split("\n")
    window = lines[line - height : line]
    assert window[-1].count("bounded_ack(") == 1, f"站点 :{line} 末行不是恰好一处包装"
    original = "\n".join(window)
    target = window[-1]
    start = target.index("bounded_ack(")
    depth = 0
    cut = None
    for offset in range(start + len("bounded_ack("), len(target)):
        if target[offset] == "(":
            depth += 1
        elif target[offset] == ")":
            if depth == 0:
                cut = offset
                break
            depth -= 1
    assert cut is not None, f"站点 :{line} 找不到与 bounded_ack( 匹配的右括号"
    window[-1] = target[:start] + target[start + len("bounded_ack(") : cut] + target[cut + 1 :]
    return original, "\n".join(window)


C1_1_OLD, C1_1_NEW = site_window(NATS, 310, 8)
C1_1B_OLD, C1_1B_NEW = site_window(NATS, 431, 8)

GUNS = [
    (
        "C1-1 consume_batch 的 Applied 臂摘掉包装",
        NATS,
        C1_1_OLD,
        C1_1_NEW,
        "rust",
        "every_nats_await_site（十次包装只剩九次）",
    ),
    (
        "C1-1b consume_batch_with_projection 的 Applied 臂摘掉包装",
        NATS,
        C1_1B_OLD,
        C1_1B_NEW,
        "rust",
        "every_nats_await_site（计数覆盖第二个批量方法）",
    ),
    (
        "C1-2 把消费侧预算改成 0 秒",
        NATS,
        "\nconst NATS_CONSUMER_ACK_BUDGET: Duration = Duration::from_secs(5);\n",
        "\nconst NATS_CONSUMER_ACK_BUDGET: Duration = Duration::from_secs(0);\n",
        "rust",
        "every_nats_await_site（数值钉）",
    ),
    (
        "C1-3 helper 接到别的时长上",
        NATS,
        "    ack_within(NATS_CONSUMER_ACK_BUDGET, ack).await\n",
        "    ack_within(Duration::from_secs(30), ack).await\n",
        "rust",
        "every_nats_await_site（接线钉 + 常量引用点）",
    ),
    (
        "C1-4 吞掉「没落回」这颗名",
        NATS,
        '        .map_err(|_| format!("在 {budget:?} 内没有落回"))?\n',
        '        .map_err(|_| format!("ack 失败"))?\n',
        "rust",
        "bounded_ack_passes_through_or_names_the_stall",
    ),
    (
        "C1-5 把预算摘掉但保留一颗没人等的 timeout",
        NATS,
        '    tokio::time::timeout(budget, ack)\n        .await\n        .map_err(|_| format!("在 {budget:?} 内没有落回"))?\n        .map_err(|error| error.to_string())\n',
        '    let _ = tokio::time::timeout(budget, std::future::ready(()));\n    let outcome = ack.await;\n    outcome.map_err(|error| error.to_string())\n',
        "rust-hang",
        "整腿超时（那颗计数钉照常绿，用例唯一的出口就是被摘掉的截止）",
    ),
    (
        "C1-6 登记行号回到旧钉",
        CAPS,
        "（nats.rs:350）",
        "（nats.rs:352）",
        "gate",
        "原子投影链两侧点名",
    ),
    (
        "C1-7 行数预算往下压一颗",
        BUDGET,
        "crates/qx-storage/src/nats.rs: 722\n",
        "crates/qx-storage/src/nats.rs: 721\n",
        "gate",
        "单文件行数预算只降不升、无未登记的超大文件",
    ),
    (
        "C1-8 门禁元组里的行号漂一颗",
        GATE,
        '("crates/qx-storage/src/nats.rs", 350, "pub fn consume_batch_with_projection")',
        '("crates/qx-storage/src/nats.rs", 352, "pub fn consume_batch_with_projection")',
        "gate",
        "登记行号逐条仍指到定义本身（V11 L3）",
    ),
]

FILES = sorted({gun[1] for gun in GUNS})
print("=== 第 0 遍：锚点普查 ===")
plan = []
bad = 0
for name, rel, old_text, new_text, leg, expect in GUNS:
    raw = read(rel)
    old, new = to_bytes(rel, old_text), to_bytes(rel, new_text)
    hits, already = raw.count(old), raw.count(new)
    print(f"{name}: 锚点 {hits} 次 / 新文本已在场 {already} 次 / 腿 {leg}")
    if hits != 1 or already != 0:
        bad += 1
    plan.append((name, rel, old, new, leg, expect))
if bad:
    print(f"BAD 普查有 {bad} 颗不对，整批停手")
    sys.exit(1)
if "1" == __import__("os").environ.get("QX_CENSUS_ONLY", ""):
    print("仅普查：9 颗锚点全部命中一次，未放枪")
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
        subprocess.run(
            ["taskkill", "/F", "/T", "/PID", str(proc.pid)],
            capture_output=True,
            check=False,
        )
        out = proc.communicate()[0] or ""
        return "TIMEOUT", time.time() - started, out


def rust_leg(timeout: int = RUST_TIMEOUT) -> tuple[object, float, str]:
    return run_leg(
        [
            "cargo",
            "test",
            "-p",
            "qx-storage",
            "--features",
            "sqlite,postgres,nats",
            "--lib",
            "--",
            "nats::tests",
        ],
        timeout,
    )


def gate_leg() -> tuple[object, float, str]:
    return run_leg([sys.executable, "-X", "utf8", "tools/check_architecture.py"], GATE_TIMEOUT)


def summarize(leg: str, rc: object, text: str) -> str:
    lines = text.splitlines()
    if rc == "TIMEOUT":
        fired = [
            line.strip()[:70] for line in lines if line.startswith("test nats::tests::")
        ]
        return f"整腿超时；挂死前已见 {'; '.join(fired[-4:]) or '无 test 行'}"
    if leg.startswith("rust"):
        failed = [
            line.split("...")[0].replace("test ", "").strip()
            for line in lines
            if line.startswith("test nats::tests::") and line.rstrip().endswith("FAILED")
        ]
        result = [line for line in lines if line.startswith("test result:")]
        compile_err = [line for line in lines if line.startswith("error[")]
        if compile_err:
            return f"编译红 {len(compile_err)} 条 / {compile_err[0][:90]}"
        return f"rc={rc} {result[-1][:56] if result else '无 test result 行'} 红 [{', '.join(failed) or '无'}]"
    rows = [line for line in lines if line.startswith("[FAIL]")]
    return f"rc={rc} 红 {len(rows)} 条 " + " | ".join(row[:130] for row in rows[:4])


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
# 挂死那颗的截止：编译+跑测的真实耗时留三倍余量，慢编译不会被误读成"挂死"。
HANG_TIMEOUT = int(max(180, seconds * 3 + 60))
print(f"挂死枪的截止 {HANG_TIMEOUT}s（基线 rust 腿 {seconds:.1f}s）")

killed = survived = notfired = 0
print("\n=== 逐颗放枪 ===")
for name, rel, old, new, leg, expect in plan:
    raw = read(rel)
    if raw.count(old) != 1:
        print(f"[未放枪] {name} 注入时锚点 {raw.count(old)} 次")
        notfired += 1
        continue
    (ROOT / rel).write_bytes(raw.replace(old, new, 1))
    if leg == "rust-hang":
        rc, elapsed, text = rust_leg(HANG_TIMEOUT)
    elif leg == "rust":
        rc, elapsed, text = rust_leg()
    else:
        rc, elapsed, text = gate_leg()
    verdict = "KILLED" if rc == "TIMEOUT" or rc != 0 else "SURVIVED"
    killed += verdict == "KILLED"
    survived += verdict == "SURVIVED"
    print(f"[{verdict}] {name} 预期『{expect}』 {elapsed:.1f}s")
    print(f"        实测 {summarize(leg, rc, text)}")
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
