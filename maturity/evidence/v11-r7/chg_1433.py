"""把一颗裸引用的"有无主人"逐颗打印出来，配合 gate 的归属规则看它凭什么有主。"""
import sys
from pathlib import Path

sys.path.insert(0, "tools")
import check_architecture as G  # noqa: E402

REL = "CHANGELOG.md"
out = []
cur = (G.ROOT / REL).read_bytes().decode("utf-8", errors="replace").splitlines()
old = Path("scratch_r7/snap8/CHANGELOG.md").read_bytes().decode("utf-8", errors="replace").splitlines()
for tag, lines in (("before", old), ("after", cur)):
    for number in (1433, 1434):
        line = lines[number - 1]
        hint = set(G.re.findall(r"(?:crates/)?(qx-[a-z][a-z0-9\-]*)", line))
        spans = []
        owners = []
        for match in G.CAP_PATHED_CITATION.finditer(line):
            spans.append((match.start(1), match.end()))
            found = G._citation_candidates(match.group(1), hint)
            if len(found) == 1:
                owners.append((match.end(), found[0]))
        for spot in G.CAP_BARE_PATH.finditer(line):
            named = G._citation_candidates(spot.group(1), hint)
            if len(named) == 1:
                owners.append((spot.end(), named[0]))
        owners.sort()
        out.append(f"== {tag} L{number}: {line.strip()}")
        out.append(f"   owners={[o[1] for o in owners]}")
        for bare in G.CAP_BARE_CITATION.finditer(line):
            prior = [row for row in owners if row[0] < bare.start()]
            out.append(
                f"   bare :{bare.group(1)} @{bare.start()} -> "
                f"{'有主 ' + prior[-1][1] if prior else '无主'}"
            )
Path("scratch_r7/chg_1433.txt").write_bytes(("\n".join(out) + "\n").encode("utf-8"))
print("done")
