"""找出"无主裸行号"逐行计数的差异行：本轮重钉不该新增无主引用，
所以 CHANGELOG 那颗 +1 必须定位到行，不能只登记一个总数。"""
import sys
from pathlib import Path

sys.path.insert(0, "tools")
sys.path.insert(0, "scratch_r7")
import check_architecture as G  # noqa: E402
from barecount import count  # noqa: E402

DOCS = [
    ("台账", G.CAPABILITIES_FILE, "scratch_r7/snap8/maturity_capabilities.yaml"),
    ("方案书", "docs/自研量化框架重构方案-V11.md", "scratch_r7/snap8/docs_自研量化框架重构方案-V11.md"),
    ("变更日志", "CHANGELOG.md", "scratch_r7/snap8/CHANGELOG.md"),
    ("白皮书", "docs/SECURITY.md", "scratch_r7/snap8/docs_SECURITY.md"),
]


def per_line(lines):
    rows = []
    for line in lines:
        rows.append(count([line])[0])
    return rows


out = []
for label, rel, snap in DOCS:
    after = per_line(
        (G.ROOT / rel).read_text(encoding="utf-8", errors="replace").splitlines()
    )
    before = per_line(Path(snap).read_bytes().decode("utf-8", errors="replace").splitlines())
    out.append(f"#### {label} {rel}  行数 {len(before)} -> {len(after)}")
    if len(before) != len(after):
        out.append("  !! 行数不一致，逐行对比无意义")
        continue
    for index, (b, a) in enumerate(zip(before, after), 1):
        if b != a:
            cur = (G.ROOT / rel).read_bytes().decode("utf-8", errors="replace").splitlines()[index - 1]
            out.append(f"  L{index} 无主 {b} -> {a}   {cur.strip()[:150]}")
Path("scratch_r7/orphan_diff.txt").write_bytes(("\n".join(out) + "\n").encode("utf-8"))
print("done")
