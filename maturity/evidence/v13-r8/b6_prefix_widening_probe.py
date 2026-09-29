"""B6 · 「把 `.github` 加进被核对前缀集」这一格 widening 的两发探针：它到底界住了什么。

这不是一支枪，是一发**判据自证**：本轮改的是量具本身（`CAP_LEDGER_PATH_PREFIX` 多了一颗前缀），
所以它不进 `*_guns.txt` 那本枪账，单独存成 `b6_prefix_widening_probe.txt`。两发的分工是：

- **P1（有 widening）**：往一颗以 `.github/` 开头的证据行末尾追加一颗**不存在**的 `.github/` 路径 token，
  门禁那颗逐 token 的存在性核对必须点名它——这才证明"以 `.github` 开头的行从此真被看着"。
- **P2（把 widening 还原）**：同一份注入，另外把 `\\.github|` 从常量里摘掉。存在性核对**不许**再点到那颗
  注入路径（红来自 widening 这一格，不来自同场那两颗前向引用红）；而 `cap_ledger_reading_check` 报的
  「以仓库内路径开头的证据行」磁盘数必须正好少掉"以 `.github/` 开头的证据行"那么多颗——同一组前缀被
  两颗判据共用，改一处必然同时动两处，这正是 README 那一格与台账
  `ledger_evidence_paths_have_partial_prefix_coverage` 那一行所声明的口径。

本轮基线**不是全绿那份**：填充之前文档里还留着 `path:@TOKEN@` 形状，所以存在性核对与 README 六格读数
这两颗在基线就是红的（见 `gate_forward-ref_red.txt`）。因此这里判的不是"红不红"而是"红里点了谁的名字"
与"磁盘数动没动"——把判定列当读数会被基线红骗过去，那正是 #213 那族形状。
"""

from __future__ import annotations

import hashlib
import importlib.util
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]
EV = pathlib.Path(__file__).resolve().parent
CAP = ROOT / "maturity" / "capabilities.yaml"
GATE = ROOT / "tools" / "check_architecture.py"
OUT = EV / "b6_prefix_widening_probe.txt"

ANCHOR_LINE = "      - .github/workflows/ci.yml service-backends job (postgres:16 + nats -js service containers)"
INJECT = " 另注 `.github/workflows/ci-absent-probe.yml`"
PROBE_PATH = ".github/workflows/ci-absent-probe.yml"
CONST_WITH = r'CAP_LEDGER_PATH_PREFIX = re.compile(r"^(?:crates|tools|deploy|maturity|docs|schemas|python|\.github)/")'
CONST_WITHOUT = r'CAP_LEDGER_PATH_PREFIX = re.compile(r"^(?:crates|tools|deploy|maturity|docs|schemas|python)/")'
# 判定列按前缀认，不按整句抄：门禁那句文案里带着轮次后缀，抄全文的探针会在别人改文案那天静默失效。
EXIST_KEY = "能力矩阵证据路径全部存在"
README_KEY = "README 的台账六格读数与磁盘同数"
PATHS_FIELD = re.compile(r"以仓库内路径开头的证据行：文档 \[.*?\]，磁盘 (\d+)")

LOG: list[str] = []


def emit(line: str = "") -> None:
    LOG.append(line)
    OUT.write_bytes(("\r\n".join(LOG) + "\r\n").encode("utf-8"))


def die(msg: str) -> None:
    emit("ABORT " + msg.encode("ascii", "backslashreplace").decode("ascii"))
    emit("PROBE_EXIT=2")
    raise SystemExit(2)


def sha(p: pathlib.Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()[:12]


def lines_of(p: pathlib.Path) -> list[str]:
    raw = p.read_bytes()
    if b"\r\n" in raw:
        die(f"{p.name} 里有 CRLF，探针不许在这种字节上写回")
    return raw.decode("utf-8").splitlines()


def run_gate() -> dict[str, str]:
    """整跑一次门禁，把红行收成 {前缀键: 详情}；读不出红却 rc!=0（或反之）都算判分失灵。"""
    proc = subprocess.run(
        [sys.executable, "-X", "utf8", "tools/check_architecture.py"],
        cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace",
    )
    out = (proc.stdout or "") + (proc.stderr or "")
    red: dict[str, str] = {}
    for line in out.splitlines():
        if not line.startswith("[FAIL] "):
            continue
        name, _, detail = line[len("[FAIL] "):].partition(" — ")
        key = EXIST_KEY if name.startswith(EXIST_KEY) else README_KEY if name.startswith(README_KEY) else name
        if key in red:
            die(f"同一颗判据印出两行红，读数没法归到一格：{name}")
        red[key] = detail
    rows = len(re.findall(r"^\[PASS\]", out, re.M))
    if (proc.returncode == 0) != (not red):
        die(f"门禁 rc={proc.returncode} 与红行颗数 {len(red)} 不自洽")
    if not rows:
        die("门禁一行 [PASS] 都没读出来，这一份不能用")
    return red


def github_evidence_rows() -> int:
    """以被核对前缀开头的证据行有几颗——尺子用门禁自己那两颗（`ledger_list_items` + 同一颗常量）。"""
    spec = importlib.util.spec_from_file_location("gate_b6", GATE)
    gate = importlib.util.module_from_spec(spec)
    assert spec and spec.loader
    spec.loader.exec_module(gate)
    items = gate.ledger_list_items(lines_of(CAP), "evidence")
    return len([i for i in items if i.startswith(".github/")])


def main() -> int:
    cap_lines_before = lines_of(CAP)
    gate_lines_before = lines_of(GATE)
    if sum(1 for l in cap_lines_before if l == ANCHOR_LINE) != 1:
        die("注入锚点那颗证据行不是恰好一行，注入会落到别处")
    if sum(1 for l in gate_lines_before if l == CONST_WITH) != 1:
        die("前缀常量的定义行不是恰好一行，还原会改不到地方")
    cap_base = CAP.read_bytes()
    gate_base = GATE.read_bytes()
    if cap_base.replace(ANCHOR_LINE.encode("utf-8"), (ANCHOR_LINE + INJECT).encode("utf-8")) == cap_base:
        die("注入是空操作：锚点字节没匹配上，两发都会假装通过")
    emit("== stage-0 锚点普查")
    emit(f"   注入锚点 ×1（台账第 {cap_lines_before.index(ANCHOR_LINE) + 1} 行）、前缀常量定义 ×1")
    emit(f"   基线 sha：{CAP.name}={sha(CAP)} {GATE.name}={sha(GATE)}")
    injected_cap = cap_base.replace(ANCHOR_LINE.encode("utf-8"), (ANCHOR_LINE + INJECT).encode("utf-8"))

    def shot(tag: str, cap: bytes, gate: bytes) -> dict[str, str]:
        CAP.write_bytes(cap)
        GATE.write_bytes(gate)
        red = run_gate()
        emit(f"== {tag}")
        emit(f"   注入在场 = {cap != cap_base} / 前缀常量已还原 = {gate != gate_base}")
        emit(f"   红 {len(red)} 颗：{sorted(red)}")
        exist = red.get(EXIST_KEY, "")
        emit(f"   存在性核对点到注入路径 = {PROBE_PATH in exist}")
        named = sorted({p for p in re.findall(r"\.github/[\w./-]*", exist)})
        emit(f"   那一行红里带出的 .github token = {named or '无'}")
        paths = PATHS_FIELD.findall(red.get(README_KEY, ""))
        emit(f"   README 那颗报的『以仓库内路径开头』磁盘数 = {paths or '那颗没红'}")
        CAP.write_bytes(cap_base)
        GATE.write_bytes(gate_base)
        if CAP.read_bytes() != cap_base or GATE.read_bytes() != gate_base:
            die(f"{tag}：还原后字节与基线不符")
        return {"hit": str(PROBE_PATH in exist), "paths": (paths or ["<无>"])[0], "keys": sorted(red)}

    try:
        base = shot("基线（未注入、常量原样）", cap_base, gate_base)
        if set(base["keys"]) != {EXIST_KEY, README_KEY}:
            die(f"基线红的名册不是那两颗前向引用：{base['keys']}")
        paths_with = int(base["paths"])
        rows = github_evidence_rows()
        p1 = shot("P1 只注入：`.github` 仍在被核对前缀里", injected_cap, gate_base)
        if p1["hit"] != "True":
            die("P1 没点名注入的那颗路径：widening 界不住任何东西，README 与台账那一格就成了假话")
        p2 = shot(
            "P2 同一份注入 + 还原前缀常量：`.github` 不在被核对前缀里",
            injected_cap,
            gate_base.replace(CONST_WITH.encode("utf-8"), CONST_WITHOUT.encode("utf-8")),
        )
        if p2["hit"] != "False":
            die("P2 仍点到注入路径：红不是来自那颗前缀常量，这颗对照没有把变量分开")
        paths_without = int(p2["paths"])
        if paths_with - paths_without != rows:
            die(f"共用量规对照不符：{paths_with} - {paths_without} != 以 .github/ 开头的证据行 {rows} 颗")
        if PROBE_PATH in p2["paths"]:
            die("P2 的 README 读数里还带着注入路径")
        after = shot("收枪复跑（两份字节都回到基线）", cap_base, gate_base)
        if after["keys"] != base["keys"] or after["paths"] != base["paths"]:
            die("收枪复跑的红与基线不等：还原没有把判据放回原位")
        if len(lines_of(CAP)) != len(cap_lines_before) or len(lines_of(GATE)) != len(gate_lines_before):
            die("行数变了")
        emit("== 合计")
        emit("   2 发：P1 咬（widening 界得住那颗不存在的路径）/ P2 隔离对照（红来自那一颗常量，两颗判据共用同一把尺）")
        emit(f"   共用量规实测：带 `.github` 时 {paths_with} 行、摘掉后 {paths_without} 行，差 {rows} 颗"
             f"（= 台账里以 `.github/` 开头的证据行颗数）")
        emit("   基线不是全绿那份：填充前那两颗前向引用红必然在场，见 gate_forward-ref_red.txt")
        emit(f"   收尾 sha 复核：{CAP.name}={sha(CAP)} {GATE.name}={sha(GATE)}（与基线同行同值）")
        emit("PROBE_EXIT=0")
    finally:
        CAP.write_bytes(cap_base)
        GATE.write_bytes(gate_base)
    if "PROBE_EXIT=0" not in "\n".join(LOG):
        return 2
    print("PROBE_EXIT=0")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
