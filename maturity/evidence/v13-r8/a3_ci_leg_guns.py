"""V13 第 8 轮 A3 的变异取证：那条 NATS `--lib` 腿的判据有没有牙（6 颗）。

两条腿各管一头：`gate` = 整份门禁（判据本身），`ci` = 把 ci.yml 里那条腿原样跑一遍
（命令是从被改过的 ci.yml 现读的，不是手抄的），用它证明"腿绿"和"守卫跑了"是两件事。
所有注入都保持行数不变：ci.yml 与门禁脚本的行数被文档锚点与判据计数指着，一颗多行就是
一片假红，会把"红的到底是哪一面"这件事糊掉。还原按 sha 校验，校验不上整批停手。
"""

import hashlib
import pathlib
import subprocess
import sys
import time

sys.stdout.reconfigure(encoding="utf-8")
ROOT = pathlib.Path(".")
CI = ".github/workflows/ci.yml"
NATS = "crates/qx-storage/src/nats.rs"
GATE = "tools/check_architecture.py"
CI_TIMEOUT = 600
GATE_TIMEOUT = 600

EOL: dict[str, str] = {}


def read(rel: str) -> bytes:
    return (ROOT / rel).read_bytes()


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:16]


def eol(rel: str) -> str:
    if rel not in EOL:
        EOL[rel] = "\r\n" if b"\r\n" in read(rel) else "\n"
    return EOL[rel]


def text(rel: str) -> str:
    return read(rel).decode("utf-8").replace(eol(rel), "\n")


def to_bytes(rel: str, body: str) -> bytes:
    return body.replace("\n", eol(rel)).encode("utf-8")


LEG = "        run: cargo test -p qx-storage --features sqlite,postgres,nats --lib"
assert text(CI).count(LEG) == 1, "ci.yml 里那条腿的锚点不止一颗"

GUNS = [
    (
        "A3-1 CI 腿加上 `-- --ignored`",
        CI,
        LEG,
        LEG + " -- --ignored",
        ["gate", "ci"],
        "整行逐字钉；ci 腿这五颗全被过滤掉",
    ),
    (
        "A3-2 CI 腿加上 `--no-run`（编译但不执行）",
        CI,
        LEG,
        LEG + " --no-run",
        ["gate", "ci"],
        "第一版判据漏掉的那一面：命令名还在、也不带 `--ignored`，只有整行等式看得见",
    ),
    (
        "A3-3 给一颗用例加上 `#[ignore]`（写在同一行，行数不动）",
        NATS,
        "    #[test]\n    fn bounded_wait_returns_the_completion_inside_its_budget(",
        "    #[test] #[ignore]\n    fn bounded_wait_returns_the_completion_inside_its_budget(",
        ["gate", "ci"],
        "`#[ignore` 那颗面；ci 腿少跑一颗仍然全绿，只有门禁看得见",
    ),
    (
        "A3-4 把一颗静态守卫改名但保留门禁钉的整个前缀",
        NATS,
        "fn every_nats_await_site_goes_through_the_bounded_wait(",
        "fn every_nats_await_site_goes_through_the_bounded_wait_v2(",
        ["gate", "ci"],
        "名册逐颗带左括号（C5-7b 那一课）；ci 腿照跑不误，只是名换了",
    ),
    (
        "A3-5 把腿的特性组合改成不含 nats（整段 `mod nats` 从此不编译）",
        CI,
        LEG,
        "        run: cargo test -p qx-storage --features sqlite,postgres --lib",
        ["gate", "ci"],
        "恰一条那颗面（腿没了）；ci 腿里这五颗连同 `mod nats` 一起消失，但仍 rc=0",
    ),
    (
        "A3-6 反面对照：只把门禁名册里的一颗改成仓内不存在的名字",
        GATE,
        '    "bounded_ack_passes_through_or_names_the_stall",',
        '    "bounded_ack_passes_through_or_names_the_statl",',
        ["gate"],
        "名册不是空转的字符串集合：不现场就红（这一颗不需要 ci 腿）",
    ),
]

FILES = sorted({gun[1] for gun in GUNS})
print("=== 第 0 遍：锚点普查 ===")
plan = []
bad = 0
for name, rel, old_text, new_text, legs, expect in GUNS:
    raw = read(rel)
    old, new = to_bytes(rel, old_text), to_bytes(rel, new_text)
    hits, already = raw.count(old), raw.count(new)
    print(f"{name}: 锚点 {hits} 次 / 新文本已在场 {already} 次（须缺席） / 腿 {legs}")
    if hits != 1 or already != 0:
        bad += 1
    plan.append((name, rel, old, new, legs, expect))
if bad:
    print(f"BAD 普查有 {bad} 颗不对，整批停手")
    sys.exit(1)
if __import__("os").environ.get("QX_CENSUS_ONLY") == "1":
    print(f"仅普查：{len(plan)} 颗锚点全部命中一次，未放枪")
    sys.exit(0)

ORIG = {rel: read(rel) for rel in FILES}
ORIG_LINES = {rel: len(ORIG[rel].decode("utf-8").replace(eol(rel), "\n").splitlines()) for rel in FILES}
for rel in FILES:
    print(f"原件 {rel} sha {sha(ORIG[rel])} 行数 {ORIG_LINES[rel]}")


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
            ["taskkill", "/F", "/T", "/PID", str(proc.pid)], capture_output=True, check=False
        )
        out = proc.communicate()[0] or ""
        return "TIMEOUT", time.time() - started, out


def gate_leg() -> tuple[object, float, str]:
    return run_leg([sys.executable, "-X", "utf8", "tools/check_architecture.py"], GATE_TIMEOUT)


def ci_leg() -> tuple[object, float, str]:
    """把 ci.yml 里当前那条腿原样跑一遍——命令从被改过的文件现读，不手抄。"""
    matches = [
        item
        for item in text(CI).splitlines()
        if item.strip().startswith("run: cargo test -p qx-storage") and "--lib" in item
    ]
    if len(matches) != 1:
        return "NOLEG", 0.0, f"ci.yml 里读到 {len(matches)} 条 `--lib` 腿，无法照原样跑"
    return run_leg(matches[0].strip()[len("run: ") :].split(), CI_TIMEOUT)


def summarize(leg: str, rc: object, body_text: str) -> str:
    lines = body_text.splitlines()
    if rc == "TIMEOUT":
        return "整腿超时"
    if leg == "gate":
        rows = [line for line in lines if line.startswith("[FAIL]")]
        detail = [line for line in lines if "✗" in line]
        return f"rc={rc} 红 {len(rows)} 条 " + " | ".join(row[:220] for row in detail[:3])
    result = [line for line in lines if line.startswith("test result:")]
    nats = [line for line in lines if "nats::tests::" in line]
    ignored = [line for line in nats if line.rstrip().endswith("ignored")]
    errors = [line for line in lines if line.startswith("error")]
    if errors:
        return f"rc={rc} 编译红 {len(errors)} 条 / {errors[0][:90]}"
    return (
        f"rc={rc} {result[-1][:56] if result else '无 test result 行'} "
        f"nats 用例 {len(nats)} 颗在场 / {len(ignored)} 颗被标 ignored"
    )


print("\n=== 基线（未放枪）===")
restore()
rc, ci_seconds, body_text = ci_leg()
print(f"ci 腿 rc={rc} {ci_seconds:.1f}s {summarize('ci', rc, body_text)}")
if rc != 0:
    print("基线 ci 腿不绿，整批停手")
    sys.exit(1)
rc, gate_seconds, body_text = gate_leg()
print(f"gate 腿 rc={rc} {gate_seconds:.1f}s {summarize('gate', rc, body_text)}")
if rc != 0:
    print("基线门禁不绿，整批停手")
    sys.exit(1)

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
        rc, elapsed, body_text = gate_leg() if leg == "gate" else ci_leg()
        if rc == "TIMEOUT" or rc != 0:
            red = True
        flag = "红" if rc == "TIMEOUT" or rc != 0 else "绿"
        results.append(f"    {leg}: {flag} {elapsed:.1f}s {summarize(leg, rc, body_text)}")
    verdict = "KILLED" if red else "SURVIVED"
    killed += verdict == "KILLED"
    survived += verdict == "SURVIVED"
    print(f"[{verdict}] {name} 预期『{expect}』")
    for line in results:
        print(line)
    restore()

print("\n=== 收枪复跑 ===")
rc, elapsed, body_text = ci_leg()
print(f"ci 腿 rc={rc} {elapsed:.1f}s {summarize('ci', rc, body_text)}")
if rc != 0:
    print("BAD 收枪 ci 不绿")
rc, elapsed, body_text = gate_leg()
print(f"gate 腿 rc={rc} {elapsed:.1f}s {summarize('gate', rc, body_text)}")
if rc != 0:
    print("BAD 收枪 gate 不绿")
for rel in FILES:
    print(
        f"残留检查 {rel} sha {sha(read(rel))} 原件 {sha(ORIG[rel])} "
        f"行数 {len(text(rel).splitlines())} / 原 {len(ORIG[rel].decode('utf-8').replace(eol(rel), chr(10)).splitlines())}"
    )
print(f"\n合计 {len(plan)} 颗：KILLED {killed} / SURVIVED {survived} / 未放枪 {notfired}")
