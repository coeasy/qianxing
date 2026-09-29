"""第 8 轮所有回写数字的唯一来源：从仓内日志与源码里现读现算，不做人工誊写。

`r8_measures()` 返回 token -> 字符串的映射；每一项都带一条自证断言（读不到、读出多解、
或枪日志自己的分档算术不符就 die）。落地脚本与填充脚本共用这一份，避免两处各数一遍。
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]
EV = ROOT / "maturity" / "evidence" / "v13-r8"


def die(msg: str) -> None:
    raise SystemExit(f"MEASURE DIE: {msg}")


def text(rel: str) -> str:
    p = EV / rel if not rel.startswith(("tools/", "docs/", "crates/", "python/", "maturity/")) else ROOT / rel
    if not p.exists():
        die(f"日志或源文件不在场：{p}")
    return p.read_bytes().decode("utf-8", "replace")


# ---- 每份枪日志：分档计数 + 该日志自己的合计行自证 ----


def tally_from_rows(body: str, killed_pat: str, ctrl_pat: str, surv_pat: str, uns_pat: str, label: str) -> dict:
    k = len(re.findall(killed_pat, body, re.M))
    c = len(re.findall(ctrl_pat, body, re.M))
    s = len(re.findall(surv_pat, body, re.M))
    u = len(re.findall(uns_pat, body, re.M))
    total = k + c + s + u
    if total == 0:
        die(f"{label}：一行判定都没读到")
    return {"killed": k, "ctrl": c, "surv": s, "unfired": u, "fired": total, "label": label}


def gun_tallies() -> dict:
    out = {}

    s = text("c1_nats_bounded_wait_guns.txt")
    out["C1"] = tally_from_rows(s, r"^\[KILLED\] C1-", r"^\[GREEN", r"^\[SURVIVED\]", r"^\[未放枪\]", "C1")
    m = re.search(r"合计 (\d+) 颗：KILLED (\d+) / SURVIVED (\d+) / 未放枪 (\d+)", s)
    if not m:
        die("C1 合计行读不出")
    out["C1"].update({"killed": int(m.group(2)), "surv": int(m.group(3)), "unfired": int(m.group(4)),
                      "fired": int(m.group(1))})
    _selfcheck(out["C1"], s, r"合计 (\d+) 颗：KILLED (\d+) / SURVIVED (\d+) / 未放枪 (\d+)")

    s = text("c2_postgres_keepalive_guns.txt")
    m = re.search(r"合计：KILLED (\d+) \[[^\]]*\] / GREEN（对照）(\d+) \[[^\]]*\] / SURVIVED (\d+) \[[^\]]*\] / 未放枪 (\d+) \[[^\]]*\]", s)
    if not m:
        die("C2 合计行读不出")
    out["C2"] = {"killed": int(m.group(1)), "ctrl": int(m.group(2)), "surv": int(m.group(3)),
                 "unfired": int(m.group(4)), "fired": int(m.group(1)) + int(m.group(2)) + int(m.group(3)) + int(m.group(4))}
    _rowcheck(out["C2"], len(re.findall(r"^== M\d ", s, re.M)), "C2")

    s = text("c4_parent_liveness_guns.txt")
    m = re.search(r"合计 (\d+) 发：KILLED (\d+) / GREEN（对照）(\d+) / SURVIVED 或红错了格 (\d+) / 未放枪 (\d+)", s)
    if not m:
        die("C4 合计行读不出")
    out["C4"] = {"killed": int(m.group(2)), "ctrl": int(m.group(3)), "surv": int(m.group(4)),
                 "unfired": int(m.group(5)), "fired": int(m.group(1))}
    if out["C4"]["fired"] != out["C4"]["killed"] + out["C4"]["ctrl"] + out["C4"]["surv"] + out["C4"]["unfired"]:
        die("C4 分档算术不符")

    s = text("c5_ws_dribble_guns.txt")
    m = re.search(r"合计 (\d+) 颗：KILLED (\d+) / SURVIVED (\d+) / 未放枪 (\d+)", s)
    out["C5"] = {"killed": int(m.group(2)), "ctrl": 0, "surv": int(m.group(3)), "unfired": int(m.group(4)),
                 "fired": int(m.group(1))}
    if out["C5"]["fired"] != out["C5"]["killed"] + out["C5"]["surv"] + out["C5"]["unfired"]:
        die("C5 分档算术不符")

    s = text("c7_acceptance_timeout_guns.txt")
    m = re.search(r"合计 (\d+) 颗：KILLED (\d+) / GREEN（对照）(\d+) / SURVIVED (\d+) / 未放枪 (\d+)", s)
    if not m:
        die("C7 合计行读不出")
    out["C7"] = {"killed": int(m.group(2)), "ctrl": int(m.group(3)), "surv": int(m.group(4)),
                 "unfired": int(m.group(5)), "fired": int(m.group(1))}
    if out["C7"]["fired"] != out["C7"]["killed"] + out["C7"]["ctrl"] + out["C7"]["surv"] + out["C7"]["unfired"]:
        die("C7 分档算术不符")
    rows = len(re.findall(r"^C7-[0-9C]+ (?:KILLED|GREEN|SURVIVED|未放枪)", s, re.M))
    if rows != out["C7"]["fired"]:
        die(f"C7 判定行 {rows} 颗与合计 {out['C7']['fired']} 发不符")
    if "C7_BATTERY_EXIT=0" not in s:
        die("C7 那份日志不是收口那份（没有 C7_BATTERY_EXIT=0）")
    if "c7_acceptance_timeout_guns_run1_mixed-w98-dump.txt" not in "".join(
        p.name for p in EV.iterdir()
    ):
        die("C7 那份混编日志的作废存档不在场，文档里「整跑日志另放一处」的指针不可达")

    s = text("c8_pump_guns.txt")
    m = re.search(r"合计 (\d+) 发：KILLED (\d+) / GREEN（对照）(\d+) / SURVIVED 或红错了格 (\d+) / 未放枪 (\d+)", s)
    if not m:
        die("C8 合计行读不出")
    out["C8"] = {"killed": int(m.group(2)), "ctrl": int(m.group(3)), "surv": int(m.group(4)),
                 "unfired": int(m.group(5)), "fired": int(m.group(1))}
    if out["C8"]["fired"] != out["C8"]["killed"] + out["C8"]["ctrl"] + out["C8"]["surv"] + out["C8"]["unfired"]:
        die("C8 分档算术不符")
    if "BATTERY_EXIT=0" not in s:
        die("C8 那份日志不是收口那份（没有 BATTERY_EXIT=0）")

    s = text("a3_ci_leg_guns.txt")
    m = re.search(r"合计 (\d+) 颗：KILLED (\d+) / SURVIVED (\d+) / 未放枪 (\d+)", s)
    out["A3"] = {"killed": int(m.group(2)), "ctrl": 0, "surv": int(m.group(3)), "unfired": int(m.group(4)),
                 "fired": int(m.group(1))}
    if out["A3"]["fired"] != out["A3"]["killed"] + out["A3"]["surv"] + out["A3"]["unfired"]:
        die("A3 分档算术不符")

    s = text("a4_parked_leg_guns.txt")
    m = re.search(r"合计： \{'KILLED': (\d+), 'SURVIVED': (\d+), '未放枪': (\d+)\}\s+共 (\d+) 颗", s)
    if not m:
        die("A4 合计行读不出")
    out["A4"] = {"killed": int(m.group(1)), "ctrl": 0, "surv": int(m.group(2)), "unfired": int(m.group(3)),
                 "fired": int(m.group(4))}
    if "A4_BATTERY_EXIT=0" not in s:
        die("A4 那份日志不是收口那份")

    s = text("a5_alert_render_guns.txt")
    m = re.search(r"\{'KILLED': (\d+), 'SURVIVED': (\d+), '未放枪': (\d+)\}\s+共 (\d+) 颗；A5-C 通过 = (\w+)", s)
    if not m:
        die("A5 合计行读不出")
    # A5-C 是批次之外单跑的那颗隔离对照：它不占 A5 的放枪数，单独作一颗自证记账。
    out["A5"] = {"killed": int(m.group(1)), "ctrl": 0, "surv": int(m.group(2)), "unfired": int(m.group(3)),
                 "fired": int(m.group(4))}
    if out["A5"]["fired"] != out["A5"]["killed"] + out["A5"]["surv"] + out["A5"]["unfired"]:
        die("A5 分档算术不符")
    if m.group(5) != "True":
        die("A5 的隔离对照没通过")
    out["A5"]["isolated"] = 1

    s = text("a6_report_readout_guns.txt")
    out["A6"] = tally_from_rows(s, r"^(?:P\d|A1) KILLED", r"^(?:P\d|A1) GREEN", r"^(?:P\d|A1) SURVIVED", r"^(?:P\d|A1) 未放枪", "A6")
    if "A6_BATTERY_EXIT=0" not in s:
        die("A6 那份日志不是收口那份（没有 A6_BATTERY_EXIT=0）")
    names = re.findall(r"^(P\d|A1)\s", s, re.M)
    if sorted(names) != sorted(set(names)) or len(names) != out["A6"]["fired"]:
        die(f"A6 的判定行名册与发数不符：{names}")
    if out["A6"]["fired"] != out["A6"]["killed"] + out["A6"]["surv"] + out["A6"]["unfired"]:
        die("A6 分档算术不符")

    s = text("citation_gate_guns.txt")
    out["CITE_GATE"] = tally_from_rows(s, r"^\s+[LB]\d: KILLED", r"^\s+[LB]\d: GREEN", r"^\s+[LB]\d: SURVIVED", r"^\s+[LB]\d: 未放枪", "引用门禁批次")

    s = text("citation_roster_guns.txt")
    m = re.search(r"合计 (\d+) 发：KILLED (\d+) / GREEN（对照）(\d+) / SURVIVED (\d+) / 未放枪 (\d+)", s)
    out["CITE_ROSTER"] = {"killed": int(m.group(2)), "ctrl": int(m.group(3)), "surv": int(m.group(4)),
                          "unfired": int(m.group(5)), "fired": int(m.group(1))}
    if out["CITE_ROSTER"]["fired"] != sum(out["CITE_ROSTER"][k] for k in ("killed", "ctrl", "surv", "unfired")):
        die("引用名册批次分档算术不符")

    return out


def _selfcheck(t: dict, body: str, pat: str) -> None:
    m = re.search(pat, body)
    if not m:
        die(f"{t['label']}：合计行读不出")
    if (int(m.group(2)), int(m.group(3)), int(m.group(4))) != (t["killed"], t["surv"], t["unfired"]):
        die(f"{t['label']}：行数与合计行不符")
    if int(m.group(1)) != t["fired"]:
        die(f"{t['label']}：发数与合计行不符")


def _rowcheck(t: dict, rows: int, label: str) -> None:
    if rows != t["fired"]:
        die(f"{label}：枪标题行 {rows} 颗与合计 {t['fired']} 发不符")


# ---- 门禁与六条腿 ----


def gate_reading(log: str, require_rows: bool = False) -> dict:
    """门禁读数：数 `[PASS]` 行、数 ✗ 行，并与门禁自己打印的总项数与地板逐颗对齐。

    两份形态都要能读：整跑那份结尾是 `架构不变量自检全部通过 ✓（N 项）`，六腿摘要那份是
    `GATE_EXIT=0 [PASS]=N [FAIL]=M`（摘要那份不含逐行输出，所以 `require_rows` 只对写进文档
    的那份打开——实数行必须等于自报总项数，谁对不齐就 die，不给"抄错一位"留余地）。
    """
    s = text(log)
    rows = len(re.findall(r"^\[PASS\]", s, re.M))
    fails = len(re.findall(r"^\s*✗", s, re.M))
    decl = re.search(r"架构不变量自检全部通过 ✓（(\d+) 项）", s)
    summ = re.search(r"GATE_EXIT=(\d+) \[PASS\]=(\d+) \[FAIL\]=(\d+)", s)
    if decl:
        declared, rc = int(decl.group(1)), 0
        if summ:
            die(f"{log}：同时出现整跑结尾与摘要读数，不知道读哪份")
    elif summ:
        declared, rc = int(summ.group(2)), int(summ.group(1))
        if int(summ.group(3)) != fails:
            die(f"{log}：摘要 ✗ 数 {summ.group(3)} 与实数 {fails} 不符")
    else:
        die(f"{log}：既没有整跑结尾也没有摘要读数")
    if require_rows and rows != declared:
        die(f"{log}：`[PASS]` 实数 {rows} 与门禁自报总项数 {declared} 不符")
    if require_rows and not rows:
        die(f"{log}：要逐行读的那份没有 `[PASS]` 行")
    floor = re.search(r"GATE_CHECK_FLOOR\s*=\s*(\d+)", text("tools/check_architecture.py"))
    if not floor:
        die("读不到 GATE_CHECK_FLOOR")
    floor_n = int(floor.group(1))
    if require_rows and f"至少执行 {floor_n} 条判据" not in s:
        die(f"{log}：门禁自报地板 {floor_n} 那条 `[PASS]` 不在场")
    return {
        "rows": rows,
        "declared": declared,
        "fails": fails,
        "rc": rc,
        "r8_rows": len([l for l in s.splitlines() if l.startswith("[PASS]") and "第 8 轮" in l]),
        "r8_by_label": r8_label_counts(s),
        "floor": floor_n,
    }


def r8_label_counts(body: str) -> dict:
    """门禁那些"第 8 轮"行按票面标签分颗——文档要写"C2 五格、C5 一格、C1 一格都没有"就靠这一颗。

    只数 `[PASS]` 行，红行不算（红行由 `fails` 单独界住）；带 `第 8 轮` 却没有可解析标签的行
    收进 `B`，这样"标签总数 == 第 8 轮行数"这颗算术不会因为一类写法而静默少算。
    """
    counts: dict[str, int] = {}
    tagged = 0
    for line in body.splitlines():
        if not line.startswith("[PASS]") or "第 8 轮" not in line:
            continue
        tagged += 1
        m = re.search(r"第 8 轮\s+([AC]\d|B)\b", line)
        key = m.group(1) if m else "B"
        counts[key] = counts.get(key, 0) + 1
    counts["_total"] = tagged
    return counts


def arm_line(name: str) -> int:
    s = text("tools/check_architecture.py")
    lines = s.splitlines()
    hits = [n for n, l in enumerate(lines, 1) if l.startswith(f"def {name}(")]
    if len(hits) != 1:
        die(f"判据 {name} 的 def 行命中 {len(hits)} 次")
    return hits[0]


def c4_rounds() -> int:
    """C4 的延迟测量真跑了几轮——文档那句"实测 N 轮"读这一颗。"""
    s = text("c4_worker_latency.txt")
    m = re.search(r"轮数：(\d+)（capacity=", s)
    if not m:
        die("C4 的轮数行读不出")
    arr = re.search(r"启动→答上话 \(s\): \[([^\]]*)\]", s)
    if not arr:
        die("C4 的启动样本数组读不出")
    if len(arr.group(1).split(",")) != int(m.group(1)):
        die(f"C4 自报轮数 {m.group(1)} 与启动样本颗数不符")
    return int(m.group(1))


def yaml_reading() -> dict:
    """台账那四颗被文档引用的读数：行数、`- ` 条目、以反引号开头的条目、解析失败的行。

    本轮要往这份台账里插行，那几颗数会跟着动，所以它们必须现读，不能沿用上一轮的存档读数。
    """
    try:
        import yaml
    except ImportError:
        die("解析失败的行号要靠 pyyaml 现场数；它是取证的临时探针，不是项目依赖")

    raw = (ROOT / "maturity" / "capabilities.yaml").read_bytes().decode("utf-8")
    lines = raw.splitlines()
    items = [l.strip() for l in lines if l.strip().startswith("- ")]
    backtick = [i for i in items if i[2:3] == "`"]
    try:
        yaml.safe_load(raw)
    except Exception as exc:
        fail_line = None
        for m in re.finditer(r"line (\d+), column \d+", str(exc)):
            fail_line = int(m.group(1))
        if fail_line is None:
            die(f"capabilities.yaml 的解析错误读不出行号：{type(exc).__name__}")
    else:
        die("capabilities.yaml 今天能解析了——文档那句『从来没被解析过』要改写")
    return {
        "YAML_LINES": str(len(lines)),
        "YAML_DASH": str(len(items)),
        "YAML_BACKTICK": str(len(backtick)),
        "YAML_FAIL_LINE": str(fail_line),
    }


def pump_case_faces() -> tuple[int, int, int]:
    host = text("crates/qx-cli/src/strategy_host.rs").splitlines()
    cases = text("crates/qx-cli/src/tests/strategy_pump_bounds.rs").splitlines()
    mount = text("crates/qx-cli/src/tests/mod.rs").splitlines()
    names = [m.group(1) for m in re.finditer(r"^#\[test\]\nfn (\w+)\(\)", "\n".join(cases), re.M)]
    n = len(names)
    if n != len(set(names)):
        die("泵用例名有重复")
    if n != len(re.findall(r"^#\[test\]$", "\n".join(cases), re.M)):
        die("泵用例的 #[test] 数与『带名字的常驻用例』数不符")
    if n < 5:
        die(f"泵用例只数到 {n} 颗")
    mline = [i for i, l in enumerate(mount, 1) if l.strip() == "mod strategy_pump_bounds;"]
    if len(mline) != 1:
        die(f"挂载行命中 {len(mline)} 次")
    cap = [i for i, l in enumerate(host, 1) if "mpsc::sync_channel(STRATEGY_PUMP_BACKLOG)" in l]
    if len(cap) != 1:
        die(f"有界通道写法命中 {len(cap)} 次")
    return n, mline[0], cap[0]


def latency_medians() -> tuple[str, str]:
    s = text("c4_worker_latency.txt")
    m = re.search(r"中位数  启动=([\d.]+)  退出=([\d.]+)", s)
    if not m:
        die("C4 延迟中位数读不出")
    return m.group(1), m.group(2)


def citation_floor_v13() -> tuple[int, int]:
    """V13 在引用名册里的地板：直接读 `DOC_CITATION_TARGETS` 那一行末尾的两颗数。"""
    s = text("tools/check_architecture.py")
    lines = s.splitlines()
    hits = [l for l in lines if re.search(r'"docs/[^"]*V13\.md",\s*"V13[^"]*",\s*\d+,\s*\d+', l)]
    if len(hits) != 1:
        die(f"DOC_CITATION_TARGETS 里 V13 那行命中 {len(hits)} 次（要恰好 1 次）")
    nums = re.findall(r"(\d+)", hits[0].split("#")[0].rstrip().rstrip(","))
    if len(nums) < 2:
        die(f"V13 那行读不出地板两颗数：{hits[0]!r}")
    return int(nums[-2]), int(nums[-1])


def citation_measured_v13() -> tuple[int, int]:
    """复测器实际量到的 V13 引用数——地板必须不超过它，否则门禁自己就会红。"""
    s = text("citation_roster_guns.txt")
    m = re.search(r"实测：V13 引用 (\d+) 颗 / 带名绑定 (\d+) 颗", s)
    if not m:
        die("citation_roster_guns.txt 里读不到 V13 的实测行")
    return int(m.group(1)), int(m.group(2))


def census_comma() -> tuple[int, int, int]:
    """引用盲点普查那三份数：斜杠尾巴被核对 N 颗 / 逗号尾巴被丢弃 N 颗 / 交给当轮判据会红 N 颗。"""
    s = text("citation_blind_spot_census.txt")
    m = re.search(r"修复前斜杠尾巴被核对 (\d+) 颗、逗号尾巴被丢弃 (\d+) 颗", s)
    if not m:
        die("普查段 2 的斜杠/逗号两颗数读不出")
    red = re.search(r"交给今天的判据，会红：(\d+) 颗", s)
    if not red:
        die("普查段 2 的『会红几颗』读不出")
    return int(m.group(1)), int(m.group(2)), int(red.group(1))


def gate_module():
    """把门禁自己装进来共用它的量尺——计数要么用它的前缀正则，要么用它那两个解析函数，不另起一把。"""
    import importlib.util

    if getattr(gate_module, "_cached", None) is not None:
        return gate_module._cached
    spec = importlib.util.spec_from_file_location("qx_gate", ROOT / "tools" / "check_architecture.py")
    if spec is None or spec.loader is None:
        die("装不上 tools/check_architecture.py 来共用它的量尺")
    gate = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(gate)
    gate_module._cached = gate  # type: ignore[attr-defined]
    return gate


def readme_cells() -> dict:
    """README 与台账那几格——直接用门禁自己的量尺（`ledger_list_items` 与前缀正则），不另起一把。"""
    gate = gate_module()
    gate_text = text("tools/check_architecture.py")
    lines = (ROOT / "maturity" / "capabilities.yaml").read_text(encoding="utf-8").splitlines()
    evidence = gate.ledger_list_items(lines, "evidence")
    inside = [i for i in evidence if gate.CAP_LEDGER_PATH_PREFIX.match(i)]
    outside = [i for i in evidence if not gate.CAP_LEDGER_PATH_PREFIX.match(i)]
    github = [i for i in evidence if i.startswith(".github/")]
    if len(evidence) != len(inside) + len(outside):
        die(f"台账 evidence 的内外两半加起来 {len(inside) + len(outside)} 与总数 {len(evidence)} 不符")
    if any(not gate.CAP_LEDGER_PATH_PREFIX.match(i) for i in github):
        die(".github/ 那颗前缀不在 CAP_LEDGER_PATH_PREFIX 里，文档『本轮补上了 .github』就是假的")
    blocks: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    for l in lines:
        head = re.match(r"^  ([a-z0-9_]+):", l)
        if head:
            current = {"name": head.group(1)}
            blocks.append(current)
            continue
        if current is None:
            continue
        flag = re.match(r"^    (implementation|code_tested|sandbox_tested|production_approved): (.*)$", l)
        if flag:
            current[flag.group(1)] = flag.group(2)
    if len(blocks) != len([l for l in lines if re.match(r"^  [a-z0-9_]+:", l)]):
        die("能力块的名册数与缩进两格的条目数不符，四档旗标读的是另一棵树")
    both = [b for b in blocks if b.get("implementation") == "true" and b.get("code_tested") == "true"]
    # 前缀之外的那几十条到底还有几条被逐 token 核对：照 `capabilities_check` 取 token 与豁免通配的那几行
    # 原样走一遍，不用另一把尺——"数多少条落在核对之外"与"核对时实际看什么"必须是同一颗判据的口径。
    token_ruler = re.compile(
        gate.CAP_LEDGER_PATH_PREFIX.pattern.removeprefix("^") + r"[^\s，。；、：（）()「」\"`]+"
    )

    def checked_tokens(item: str) -> list[str]:
        picked = []
        for token in token_ruler.findall(item):
            candidate = re.sub(r":\d+$", "", token).rstrip(".,:;")
            if "*" in candidate or "?" in candidate:
                continue
            picked.append(candidate)
        return picked

    checked_outside = [i for i in outside if checked_tokens(i)]
    if len(inside) + len(outside) != len(evidence):
        die(f"证据行分档算术不符：行首带前缀 {len(inside)} + 前缀之外 {len(outside)} != 总数 {len(evidence)}")
    if len(checked_outside) + (len(outside) - len(checked_outside)) != len(outside):
        die("前缀之外的行内核对分档不符")
    return {
        "README_ENTRIES": str(len(blocks)),
        "README_BLOCKS": str(len([l for l in lines if re.match(r"^    implementation:", l)])),
        "README_BOTH": str(len(both)),
        "README_SANDBOX_TRUE": str(sum(1 for b in blocks if b.get("sandbox_tested") == "true")),
        "README_PROD_TRUE": str(sum(1 for b in blocks if b.get("production_approved") == "true")),
        "README_EVIDENCE": str(len(evidence)),
        "README_EVIDENCE_PATHS": str(len(inside)),
        "LEDGER_OUTSIDE": str(len(outside)),
        "LEDGER_OUTSIDE_CHECKED": str(len(checked_outside)),
        "LEDGER_OUTSIDE_UNCHECKED": str(len(outside) - len(checked_outside)),
        "LEDGER_CHECKED_TOTAL": str(len(inside) + len(checked_outside)),
        "LEDGER_GITHUB": str(len(github)),
        "LEDGER_GITHUB_DISTINCT": str(len({i.split()[0] for i in github})),
        "PREFIX_LITERALS": str(
            sum(1 for l in gate_text.splitlines() if "crates|tools|deploy|maturity|docs|schemas|python" in l)
        ),
        "README_LIMITATIONS": str(len(gate.ledger_list_items(lines, "limitations"))),
        "README_FILE_LINES": str(len(lines)),
    }


def ledger_repinned_rows() -> dict:
    """第 8 轮 A1/A2 逐颗打开重读的那五行台账今天在第几行——行号是这段文案里唯一的自指读数。

    那一行的文案写的是「台账 418/479/486/487/656 五行」与「台账第 479 行」，而本轮往这份文件里
    插了七行，五颗自读数会各自漂 9~19 行。所以按行首内容点名（一颗都不许命中两次），现读现算。

    名册里有两行的行首内容本身也在本轮改过：A1/A2 的裁决就是把登记改成修复的正反面，于是
    `websocket_event_stream_exits_only_on_client_or_bus` 被 `live_connection_slots_are_a_constant_and_have_no_idle_deadline`
    替掉、`nats_blocking_waits_inherit_async_nats_timeouts` 被 `nats_blocking_waits_have_their_own_budgets` 替掉。
    这两颗 key 因此按**改口之后**的行首写——而旧名仍留在替掉它的那一行的正文里，只有行首的
    `      - ` 锚点能把两半分开。
    """
    keys = (
        "      - live_connection_slots_are_a_constant_and_have_no_idle_deadline",
        "      - 三处地址解析共用一颗入口 resolve_socket_address",
        "      - silent_stream_has_no_liveness_deadline",
        "      - websocket_message_budget_is_a_constant",
        "      - nats_blocking_waits_have_their_own_budgets",
    )
    lines = (ROOT / "maturity" / "capabilities.yaml").read_text(encoding="utf-8").splitlines()
    rows: list[int] = []
    for key in keys:
        hits = [n for n, l in enumerate(lines, 1) if l.startswith(key)]
        if len(hits) != 1:
            die(f"台账自指名册的 {key.strip()} 命中 {len(hits)} 次（要恰好 1 次）")
        rows.append(hits[0])
    if rows != sorted(rows):
        die(f"台账自指名册的五行不递增：{rows}——文案里那串斜杠并列是按这个顺序写的")
    return {"LEDGER_REPIN_ROWS": "/".join(str(r) for r in rows), "LEDGER_WS_ROW": str(rows[1])}


def yml_citation_face() -> tuple[int, int]:
    """`.yml` 那族行号引用今天被解析到几颗、解析失败几颗——逐字走 `ci_citation_coverage_check` 的循环。

    这一颗要的是数字而不是"判据绿不绿"：判据的地板是 `cited >= 10`，读数若只写"够 10 颗"就把
    实测塌成了布尔。所以这里用门禁自己的 `CAP_PATHED_CITATION` 与 `_citation_candidates`，名册与
    扫描面都取门禁的常量，不在这份脚本里维护第二份文件清单。
    """
    gate = gate_module()
    cited = 0
    unresolved: list[str] = []
    for rel in [gate.CAPABILITIES_FILE, *[entry[0] for entry in gate.DOC_CITATION_TARGETS]]:
        path = ROOT / rel
        if not path.is_file():
            continue
        for raw, number in gate.CAP_PATHED_CITATION.findall(path.read_text(encoding="utf-8")):
            if not raw.endswith(".yml"):
                continue
            if len(gate._citation_candidates(raw, set())) == 1:
                cited += 1
            else:
                unresolved.append(f"{rel} 的 {raw}:{number}")
    if cited < 10:
        die(f"`.yml` 引用只解析到 {cited} 颗，低于门禁地板 10——文档那句『实测 N 颗被解析』不能这么写")
    return cited, len(unresolved)


def ci_step_range(start_line: str, end_line: str, must_contain: str) -> str:
    """CI 作业文件里那块被文档点名的行号范围：起点唯一，终点取起点之后最近的一颗。

    `done` 在这份文件里有两颗（225 与 305），所以终点不能按全文件唯一去要——按起点之后最近
    那颗取，再用块内必须恰好印着的那句话把范围钉住，谁挪动了块就_die_而不是给一个越界的数。
    """
    lines = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8").splitlines()
    starts = [n for n, l in enumerate(lines, 1) if l.strip() == start_line]
    if len(starts) != 1:
        die(f"ci.yml 的起点 {start_line!r} 命中 {len(starts)} 次（要恰好 1 次）")
    start = starts[0]
    ends = [n for n in range(start, len(lines) + 1) if lines[n - 1].strip() == end_line]
    if not ends:
        die(f"ci.yml 第 {start} 行之后找不到收尾的 {end_line!r}")
    end = ends[0]
    block = "\n".join(lines[start - 1 : end])
    if block.count(must_contain) != 1:
        die(f"ci.yml 的 {start}-{end} 这块里 {must_contain!r} 出现 {block.count(must_contain)} 次（要恰好 1 次）")
    return f"{start}-{end}"


def cite_log_line() -> str:
    """台账那句"`CHANGELOG.md:NNNN` 的 ci.yml 引用"里的那颗数：按内容现读 CHANGELOG 的那一行。"""
    lines = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8").splitlines()
    hits = [n for n, l in enumerate(lines, 1) if ".github/workflows/ci.yml:216-225" in l]
    if len(hits) != 1:
        die(f"CHANGELOG 里那行 ci.yml for-loop 引用命中 {len(hits)} 次（要恰好 1 次）")
    return str(hits[0])


def pump_helper_lines() -> dict:
    """C8 那四颗面的行号：容量常量、单行上限、有界读法、排空迟到的答复——每一颗都要 def/调用点各一。"""
    host = (ROOT / "crates" / "qx-cli" / "src" / "strategy_host.rs").read_text(encoding="utf-8").splitlines()

    def one(pred, label):
        hits = [n for n, l in enumerate(host, 1) if pred(l)]
        if len(hits) != 1:
            die(f"strategy_host.rs 的 {label} 命中 {len(hits)} 次（要恰好 1 次）")
        return hits[0]

    return {
        "PUMP_CONST": str(one(lambda l: "pub(crate) const STRATEGY_PUMP_BACKLOG" in l, "容量常量")),
        "PUMP_CAP_FN": str(one(lambda l: "fn jsonl_line_cap_bytes(" in l, "单行上限")),
        "PUMP_READ_FN": str(one(lambda l: "fn read_jsonl_line_within" in l, "有界单行读")),
        "PUMP_DRAIN_FN": str(one(lambda l: "fn drain_stale_responses" in l, "排空迟到答复")),
        "PUMP_DRAIN_CALL": str(
            one(lambda l: "drain_stale_responses" in l and "fn drain_stale_responses" not in l, "排空的调用点")
        ),
    }


def legs(log: str) -> dict:
    s = text(log)

    def grab(pat: str, label: str, flags: int = re.S) -> str:
        hits = re.findall(pat, s, flags)
        if len(hits) != 1:
            die(f"{log}：{label} 命中 {len(hits)} 次（要恰好 1 次）")
        g = hits[0]
        return g[0] if isinstance(g, tuple) else g

    ws = re.search(r"LEG cargo_test_workspace.*?SEGMENTS=(\d+) non-ok=(\d+) passed=(\d+) failed=(\d+)", s, re.S)
    st = re.search(r"LEG feature_matrix_lib.*?SEGMENTS=(\d+) non-ok=(\d+) passed=(\d+) failed=(\d+)", s, re.S)
    if not ws or not st:
        die(f"{log}：cargo 两腿的 SEGMENTS 行读不出")
    if ws.group(2) != "0" or st.group(2) != "0":
        die(f"{log}：cargo 有非 ok 段")
    return {
        "WS_SEG": ws.group(1),
        "WS_PASS": ws.group(3),
        "WS_FAIL": ws.group(4),
        "STOR_SEG": st.group(1),
        "STOR_PASS": st.group(3),
        "STOR_FAIL": st.group(4),
        "PY_RAN": grab(r"Ran (\d+) tests", "Python 用例数"),
        "PY_SKIP": grab(r"OK \(skipped=(\d+)\)", "Python skip"),
        "FMT_DIFF": grab(r"FMT_EXIT=0 diff-lines=(\d+)", "fmt 差异"),
        "CLIPPY_DIAG": grab(r"CLIPPY_EXIT=0 non-empty-lines=\d+ diagnostic-lines=(\d+)", "clippy 诊断行"),
    }


def acceptance_faces() -> dict:
    """C7 那颗截止的每一面：预算常量、唯一那条 subprocess.run、timeout、超时捕获、失败退出、调用点颗数。

    台账那一格要按 `path:line` 指过去，所以每一颗都得现读；调用点颗数把「12 支腿」这种早期估算
    换成脚本里真正数到的 `run(...)` 落点数，`def run(` 自己不算一颗。
    """
    gate = gate_module()
    body = (ROOT / "tools" / "binance_testnet_acceptance.py").read_text(encoding="utf-8")
    lines = body.splitlines()
    if len(gate.acceptance_run_calls(body)) != 1:
        die("门禁的 `acceptance_run_calls` 数到的子进程调用不是 1 条，台账那句『唯一那条』要改口")

    def one(pred, label):
        hits = [n for n, l in enumerate(lines, 1) if pred(l)]
        if len(hits) != 1:
            die(f"验收脚本的 {label} 命中 {len(hits)} 次（要恰好 1 次）")
        return hits[0]

    calls = [n for n, l in enumerate(lines, 1) if re.search(r"(?<![\w.])run\(", l)]
    def_lines = [n for n, l in enumerate(lines, 1) if l.startswith("def run(")]
    if len(def_lines) != 1 or def_lines[0] not in calls:
        die(f"验收脚本的 `def run(` 命中 {def_lines}，与 run( 名册 {calls} 对不齐")
    budget = one(lambda l: l.startswith("LEG_BUDGET_SECONDS = "), "预算常量")
    value = int(lines[budget - 1].split("=")[1])
    if value <= 0:
        die(f"预算常量是 {value}，不是正数")
    catch = one(lambda l: "except subprocess.TimeoutExpired" in l, "超时捕获")
    exits = [n for n in range(catch, catch + 16) if lines[n - 1].strip() == "raise SystemExit("]
    if len(exits) != 1:
        die(f"超时捕获之后 15 行之内的 `raise SystemExit(` 命中 {len(exits)} 次（要恰好 1 次）")
    return {
        "ACC_LINES": str(len(lines)),
        "ACC_BUDGET": str(budget),
        "ACC_SECONDS": str(value),
        "ACC_DEF": str(def_lines[0]),
        "ACC_RUN": str(
            one(lambda l: "subprocess.run(" in l and not l.strip().startswith("#"), "唯一那条子进程调用")
        ),
        "ACC_TIMEOUT": str(one(lambda l: l.strip() == "timeout=LEG_BUDGET_SECONDS,", "timeout 那一格")),
        "ACC_CATCH": str(catch),
        "ACC_EXIT": str(exits[0]),
        "ACC_CALLS": str(len(calls) - 1),
    }


def c8_oracle_split() -> dict:
    """C8 那批逐发读「门禁红 / 用例红」两个 oracle 的组合，给台账那句「谁单独咬下这一发」用。

    每一发的块里必须同时有 GATE_EXIT 与 ROWS_EXIT 两颗读数，缺任何一颗就 die——不做「读到几颗算几颗」
    的宽松解释，否则把用例 oracle 从没跑起来的那一发读成「只有门禁红」。
    """
    s = text("c8_pump_guns.txt")
    blocks = re.split(r"^== ", s, flags=re.M)[1:]
    gate_only = case_only = both = compile_tier = ctrl = 0
    seen = []
    for block in blocks:
        head = block.splitlines()[0]
        tag = head.split()[0]
        if not re.match(r"^[GT]\d+$", tag):
            continue
        g = re.search(r"GATE_EXIT=(-?\d+)", block)
        r = re.search(r"ROWS_EXIT=(-?\d+)", block)
        if not g or not r:
            die(f"C8 的 {tag} 那一发缺 GATE_EXIT 或 ROWS_EXIT 读数")
        v = re.search(r"判定：(KILLED(?:（[^）]*）)?)", block)
        if not v:
            die(f"C8 的 {tag} 那一发没有判定行")
        if "KILLED" not in v.group(1):
            die(f"C8 的 {tag} 判定是 {v.group(1)}，与收口那份不符")
        seen.append(tag)
        gate_red, rows_red = int(g.group(1)) != 0, int(r.group(1)) != 0
        if "COMPILE_RED" in block:
            compile_tier += 1
        if gate_red and rows_red:
            both += 1
        elif gate_red:
            gate_only += 1
        elif rows_red:
            case_only += 1
        else:
            die(f"C8 的 {tag} 两个 oracle 都没红，却被判成 KILLED")
    ctrl = len(re.findall(r"^== C\d+ ", s, re.M))
    total = gate_only + case_only + both
    t = gun_tallies()["C8"]
    if total != t["killed"]:
        die(f"C8 逐发读到的 KILLED {total} 与该日志合计行的 {t['killed']} 不符")
    if ctrl != t["ctrl"]:
        die(f"C8 的对照块 {ctrl} 与合计行的 GREEN（对照）{t['ctrl']} 不符")
    return {
        "C8_GATE_ONLY": str(gate_only),
        "C8_CASE_ONLY": str(case_only),
        "C8_BOTH": str(both),
        "C8_COMPILE": str(compile_tier),
    }


def measure_all(final_legs: str, gate_log: str = "gate_final.txt") -> dict:
    g = gun_tallies()
    G = gate_reading(gate_log, require_rows=True)
    passes, fails, rc = G["rows"], G["fails"], G["rc"]
    n_cases, mount_line, chan_line = pump_case_faces()
    start, exit_ = latency_medians()
    rounds = c4_rounds()
    yml = yaml_reading()
    yml_cited, yml_unresolved = yml_citation_face()
    ci_loop = ci_step_range(
        "- name: Validate all non-production runtime examples", "done", "for config in deploy/qianxing.runtime*.json"
    )
    ci_cpp = ci_step_range("cpp-sdk:", "os: [ubuntu-latest, windows-latest, macos-latest]", "fail-fast: false")
    cite_log = cite_log_line()
    floor_c, floor_n = citation_floor_v13()
    meas_c, meas_n = citation_measured_v13()
    cen_slash, cen_comma, cen_red = census_comma()
    if floor_c > meas_c or floor_n > meas_n:
        die(f"V13 地板 {floor_c}/{floor_n} 高于实测 {meas_c}/{meas_n}，门禁自己就会红")
    L = legs(final_legs)
    fired = sum(t["fired"] for t in g.values())
    killed = sum(t["killed"] for t in g.values())
    ctrl = sum(t["ctrl"] for t in g.values())
    surv = sum(t["surv"] for t in g.values())
    unfired = sum(t["unfired"] for t in g.values())
    if killed + ctrl + surv + unfired != fired:
        die("本轮枪数总账算术不符")
    if surv or unfired:
        die(f"本轮枪账里还有 SURVIVED={surv} 或未放枪={unfired}，不能按收口回写")
    mid = gate_reading("r8_six_legs.txt")
    if passes < mid["declared"]:
        die(f"门禁项数从本轮中段的 {mid['declared']} 掉到 {passes}：有人在轮中途删了判据")
    new_arms = [
        "ci_citation_coverage_check",
        "postgres_parked_leg_has_teeth_check",
        "report_readout_provenance_check",
        "alert_names_render_inside_their_bodies_check",
        "strategy_ring_parent_liveness_check",
        "strategy_pump_bounds_check",
    ]
    arms = {name: arm_line(name) for name in new_arms}
    for name in new_arms:
        if not 12000 < arms[name] < 14000:
            die(f"判据 {name} 的行号 {arms[name]} 不在本轮新增区")
    if L["WS_FAIL"] != "0" or L["STOR_FAIL"] != "0" or fails != 0 or rc != 0:
        die("六条腿或门禁不是全绿那份，不能回写")
    gun_logs = sorted(p.name for p in EV.glob("*_guns.txt"))
    if len(gun_logs) != len(g):
        die(f"枪日志份数 {len(gun_logs)} 与读数到的 {len(g)} 份不符：{gun_logs}")
    if not (EV / "c8_pump_guns_run1_summary-binning-defect.txt").exists():
        die("那份作废存档不在场，文档里的存档指针不可达")
    tok = {
        "GATE_PASS": str(passes),
        "GATE_FAIL": str(fails),
        "GATE_RC": str(rc),
        "GATE_FLOOR": str(G["floor"]),
        "GATE_R8_ROWS": str(G["r8_rows"]),
        "GUNS_FIRED": str(fired),
        "GUNS_KILLED": str(killed),
        "GUNS_CTRL": str(ctrl),
        "GUNS_SURV": str(surv),
        "GUNS_UNFIRED": str(unfired),
        "GUN_LOGS": str(len(g)),
        "GUNS_ASSERT": f"{len(g)} 份枪日志逐份复算",
        "NEW_ARMS": str(len(new_arms)),
        "PUMP_TESTS": str(n_cases),
        "PUMP_MOUNT": str(mount_line),
        "PUMP_CHANNEL": str(chan_line),
        "C4_START": start,
        "C4_EXIT": exit_,
        "C4_ROUNDS": str(rounds),
        **yml,
        "V13_CITES": str(floor_c),
        "V13_NAMED": str(floor_n),
        "CENSUS_SLASH": str(cen_slash),
        "CENSUS_COMMA": str(cen_comma),
        "CENSUS_RED": str(cen_red),
        **readme_cells(),
        **ledger_repinned_rows(),
        "YML_CITED": str(yml_cited),
        "YML_UNRESOLVED": str(yml_unresolved),
        "CITE_LOG": cite_log,
        "CI_LOOP_RANGE": ci_loop,
        "CI_CPP_RANGE": ci_cpp,
        **pump_helper_lines(),
        **acceptance_faces(),
        **c8_oracle_split(),
        **L,
    }
    arm_names = {
        "ci_citation_coverage_check": "ARM1",
        "postgres_parked_leg_has_teeth_check": "ARM2",
        "report_readout_provenance_check": "ARM3",
        "alert_names_render_inside_their_bodies_check": "ARM4",
        "strategy_ring_parent_liveness_check": "ARM5",
        "strategy_pump_bounds_check": "ARM6",
    }
    for name, tag in arm_names.items():
        tok[tag] = str(arms[name])
    for k in ("C1", "C2", "C4", "C5", "C7", "C8", "A3", "A4", "A5", "A6", "CITE_GATE", "CITE_ROSTER"):
        tok[f"{k}_KILL"] = str(g[k]["killed"])
        tok[f"{k}_CTRL"] = str(g[k]["ctrl"])
        tok[f"{k}_SURV"] = str(g[k]["surv"])
        tok[f"{k}_FIRED"] = str(g[k]["fired"])
    tok["CITE_KILL"] = f"{g['CITE_GATE']['killed']} + {g['CITE_ROSTER']['killed']}"
    tok["A5_ISOLATED"] = str(g["A5"].get("isolated", 0))
    for tag, count in G["r8_by_label"].items():
        if tag != "_total":
            tok[f"G8_{tag}"] = str(count)
    for tag in ("C1", "A1", "A2", "C3"):
        tok.setdefault(f"G8_{tag}", "0")
    tok["G8_SUM"] = str(sum(v for k, v in G["r8_by_label"].items() if k != "_total"))
    if int(tok["GATE_R8_ROWS"]) < 20:
        die(f"门禁里带『第 8 轮』标签的行只有 {tok['GATE_R8_ROWS']} 条，不像收口那份")
    if tok["G8_SUM"] != tok["GATE_R8_ROWS"]:
        die(f"门禁第 8 轮行按标签分颗合计 {tok['G8_SUM']} 与总数 {tok['GATE_R8_ROWS']} 不符")
    extra = {
        "PUMP_MOUNT_LINE": str(mount_line),
        "PUMP_CHANNEL_LINE": str(chan_line),
    }
    extra.update({f"ARM_{k}": str(v) for k, v in arms.items()})
    return tok, extra, g


if __name__ == "__main__":
    tok, extra, g = measure_all(
        sys.argv[1] if len(sys.argv) > 1 else "r8_six_legs_final.txt",
        sys.argv[2] if len(sys.argv) > 2 else "gate_final.txt",
    )
    print("== 枪日志逐份（分档 / 发数）")
    for k, t in g.items():
        print(f"   {k:12s} KILLED {t['killed']:2d} / 对照 {t['ctrl']} / SURVIVED {t['surv']} / 未放枪 {t['unfired']} / 共 {t['fired']}")
    print("== token 表")
    for k in sorted(tok):
        print(f"   @{k}@ = {tok[k]}")
    print("== 行号辅助（只在源码里读，不进文档当计数）")
    for k in sorted(extra):
        print(f"   {k} = {extra[k]}")
