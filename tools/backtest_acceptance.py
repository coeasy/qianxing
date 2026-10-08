#!/usr/bin/env python3
"""回测轨验收脚本（P0-1）：**一条交易所凭据都不用**就能完整跑完，并留下可核对的记录。

## 为什么单独有一条「回测轨」

`maturity/evidence/testnet/` 下那份 Binance 验收记录 `outcome = "skipped"`（缺
`QX_BINANCE_TESTNET_API_KEY` / `_SECRET`），于是 `maturity/capabilities.yaml` 的
`sandbox_tested` / `production_approved` 只能全为 false。这两档**只对需要外部 venue 的能力
有意义**；而本仓的主用法是回测与 Paper 闭环，它一条凭据都不用。把两条轨混在一张表里，
会让「实盘待外部证据」读成「整体未落地」——这正是 P0-1 卡住的那一格。

本脚本把回测轨做成**可复现的验收**：结论落进 `maturity/backtest_acceptance.yaml`
（与 `maturity/evidence/**` 不同，**这份是仓库资产**，因为它是"无需凭据"这件事本身的证据）。

## 判据

**同一目录重跑（逐格相等）**——同输入两次 `backtest`，下列每一格都必须逐字相等：

* `result_hash`、`data_fingerprint`、`config_fingerprint`；
* 四份产物的**文件名**（含配置指纹后缀）与**内容 sha256**；
* `replay_verdict`。

**两个独立目录（只要求语义相等）**——两个互不相干的项目各跑一轮，下列必须相等：
`result_hash`、`data_fingerprint`、`replay_verdict`，以及四份产物**归一化之后**的 sha256
（`equity.csv` / `fills.csv` 这两份纯数据要求**逐字节**相等，不做归一化）。

**允许不同、且必须只差这些**：配置里的 `storage.data_dir` 是绝对路径，两处目录不同 →
配置内容不同 → 内容寻址的 `config_hash` 不同 → 由它派生的产物**文件名后缀**也不同；
`summary.json` / `run.json` 里各自写着本轮的绝对路径与那个 `config_hash`。
归一化只抹掉这三样（路径、`config_hash`、由它派生的文件名后缀），**其余仍要求逐字节相等**
——归一化不是放水，它把「差异只来自路径」这句话变成了断言。这恰好是 §46 那条
「artifact digest vs result_hash」的区分：**结果**必须与路径无关，**产物身份**不必。

**无需凭据**——两次都读不到任何凭据环境变量（脚本显式把它们从子进程环境里摘掉），
且回读的 `status` 必须自报 `network_accessed=false` / `orders_sent=false`。

退出码：0 全过；2 有判据不成立。任一判据不成立时**不写**验收记录（fail closed）——
留一份"看起来通过了"的记录比没有记录更坏。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

WORKSPACE = Path(__file__).resolve().parents[1]
RECORD = WORKSPACE / "maturity" / "backtest_acceptance.yaml"

EXIT_FAILURE = 2
LEG_BUDGET_SECONDS = 300

# 这些环境变量在回测轨上**必须缺席**：它们的存在会让"不需要凭据"这句话失去证据。
CREDENTIAL_ENV_PREFIXES = ("QX_BINANCE_", "QX_CCXT_", "QX_OKX_")

# 产物按**种类**比对内容：文件名带配置指纹后缀，跨目录本来就不该相同。
ARTIFACT_KINDS = {
    ".summary.json": "summary",
    ".equity.csv": "equity",
    ".fills.csv": "fills",
    ".run.json": "run_manifest",
}

RUN_MANIFEST_PATH = re.compile(r"\[RunManifest\]\s+path=(\S+)")
ARTIFACTS_LINE = re.compile(r"\[Artifacts\]\s+summary=(\S+)\s+equity=(\S+)\s+fills=(\S+)")
BACKTEST_RESULT_HASH = re.compile(r"\[Strategy · Backtest\].*?result_hash=(\S+)")

# 跨目录归一化时要抹掉的两样（都是 `storage.data_dir` 绝对路径的下游）：内容寻址的
# `config_hash`，以及由它派生的产物文件名后缀 `...-<16 hex>.<kind>`。
CONFIG_HASH = re.compile(r'"config_hash":"[0-9a-f]{64}"')
ARTIFACT_STEM = re.compile(
    r"-[0-9a-f]{16}\.(summary\.json|equity\.csv|fills\.csv|record\.json|run\.json)"
)


def default_binary() -> Path:
    for candidate in (
        WORKSPACE / ".cargo-target" / "debug" / "qx-cli.exe",
        WORKSPACE / ".cargo-target" / "debug" / "qx-cli",
        WORKSPACE / "target" / "debug" / "qx-cli.exe",
        WORKSPACE / "target" / "debug" / "qx-cli",
    ):
        if candidate.is_file():
            return candidate
    raise SystemExit("找不到被测 binary：先 `cargo build -p qx-cli`，或用 --binary 指定")


def scrubbed_env() -> dict[str, str]:
    """去掉一切凭据环境变量后的子进程环境。"""
    return {
        name: value
        for name, value in os.environ.items()
        if not name.startswith(CREDENTIAL_ENV_PREFIXES)
    }


def run(argv: list[str], cwd: Path) -> tuple[int, str]:
    completed = subprocess.run(
        argv,
        cwd=str(cwd),
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env=scrubbed_env(),
        timeout=LEG_BUDGET_SECONDS,
    )
    return completed.returncode, (completed.stdout or "") + (completed.stderr or "")


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def sha256_normalized(path: Path, root: Path) -> str:
    """把「按目录而变的那三样」抹掉之后的 sha256。

    只抹三样：① 项目根绝对路径（`data_dir`、产物路径）；② 内容寻址的 `config_hash`
    （配置里含绝对 `data_dir`，两处目录必然不同）；③ 由 `config_hash` 派生的产物**文件名
    后缀**（`...-<16 hex>.summary.json`）。抹掉之后仍要求逐字节相等，才真正证明
    「差异只来自路径」——而不是把这一格跳过。
    """
    text = path.read_text(encoding="utf-8")
    escaped = json.dumps(str(root))[1:-1]  # JSON 串里的转义形态（Windows 反斜杠）
    for variant in {str(root), str(root).replace("\\", "/"), escaped}:
        text = text.replace(variant, "<ROOT>")
    text = CONFIG_HASH.sub('"config_hash":"<CONFIG_HASH>"', text)
    text = ARTIFACT_STEM.sub(r"-<CONFIG_HASH>.\1", text)
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def read_status(binary: Path, runtime: Path, cwd: Path) -> dict[str, Any]:
    code, output = run([str(binary), "status", str(runtime), "--json"], cwd)
    if code != 0:
        raise SystemExit(f"`status --json` 以 {code} 退出：\n{output}")
    start, end = output.find("{"), output.rfind("}")
    if start < 0 or end < 0:
        raise SystemExit(f"`status --json` 没有交出 JSON：\n{output}")
    return json.loads(output[start : end + 1])


def snapshot_from_output(output: str, project: Path) -> dict[str, Any]:
    """从 `backtest` 的 stdout 里取**这一轮**的产物路径，再逐份算摘要。

    按输出取路径而不是扫目录：`quickstart` 自带的那一轮与本脚本受控的那两轮落在同一个
    `runs/` 目录里，扫目录会把它们混在一起。
    """
    manifest = RUN_MANIFEST_PATH.search(output)
    artifacts = ARTIFACTS_LINE.search(output)
    result_hash = BACKTEST_RESULT_HASH.search(output)
    if not manifest or not artifacts or not result_hash:
        raise SystemExit(f"`backtest` 的输出缺少可解析的产物行：\n{output}")

    manifest_path = Path(manifest.group(1))
    paths = {
        "summary": Path(artifacts.group(1)),
        "equity": Path(artifacts.group(2)),
        "fills": Path(artifacts.group(3)),
        "run_manifest": manifest_path,
    }
    record_path = manifest_path.with_name(
        manifest_path.name.replace(".run.json", ".record.json")
    )
    for path in (*paths.values(), record_path):
        if not path.is_file():
            raise SystemExit(f"输出点名的产物不在盘：{path}")

    record = json.loads(record_path.read_text(encoding="utf-8"))
    summary = json.loads(paths["summary"].read_text(encoding="utf-8"))
    return {
        "stem": manifest_path.name.removesuffix(".run.json"),
        "result_hash": summary.get("result_hash") or result_hash.group(1),
        "data_fingerprint": record.get("input_digest"),
        "config_fingerprint": record.get("config_fingerprint"),
        "replay_verdict": record.get("replay_verdict"),
        "contents": {kind: sha256_file(path) for kind, path in paths.items()},
        "normalized": {kind: sha256_normalized(path, project) for kind, path in paths.items()},
    }


def backtest(binary: Path, project: Path, cwd: Path) -> dict[str, Any]:
    code, output = run(
        [
            str(binary),
            "backtest",
            str(project / "qianxing.runtime.json"),
            str(project / "qianxing.bar-frame.example.json"),
            str(project / "qianxing.binance.spot.spec.json"),
        ],
        cwd,
    )
    if code != 0:
        raise SystemExit(f"backtest 以 {code} 退出：\n{output}")
    return snapshot_from_output(output, project)


def one_leg(binary: Path, label: str) -> dict[str, Any]:
    """独立目录里建项目，然后**同目录跑两轮** `backtest`。"""
    root = Path(os.environ.get("TEMP", "/tmp")) / f"qianxing-backtest-acceptance-{label}"
    if root.exists():
        shutil.rmtree(root)
    root.mkdir(parents=True)
    project = root / "project"

    code, output = run([str(binary), "quickstart", str(project)], root)
    if code != 0:
        raise SystemExit(f"[{label}] quickstart 以 {code} 退出：\n{output}")

    first = backtest(binary, project, root)
    second = backtest(binary, project, root)
    status = read_status(binary, project / "qianxing.runtime.json", root)
    return {
        "label": label,
        "first": first,
        "second": second,
        "network_accessed": status.get("network_accessed"),
        "orders_sent": status.get("orders_sent"),
    }


def compare_reruns(leg: dict[str, Any]) -> list[str]:
    """同一目录重跑：逐格相等（含产物文件名与配置指纹）。"""
    problems: list[str] = []
    first, second = leg["first"], leg["second"]
    for field in ("stem", "result_hash", "data_fingerprint", "config_fingerprint", "replay_verdict"):
        if first[field] != second[field]:
            problems.append(f"[{leg['label']}] 重跑 {field} 不等：{first[field]!r} vs {second[field]!r}")
    if first["contents"] != second["contents"]:
        changed = sorted(k for k in first["contents"] if first["contents"][k] != second["contents"][k])
        problems.append(f"[{leg['label']}] 重跑产物内容不等：{changed}")
    return problems


def compare_independent(legs: list[dict[str, Any]]) -> list[str]:
    """两个独立目录：结果与**归一化后的**产物内容相等，纯数据产物逐字节相等。"""
    problems: list[str] = []
    first, second = legs[0]["first"], legs[1]["first"]
    for field in ("result_hash", "data_fingerprint", "replay_verdict"):
        if first[field] != second[field]:
            problems.append(f"独立目录 {field} 不等：{first[field]!r} vs {second[field]!r}")
    if first["normalized"] != second["normalized"]:
        changed = sorted(
            k for k in first["normalized"] if first["normalized"][k] != second["normalized"][k]
        )
        problems.append(f"独立目录产物在**归一化路径后**仍不等：{changed}（差异不只是路径）")
    for kind in ("equity", "fills"):
        if first["contents"][kind] != second["contents"][kind]:
            problems.append(f"独立目录 {kind} 未逐字节相等（纯数据产物必须与路径无关）")
    if not first["result_hash"]:
        problems.append("result_hash 为空：摘要没有交出确定性指纹")
    if set(first["contents"]) != set(ARTIFACT_KINDS.values()):
        problems.append(f"产物种类不全：{sorted(first['contents'])}")
    return problems


def compare_no_credentials(legs: list[dict[str, Any]]) -> list[str]:
    problems: list[str] = []
    for leg in legs:
        for field in ("replay_verdict",):
            if leg["first"][field] != "verified":
                problems.append(
                    f"[{leg['label']}] {field}={leg['first'][field]!r}，回放必须 verified"
                )
        if leg["network_accessed"] is not False:
            problems.append(
                f"[{leg['label']}] status.network_accessed={leg['network_accessed']!r}，回测轨必须为 false"
            )
        if leg["orders_sent"] is not False:
            problems.append(
                f"[{leg['label']}] status.orders_sent={leg['orders_sent']!r}，回测轨必须为 false"
            )
    return problems


def write_record(legs: list[dict[str, Any]], binary: Path) -> None:
    first = legs[0]["first"]
    contents = "\n".join(
        f"  {kind}: {digest}" for kind, digest in sorted(first["contents"].items())
    )
    RECORD.write_text(
        "# 回测轨验收记录（P0-1）——由 tools/backtest_acceptance.py 生成，请勿手改。\n"
        "#\n"
        "# 这条轨**不需要任何交易所凭据**：它是本仓主用法（回测 + Paper）的验收边界。\n"
        "# `sandbox_tested` / `production_approved` 两档对这条轨**不适用**（不是「待补」）——\n"
        "# 那两档只对需要外部 venue 的能力有意义，证据在 maturity/evidence/testnet|production/。\n"
        "schema_version: 1\n"
        "kind: backtest-acceptance\n"
        "outcome: passed\n"
        "generated_by: tools/backtest_acceptance.py\n"
        f"generated_at_unix: {int(time.time())}\n"
        f"binary: {binary.name}\n"
        "credentials_required: false\n"
        "external_venues: none\n"
        "network_accessed: false\n"
        "orders_sent: false\n"
        "same_dir_rerun: byte_identical\n"
        "independent_dirs: result_hash_and_artifact_contents_equal\n"
        f"independent_runs: {len(legs)}\n"
        f"result_hash: {first['result_hash']}\n"
        f"data_fingerprint: {first['data_fingerprint']}\n"
        f"config_fingerprint: {first['config_fingerprint']}\n"
        f"replay_verdict: {first['replay_verdict']}\n"
        "artifact_contents:\n"
        f"{contents}\n",
        encoding="utf-8",
        newline="\n",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="回测轨验收（无需交易所凭据）")
    parser.add_argument("--binary", type=Path, default=None)
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="只跑判据、不写 maturity/backtest_acceptance.yaml",
    )
    args = parser.parse_args()
    binary = args.binary or default_binary()

    legs = [one_leg(binary, "a"), one_leg(binary, "b")]
    problems = (
        [p for leg in legs for p in compare_reruns(leg)]
        + compare_independent(legs)
        + compare_no_credentials(legs)
    )
    if problems:
        print("回测轨验收未通过（fail closed，不写记录）：")
        for problem in problems:
            print(f"  x {problem}")
        return EXIT_FAILURE

    print(f"回测轨验收通过：result_hash={legs[0]['first']['result_hash']}")
    print(f"  产物 {len(legs[0]['first']['contents'])} 份；同目录重跑逐格相等、两个独立目录内容相等")
    print(f"  凭据：不需要（子进程环境已摘掉 {'/'.join(CREDENTIAL_ENV_PREFIXES)}）")
    if not args.dry_run:
        write_record(legs, binary)
        print(f"已写入 {RECORD.relative_to(WORKSPACE).as_posix()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
