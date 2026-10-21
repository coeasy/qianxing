"""R9 独立交叉检查：类型面（pub struct / pub enum / pub type）的生产读者统计。

门禁的零读者判据只索引 pub fn / pub const / pub(crate) fn，类型完全不在扫描范围内。
本脚本复用门禁的 _production_lines 与 RE_EXPORT_LINE 做同族统计，找出零生产读者的类型。
只读：不修改仓库任何文件。
"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

REPO = str(Path(__file__).resolve().parents[1])

# 复用门禁自身的裁剪口径，避免自己造一套判据
gate_src = open(f"{REPO}/tools/check_architecture.py", encoding="utf-8").read()


def _load_gate_module():
    """把门禁当作模块加载，只取其字符串裁剪与正则定义。"""
    ns: dict = {"__file__": f"{REPO}/tools/check_architecture.py"}
    # 门禁顶层会执行 sys.exit 之外的定义；这里只 exec 到 import 块结束后的常量定义段。
    # 更稳的做法：用 ast 抽出需要的名字。
    import ast

    tree = ast.parse(gate_src)
    wanted = {"_production_lines", "_strip_cfg_test_items", "RE_EXPORT_LINE", "TEST_PATH", "CFG_TEST_MARKER"}
    grab: list[str] = ["import re"]
    for node in tree.body:
        if isinstance(node, (ast.Import, ast.ImportFrom, ast.Assign, ast.AnnAssign)):
            names: set[str] = set()
            if isinstance(node, ast.Assign):
                for t in node.targets:
                    if isinstance(t, ast.Name):
                        names.add(t.id)
            elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
                names.add(node.target.id)
            if names & wanted:
                grab.append(ast.get_source_segment(gate_src, node) or "")
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name in wanted:
            grab.append(ast.get_source_segment(gate_src, node) or "")
    src = "\n\n".join(g for g in grab if g)
    ns2: dict = {"__file__": f"{REPO}/tools/check_architecture.py"}
    exec(compile(src, "gate_extract", "exec"), ns2)
    return ns2


gate = _load_gate_module()
_production_lines = gate["_production_lines"]
RE_EXPORT_LINE = gate["RE_EXPORT_LINE"]

TYPE_DEF = re.compile(r"^\s*pub\s+(?:struct|enum|type)\s+(\w+)")
# serde/derive 属性行不算定义
DERIVE_LINE = re.compile(r"^\s*(\[[\w:]+\]|pub\s+use\s)")

ROOT = Path(REPO)
CRATES = ROOT / "crates"

sources: dict[str, list[str]] = {}
for p in sorted(CRATES.glob("*/src/**/*.rs")):
    rel = p.relative_to(ROOT).as_posix()
    if re.search(r"(/tests/|test_)", rel):
        continue
    sources[rel] = _production_lines(p.read_text(encoding="utf-8"))

defs: dict[str, list[tuple[str, int]]] = {}
definition_lines: dict[str, set[tuple[str, int]]] = {}
type_names: set[str] = set()
for path, lines in sources.items():
    crate = path.split("/")[1]
    for i, line in enumerate(lines, 1):
        m = TYPE_DEF.match(line)
        if not m:
            continue
        name = m.group(1)
        defs.setdefault(f"{crate}::{name}", []).append((path, i))
        definition_lines.setdefault(name, set()).add((path, i))
        type_names.add(name)

print(f"类型定义总数: {len(defs)}")
print(f"生产源码文件数: {len(sources)}")
print()

# 建一次按标识符的读者索引，避免「每个类型 × 所有源码行」的重复正则扫描。
# 统计口径仍是生产代码中出现过同名类型标识符的行，且跳过重导出和定义行。
identifier = re.compile(r"\b[A-Za-z_]\w*\b")
reader_lines: dict[str, set[tuple[str, int]]] = {}
for path, lines in sources.items():
    for i, line in enumerate(lines, 1):
        if RE_EXPORT_LINE.match(line):
            continue
        candidates = set(identifier.findall(line)) & type_names
        for name in candidates:
            if (path, i) not in definition_lines.get(name, set()):
                reader_lines.setdefault(name, set()).add((path, i))

orphans = [
    (key, sites)
    for key, sites in sorted(defs.items())
    if not reader_lines.get(key.split("::", 1)[1])
]

print(f"=== 零生产读者类型: {len(orphans)} ===")
for key, sites in orphans:
    where = ", ".join(f"{p}:{n}" for p, n in sites)
    print(f"  {key}   [{where}]")
