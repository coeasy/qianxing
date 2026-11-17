"""Remove local build/runtime outputs from recorded strategy source manifests.

Use --write once when migrating existing acceptance records after the source
manifest exclusion rules are corrected. Normal use without --write is a report.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile

from record_strategy_acceptance import is_excluded_source_path


ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "maturity" / "strategy-run-acceptance-v1.json"


def digest_manifest(files: list[dict[str, str]]) -> str:
    digest = hashlib.sha256()
    for item in files:
        path = item["path"].encode("utf-8")
        digest.update(len(path).to_bytes(4, "big"))
        digest.update(path)
        digest.update(bytes.fromhex(item["sha256"]))
    return digest.hexdigest()


def sha256_bytes(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def build_plan() -> tuple[dict[str, object], dict[Path, bytes], int]:
    catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
    updated_files: dict[Path, bytes] = {}
    removed_entries = 0

    for run in catalog.get("runs", []):
        relative_manifest = run.get("source_manifest")
        if not isinstance(relative_manifest, str):
            continue
        manifest_path = (ROOT / relative_manifest).resolve()
        manifest_path.relative_to(ROOT)
        original_bytes = manifest_path.read_bytes()
        manifest = json.loads(original_bytes.decode("utf-8"))
        files = manifest.get("files")
        if not isinstance(files, list):
            raise ValueError(f"invalid file list in {relative_manifest}")
        old_digest = digest_manifest(files)
        if old_digest != manifest.get("source_tree_sha256") or old_digest != run.get("source_tree_sha256"):
            raise ValueError(f"source tree digest mismatch before migration: {relative_manifest}")

        artifact_entries = [
            item for item in run.get("artifacts", [])
            if isinstance(item, dict) and item.get("path") == relative_manifest
        ]
        if len(artifact_entries) != 1 or artifact_entries[0].get("sha256") != sha256_bytes(original_bytes):
            raise ValueError(f"source manifest artifact hash mismatch before migration: {relative_manifest}")

        retained = [item for item in files if not is_excluded_source_path(item.get("path", ""))]
        retained_paths = {item.get("path") for item in retained}
        for input_item in run.get("inputs", []):
            if input_item.get("path") not in retained_paths:
                raise ValueError(f"migration would remove an acceptance input: {input_item.get('path')}")

        removed_entries += len(files) - len(retained)
        if len(retained) == len(files):
            continue

        manifest["files"] = retained
        new_digest = digest_manifest(retained)
        manifest["source_tree_sha256"] = new_digest
        new_bytes = (json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
        updated_files[manifest_path] = new_bytes
        run["source_tree_sha256"] = new_digest
        artifact_entries[0]["sha256"] = sha256_bytes(new_bytes)

    return catalog, updated_files, removed_entries


def atomic_write(path: Path, content: bytes) -> None:
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="rewrite source manifests and their catalog hashes")
    args = parser.parse_args()
    catalog, updated_files, removed_entries = build_plan()
    print(f"manifests_to_update={len(updated_files)} excluded_entries={removed_entries}")
    if not args.write:
        print("report only; pass --write to apply the migration")
        return 0

    for path, content in updated_files.items():
        atomic_write(path, content)
    catalog_bytes = (json.dumps(catalog, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    atomic_write(CATALOG, catalog_bytes)
    print(f"updated={len(updated_files)} catalog={CATALOG.relative_to(ROOT).as_posix()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
