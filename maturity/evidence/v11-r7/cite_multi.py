# -*- coding: utf-8 -*-
"""跑 cite_check.audit 对若干份文档出逐份六格 + 无主裸号，终端只打 ASCII。"""
import importlib.util
import io
import json
import sys
from pathlib import Path

spec = importlib.util.spec_from_file_location("cite_check", Path(__file__).with_name("cite_check.py"))
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

files = sys.argv[1:] or [
    "CHANGELOG.md",
    "docs/自研量化框架重构方案-V11.md",
    "docs/自研量化框架白皮书-V10.md",
    "maturity/capabilities.yaml",
]
report = io.StringIO()
summary = []
for f in files:
    p = Path(f)
    if not p.exists():
        summary.append((f, "MISSING"))
        continue
    lines = p.read_text(encoding="utf-8").splitlines()
    out, total, bound, pinned, orphan = mod.audit(lines)
    reds = sum(len(v) for k, v in out.items() if k != "orphan")
    summary.append((f, total, bound, pinned, orphan, reds))
    report.write("=== %s :: rows=%d refs=%d named=%d histpin=%d orphan=%d reds=%d\n"
                 % (f, len(lines), total, bound, pinned, orphan, reds))
    for k in ("missing", "misaligned", "out_of_range", "blank", "wrong_symbol"):
        for row in out.get(k, []):
            report.write("  RED %-14s %s\n" % (k, row))
    for row in out.get("orphan", []):
        report.write("  ORP %-14s %s\n" % ("orphan", row))
Path(__file__).with_name("cite_multi_report.txt").write_text(report.getvalue(), encoding="utf-8")
for row in summary:
    print(json.dumps([str(x) for x in row], ensure_ascii=False).encode("ascii", "backslashreplace").decode())
