# -*- coding: utf-8 -*-
"""R7-11 取证：迁进证据目录的三份 R6 脚本按新落点复跑，退出码与逐行输出一起落盘。

只读现仓，不改任何被审文件；输出写 maturity/evidence/v11-r7/r6_evidence_rerun.txt。
"""
import subprocess
import sys
from pathlib import Path

EV = Path("maturity/evidence/v11-r6")
OUT = Path("maturity/evidence/v11-r7/r6_evidence_rerun.txt")
RUNS = [
    ("probe_r69h.py（§54 起点切开的数法，对同版快照 V11.md.snap8）", [sys.executable, "-X", "utf8", str(EV / "probe_r69h.py")]),
    ("gate_shape_audit.py docs/SECURITY.md（今天的判据形状）", [sys.executable, "-X", "utf8", str(EV / "gate_shape_audit.py"), "docs/SECURITY.md"]),
    ("gate_shape_audit.py --legacy docs/SECURITY.md（R6-8 之前的旧形状）", [sys.executable, "-X", "utf8", str(EV / "gate_shape_audit.py"), "--legacy", "docs/SECURITY.md"]),
    ("bare_orphan.py 四份同版快照（合计 400 颗那一步的依赖：只有 V11 一份进了证据目录）", [sys.executable, "-X", "utf8", str(EV / "bare_orphan.py")] + [str(EV / "V11.md.snap8")]),
]
rows = ["R7-11 复跑：迁进 maturity/evidence/v11-r6/ 的 R6 取证脚本按新落点改口之后"]
for label, cmd in RUNS:
    p = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace", cwd=".")
    rows.append("== %s" % label)
    rows.append("   cmd: %s" % " ".join(cmd[-2:]))
    rows.append("   rc=%d" % p.returncode)
    for line in (p.stdout or "").splitlines():
        rows.append("   | %s" % line)
    for line in (p.stderr or "").splitlines():
        rows.append("   ! %s" % line)
OUT.write_text("\n".join(rows) + "\n", encoding="utf-8")
print("wrote %s lines=%d" % (OUT.as_posix(), OUT.read_text(encoding='utf-8').count('\n')))
print("rcs=" + " ".join(r.strip() for r in rows if r.startswith("   rc=")))
