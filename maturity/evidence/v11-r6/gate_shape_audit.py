"""用门禁自己的判据形状审任意一份文档的每一处行号引用，并且能退回 R6-8 之前的旧形状做对照。

正则与根目录清单不是抄的，是从 tools/check_architecture.py 里用 AST 取出来的字面量，
所以本探针与常驻门禁 `doc_citation_check` / `capabilities_citation_check` 之间不会有
"两份判据各自漂移"的问题。加 --legacy 时把三处 R6-8 新牙齿逐颗退回原形：
  1) 名字捕获的右边界收紧回 `[（(]\\s*$`（文档里 `name`（`path:line`） 这种写法绑不上名字）；
  2) 不量破折号区间的远端（`path:AAA-BBB` 的 BBB 无人核对）；
  3) 裸 `:NNN` 归属给行内最后一颗唯一解析成功的文件，而不是它前面最近的那颗。
"""
import ast, os, re, sys
from pathlib import Path

ROOT = Path(os.path.abspath(__file__)).resolve().parents[3]
LEGACY = "--legacy" in sys.argv
WANTED = {
    "CAP_PATHED_CITATION", "CAP_CONTINUATION", "CAP_BARE_CITATION", "CAP_NAMES_BEFORE",
    "CAP_CITATION_ROOTS", "CAP_CITATION_EXTS", "CAP_RANGE_END", "CAP_BARE_PATH",
}
tree_src = (ROOT / "tools" / "check_architecture.py").read_text(encoding="utf-8")
tree = ast.parse(tree_src)
lits = {}
for node in ast.walk(tree):
    if isinstance(node, ast.Assign):
        for t in node.targets:
            if isinstance(t, ast.Name) and t.id in WANTED:
                lits[t.id] = ast.get_source_segment(tree_src, node.value)
missing = WANTED - set(lits)
assert not missing, "门禁里找不到这些常量：%s" % sorted(missing)
ns = {"re": re}
for k in ("CAP_PATHED_CITATION", "CAP_CONTINUATION", "CAP_BARE_CITATION", "CAP_NAMES_BEFORE",
          "CAP_RANGE_END", "CAP_BARE_PATH"):
    ns[k] = eval(lits[k], ns)
ns["CAP_CITATION_ROOTS"] = ast.literal_eval(lits["CAP_CITATION_ROOTS"])
ns["CAP_CITATION_EXTS"] = ast.literal_eval(lits["CAP_CITATION_EXTS"])
if LEGACY:
    # 逐颗退牙：从门禁当前形状反推 R6-8 之前的形状，锚点不在了就说明门禁又前进了，
    # 此时必须让探针报错而不是默默给出一个"看起来像旧形状"的对照。
    relaxed = ns["CAP_NAMES_BEFORE"].pattern
    tail = relaxed.rfind(r"[（(]\s*`?\s*$")
    assert tail >= 0, "门禁的名字判据尾部已变，旧形状反推失配：%r" % relaxed[-24:]
    ns["CAP_NAMES_BEFORE"] = re.compile(relaxed[:tail] + r"[（(]\s*$")
DOC = next((a for a in sys.argv[1:] if not a.startswith("--")), "docs/SECURITY.md")
HISTORY_MARK = "<!-- 史 -->"
ledger = ROOT / DOC
suffixes = {}
for root_name in ns["CAP_CITATION_ROOTS"]:
    base = ROOT / root_name
    if not base.is_dir():
        continue
    for cand in base.rglob("*"):
        if not cand.is_file() or cand.suffix not in ns["CAP_CITATION_EXTS"]:
            continue
        rel = cand.relative_to(ROOT).as_posix()
        parts = rel.split("/")
        for start in range(len(parts)):
            suffixes.setdefault("/".join(parts[start:]), []).append(rel)

_cache = {}


def lines_of(rel):
    if rel not in _cache:
        _cache[rel] = (ROOT / rel).read_text(encoding="utf-8", errors="replace").splitlines()
    return _cache[rel]


def candidates(raw, hint):
    if (ROOT / raw).is_file():
        return [raw]
    for probe in (f"crates/{raw}", f"python/{raw}", f"tools/{raw}"):
        if (ROOT / probe).is_file():
            return [probe]
    parts = raw.split("/")
    if len(parts) >= 2 and parts[-1].endswith(".rs") and not parts[-2].startswith("qx-"):
        probe = "crates/qx-%s/%s" % (parts[-2], "/".join(parts[-2:]))
        if (ROOT / probe).is_file():
            return [probe]
    found = suffixes.get(raw) or suffixes.get(parts[-1]) or []
    scoped = [c for c in found if any(c.startswith(f"crates/{h}/") for h in hint)]
    return scoped or sorted(set(found))


missing, out_of_range, blank, wrong_symbol, misaligned = [], [], [], [], []
# 原型：把"行内出现过、但没带行号的文件路径"也当成裸引用的候选归属。
# 正则同样从门禁里 AST 取，不手抄——手抄的那一份会跟门禁各自漂移。
BARE_OWNER = "--bare-path-owner" in sys.argv
BARE_PATH = ns["CAP_BARE_PATH"] if BARE_OWNER else None
total = bound = pinned = 0
for number, line in enumerate(ledger.read_text(encoding="utf-8").splitlines(), 1):
    # 「史」钉：这一格记的是当时观测到的取证事实（变异日志、修复前的形状），
    # 不是今天还该对得上的锚点。豁免的颗数必须打印出来，不能悄悄吞掉。
    if HISTORY_MARK in line:
        pinned += 1
        continue
    hint = set(re.findall(r"(?:crates/)?(qx-[a-z][a-z0-9\-]*)", line))
    current = None
    owners = []
    bare_owner = False
    spans = []
    path_spans = []
    for match in ns["CAP_PATHED_CITATION"].finditer(line):
        raw = match.group(1)
        path_spans.append((match.start(1), match.end(1)))
        tail = ns["CAP_CONTINUATION"].match(line, match.end())
        refs = [int(match.group(2))] + ([int(x) for x in tail.group(0).split("/")[1:]] if tail else [])
        spans.append((match.start(2), match.end(2)))
        if tail:
            spans.append((tail.start(), tail.end()))
        listed = ns["CAP_NAMES_BEFORE"].search(line[: match.start(1)])
        names = [re.sub(r"\(\)$", "", p).strip("`") for p in listed.group(1).split("/")] if listed else []
        if names and len(names) != len(refs):
            misaligned.append("%s 的 %d 个名字对上 %d 个行号（%s 第 %d 行）" % (raw, len(names), len(refs), DOC, number))
            names = []
        found = candidates(raw, hint)
        if not found:
            missing.append("%s:%d（第 %d 行）" % (raw, refs[0], number))
            continue
        unique = len(found) == 1
        body = lines_of(found[0])
        if unique:
            current = found[0]
            owners.append((match.end(), found[0], False))
        for index, ref in enumerate(refs):
            total += 1
            fits = [o for o in found if 1 <= ref <= len(lines_of(o))]
            if not fits:
                out_of_range.append("%s:%d 越出 %d 行（第 %d 行）" % (found[0], ref, len(body), number))
                continue
            if unique and not body[ref - 1].strip():
                blank.append("%s:%d（第 %d 行）" % (found[0], ref, number))
            if unique and names and names[index] not in "".join(body[ref - 1:ref + 2]):
                wrong_symbol.append("%s:%d 那一格不是 %s（第 %d 行）" % (found[0], ref, names[index], number))
        bound += len(refs) if names else 0
        if not LEGACY:
            # 破折号区间（`path:AAA-BBB`）的远端：只量越界与空行，不参与名字对齐。
            rest = line[tail.end() if tail is not None else match.end():]
            end = ns["CAP_RANGE_END"].match(rest)
            if end and unique:
                total += 1
                ref = int(end.group(1))
                if not 1 <= ref <= len(body):
                    out_of_range.append("%s 区间远端 :%d 越出 %d 行（第 %d 行）" % (found[0], ref, len(body), number))
                elif not body[ref - 1].strip():
                    blank.append("%s 区间远端 :%d 是空行（第 %d 行）" % (found[0], ref, number))
    if BARE_OWNER:
        # 原型：把"行内只点了名、没带行号"的文件路径也算候选归属——台账与 V11 里
        # `crates/qx-api/src/lib.rs 的 X（:590）` 这种写法现在没人核对 :590 归谁。
        for bp in BARE_PATH.finditer(line):
            if any(s <= bp.start(1) < e for s, e in path_spans):
                continue
            got = candidates(bp.group(1), hint)
            if len(got) == 1:
                owners.append((bp.end(), got[0], True))
        owners.sort()
    for bare in ns["CAP_BARE_CITATION"].finditer(line):
        if any(s <= bare.start(1) < e for s, e in spans):
            continue
        if LEGACY:
            owner, via = current, "行内最后一颗带行号引用"
        else:
            prior = [row for row in owners if row[0] < bare.start()]
            owner = prior[-1][1] if prior else None
            via = ("同行只点了文件名" if prior[-1][2] else "最近那颗带行号引用") if prior else ""
        if owner is None:
            continue
        total += 1
        body = lines_of(owner)
        ref = int(bare.group(1))
        if not 1 <= ref <= len(body):
            out_of_range.append("%s:%d 裸引用越界（%s，第 %d 行）" % (owner, ref, via, number))
        elif not body[ref - 1].strip():
            blank.append("%s:%d 裸引用落在空行（%s，第 %d 行）" % (owner, ref, via, number))

shape = ("旧（R6-8 之前）" if LEGACY else
         "新（R6-8 三口径 + R6-9 同行点名）" if BARE_OWNER else "新（R6-8 三口径）")
print("%s｜判据形状=%s｜行数=%d｜引用=%d｜带名=%d｜史钉豁免=%d" % (
    DOC, shape, len(ledger.read_text(encoding="utf-8").splitlines()), total, bound, pinned))
for label, rows in (("不在场", missing), ("错位", misaligned), ("越界", out_of_range), ("空行", blank), ("点名落空", wrong_symbol)):
    for r in rows:
        print("  %s %s" % (label, r))
bad = len(missing) + len(misaligned) + len(out_of_range) + len(blank) + len(wrong_symbol)
print("问题 %d 处" % bad)
sys.exit(1 if bad else 0)
