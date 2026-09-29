"""引用「读原文」取证器（V13 第 8 轮 A1/A2）：把一行里的每颗行号引用打开成目标行原文，摊给人读。

为什么需要它：门禁对行号引用只问三格——数字在那份文件的行数之内、那一行不空、同名候选不歧义。
它不问「那一行说的是不是文案指的东西」。V13 第 8 轮 A1/A2 实测到 `maturity/capabilities.yaml` 第 479 行
两颗调用点引用就这么绿着指错了地方（WSS 那颗写成 136 行、plain HTTP 写成 657 行，真实落点是 142 与 696，
中间还夹着一次 `cargo fmt` 造成的整体下移）。这类漂移没有任何判据会红，只能读——本格就是把 W10 轮那套
人工读法搬成可复跑的打印。

它**不判红**：打印出来的是原文，判「对不对」仍然要人读，因为「这一行是不是在说这件事」不是能被正则问出来的。
裸引用的归属口径与门禁一致——同一行里最近一颗已经解析成功的带路径引用。

用法（从仓根跑）：
    python -X utf8 maturity/evidence/v13-r8/citation_readout.py 418 479 486 487 656
    python -X utf8 maturity/evidence/v13-r8/citation_readout.py --file docs/SECURITY.md 40 41
不带行号参数就通读整份文件。
"""

import pathlib
import re
import subprocess
import sys

sys.stdout.reconfigure(encoding="utf-8")
ROOT = pathlib.Path(".")
DEFAULT_TARGET = "maturity/capabilities.yaml"

CAP = re.compile(
    r"(?P<path>[A-Za-z0-9_./一-鿿\-]+\.(?:rs|py|yaml|yml|md|json|toml|sql|ts|tsx|txt)):(?P<pn>\d+)"
    r"|(?<![\w/.:\-]):(?P<bn>\d+)"
)

target = DEFAULT_TARGET
if len(sys.argv) > 1 and sys.argv[1] == "--file":
    target = sys.argv[2]
    sys.argv = [sys.argv[0], *sys.argv[3:]]

body = (ROOT / target).read_bytes().decode("utf-8", errors="replace").replace("\r\n", "\n").splitlines()
rows = [int(x) for x in sys.argv[1:]] or list(range(1, len(body) + 1))

index: dict[str, list[pathlib.Path]] = {}
listing: list[str] = []
for extra in ([], ["--others", "--exclude-standard"]):
    listing += subprocess.run(
        ["git", "ls-files", *extra],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=True,
    ).stdout.splitlines()
seen = set()
for rel_path in listing:
    if rel_path.startswith("scratch_") or rel_path in seen:
        continue
    seen.add(rel_path)
    path = ROOT / rel_path
    if path.is_file():
        index.setdefault(path.name, []).append(path)

CACHE: dict[str, list[str]] = {}


def lines_of(rel: str) -> list[str] | None:
    """按文件名取那份文件；同名多份时必须整颗路径能对上，否则按歧义处理（与门禁同一口径）。"""
    name = rel.split("/")[-1]
    cands = index.get(name, [])
    hit = cands[0] if len(cands) == 1 else next(
        (c for c in cands if str(c).replace("\\", "/") == rel), None
    )
    if hit is None:
        return None
    key = str(hit)
    if key not in CACHE:
        CACHE[key] = hit.read_bytes().decode("utf-8", errors="replace").replace("\r\n", "\n").splitlines()
    return CACHE[key]


total = bare_total = unresolved = 0
for row in rows:
    text = body[row - 1]
    matches = list(CAP.finditer(text))
    if not matches:
        continue
    pathed = sum(1 for m in matches if m.group("path"))
    bare_total += len(matches) - pathed
    print(f"\n=== {target} 第 {row} 行：{pathed} 颗带路径 + {len(matches) - pathed} 颗裸引用 ===")
    owner: str | None = None
    for m in matches:
        rel, num = m.group("path"), m.group("pn") or m.group("bn")
        if rel:
            owner = rel
        elif owner is None:
            print(f"  [ORPHAN] :{num} 这一行在此之前没有能解析的归属")
            unresolved += 1
            continue
        total += 1
        tag = owner if rel else f"{owner}（承上）"
        cited = lines_of(owner)
        if cited is None:
            print(f"  [MISS] {tag}:{num} 仓内找不到或同名歧义")
            unresolved += 1
            continue
        n = int(num)
        if n < 1 or n > len(cited):
            print(f"  [OUT] {tag}:{n} 越界（该文件 {len(cited)} 行）")
            unresolved += 1
            continue
        print(f"  {tag}:{n}  |{cited[n - 1].strip()[:170]}")
print(f"\n合计 {total} 颗（其中裸引用 {bare_total} 颗）：读不出目标行 {unresolved} 颗")
print("本格只摊原文不判红——判「这一行说的是不是文案指的东西」要人读。")
