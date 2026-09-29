"""引用解析的两颗盲区各自量过一次价：收窄被劫持、逗号尾巴被丢（V13 第 8 轮 B4，任务 #216）。

放枪之前要知道"补这条判据会打掉多少合法引用"，否则修法只能靠感觉。这份取证器不改任何文件，
它把同一批引用在修复前后两种解析口径下各量一遍（只装载门禁模块取它的正则与候选索引，不整跑判据）。

  段 1 裸名收窄：同一行里另一颗锚点的全路径，会不会替这颗裸名决定落地文件
    修复前 hint = `(?:crates/)?(qx-[a-z][a-z0-9-]*)`（无前瞻断言）
    修复后 hint = 同一条加 `(?![\\w./-])`（只认整颗 crate 提及，门禁 :9999）
    C1 = 行内只有 1 颗提示、C2 = 行内 ≥2 颗提示：C2 才是"被同行锚点劫持"的那批。
  段 2 逗号并列：`path:38,281` 的 281 在修复前根本进不了判据
    修复前 CAP_CONTINUATION = `(?:/\\d+)+`，修复后 = `(?:[/,]\\d+)+`（门禁 :9909）
    这里按"修复前"的口径把被丢掉的逗号尾巴捞出来，逐颗用今天的判据问一遍歧义、越界与空行——
    会红的颗数就是当年那些数字里真正说假话的部分。

用法：python -X utf8 maturity/evidence/v13-r8/citation_blind_spot_census.py
判红不在这份脚本里：六格判据住在 tools/check_architecture.py 的 citation_audit，
牙齿的复射见同目录 citation_gate_guns.py。
"""
import contextlib
import hashlib
import importlib.util
import io
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]
HINT_OLD = re.compile(r"(?:crates/)?(qx-[a-z][a-z0-9\-]*)")
HINT_NEW = re.compile(r"(?:crates/)?(qx-[a-z][a-z0-9\-]*)(?![\w./\-])")
CONT_OLD = re.compile(r"(?:/\d+)+")
COMMA_TAIL = re.compile(r"(?:,\d+)+")

buf = io.StringIO()
spec = importlib.util.spec_from_file_location("arch_census", ROOT / "tools/check_architecture.py")
arch = importlib.util.module_from_spec(spec)
sys.modules["arch_census"] = arch
with contextlib.redirect_stdout(buf):
    spec.loader.exec_module(arch)
rows = buf.getvalue().splitlines()
print(f"模块装载期自检读数：[PASS] {len([l for l in rows if l.startswith('[PASS]')])} 条、"
      f"✗ {len([l for l in rows if l.lstrip().startswith('✗')])} 条"
      "（全量判据在 `__main__` 里跑，本脚本只装载不整跑；整跑的基线见同目录 citation_gate_guns.txt）")

TEXTS = list(arch.DOC_CITATION_TARGETS) + [(arch.CAPABILITIES_FILE, "台账", 0, 0)]
print("核对文本：" + "、".join(label for _, label, _, _ in TEXTS))


def lands_directly(raw: str) -> bool:
    """与 _citation_candidates 前三格同一条路：能直接落地的引用不参与"靠提示收窄"。"""
    if (ROOT / raw).is_file():
        return True
    if any((ROOT / f"{probe}/{raw}").is_file() for probe in ("crates", "python", "tools")):
        return True
    parts = raw.split("/")
    if len(parts) >= 2 and parts[-1].endswith(".rs") and not parts[-2].startswith("qx-"):
        return (ROOT / f"crates/qx-{parts[-2]}/{parts[-2]}/{'/'.join(parts[1:])}").is_file()
    return False


# ---- 段 1：裸名收窄 ---------------------------------------------------------
direct = unique = 0
narrow_old: list[tuple[str, int, str, str, int, int]] = []
still_new = 0
lost_new: list[tuple[str, int, str]] = []

for rel, label, _, _ in TEXTS:
    text = (ROOT / rel).read_text(encoding="utf-8", errors="replace")
    for number, line in enumerate(text.splitlines(), 1):
        if arch.CITATION_HISTORY_MARK in line:
            continue
        for match in arch.CAP_PATHED_CITATION.finditer(line):
            raw = match.group(1)
            if lands_directly(raw):
                direct += 1
                continue
            parts = raw.split("/")
            suffixes = arch._citation_suffixes()
            found = suffixes.get(raw) or suffixes.get(parts[-1]) or []
            if not found:
                continue
            if len(found) == 1:
                unique += 1
                continue
            old = sorted({c for c in found if any(c.startswith(f"crates/{h}/") for h in set(HINT_OLD.findall(line)))})
            new = sorted({c for c in found if any(c.startswith(f"crates/{h}/") for h in set(HINT_NEW.findall(line)))})
            if len(old) == 1:
                narrow_old.append((label, number, raw, old[0], len(set(HINT_OLD.findall(line))), len(new)))
                if len(new) == 1:
                    still_new += 1
                else:
                    lost_new.append((label, number, raw))

c1 = [r for r in narrow_old if r[4] <= 1]
c2 = [r for r in narrow_old if r[4] >= 2]
print(f"\n== 段 1 裸名收窄：直接落地 {direct} 颗、同名只有一份 {unique} 颗、靠同行提示收窄活着 {len(narrow_old)} 颗")
print(f"   其中 C1（行内 1 颗提示）{len(c1)} 颗 / C2（行内 ≥2 颗提示，即被别颗锚点劫持的那批）{len(c2)} 颗")
print(f"   改成只认整颗提及后：仍收窄到一份 {still_new} 颗 / 退回多候选（今天会报歧义红）{len(lost_new)} 颗")
for label, number, raw, chosen, hints, _new_count in narrow_old:
    print(f"   · {label} 第 {number} 行 `{raw}`（提示 {hints} 颗）→ {chosen}")
for label, number, raw in lost_new:
    print(f"   ✗ 退回多候选：{label} 第 {number} 行 `{raw}`")

# ---- 段 2：逗号并列 ---------------------------------------------------------
slash_refs = comma_refs = 0
per_label: dict[str, int] = {}
would_red: list[str] = []

for rel, label, _, _ in TEXTS:
    text = (ROOT / rel).read_text(encoding="utf-8", errors="replace")
    for number, line in enumerate(text.splitlines(), 1):
        if arch.CITATION_HISTORY_MARK in line:
            continue
        hint = set(HINT_NEW.findall(line))
        for match in arch.CAP_PATHED_CITATION.finditer(line):
            raw = match.group(1)
            tail_old = CONT_OLD.match(line, match.end())
            slash_refs += len(tail_old.group(0).split("/")) - 1 if tail_old else 0
            after = COMMA_TAIL.match(line, tail_old.end() if tail_old else match.end())
            if not after:
                continue
            nums = [int(x) for x in after.group(0).split(",")[1:]]
            if not nums:
                continue
            comma_refs += len(nums)
            per_label[label] = per_label.get(label, 0) + len(nums)
            found = arch._citation_candidates(raw, hint)
            if not found:
                would_red.append(f"{label}第 {number} 行 `{raw}` 不在场，逗号尾巴 {nums} 无法核对")
                continue
            body = arch._citation_lines_of(found[0])
            for ref in nums:
                if len(found) > 1:
                    would_red.append(f"{label}第 {number} 行 `{raw}`:{ref} 有 {len(found)} 份同名候选")
                elif not 1 <= ref <= len(body):
                    would_red.append(f"{label}第 {number} 行 {found[0]}:{ref} 越出 {len(body)} 行")
                elif not body[ref - 1].strip():
                    would_red.append(f"{label}第 {number} 行 {found[0]}:{ref} 是空行")

sample = "x.rs:1/2,3/4"
start = arch.CAP_PATHED_CITATION.search(sample).end()
old_seg = CONT_OLD.match(sample, start)
new_seg = arch.CAP_CONTINUATION.match(sample, start)
print(f"\n== 段 2 逗号并列：修复前斜杠尾巴被核对 {slash_refs} 颗、逗号尾巴被丢弃 {comma_refs} 颗")
print("   按文本分布：" + "、".join(f"{k} {v} 颗" for k, v in sorted(per_label.items(), key=lambda kv: -kv[1])))
print(f"   样例 `{sample}`：修复前收 {len(old_seg.group(0).split('/')) - 1} 颗（{old_seg.group(0)}）、"
      f"修复后收 {len(re.split(r'[/,]', new_seg.group(0))) - 1} 颗（{new_seg.group(0)}）")
print(f"   把这 {comma_refs} 颗交给今天的判据，会红：{len(would_red)} 颗")
for o in would_red:
    print(f"   ✗ {o}")

print("\n== 被核对文本的 sha256 前 16 位（复跑数字变了先看到这里动过）")
for rel, label, _, _ in TEXTS:
    print(f"   {label} {hashlib.sha256((ROOT / rel).read_bytes()).hexdigest()[:16]} {rel}")
