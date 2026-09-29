"""V13 第 8 轮的 token 填充与事后复算：文档里每一个数字都由 `r8_measures.py` 现读，人不誊。

用法：
    python -X utf8 maturity/evidence/v13-r8/fill_r8_tokens.py fill
    python -X utf8 maturity/evidence/v13-r8/fill_r8_tokens.py fill r8_six_legs_final.txt gate_final.txt
    python -X utf8 maturity/evidence/v13-r8/fill_r8_tokens.py verify r8_six_legs_final.txt gate_final.txt

`fill` 在**任何**写入之前先做完所有裁决（占位名册、行数、指针可达性、枪账），一颗不合格就一颗都不写；
`verify` 在终态字节上把整张表重算一遍，与 `fill` 写进去的那一份逐颗比对——中间只要有任何一份文件
挪过一行、任何一条腿的读数变过，这里就 die，而不是让文档带着上一份字节的数字收口。

`fill` 的那两份日志可以指名：默认读本轮中段那一份（`r8_six_legs.txt` + `gate_c8_after_repinned.txt`），
终态字节落地后再指一次终态那两份，把中途发布过、后来漂移了的数字重新现读一遍。第二次 `fill` 只会写
它眼前看得见占位的那几颗，所以名册是**合并**的：没被重新注入占位的那一颗，名册里继续记文档实际带着
的旧值——这样任何一颗漂了却没人把它改回占位，都会在 `fill` 当场死，而不是让文档带着旧数字收口。
"""

from __future__ import annotations

import importlib.util
import json
import pathlib
import re
import sys

EV = pathlib.Path(__file__).resolve().parent
ROOT = EV.parents[2]
SPEC = importlib.util.spec_from_file_location("r8_measures", EV / "r8_measures.py")
measures = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(measures)

TARGETS = (
    ROOT / "docs" / "自研量化框架审计与重构方案-V13.md",
    ROOT / "CHANGELOG.md",
    ROOT / "README.md",
    ROOT / "maturity" / "capabilities.yaml",
)
SIDE = EV / "filled_tokens.json"
TOKEN = re.compile(r"@([A-Z][A-Z0-9_]{2,})@")
# 文档里以反引号点出的取证文件名：这些名字必须真的在证据目录里（B5 那族"指过去却没有东西"的缺口）。
POINTERS = re.compile(r"`([A-Za-z0-9][\w.\-]*\.(?:txt|py))`")
# 填充之前那一次门禁跑（本轮的前向引用红）与终态那份都必须在场：少了前者，"红过"这件事就只有我在说。
BOOKED_RUNS = ("gate_forward-ref_red.txt", "gate_c8_after_repinned.txt", "r8_six_legs.txt")
# 填充之后才可能存在的两份：`fill` 那一步放过，`verify` 那一步必须在场。
PENDING_ON_FILL = ("gate_final.txt", "r8_six_legs_final.txt")


def die(msg: str) -> None:
    raise SystemExit("FILL DIE: " + msg.encode("ascii", "backslashreplace").decode("ascii"))


def load_sidecar() -> dict[str, str]:
    """读回上一次 `fill` 记下的名册；第一版 producer 用 `indent="0"` 写出过非法 JSON（每行顶格一颗 `0`）。

    修法是确定性的：只在「行首那颗 `0` 后面紧跟引号」这一种形状上剥掉那颗 `0`，剥完必须能 `json.loads`，
    并且键值全是字符串——否则就当它是别的问题直接死，不猜。修好的那份立刻按合法 JSON 落回原处。
    """
    if not SIDE.exists():
        return {}
    raw = SIDE.read_text(encoding="utf-8")
    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        lines = raw.splitlines()
        fixed = "\n".join(l[1:] if l.startswith('0"') else l for l in lines)
        try:
            data = json.loads(fixed)
        except json.JSONDecodeError as exc:
            die(f"{SIDE.name} 既不是合法 JSON，也不是可确定性修复的那颗顶格 0：{exc}")
        stripped = sum(1 for l in lines if l.startswith('0"'))
        if stripped == 0:
            die(f"{SIDE.name} 非法而名册里没有一颗顶格 0，不是已知缺陷，不猜")
        if not isinstance(data, dict) or not all(
            isinstance(k, str) and isinstance(v, str) for k, v in data.items()
        ):
            die(f"{SIDE.name} 修完形状不对：要 str->str 的名册")
        SIDE.write_text(
            json.dumps(data, ensure_ascii=False, indent=1, sort_keys=True) + "\n", encoding="utf-8", newline="\n"
        )
        print(f"   名册 {SIDE.name} 的旧缺陷已确定性修复：剥掉 {stripped} 行行首的 0，共 {len(data)} 颗")
    if not isinstance(data, dict):
        die(f"{SIDE.name} 不是名册（要 dict，读到 {type(data).__name__}）")
    return {k: str(v) for k, v in data.items()}


def write_sidecar(roster: dict[str, str]) -> None:
    SIDE.write_text(
        json.dumps({t: roster[t] for t in sorted(roster)}, ensure_ascii=False, indent=1, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    if json.loads(SIDE.read_text(encoding="utf-8")) != roster:
        die(f"{SIDE.name} 落盘后重读与原名册不符")


def read(path: pathlib.Path) -> str:
    raw = path.read_bytes()
    if b"\r\n" in raw:
        die(f"{path.name} 里有 CRLF：整份写回会把每一行都改一遍，先查是谁动的")
    return raw.decode("utf-8")


def pointers_reachable(pending: tuple[str, ...] = ()) -> list[str]:
    """§9.13 点到的每一份取证文件都要在仓里能打开——名字写在文档里而磁盘上没有，就是 B5 的复发。

    口径按"读者拿这个名字找不找得到东西"来定：先当作本轮取证目录里的文件，再当作仓库相对路径，
    最后才在全仓按文件名搜一颗（排除构建与暂存目录）。搜不到就是不可达。`pending` 只放过本轮
    还要靠这次填充才能生成的那几份（终态门禁与终态六腿），它们在 `verify` 那一步必须已经落下。
    """
    text = read(ROOT / "docs" / "自研量化框架审计与重构方案-V13.md")
    section = text[text.index("### 9.13"):]
    skip = re.compile(r"(?:^|/)(?:\.git|target|__pycache__|scratch_r\d+|snapshot_r\w+)(?:/|$)")
    missing = []
    for name in sorted(set(POINTERS.findall(section))):
        if name in pending:
            continue
        if (EV / name).exists() or (ROOT / name).exists():
            continue
        if any(not skip.search(str(p.relative_to(ROOT))) for p in ROOT.rglob(name)):
            continue
        missing.append(name)
    if missing:
        die(f"文档点名的取证文件不在场：{missing}")
    return sorted(set(POINTERS.findall(section)))


def fill(legs_log: str = "r8_six_legs.txt", gate_log: str = "gate_c8_after_repinned.txt") -> None:
    for rel in BOOKED_RUNS:
        if not (EV / rel).exists():
            die(f"记账用的那一份日志不在场：{rel}")
    for rel in (legs_log, gate_log):
        if not (EV / rel).exists():
            die(f"要按它回写的日志不在场：{rel}")
    old = load_sidecar()
    tok, _extra, guns = measures.measure_all(legs_log, gate_log)
    fired = sum(t["fired"] for t in guns.values())
    killed = sum(t["killed"] for t in guns.values())
    ctrl = sum(t["ctrl"] for t in guns.values())
    # 收口的算术是"每一发都有归属"，不是"每一发都咬"：对照那几发按口径必须绿，把它们算进咬合缺口
    # 就等于要求判据咬死自己的对照件。
    if fired != killed + ctrl or any(t["surv"] or t["unfired"] for t in guns.values()):
        die(
            f"枪账没收口（发 {fired} != 咬 {killed} + 对照 {ctrl}，"
            f"SURVIVED {sum(t['surv'] for t in guns.values())} / 未放枪 {sum(t['unfired'] for t in guns.values())}），"
            "不许按收口回写"
        )
    plans = []
    unknown: set[str] = set()
    used: set[str] = set()
    for path in TARGETS:
        body = read(path)
        before = body.splitlines()
        found = set(TOKEN.findall(body))
        if not found:
            continue
        unknown |= {t for t in found if t not in tok}
        used |= found
        new = TOKEN.sub(lambda m: tok[m.group(1)] if m.group(1) in tok else m.group(0), body)
        if len(new.splitlines()) != len(before):
            die(f"{path.name}：填充把行数从 {len(before)} 改成 {len(new.splitlines())}")
        if TOKEN.search(new):
            die(f"{path.name}：仍有未替换的占位 {sorted(set(TOKEN.findall(new)))}")
        plans.append((path, new.encode("utf-8"), len(found)))
    if unknown:
        die(f"文档里有读数脚本给不出的占位：{sorted(unknown)}")
    if not plans:
        die("四份文件里一颗占位都没有——这一轮已经填过了，或者占位被整段删掉了")
    # 名册里记的是「文档现在实际带着的那份值」。这一次没被重新注入占位的那一颗，只要现读值和文档带着
    # 的值不一样，就说明有个数字漂了却没人把它改回占位——在这里死，别指望事后复算。
    stale = {
        t: f"{old[t]} -> {tok.get(t, '<读数脚本不再给这颗>')}"
        for t in old
        if t not in used and old[t] != tok.get(t)
    }
    if stale:
        die(
            f"这次填充看不见这些占位，而它们的读数已经漂了：{json.dumps(stale, ensure_ascii=False, sort_keys=True)}"
            "——把每一颗在它自己的面上改回 @名字@ 再跑，别手誊"
        )
    for path, blob, hits in plans:
        path.write_bytes(blob)
    for path, blob, _ in plans:
        if path.read_bytes() != blob:
            die(f"{path.name}：写回后重读与原字节不符")
        if TOKEN.search(path.read_bytes().decode("utf-8")):
            die(f"{path.name}：重读仍见占位")
    roster = {**old, **{t: tok[t] for t in used}}
    write_sidecar(roster)
    print("== 填充完成")
    for path, _blob, hits in plans:
        print(f"   {path.name}: 替换 {hits} 种占位")
    print(f"   读数来源 {legs_log} + {gate_log}")
    print(f"   落盘名册 {SIDE.name}: 本次替换 {len(used)} 颗，合并后共 {len(roster)} 颗，逐颗来自 r8_measures")
    print(f"   门禁读数来源 {gate_log}：[PASS]={tok['GATE_PASS']} ✗={tok['GATE_FAIL']} rc={tok['GATE_RC']}")
    print(f"   枪账：{tok['GUN_LOGS']} 份 / 放枪 {tok['GUNS_FIRED']} / KILLED {tok['GUNS_KILLED']} / 对照 {tok['GUNS_CTRL']}")
    cited = pointers_reachable(PENDING_ON_FILL)
    held = [name for name in cited if (EV / name).exists() or (ROOT / name).exists()]
    skipped = [name for name in cited if name not in held]
    if skipped:
        print(f"   指针可达性：§9.13 点名 {len(cited)} 份，{len(held)} 份已在场，按待生成放过 {skipped}")
    else:
        print(f"   指针可达性：§9.13 点名的 {len(cited)} 份取证文件全部在场（终态那两份也已落下）")


def verify(final_legs: str, gate_log: str) -> None:
    written = load_sidecar()
    if not written:
        die("没有 fill 那一次写下的名册，事后复算无从比对")
    tok, _extra, guns = measures.measure_all(final_legs, gate_log)
    drift = {
        t: (v, tok.get(t, "<没有这颗读数>"))
        for t, v in written.items()
        if tok.get(t) != v
    }
    if drift:
        die(f"终态字节的读数与写进文档的那份不符：{json.dumps(drift, ensure_ascii=False)}")
    for path in TARGETS:
        left = sorted(set(TOKEN.findall(read(path))))
        if left:
            die(f"{path.name}：终态还留着占位 {left}")
    fired = sum(t["fired"] for t in guns.values())
    killed = sum(t["killed"] for t in guns.values())
    ctrl = sum(t["ctrl"] for t in guns.values())
    if fired != killed + ctrl:
        die(f"枪账没收口（发 {fired} != 咬 {killed} + 对照 {ctrl}）")
    print("== 事后复算通过")
    print(f"   {len(written)} 颗读数在 {final_legs} + {gate_log} 上与写进文档的那一份逐颗相等")
    print(f"   门禁 [PASS]={tok['GATE_PASS']} ✗={tok['GATE_FAIL']} rc={tok['GATE_RC']}，地板 {tok['GATE_FLOOR']}")
    print(
        f"   枪账 {tok['GUN_LOGS']} 份 / 放枪 {fired} / KILLED {killed} / 对照 {ctrl}"
        f" / SURVIVED {tok['GUNS_SURV']} / 未放枪 {tok['GUNS_UNFIRED']}"
    )
    cited = pointers_reachable()
    print(f"   指针可达性：§9.13 点名的 {len(cited)} 份取证文件全部在场")
    # 这句"都在册"要是只靠我说了算，它就和「审计已复核」那类空话一样：先按磁盘逐颗 open 过一遍再说。
    for rel in (*BOOKED_RUNS, final_legs, gate_log):
        if not (EV / rel).exists():
            die(f"复算说它在册，磁盘上却没有：{rel}")
    print(
        "   前向引用那一份红跑与终态全绿那一份都在册："
        + "、".join((*BOOKED_RUNS, final_legs, gate_log))
    )


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    if mode == "fill" and len(sys.argv) in (2, 4):
        if len(sys.argv) == 2:
            fill()
        else:
            fill(sys.argv[2], sys.argv[3])
    elif mode == "verify" and len(sys.argv) == 4:
        verify(sys.argv[2], sys.argv[3])
    else:
        die('用法：fill [final_legs.txt gate_log.txt] | verify <final_legs.txt> <gate_log.txt>')
