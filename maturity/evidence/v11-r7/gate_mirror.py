"""V11 R7-10c：复刻门禁 citation_audit 的六格判据，但把清单打全（门禁只印 [:6]）。
用同一批正则与同一颗解析器，避免"我以为扫绿了其实是被截断了"。"""
import sys

sys.path.insert(0, "tools")
import check_architecture as G  # noqa: E402

TARGETS = [(G.CAPABILITIES_FILE, "台账", G.CAP_CITATION_FLOOR, G.CAP_BOUND_NAME_FLOOR)]
TARGETS += list(G.DOC_CITATION_TARGETS)

report = []
for rel, label, floor_refs, floor_bound in TARGETS:
    lines = (G.ROOT / rel).read_text(encoding="utf-8", errors="replace").splitlines()
    missing, out_of_range, blank, wrong_symbol, misaligned = [], [], [], [], []
    total = bound = pinned = 0
    for number, line in enumerate(lines, 1):
        if G.CITATION_HISTORY_MARK in line:
            pinned += 1
            continue
        hint = set(G.re.findall(r"(?:crates/)?(qx-[a-z][a-z0-9\-]*)", line))
        owners = []
        current = None
        spans = []
        for match in G.CAP_PATHED_CITATION.finditer(line):
            raw = match.group(1)
            tail = G.CAP_CONTINUATION.match(line, match.end())
            refs = [int(match.group(2))] + (
                [int(part) for part in tail.group(0).split("/")[1:]] if tail else []
            )
            spans.append((match.start(2), match.end(2)))
            if tail:
                spans.append((tail.start(), tail.end()))
            head = line[: match.start(1)]
            listed = G.CAP_NAMES_BEFORE.search(head)
            names = (
                [G.re.sub(r"\(\)$", "", part).strip("`") for part in listed.group(1).split("/")]
                if listed
                else []
            )
            if names and len(names) != len(refs):
                misaligned.append(
                    f"L{number} {raw} 的 {len(names)} 个名字对上 {len(refs)} 个行号"
                )
                names = []
            found = G._citation_candidates(raw, hint)
            if not found:
                missing.append(f"L{number} {raw}:{refs[0]}")
                continue
            unique = len(found) == 1
            body = G._citation_lines_of(found[0])
            if unique:
                current = found[0]
                owners.append((match.end(), found[0], False))
            for index, ref in enumerate(refs):
                total += 1
                fits = [
                    other for other in found if 1 <= ref <= len(G._citation_lines_of(other))
                ]
                if not fits:
                    out_of_range.append(
                        f"L{number} {found[0]}:{refs[0]}…{ref} 越出 {len(body)} 行"
                        if unique
                        else f"L{number} {raw}:{ref} 在 {len(found)} 份同名文件里都越界"
                    )
                    continue
                if unique and not body[ref - 1].strip():
                    blank.append(f"L{number} {found[0]}:{ref}")
                if unique and names and names[index] not in "".join(body[ref - 1 : ref + 2]):
                    wrong_symbol.append(
                        f"L{number} {found[0]}:{ref} 那一格不是 {names[index]}"
                    )
            bound += len(refs) if names else 0
            rest = line[tail.end() if tail else match.end() :]
            far = G.CAP_RANGE_END.match(rest)
            if far and unique:
                total += 1
                edge = int(far.group(1))
                if not 1 <= edge <= len(body):
                    out_of_range.append(
                        f"L{number} {found[0]} 区间远端 :{edge} 越出 {len(body)} 行"
                    )
                elif not body[edge - 1].strip():
                    blank.append(f"L{number} {found[0]} 区间远端 :{edge} 是空行")
        for spot in G.CAP_BARE_PATH.finditer(line):
            named = G._citation_candidates(spot.group(1), hint)
            if len(named) == 1:
                owners.append((spot.end(), named[0], True))
        owners.sort()
        for bare in G.CAP_BARE_CITATION.finditer(line):
            if any(start <= bare.start(1) < end for start, end in spans):
                continue
            prior = [row for row in owners if row[0] < bare.start()]
            owner = prior[-1][1] if prior else None
            if owner is None:
                continue
            total += 1
            body = G._citation_lines_of(owner)
            ref = int(bare.group(1))
            if not 1 <= ref <= len(body):
                out_of_range.append(f"L{number} {owner}:{ref} 裸引用越界")
            elif not body[ref - 1].strip():
                blank.append(f"L{number} {owner}:{ref} 裸引用落在空行")
    report.append(
        f"######## {label} {rel}: 引用 {total}（地板 {floor_refs}）"
        f"带名绑定 {bound}（地板 {floor_bound}）史钉 {pinned} 行"
    )
    for title, rows in (
        ("MISSING", missing),
        ("MISALIGNED", misaligned),
        ("OUT-OF-RANGE", out_of_range),
        ("BLANK", blank),
        ("WRONG-SYMBOL", wrong_symbol),
    ):
        report.append(f"  -- {title}: {len(rows)}")
        report.extend("     " + r for r in rows)
open("scratch_r7/gate_mirror.txt", "wb").write(
    ("\n".join(report) + "\n").encode("utf-8")
)
print("sections", len(TARGETS), "lines", len(report))
