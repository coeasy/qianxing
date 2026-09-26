"""量"裸 :NNN 的候选归属扩到行内无行号的文件路径"这一颗牙齿会打到什么。

现状：常驻门禁的位置化归属只认「行内它之前最近的、带行号且唯一解析成功的引用」。
如果一行写成 `crates/qx-api/src/lib.rs 的 X（:590）`——路径出现了但没带行号——那 :590 无人核对。
这里把这类"当前盲区"逐颗量出来，并按候选规则（把无行号的路径也算归属）给出判决，
用于决定是补牙还是登记缺口。判决分四档：无归属 / 越界 / 落空行 / 对得上。
"""
import ast, os, re
from pathlib import Path

ROOT = Path(os.path.dirname(os.path.abspath(__file__))).parents[2]
src = (ROOT / "tools" / "check_architecture.py").read_text(encoding="utf-8")
tree = ast.parse(src)
keep = {"CAP_PATHED_CITATION", "CAP_CONTINUATION", "CAP_BARE_CITATION", "CAP_CITATION_ROOTS",
        "CAP_CITATION_EXTS", "CAP_BARE_PATH"}
ns = {"re": re}
caps = {}
for node in ast.walk(tree):
    if isinstance(node, ast.Assign):
        for t in node.targets:
            if isinstance(t, ast.Name) and t.id in keep:
                caps[t.id] = ast.get_source_segment(src, node.value)
assert set(caps) == keep, sorted(keep - set(caps))
for k in ("CAP_PATHED_CITATION", "CAP_CONTINUATION", "CAP_BARE_CITATION", "CAP_BARE_PATH"):
    ns[k] = eval(caps[k], ns)
roots = ast.literal_eval(caps["CAP_CITATION_ROOTS"])
exts = ast.literal_eval(caps["CAP_CITATION_EXTS"])
BARE_PATH = ns["CAP_BARE_PATH"]

suffixes = {}
for root_name in roots:
    base = ROOT / root_name
    if base.is_dir():
        for cand in base.rglob("*"):
            if cand.is_file() and cand.suffix in exts:
                rel = cand.relative_to(ROOT).as_posix()
                parts = rel.split("/")
                for start in range(len(parts)):
                    suffixes.setdefault("/".join(parts[start:]), []).append(rel)

_cache = {}


def lines_of(rel):
    if rel not in _cache:
        _cache[rel] = (ROOT / rel).read_text(encoding="utf-8", errors="replace").splitlines()
    return _cache[rel]


def resolve(raw, line):
    if (ROOT / raw).is_file():
        return [raw]
    for probe in ("crates/%s" % raw, "python/%s" % raw, "tools/%s" % raw):
        if (ROOT / probe).is_file():
            return [probe]
    parts = raw.split("/")
    if len(parts) >= 2 and parts[-1].endswith(".rs") and not parts[-2].startswith("qx-"):
        probe = "crates/qx-%s/%s" % (parts[-2], "/".join(parts[-2:]))
        if (ROOT / probe).is_file():
            return [probe]
    found = suffixes.get(raw) or suffixes.get(parts[-1]) or []
    hint = set(re.findall(r"(?:crates/)?(qx-[a-z][a-z0-9\-]*)", line))
    scoped = [c for c in found if any(c.startswith("crates/%s/" % h) for h in hint)]
    return scoped or sorted(set(found))


TARGETS = tuple(
    a for a in __import__("sys").argv[1:]
    if not a.startswith("-") and (ROOT / a).exists() and not a.endswith(".py")
) or ("maturity/capabilities.yaml", "docs/SECURITY.md", "docs/自研量化框架重构方案-V11.md", "CHANGELOG.md")
tot = {"无归属": 0, "越界": 0, "落空行": 0, "对得上": 0}
samples = {k: [] for k in tot}
for rel in TARGETS:
    counts = {"无归属": 0, "越界": 0, "落空行": 0, "对得上": 0}
    orphan_lines = []
    lines = (ROOT / rel).read_text(encoding="utf-8", errors="replace").splitlines()
    for number, line in enumerate(lines, 1):
        if "<!-- 史 -->" in line:
            continue
        spans = []
        pathed = []
        path_spans = []
        for match in ns["CAP_PATHED_CITATION"].finditer(line):
            spans.append((match.start(2), match.end(2)))
            path_spans.append((match.start(1), match.end(1)))
            tail = ns["CAP_CONTINUATION"].match(line, match.end())
            if tail:
                spans.append((tail.start(), tail.end()))
            got = resolve(match.group(1), line)
            if len(got) == 1:
                pathed.append((match.end(), got[0]))
        owners = list(pathed)
        for bp in BARE_PATH.finditer(line):
            if any(s <= bp.start() < e for s, e in path_spans):
                continue
            got = resolve(bp.group(1), line)
            if len(got) == 1:
                owners.append((bp.end(), got[0]))
        owners.sort()
        for bare in ns["CAP_BARE_CITATION"].finditer(line):
            if any(s <= bare.start(1) < e for s, e in spans):
                continue
            if any(pos < bare.start() for pos, _ in pathed):
                continue  # 现在已经被位置化规则覆盖，不进盲区统计
            prior = [owner for pos, owner in owners if pos < bare.start()]
            ref = int(bare.group(1))
            if not prior:
                verdict = "无归属"
                detail = ""
            else:
                body = lines_of(prior[-1])
                if not 1 <= ref <= len(body):
                    verdict = "越界"
                    detail = "%s 只有 %d 行" % (prior[-1], len(body))
                elif not body[ref - 1].strip():
                    verdict = "落空行"
                    detail = prior[-1]
                else:
                    verdict = "对得上"
                    detail = prior[-1]
            counts[verdict] += 1
            tot[verdict] += 1
            if verdict == "无归属":
                orphan_lines.append(number)
            if verdict in ("越界", "落空行") or len(samples[verdict]) < 8:
                samples[verdict].append("%s:%d :%s %s ｜ %s" % (rel, number, ref, detail, line.strip()[:96]))
    span = "最靠前 %d、最靠后 %d" % (orphan_lines[0], orphan_lines[-1]) if orphan_lines else "无"
    print("%-42s 盲区 %d 颗 ｜ 越界 %d 落空行 %d 对得上 %d 仍无归属 %d ｜ %s" % (
        rel, sum(counts.values()), counts["越界"], counts["落空行"], counts["对得上"], counts["无归属"], span))
print("合计：候选规则能量到 %d 颗，其中判红 %d 颗（越界 %d + 空行 %d），对得上 %d 颗，仍然无人核对 %d 颗" % (
    tot["越界"] + tot["落空行"] + tot["对得上"], tot["越界"] + tot["落空行"], tot["越界"], tot["落空行"], tot["对得上"], tot["无归属"]))
for verdict in ("越界", "落空行", "对得上", "无归属"):
    print("—— %s ——" % verdict)
    for s in samples[verdict]:
        print("   " + s)
