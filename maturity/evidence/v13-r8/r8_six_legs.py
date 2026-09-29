"""V13 第 8 轮收口的六条腿驱动：每条腿跑之前先把终态字节重新哈希，跑完只登记读出来的数。

口径写死在这里，避免"手抄门禁数字"：
- 判分只看 `test result:` 行（ok 与 FAILED 都算一条），段数与 passed/failed/ignored 逐项相加；
  读不出结果行、读不出任何 passed、有非 ok 段或有 failed 时，这一腿判红：判分器失灵不许被读成通过；
- Clippy 的牙齿是"诊断行数为 0"，所以要剥掉 cargo 自己的 `Checking`/`Finished`/`Blocking`/`warning: unused manifest key` 之类非诊断行；
- 每条腿跑之前重新哈希本轮改过的文件，跑与跑之间字节变了就当场点名（背景轮次会还原文件，这一格防的是"绿的是上一份字节"）。
"""

from __future__ import annotations

import hashlib
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
# 落点按调用给：本轮要把"中途那份"与"终态字节那份"分开存，覆盖掉前一份就没有对照了
# （`r8_measures.py` 读中段那份的门禁自报项数，是为了抓"轮中途有人删判据"）。
OUT = (
    (Path(__file__).parent / sys.argv[1]).resolve()
    if len(sys.argv) > 1
    else Path(__file__).with_suffix(".txt")
)

WATCH = (
    "tools/check_architecture.py",
    "maturity/capabilities.yaml",
    "crates/qx-storage/src/postgres.rs",
    "crates/qx-storage/src/nats.rs",
    "crates/qx-adapter/src/lib.rs",
    "docs/SECURITY.md",
    "README.md",
)

LOG = []


def emit(line: str = "") -> None:
    LOG.append(line)
    with OUT.open("w", encoding="utf-8", newline="\n") as fh:
        fh.write("\r\n".join(LOG) + "\r\n")


def sha(rel: str) -> str:
    return hashlib.sha256((ROOT / rel).read_bytes()).hexdigest()[:12]


def die(message: str) -> None:
    # 诊断行一律 ASCII 转义：这台机器的控制台是 GBK，带 CJK 的 refusal 会先炸成
    # UnicodeEncodeError 回溯，而不是留下这一行可读的判决。
    emit("ABORT " + message.encode("ascii", "backslashreplace").decode("ascii"))
    raise SystemExit(2)


def census(tag: str) -> None:
    emit(f"[{tag}] sha " + " ".join(f"{Path(p).name}={sha(p)}" for p in WATCH))


def run(name: str, argv: list[str]):
    emit(f"== LEG {name}: {' '.join(argv)}")
    started = time.time()
    proc = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace")
    out = (proc.stdout or "") + (proc.stderr or "")
    emit(f"-- LEG {name} rc={proc.returncode} elapsed={time.time() - started:.0f}s")
    return proc, out


CLIPPY_NOISE = re.compile(
    r"^(?:\s*(?:Checking|Compiling|Finished|Blocking|Updating|Locking|Downloading|Downloaded|warning: unused manifest key)"
    r"|warning: .*generated \d+ warning"
    r"|error: could not compile"
    r"|\s+-->)"
)


RESULT_ROW = re.compile(
    r"^test result: (\S+)\. (\d+) passed; (\d+) failed; (\d+) ignored", re.M
)


def parse_result_rows(out: str) -> list[tuple[str, int, int, int]]:
    # 返回的是 (状态, passed, failed, ignored) 且计数已转成 int：
    # `re.findall` 只会给字符串，第一版拿 int 字面量比对它，自测当场判红并中止了这一轮。
    return [(status, int(p), int(f), int(i)) for status, p, f, i in RESULT_ROW.findall(out)]


def selftest_classifier() -> None:
    """判分器自己先被打一发：真实 cargo 行必须读得出数，读不出就不许进任何腿。

    这一格不是形式：第一版把正则的点号前多写了一个空格（`ok` 与点号之间），
    两条 cargo 腿因此各报 `SEGMENTS=0`，而 `RESULT legs_failed=0` 照样绿着 ——
    判分器失灵时"没有结果行"被读成了"通过"。
    """
    ok = "test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.28s\n"
    bad = "test result: FAILED. 11 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s\n"
    rows = parse_result_rows(ok + bad)
    if len(rows) != 2:
        die(f"CLASSIFIER_SELFTEST rows={len(rows)} want=2 (判分器读不出真实 cargo 行，不进腿)")
    if rows[0] != ("ok", 12, 0, 0) or rows[1] != ("FAILED", 11, 1, 0):
        die(f"CLASSIFIER_SELFTEST parsed={rows!r}")
    if parse_result_rows("test result: ok 12 passed\n"):
        die("CLASSIFIER_SELFTEST 读进了不完整的行")
    emit("CLASSIFIER_SELFTEST rows=2 ok")


def score_tests(out: str) -> tuple[str, bool]:
    rows = parse_result_rows(out)
    if not rows:
        return "SEGMENTS=0 (没有结果行：判分器失灵，这一格判红)", True
    passed = sum(r[1] for r in rows)
    failed = sum(r[2] for r in rows)
    ignored = sum(r[3] for r in rows)
    bad = sum(1 for r in rows if r[0] != "ok")
    unusable = passed == 0 or failed != 0 or bad != 0
    return (
        f"SEGMENTS={len(rows)} non-ok={bad} passed={passed} failed={failed} ignored={ignored}",
        unusable,
    )


def main() -> int:
    emit(f"python={sys.version.split()[0]} cwd={ROOT} out={OUT.name}")
    selftest_classifier()
    census("stage-0")
    failures = 0

    # 1) validate_core
    proc, out = run("validate_core", [sys.executable, "-X", "utf8", "tools/validate_core.py"])
    tail = [l for l in out.splitlines() if l.strip()][-3:] if out.strip() else []
    emit("   tail: " + " | ".join(t[-90:] for t in tail))
    failures += proc.returncode != 0

    # 2) 架构门禁
    proc, out = run("check_architecture", [sys.executable, "-X", "utf8", "tools/check_architecture.py"])
    npass = sum(1 for l in out.splitlines() if "[PASS]" in l)
    nfail = sum(1 for l in out.splitlines() if "[FAIL]" in l)
    marks = out.count("\u2717")
    emit(f"   GATE_EXIT={proc.returncode} [PASS]={npass} [FAIL]={nfail} cross-mark={marks}")
    for l in out.splitlines():
        if "[FAIL]" in l:
            emit("   RED " + l[:220].encode("ascii", "backslashreplace").decode("ascii"))
    failures += proc.returncode != 0

    # 3) Python 用例
    proc, out = run("python_tests", [sys.executable, "-X", "utf8", "-m", "unittest", "discover", "-s", "python/tests", "-q"])
    tail = [l for l in out.splitlines() if l.strip()][-4:]
    emit("   tail: " + " | ".join(t[:110].encode("ascii", "backslashreplace").decode("ascii") for t in tail))
    failures += proc.returncode != 0

    census("before-cargo")

    # 4) fmt
    proc, out = run("cargo_fmt", ["cargo", "fmt", "--all", "--", "--check"])
    emit("   FMT_EXIT=" + str(proc.returncode) + " diff-lines=" + str(len([l for l in out.splitlines() if l.strip()])))
    failures += proc.returncode != 0

    # 5) 过期 binary 守卫：先真造一颗 nats 特性的 qx-cli，再跑 Clippy
    proc, out = run("cargo_build_cli_nats", ["cargo", "build", "-p", "qx-cli", "--features", "nats"])
    failures += proc.returncode != 0

    # 6) clippy
    proc, out = run("clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])
    raw = [l for l in out.splitlines() if l.strip()]
    diags = [l for l in raw if not CLIPPY_NOISE.match(l)]
    emit(f"   CLIPPY_EXIT={proc.returncode} non-empty-lines={len(raw)} diagnostic-lines={len(diags)}")
    for l in diags[:15]:
        emit("   DIAG " + l[:180].encode("ascii", "backslashreplace").decode("ascii"))
    failures += proc.returncode != 0 or len(diags) != 0

    census("before-test")

    # 7) 整树用例
    proc, out = run("cargo_test_workspace", ["cargo", "test", "--workspace", "--all-targets", "--no-fail-fast"])
    tally, unusable = score_tests(out)
    emit("   " + tally)
    for line in out.splitlines():
        if line.startswith("test result:"):
            emit("   ROW " + line[:150].encode("ascii", "backslashreplace").decode("ascii"))
    for l in out.splitlines():
        if l.startswith("error") or "FAILED" in l and "test result" not in l:
            emit("   TRED " + l[:180].encode("ascii", "backslashreplace").decode("ascii"))
    failures += proc.returncode != 0 or unusable

    # 8) 特性矩阵那一腿（门后用例的唯一执行者）
    proc, out = run(
        "feature_matrix_lib",
        ["cargo", "test", "-p", "qx-storage", "--features", "sqlite,postgres,nats", "--lib", "--no-fail-fast"],
    )
    tally, unusable = score_tests(out)
    emit("   " + tally)
    for line in out.splitlines():
        if line.startswith("test result:"):
            emit("   ROW " + line[:150].encode("ascii", "backslashreplace").decode("ascii"))
    failures += proc.returncode != 0 or unusable

    census("final")
    emit(f"RESULT legs_failed={failures}")
    emit("DONE")
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
