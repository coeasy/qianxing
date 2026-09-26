"""V11 R7：量"无主裸行号"——一行里既没有带行号的引用、也没有能唯一解析的文件名时，
那颗裸 `:NNN` 落在门禁判据之外。同一条规则跑两份文本（snap8 = 本轮重钉之前，
当前工作树 = 之后），这样"这轮收掉了多少颗"是量出来的，不是推算的。"""
import sys
from pathlib import Path

sys.path.insert(0, "tools")
import check_architecture as G  # noqa: E402

DOCS = [
    ("台账", G.CAPABILITIES_FILE, "scratch_r7/snap8/maturity_capabilities.yaml"),
    ("方案书", "docs/自研量化框架重构方案-V11.md", "scratch_r7/snap8/docs_自研量化框架重构方案-V11.md"),
    ("变更日志", "CHANGELOG.md", "scratch_r7/snap8/CHANGELOG.md"),
    ("白皮书", "docs/SECURITY.md", "scratch_r7/snap8/docs_SECURITY.md"),
]


def count(lines):
    orphan = 0
    orphan_on_history = 0
    owned = 0
    for line in lines:
        history = G.CITATION_HISTORY_MARK in line
        hint = set(G.re.findall(r"(?:crates/)?(qx-[a-z][a-z0-9\-]*)", line))
        spans = []
        owners = []
        for match in G.CAP_PATHED_CITATION.finditer(line):
            spans.append((match.start(1), match.end()))
            tail = G.CAP_CONTINUATION.match(line, match.end())
            if tail:
                spans.append((tail.start(), tail.end()))
            found = G._citation_candidates(match.group(1), hint)
            if len(found) == 1:
                owners.append((match.end(), found[0]))
        for spot in G.CAP_BARE_PATH.finditer(line):
            named = G._citation_candidates(spot.group(1), hint)
            if len(named) == 1:
                owners.append((spot.end(), named[0]))
        owners.sort()
        for bare in G.CAP_BARE_CITATION.finditer(line):
            if any(start <= bare.start(1) < end for start, end in spans):
                continue
            prior = [row for row in owners if row[0] < bare.start()]
            if prior:
                owned += 1
            elif history:
                orphan_on_history += 1
            else:
                orphan += 1
    return orphan, orphan_on_history, owned


out = []
totals_before = [0, 0]
totals_after = [0, 0]
for label, rel, snap in DOCS:
    after = count((G.ROOT / rel).read_text(encoding="utf-8", errors="replace").splitlines())
    before = count(
        Path(snap).read_bytes().decode("utf-8", errors="replace").splitlines()
    )
    totals_before[0] += before[0] + before[1]
    totals_after[0] += after[0] + after[1]
    totals_before[1] += before[2]
    totals_after[1] += after[2]
    out.append(
        f"{label}\t{rel}\n"
        f"  无主（判据外）  {before[0]:4d} -> {after[0]:4d}"
        f"   （史钉行内另有 {before[1]:3d} -> {after[1]:3d}）\n"
        f"  有主（判据内）  {before[2]:4d} -> {after[2]:4d}\n"
    )
out.append(
    f"合计  无主 {totals_before[0]} -> {totals_after[0]}"
    f"   有主 {totals_before[1]} -> {totals_after[1]}\n"
)
Path("scratch_r7/barecount2.txt").write_bytes(("\n".join(out) + "\n").encode("utf-8"))
print("done")
