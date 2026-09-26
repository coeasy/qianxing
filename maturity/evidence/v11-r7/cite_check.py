"""复刻门禁的六格行号判据（tools/check_architecture.py 的 citation_audit）来预检 §55 草稿。"""
import re
from pathlib import Path

ROOT = Path(".")
CAP_CITATION_ROOTS = ("crates", "python", "tools", "schemas", "deploy")
CAP_CITATION_EXTS = (".rs", ".py", ".json", ".yaml", ".md", ".toml", ".sh", ".ps1")
CAP_PATHED_CITATION = re.compile(r"([A-Za-z0-9_./\-]+\.(?:rs|py|json|yaml|md|toml|sh|ps1)):(\d+)")
CAP_BARE_PATH = re.compile(r"([A-Za-z0-9_./\-]+\.(?:rs|py|json|yaml|md|toml|sh|ps1))(?!:)")
CAP_CONTINUATION = re.compile(r"(?:/\d+)+")
CAP_BARE_CITATION = re.compile(r"(?<![\w/.:\-]):(\d+)")
CAP_NAMES_BEFORE = re.compile(
    r"((?:`?[A-Za-z_][A-Za-z0-9_]{2,}`?/)*`?[A-Za-z_][A-Za-z0-9_]{2,}`?(?:\(\))?)\s*[（(]\s*`?\s*$"
)
CAP_RANGE_END = re.compile(r"-(\d+)(?!\d)")
HISTORY = "<!-- 史 -->"

_index = {}


def suffixes():
    if not _index:
        for root_name in CAP_CITATION_ROOTS:
            root = ROOT / root_name
            if not root.is_dir():
                continue
            for candidate in root.rglob("*"):
                if not candidate.is_file() or candidate.suffix not in CAP_CITATION_EXTS:
                    continue
                rel = candidate.relative_to(ROOT).as_posix()
                parts = rel.split("/")
                for start in range(len(parts)):
                    _index.setdefault("/".join(parts[start:]), []).append(rel)
    return _index


_cache = {}


def body_of(rel):
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
    found = suffixes().get(raw) or suffixes().get(parts[-1]) or []
    scoped = [c for c in found if any(c.startswith(f"crates/{h}/") for h in hint)]
    return scoped or sorted(set(found))


def audit(lines):
    out = {"missing": [], "out_of_range": [], "blank": [], "wrong_symbol": [], "misaligned": []}
    total = bound = pinned = orphan = 0
    for number, line in enumerate(lines, 1):
        if HISTORY in line:
            pinned += 1
            continue
        hint = set(re.findall(r"(?:crates/)?(qx-[a-z][a-z0-9\-]*)", line))
        owners = []
        current = None
        spans = []
        for match in CAP_PATHED_CITATION.finditer(line):
            raw = match.group(1)
            tail = CAP_CONTINUATION.match(line, match.end())
            refs = [int(match.group(2))] + (
                [int(part) for part in tail.group(0).split("/")[1:]] if tail else []
            )
            spans.append((match.start(2), match.end(2)))
            if tail:
                spans.append((tail.start(), tail.end()))
            head = line[: match.start(1)]
            listed = CAP_NAMES_BEFORE.search(head)
            names = (
                [re.sub(r"\(\)$", "", part).strip("`") for part in listed.group(1).split("/")]
                if listed
                else []
            )
            if names and len(names) != len(refs):
                out["misaligned"].append(
                    f"L{number}: {raw} {len(names)} 名字 / {len(refs)} 行号"
                )
                names = []
            found = candidates(raw, hint)
            if not found:
                out["missing"].append(f"L{number}: {raw}:{refs[0]}")
                continue
            unique = len(found) == 1
            body = body_of(found[0])
            if unique:
                current = found[0]
                owners.append((match.end(), found[0], False))
            for index, ref in enumerate(refs):
                total += 1
                fits = [o for o in found if 1 <= ref <= len(body_of(o))]
                if not fits:
                    out["out_of_range"].append(f"L{number}: {found[0]}:{ref} 越界({len(body)})")
                    continue
                if unique and not body[ref - 1].strip():
                    out["blank"].append(f"L{number}: {found[0]}:{ref} 空行")
                if unique and names and names[index] not in "".join(body[ref - 1 : ref + 2]):
                    out["wrong_symbol"].append(
                        f"L{number}: {found[0]}:{ref} 不是 {names[index]}"
                    )
            bound += len(refs) if names else 0
            rest = line[tail.end() if tail else match.end() :]
            far = CAP_RANGE_END.match(rest)
            if far and unique:
                total += 1
                edge = int(far.group(1))
                if not 1 <= edge <= len(body):
                    out["out_of_range"].append(f"L{number}: {found[0]} 区间远端 {edge} 越界")
                elif not body[edge - 1].strip():
                    out["blank"].append(f"L{number}: {found[0]} 区间远端 {edge} 空行")
        for spot in CAP_BARE_PATH.finditer(line):
            named = candidates(spot.group(1), hint)
            if len(named) == 1:
                owners.append((spot.end(), named[0], True))
        owners.sort()
        for bare in CAP_BARE_CITATION.finditer(line):
            if any(start <= bare.start(1) < end for start, end in spans):
                continue
            prior = [row for row in owners if row[0] < bare.start()]
            if not prior:
                orphan += 1
                out.setdefault("orphan", []).append(f"L{number}: {bare.group(0)}")
                continue
            total += 1
            owner = prior[-1][1]
            body = body_of(owner)
            ref = int(bare.group(1))
            if not 1 <= ref <= len(body):
                out["out_of_range"].append(f"L{number}: {owner}:{ref} 裸引用越界")
            elif not body[ref - 1].strip():
                out["blank"].append(f"L{number}: {owner}:{ref} 裸引用空行")
    return out, total, bound, pinned, orphan


if __name__ == "__main__":
    import sys

    lines = Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
    out, total, bound, pinned, orphan = audit(lines)
    rows = [
        f"扫描 {len(lines)} 行 引用 {total} 带名 {bound} 史钉 {pinned} 无主 {orphan}",
    ]
    for k in ("missing", "misaligned", "out_of_range", "blank", "wrong_symbol", "orphan"):
        v = out.get(k, [])
        rows.append(f"-- {k}: {len(v)}")
        rows.extend(f"     {x}" for x in v[:20])
    Path("scratch_r7/sec55_precheck.txt").write_text("\n".join(rows) + "\n", encoding="utf-8")
    print("rows written; reds =", sum(len(out.get(k, [])) for k in
          ("missing", "misaligned", "out_of_range", "blank", "wrong_symbol")))
