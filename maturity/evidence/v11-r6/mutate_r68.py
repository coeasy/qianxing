"""R6-8 / R6-9 的变异取证：六颗新牙齿逐颗打红，并把"旧形状看不看得到"量出来而不是我口头断言。

规则与平时一样：先量绿色基线，一颗一颗打，落盘用二进制、还原按 sha 逐字节校验，
整套跑在前台（后台跑变异会互相踩文件）。每颗变异同时问五遍：
  A 常驻门禁（今天这一版）—— 杀手的判据在这里
  B 快照门禁（maturity/evidence/v11-r6/check_architecture.py.snap_r68，R6-8 之前的门禁）—— 看旧门禁有没有牙齿
  C/D/E 探针三种判据形状（gate_shape_audit.py）：R6-8 之前 / R6-8 / R6-9 —— 逐颗退牙看谁在说话
分层计数只认真实观测：断言红 = cargo 用例先红；仅门禁红 = 只有常驻门禁红；SURVIVED = 打完没人说话。
"""
import hashlib
import re
import subprocess
import sys

FILES = {
    "SEC": "docs/SECURITY.md",
    "V11": "docs/自研量化框架重构方案-V11.md",
    "CHG": "CHANGELOG.md",
}
GATE_NEW = [sys.executable, "-X", "utf8", "tools/check_architecture.py"]
GATE_OLD = [sys.executable, "-X", "utf8", "maturity/evidence/v11-r6/check_architecture.py.snap_r68"]
PROBE = [sys.executable, "-X", "utf8", "maturity/evidence/v11-r6/gate_shape_audit.py"]
# (编号, 文件, 旧串, 新串, 期望红的判据关键词)
MUTATIONS = [
    ("M1 名字牙齿：把带名引用挪到同一文件里另一颗合法行",
     "SEC",
     "`submit_command`（`crates/qx-api/src/lib.rs:1596`）",
     "`submit_command`（`crates/qx-api/src/lib.rs:1597`）",
     "点名某颗东西"),
    ("M2 区间远端：把刚修好的 73-85 改回修复前的 73-86（86 是空行）",
     "CHG",
     "`leg_funding.rs:73-85`",
     "`leg_funding.rs:73-86`",
     "指过去的那一行不是空行"),
    ("M6 同行点名归属：:600 前只出现过 venue.rs 这个文件名，没有带行号的引用",
     "SEC",
     "\n## 10.",
     "\n负控：`crates/qx-xingban/src/venue.rs` 的下一半在 :600。\n\n## 10.",
     "没有越出被引文件的长度"),
    ("M3 位置化裸引用：:600 前面最近的是 venue.rs（573 行），行内最后一颗是 lib.rs（2445 行）",
     "SEC",
     "\n## 10.",
     "\n负控：`crates/qx-xingban/src/venue.rs:10` 的另一半在 :600，后面还有 `crates/qx-api/src/lib.rs:10`。\n\n## 10.",
     "没有越出被引文件的长度"),
    ("M4 逐份地板：把白皮书砍到只剩前 200 行（引用密度整体塌掉）",
     "SEC",
     None,
     None,
     "扫得到安全白皮书"),
    ("M5 史钉是承重的：抽掉 V11 §4 那一行的 `<!-- 史 -->`",
     "V11",
     "`cli.rs:133-674`）；CLI 深取他 crate 内部 <!-- 史 -->",
     "`cli.rs:133-674`）；CLI 深取他 crate 内部",
     "没有越出被引文件的长度"),
]


def gate_fails(cmd):
    proc = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    out = proc.stdout + proc.stderr
    fails = {line for line in out.splitlines() if line.startswith("[FAIL]")}
    count = next((line for line in out.splitlines() if "架构不变量自检" in line), "?")
    return proc.returncode, fails, count


def probe(doc, shape):
    """同一份文档在三种判据形状下各扫一遍：R6-8 之前 / R6-8 / R6-9。"""
    flags = {"legacy": ["--legacy"], "r68": [], "r69": ["--bare-path-owner"]}[shape]
    cmd = PROBE + [doc] + flags
    proc = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    out = proc.stdout + proc.stderr
    row = next((line for line in out.splitlines() if "判据形状=" in line), "?")
    problems = int(re.search(r"问题 (\d+) 处", out).group(1)) if "问题 " in out else -1
    hits = sorted(line.strip() for line in out.splitlines() if line.startswith("  "))
    return problems, row[:20], hits


def main():
    originals = {key: open(path, "rb").read() for key, path in FILES.items()}
    hashes = {key: hashlib.sha256(raw).hexdigest()[:16] for key, raw in originals.items()}
    code, base_new, count = gate_fails(GATE_NEW)
    ocode, base_old, ocount = gate_fails(GATE_OLD)
    print("基线：新门禁 exit=%d %s" % (code, count))
    for line in sorted(base_new):
        print("   新门禁基线不该有红：" + line[:200])
    print("基线：快照门禁 exit=%d %s（红 %d 颗，逐颗按集合差比对，不当基线清白）" % (ocode, ocount, len(base_old)))
    if code != 0:
        sys.exit(1)

    killed = survived = 0
    for label, key, old, new, want in MUTATIONS:
        path = FILES[key]
        raw = originals[key]
        if old is None:  # M4：整份截断
            mutated = b"\n".join(raw.split(b"\n")[:200])
        else:
            old_b, new_b = old.encode("utf-8"), new.encode("utf-8")
            assert raw.count(old_b) == 1, "锚点不是恰好一颗：%r ×%d" % (old[:40], raw.count(old_b))
            mutated = raw.replace(old_b, new_b, 1)
        open(path, "wb").write(mutated)
        try:
            code, fails, count = gate_fails(GATE_NEW)
            ocode, ofails, ocount = gate_fails(GATE_OLD)
            shapes = {s: probe(path, s) for s in ("legacy", "r68", "r69")}
        finally:
            open(path, "wb").write(originals[key])
            assert hashlib.sha256(open(path, "rb").read()).hexdigest()[:16] == hashes[key]
        hit = [line for line in fails if want in line]
        tier = "仅门禁红" if hit else ("断言红" if code != 0 else "SURVIVED")
        if hit or code != 0:
            killed += 1
        else:
            survived += 1
        print("%s\n  → 新门禁 %s（%s）｜旧门禁新增红 %d｜三形状问题数 R6-8 前=%d R6-8=%d R6-9=%d" % (
            label, tier, "exit=%d" % code, len(ofails - base_old),
            shapes["legacy"][0], shapes["r68"][0], shapes["r69"][0],
        ))
        for line in (hit or sorted(fails))[:2]:
            print("     " + line[:230])
        for shape in ("legacy", "r68", "r69"):
            for row in shapes[shape][2][:1]:
                print("     探针·%s %s" % (shape, row[:170]))
        if tier == "SURVIVED":
            print("     ✗ 这颗存活，判据没有牙齿")
    print("变异 6 颗：杀掉 %d，存活 %d（六颗全打在文档上，无 cargo 用例参与，最高档只到「仅门禁红」）" % (killed, survived))
    code, fails, count = gate_fails(GATE_NEW)
    print("还原后复测：exit=%d %s" % (code, count))
    for line in sorted(fails):
        print("   还原不该有红：" + line[:200])
    sys.exit(0 if code == 0 and not survived else 1)


if __name__ == "__main__":
    main()
