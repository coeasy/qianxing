"""逐份 V11 快照量：全文件无主裸行号 / §54 之后的那一段 / §54.13 是否已在文件里。

要写进 §54.11 的那句"本轮自己那一章落下多少颗"必须来自可复现的快照，不能凭记忆。
"""
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
src = (HERE / "bare_orphan.py").read_text(encoding="utf-8")
ns = {}
head = src.split("TARGETS =")[0].replace("__file__", repr(str(HERE / "bare_orphan.py")))
head += "\nGLOBS = globals()\n"
exec(compile(head, "<probe>", "exec"), ns)
ns = ns["GLOBS"]
for k in ("CAP_PATHED_CITATION", "CAP_BARE_CITATION", "CAP_CONTINUATION"):
    ns[k] = ns["ns"][k]


def orphans(path):
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    start = next((n for n, l in enumerate(lines, 1) if l.startswith("### 54.")), len(lines) + 1)
    hits = []
    for number, line in enumerate(lines, 1):
        if "<!-- 史 -->" in line:
            continue
        spans, pathed, path_spans = [], [], []
        for match in ns["CAP_PATHED_CITATION"].finditer(line):
            spans.append((match.start(2), match.end(2)))
            path_spans.append((match.start(1), match.end(1)))
            tail = ns["CAP_CONTINUATION"].match(line, match.end())
            if tail:
                spans.append((tail.start(), tail.end()))
            got = ns["resolve"](match.group(1), line)
            if len(got) == 1:
                pathed.append((match.end(), got[0]))
        owners = list(pathed)
        for bp in ns["BARE_PATH"].finditer(line):
            if any(s <= bp.start() < e for s, e in path_spans):
                continue
            got = ns["resolve"](bp.group(1), line)
            if len(got) == 1:
                owners.append((bp.end(), got[0]))
        owners.sort()
        for bare in ns["CAP_BARE_CITATION"].finditer(line):
            if any(s <= bare.start(1) < e for s, e in spans):
                continue
            if any(pos < bare.start() for pos, _ in pathed):
                continue
            if not [o for o in owners if o[0] < bare.start()]:
                hits.append(number)
    tail = [n for n in hits if n >= start]
    has_13 = any(l.startswith("### 54.13") for l in lines)
    return len(lines), start, len(hits), len(tail), (tail[-1] if tail else 0), has_13


for p in sorted(HERE.glob("V11.md.snap*")) + [ROOT / "docs" / "自研量化框架重构方案-V11.md"]:
    rows, start, total, chapter, last, has13 = orphans(p)
    print("%-34s 行数 %4d ｜ §54 起于 %4d ｜ 全文件无主 %3d ｜ §54 之后 %3d ｜ 最靠后 %4d ｜ 有 §54.13 %s" % (
        p.name, rows, start, total, chapter, last, "是" if has13 else "否"))
