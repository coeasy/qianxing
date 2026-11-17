"""项目内数据集包：合成样例与真实取数共用的落盘/校验形状（方案 D2/D6/D14）。

一个 bundle 目录里有四份可复核文件 + 一份索引：

    bars.json               冻结 BarFrame
    bars.manifest.json      DatasetManifestV2（Rust 发布）
    snapshot.context.json   DatasetSnapshotContextV1
    snapshot.json           DatasetSnapshotV1（绑定帧字节）
    bundle.json             索引：kind/schema/is_sample/来源/每个文件的 sha256

``is_sample`` 必须显式出现：合成样例写成 ``true`` 并带许可标签，真实取数写成 ``false``
并带 provider/version/区间/复权/``source_hash``。缺这个字段的包一律拒绝入库。
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any

from qianxing_bridge import BarFrame, native

BUNDLE_SCHEMA_VERSION = 1
BUNDLE_KIND = "qianxing-dataset-bundle-v1"
BUNDLE_FILE = "bundle.json"
BARS_FILE = "bars.json"
MANIFEST_FILE = "bars.manifest.json"
CONTEXT_FILE = "snapshot.context.json"
SNAPSHOT_FILE = "snapshot.json"

FREQUENCY_MS = {
    "1m": 60_000,
    "5m": 300_000,
    "15m": 900_000,
    "30m": 1_800_000,
    "60m": 3_600_000,
    "daily": 86_400_000,
    "weekly": 604_800_000,
    "monthly": 2_592_000_000,
}


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def frame_gaps(frame: BarFrame, frequency: str) -> list[dict[str, int]]:
    """与 Rust 发布侧同一算法：相邻时间戳超过一个步长处即记为缺口。"""
    interval = FREQUENCY_MS[frequency]
    gaps = []
    timestamps = list(frame.ts)
    for previous, current in zip(timestamps, timestamps[1:]):
        if current - previous > interval:
            gaps.append({"start_ts_ms": previous + interval, "end_ts_ms": current})
    return gaps


def source_context(
    instrument: str, *, frequency: str, adjustment: str,
    captured_at_ms: int | None = None, gaps: list[dict[str, int]] | None = None,
) -> dict[str, Any]:
    import time

    exchange = instrument.rsplit(".", 1)[-1]
    if frequency not in FREQUENCY_MS:
        raise ValueError(f"unsupported snapshot frequency: {frequency}")
    return {
        "schema_version": 1, "instrument_role": "equity",
        "captured_at_ms": captured_at_ms if captured_at_ms is not None else time.time_ns() // 1_000_000,
        "frequency_ms": FREQUENCY_MS[frequency], "timestamp_semantics": "bar_open",
        "price_precision": 2, "quantity_precision": 0,
        "price_unit": "CNY", "quantity_unit": "SHARE",
        "calendar_id": f"CN-{exchange}", "calendar_version": "1", "gaps": gaps or [],
        "corporate_action_policy": "unadjusted" if adjustment == "none" else "adjusted",
    }


def write_dataset_bundle(
    directory: Path | str,
    frame: BarFrame,
    context: dict[str, Any],
    provenance: dict[str, Any],
    *,
    dataset_id: str,
    version: str,
) -> dict[str, Any]:
    """把帧、清单、快照与索引写进一个自校验目录，返回 bundle 索引。"""
    if type(provenance.get("is_sample")) is not bool:
        raise ValueError("dataset bundle provenance must declare boolean is_sample")
    reserved = {"schema_version", "kind", "dataset_id", "version", "instrument", "rows",
                "bars_path", "manifest_path", "snapshot_context_path", "snapshot_path", "files"}
    if reserved.intersection(provenance):
        raise ValueError("provenance cannot override dataset bundle structure")
    target = Path(directory)
    target.mkdir(parents=True, exist_ok=True)
    bars_path = target / BARS_FILE
    bars_path.write_text(frame.to_json(), encoding="utf-8", newline="\n")
    manifest_result = json.loads(
        native.app_validate_dataset_manifest(
            json.dumps(
                {
                    "schema_version": 1,
                    "frame_path": str(bars_path.resolve()),
                    "dataset_id": dataset_id,
                    "version": version,
                    "interval_ms": int(context["frequency_ms"]),
                    "timezone": "UTC",
                    "output_path": str((target / MANIFEST_FILE).resolve()),
                }
            )
        )
    )
    if manifest_result.get("degraded_fingerprint"):
        raise ValueError(
            "dataset manifest fell back to a content hash: "
            + str(manifest_result["degraded_fingerprint"])
        )
    context_path = target / CONTEXT_FILE
    context_path.write_text(
        json.dumps(context, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n"
    )
    native.app_publish_dataset_snapshot(
        json.dumps(
            {
                "schema_version": 1,
                "data_path": str(bars_path.resolve()),
                "manifest_path": str((target / MANIFEST_FILE).resolve()),
                "context_path": str(context_path.resolve()),
                "output_path": str((target / SNAPSHOT_FILE).resolve()),
            }
        )
    )
    files = {}
    for name in (BARS_FILE, MANIFEST_FILE, CONTEXT_FILE, SNAPSHOT_FILE):
        files[name] = sha256_file(target / name)
    bundle = {
        "schema_version": BUNDLE_SCHEMA_VERSION,
        "kind": BUNDLE_KIND,
        "dataset_id": dataset_id,
        "version": version,
        "instrument": frame.instrument,
        "rows": len(frame.ts),
        "bars_path": BARS_FILE,
        "manifest_path": MANIFEST_FILE,
        "snapshot_context_path": CONTEXT_FILE,
        "snapshot_path": SNAPSHOT_FILE,
        "files": files,
        **provenance,
    }
    (target / BUNDLE_FILE).write_text(
        json.dumps(bundle, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n"
    )
    return bundle


def read_bundle(directory: Path | str) -> dict[str, Any]:
    path = Path(directory) / BUNDLE_FILE
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except OSError as exc:
        raise ValueError(f"dataset bundle index is missing: {path}") from exc
    except json.JSONDecodeError as exc:
        raise ValueError(f"dataset bundle index is invalid at line {exc.lineno}: {exc.msg}") from exc
    if not isinstance(value, dict) or value.get("kind") != BUNDLE_KIND:
        raise ValueError(f"dataset bundle index has an unknown kind: {path}")
    if value.get("schema_version") != BUNDLE_SCHEMA_VERSION:
        raise ValueError(f"dataset bundle schema_version must be {BUNDLE_SCHEMA_VERSION}")
    return value


def verify_bundle(directory: Path | str) -> dict[str, Any]:
    """校验索引与每个文件摘要；缺一份或摘要不符即拒绝。"""
    target = Path(directory)
    bundle = read_bundle(target)
    if type(bundle.get("is_sample")) is not bool:
        raise ValueError("dataset bundle must declare is_sample (true or false)")
    references = {"bars_path": BARS_FILE, "manifest_path": MANIFEST_FILE,
                  "snapshot_context_path": CONTEXT_FILE, "snapshot_path": SNAPSHOT_FILE}
    if any(bundle.get(key) != name for key, name in references.items()):
        raise ValueError("dataset bundle has invalid file references")
    files = bundle.get("files")
    if not isinstance(files, dict) or set(files) != set(references.values()):
        raise ValueError("dataset bundle must list all four per-file sha256 digests")
    if any(not isinstance(digest, str) or len(digest) != 64
           or any(char not in "0123456789abcdef" for char in digest) for digest in files.values()):
        raise ValueError("dataset bundle contains an invalid sha256 digest")
    if any((target / name).resolve().parent != target.resolve() for name in files):
        raise ValueError("dataset bundle files must remain inside the bundle directory")
    missing = [name for name in files if not (target / name).is_file()]
    mismatched = [
        name
        for name, digest in files.items()
        if (target / name).is_file() and sha256_file(target / name) != digest
    ]
    if missing or mismatched:
        raise ValueError(
            f"dataset bundle is incomplete: missing={sorted(missing)} mismatched={sorted(mismatched)}"
        )
    return bundle


def project_provenance(root: Path, spec: dict[str, Any]) -> list[dict[str, Any]]:
    records: dict[str, dict[str, Any]] = {}

    def visit(value: Any) -> None:
        if isinstance(value, dict):
            snapshot = value.get("dataset_snapshot_path")
            bars = value.get("bars_path")
            if isinstance(snapshot, str) and isinstance(bars, str):
                path = (root / Path(snapshot).expanduser()).resolve()
                index = path.parent / BUNDLE_FILE
                if index.is_file():
                    bundle = verify_bundle(path.parent)
                    if path != (path.parent / bundle["snapshot_path"]).resolve() or (
                        root / Path(bars).expanduser()
                    ).resolve() != (path.parent / bundle["bars_path"]).resolve():
                        raise ValueError("input paths do not match their dataset bundle")
                    fields = ("is_sample", "source", "source_kind", "provider", "provider_version",
                              "source_hash", "instrument", "frequency", "adjustment", "start",
                              "end", "rows", "license", "cached")
                    records[str(index)] = {
                        **{key: bundle[key] for key in fields if key in bundle},
                        "bundle": str(index), "bundle_sha256": sha256_file(index),
                        "snapshot_sha256": bundle["files"][SNAPSHOT_FILE],
                    }
            for item in value.values():
                visit(item)
        elif isinstance(value, list):
            for item in value:
                visit(item)

    visit(spec)
    return list(records.values())


def install_bundle(bundle_directory: Path | str, target_directory: Path | str) -> dict[str, Any]:
    """把已校验的 bundle 复制到项目数据目录（只做校验与落盘，不联网）。"""
    source = Path(bundle_directory)
    bundle = verify_bundle(source)
    target = Path(target_directory)
    target.mkdir(parents=True, exist_ok=True)
    names = [*bundle["files"], BUNDLE_FILE]
    if any((target / name).resolve().parent != target.resolve() for name in names):
        raise ValueError("dataset installation cannot write outside the target directory")
    for name in names:
        (target / name).write_bytes((source / name).read_bytes())
    installed = verify_bundle(target)
    return {
        "status": "installed",
        "target": str(target),
        "instrument": installed["instrument"],
        "rows": installed["rows"],
        "is_sample": installed["is_sample"],
        "source": installed.get("source"),
        "snapshot": str(target / installed["snapshot_path"]),
        "bars": str(target / installed["bars_path"]),
    }
