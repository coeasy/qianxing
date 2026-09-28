"""W11：把"无主裸行号"这一格从四份文本量到五份（V12 方案书进了引用名册）。

口径不自造：直接复用 `maturity/evidence/v11-r7/barecount.py` 里那颗 `count`，
它用的正则与解析器就是门禁本尊（`sys.path.insert(0, "tools")` 之后 import）。
差的一件事只是名册从四份变五份，所以这里只把 `DOC_CITATION_TARGETS` 摊开逐份量。
"""
import importlib.util
import sys
from pathlib import Path

HERE = Path(__file__).resolve()
ROOT = HERE.parents[3]
sys.path.insert(0, str(ROOT / "tools"))
import check_architecture as G  # noqa: E402

spec = importlib.util.spec_from_file_location("barecount", HERE.parent.parent / "v11-r7" / "barecount.py")
B = importlib.util.module_from_spec(spec)
spec.loader.exec_module(B)  # 它自己会跑一遍四份文本的 before/after，正是同一口径的复核

TEXTS = [(G.CAPABILITIES_FILE, "台账")] + [
    (rel, label) for rel, label, _r, _b in G.DOC_CITATION_TARGETS
]
rows = []
total_orphan = total_history_orphan = total_owned = 0
for rel, label in TEXTS:
    lines = (G.ROOT / rel).read_text(encoding="utf-8", errors="replace").splitlines()
    orphan, on_history, owned = B.count(lines)
    total_orphan += orphan
    total_history_orphan += on_history
    total_owned += owned
    rows.append(f"{label}\t{rel}\n  无主（判据外）{orphan:4d}   史钉行内另有 {on_history:3d}\n  有主（判据内）{owned:4d}")
rows.append(f"合计  无主 {total_orphan}   史钉行内 {total_history_orphan}   有主 {total_owned}")

out = ROOT / "maturity" / "evidence" / "v12-w11" / "orphan_after_v12.txt"
out.write_bytes(("\n".join(rows) + "\n").encode("utf-8"))
print("\n".join(rows))
