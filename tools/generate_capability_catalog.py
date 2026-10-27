#!/usr/bin/env python3
"""Project product coverage and maturity evidence into the API's static catalog.

Install the optional generator dependency with:
    python -m pip install -r tools/requirements-capability-catalog.txt

The generated JSON is committed and embedded by qx-api. Normal builds and the
architecture gate do not require PyYAML; the gate verifies the source digests.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path
from typing import Any

import yaml


ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = Path("maturity/capability-matrix.yaml")
MATURITY_PATH = Path("maturity/capabilities.yaml")
CROSSWALK_PATH = Path("maturity/capability-crosswalk.yaml")
SOURCE_PATHS = (MATRIX_PATH, MATURITY_PATH, CROSSWALK_PATH)
OUTPUT_PATHS = (
    Path("crates/qx-api/src/capability_catalog.generated.json"),
    Path("crates/qx-python/src/capability_catalog.generated.json"),
)
MARKDOWN_OUTPUT_PATH = Path("docs/capability-catalog.md")
MATRIX_SECTIONS = ("domains", "venues", "providers")
EXECUTION_SCOPE_FIELDS = (
    "venue",
    "product",
    "account_mode",
    "account_models",
    "margin_modes",
    "position_modes",
    "operations",
    "order_shapes",
    "maturity_refs",
    "status",
    "evidence",
)
ACCOUNT_QUERY_SCOPE_FIELDS = (
    "venue",
    "product",
    "account_models",
    "operations",
    "maturity_refs",
    "status",
    "evidence",
)
ACCOUNT_QUERY_OPERATIONS = {
    "balance.read",
    "orders.read",
    "positions.read",
    "cashflows.read",
    "funding.read",
    "account_settings.read",
}
MATURITY_STAGES = (
    "implementation",
    "code_tested",
    "sandbox_tested",
    "production_approved",
)
EXECUTION_OPERATIONS = {"submit", "cancel"}


def read_yaml(path: Path) -> dict[str, Any]:
    try:
        value = yaml.safe_load((ROOT / path).read_text(encoding="utf-8"))
    except (OSError, yaml.YAMLError) as error:
        raise ValueError(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{path}: expected a YAML mapping at the root")
    return value


def source_digests() -> dict[str, str]:
    return {
        path.as_posix(): hashlib.sha256((ROOT / path).read_bytes()).hexdigest()
        for path in SOURCE_PATHS
    }


def execution_scopes_for_row(
    section: str, row: dict[str, Any], levels: list[Any]
) -> list[dict[str, Any]]:
    scopes = row.get("execution_scopes", [])
    if section != "venues" and scopes:
        raise ValueError(f"{section}/{row.get('name')}: execution scopes only belong to venues")
    if not isinstance(scopes, list):
        raise ValueError(f"venues/{row.get('name')}: execution_scopes must be a list")
    normalized: list[dict[str, Any]] = []
    seen: set[tuple[str, str, str, tuple[str, ...]]] = set()
    for index, scope in enumerate(scopes):
        label = f"venues/{row.get('name')}.execution_scopes[{index}]"
        if not isinstance(scope, dict):
            raise ValueError(f"{label}: expected a mapping")
        missing = [field for field in EXECUTION_SCOPE_FIELDS if field not in scope]
        unknown = sorted(set(scope) - set(EXECUTION_SCOPE_FIELDS))
        if missing or unknown:
            raise ValueError(f"{label}: missing={missing}, unknown={unknown}")
        venue = scope["venue"]
        product = scope["product"]
        account_mode = scope["account_mode"]
        if not all(isinstance(value, str) and value.strip() for value in (venue, product, account_mode)):
            raise ValueError(f"{label}: venue/product/account_mode must be non-empty strings")
        if venue != venue.strip().upper() or product != product.strip().lower() or account_mode != account_mode.strip().lower():
            raise ValueError(f"{label}: venue must be uppercase and product/account_mode lowercase")
        if scope["status"] not in levels:
            raise ValueError(f"{label}: status is outside the declared product-level vocabulary")
        account_models = scope["account_models"]
        margin_modes = scope["margin_modes"]
        if not isinstance(account_models, list) or not account_models or any(
            not isinstance(value, str) or not value.strip() for value in account_models
        ):
            raise ValueError(f"{label}: account_models must be a non-empty string list")
        if not isinstance(margin_modes, list) or not margin_modes or any(
            not isinstance(value, str) or not value.strip() for value in margin_modes
        ):
            raise ValueError(f"{label}: margin_modes must be a non-empty string list")
        if any(value != value.strip().lower() for value in account_models + margin_modes):
            raise ValueError(f"{label}: account_models and margin_modes must be lowercase")
        if len(set(account_models)) != len(account_models) or len(set(margin_modes)) != len(margin_modes):
            raise ValueError(f"{label}: account_models and margin_modes must be unique")
        position_modes = scope["position_modes"]
        if not isinstance(position_modes, list) or not position_modes or any(
            not isinstance(value, str) or not value.strip() for value in position_modes
        ):
            raise ValueError(f"{label}: position_modes must be a non-empty string list")
        if any(value != value.strip().lower() for value in position_modes):
            raise ValueError(f"{label}: position_modes must be lowercase")
        if len(set(position_modes)) != len(position_modes):
            raise ValueError(f"{label}: position_modes must be unique")
        operations = scope["operations"]
        if not isinstance(operations, list) or not operations or any(
            not isinstance(value, str)
            or value != value.strip().lower()
            or value not in EXECUTION_OPERATIONS
            for value in operations
        ):
            raise ValueError(f"{label}: operations must be a non-empty list from {sorted(EXECUTION_OPERATIONS)}")
        if len(set(operations)) != len(operations):
            raise ValueError(f"{label}: operations must be unique")
        order_shapes = scope["order_shapes"]
        if not isinstance(order_shapes, list) or not order_shapes:
            raise ValueError(f"{label}: order_shapes must be a non-empty list")
        normalized_shapes: list[dict[str, str]] = []
        seen_shapes: set[tuple[str, str, str]] = set()
        for shape in order_shapes:
            if not isinstance(shape, dict) or set(shape) != {"order_type", "quantity_unit", "side"}:
                raise ValueError(
                    f"{label}: every order shape requires exactly order_type, quantity_unit, side"
                )
            order_type = shape["order_type"]
            quantity_unit = shape["quantity_unit"]
            side = shape["side"]
            if any(
                not isinstance(value, str)
                or value != value.strip().lower()
                or not value
                for value in (order_type, quantity_unit, side)
            ):
                raise ValueError(f"{label}: order shape values must be lowercase strings")
            if order_type not in {"market", "limit"} or quantity_unit not in {
                "base",
                "contracts",
                "quote",
            } or side not in {"buy", "sell"}:
                raise ValueError(f"{label}: unsupported order shape {shape}")
            if quantity_unit == "quote" and (order_type != "market" or side != "buy"):
                raise ValueError(f"{label}: quote amount is only valid for market buy orders")
            key_shape = (order_type, quantity_unit, side)
            if key_shape in seen_shapes:
                raise ValueError(f"{label}: duplicate order shape {key_shape}")
            seen_shapes.add(key_shape)
            normalized_shapes.append(
                {"order_type": order_type, "quantity_unit": quantity_unit, "side": side}
            )
        maturity_refs = scope["maturity_refs"]
        if not isinstance(maturity_refs, list) or not maturity_refs or any(
            not isinstance(value, str) or not value.strip() for value in maturity_refs
        ):
            raise ValueError(f"{label}: maturity_refs must be a non-empty string list")
        if len(set(maturity_refs)) != len(maturity_refs):
            raise ValueError(f"{label}: maturity_refs must be unique")
        evidence = scope["evidence"]
        if not isinstance(evidence, list) or not evidence or any(
            not isinstance(value, str) or not value.strip() for value in evidence
        ):
            raise ValueError(f"{label}: evidence must be a non-empty string list")
        missing_evidence = [path for path in evidence if not (ROOT / path).is_file()]
        if missing_evidence:
            raise ValueError(f"{label}: evidence paths missing: {missing_evidence}")
        key = (venue, product, account_mode, tuple(sorted(account_models)))
        if key in seen:
            raise ValueError(f"{label}: duplicate execution scope {key}")
        seen.add(key)
        normalized.append(
            {
                "venue": venue,
                "product": product,
                "account_mode": account_mode,
                "account_models": sorted(account_models),
                "margin_modes": sorted(margin_modes),
                "position_modes": sorted(position_modes),
                "operations": sorted(operations),
                "order_shapes": sorted(
                    normalized_shapes,
                    key=lambda item: (item["order_type"], item["quantity_unit"], item["side"]),
                ),
                "maturity_refs": sorted(maturity_refs),
                "status": scope["status"],
                "evidence": evidence,
            }
        )
    return normalized


def account_query_scopes_for_row(
    section: str, row: dict[str, Any], levels: list[Any]
) -> list[dict[str, Any]]:
    scopes = row.get("account_query_scopes", [])
    if section != "venues" and scopes:
        raise ValueError(f"{section}/{row.get('name')}: account query scopes only belong to venues")
    if not isinstance(scopes, list):
        raise ValueError(f"venues/{row.get('name')}: account_query_scopes must be a list")
    normalized: list[dict[str, Any]] = []
    seen: set[tuple[str, str, tuple[str, ...]]] = set()
    for index, scope in enumerate(scopes):
        label = f"venues/{row.get('name')}.account_query_scopes[{index}]"
        if not isinstance(scope, dict):
            raise ValueError(f"{label}: expected a mapping")
        missing = [field for field in ACCOUNT_QUERY_SCOPE_FIELDS if field not in scope]
        unknown = sorted(set(scope) - set(ACCOUNT_QUERY_SCOPE_FIELDS))
        if missing or unknown:
            raise ValueError(f"{label}: missing={missing}, unknown={unknown}")
        venue = scope["venue"]
        product = scope["product"]
        if not isinstance(venue, str) or venue != venue.strip().upper() or not venue:
            raise ValueError(f"{label}: venue must be a non-empty uppercase string")
        if not isinstance(product, str) or product != product.strip().lower() or not product:
            raise ValueError(f"{label}: product must be a non-empty lowercase string")
        account_models = scope["account_models"]
        if not isinstance(account_models, list) or not account_models or any(
            not isinstance(value, str) or value != value.strip().lower() or not value
            for value in account_models
        ) or len(set(account_models)) != len(account_models):
            raise ValueError(f"{label}: account_models must be a unique non-empty lowercase list")
        operations = scope["operations"]
        if not isinstance(operations, list) or not operations or any(
            not isinstance(value, str) or value not in ACCOUNT_QUERY_OPERATIONS
            for value in operations
        ) or len(set(operations)) != len(operations):
            raise ValueError(f"{label}: operations must be a unique non-empty known operation list")
        if scope["status"] not in levels:
            raise ValueError(f"{label}: status is outside the declared product-level vocabulary")
        maturity_refs = scope["maturity_refs"]
        if not isinstance(maturity_refs, list) or not maturity_refs or any(
            not isinstance(value, str) or not value.strip() for value in maturity_refs
        ) or len(set(maturity_refs)) != len(maturity_refs):
            raise ValueError(f"{label}: maturity_refs must be a unique non-empty string list")
        evidence = scope["evidence"]
        if not isinstance(evidence, list) or not evidence or any(
            not isinstance(value, str) or not value.strip() for value in evidence
        ):
            raise ValueError(f"{label}: evidence must be a non-empty string list")
        missing_evidence = [path for path in evidence if not (ROOT / path).is_file()]
        if missing_evidence:
            raise ValueError(f"{label}: evidence paths missing: {missing_evidence}")
        for account_model in account_models:
            key = (venue, product, account_model)
            if key in seen:
                raise ValueError(f"{label}: duplicate account query scope {key}")
            seen.add(key)
        normalized.append(
            {
                "venue": venue,
                "product": product,
                "account_models": sorted(account_models),
                "operations": sorted(operations),
                "maturity_refs": sorted(maturity_refs),
                "status": scope["status"],
                "evidence": evidence,
            }
        )
    return normalized


def build_catalog() -> dict[str, Any]:
    matrix = read_yaml(MATRIX_PATH)
    maturity = read_yaml(MATURITY_PATH)
    crosswalk = read_yaml(CROSSWALK_PATH)

    if matrix.get("kind") != "capability-matrix":
        raise ValueError(f"{MATRIX_PATH}: unexpected kind")
    if maturity.get("schema_version") != 1:
        raise ValueError(f"{MATURITY_PATH}: unsupported schema_version")
    if crosswalk.get("kind") != "capability-crosswalk":
        raise ValueError(f"{CROSSWALK_PATH}: unexpected kind")

    maturity_records = maturity.get("capabilities")
    bindings = crosswalk.get("bindings")
    if not isinstance(maturity_records, dict) or not isinstance(bindings, list):
        raise ValueError("maturity or crosswalk records are missing")

    bindings_by_id: dict[str, dict[str, Any]] = {}
    for binding in bindings:
        if not isinstance(binding, dict):
            raise ValueError("crosswalk binding must be a mapping")
        capability_id = binding.get("name")
        if not isinstance(capability_id, str) or not capability_id:
            raise ValueError("crosswalk binding is missing its stable ID")
        if capability_id in bindings_by_id:
            raise ValueError(f"duplicate crosswalk ID {capability_id}")
        bindings_by_id[capability_id] = binding

    projected: list[dict[str, Any]] = []
    index: list[dict[str, Any]] = []
    seen_ids: set[str] = set()
    referenced_maturity: set[str] = set()
    for section in MATRIX_SECTIONS:
        rows = matrix.get(section)
        if not isinstance(rows, list):
            raise ValueError(f"{MATRIX_PATH}: {section} must be a list")
        for row in rows:
            if not isinstance(row, dict):
                raise ValueError(f"{MATRIX_PATH}: {section} entry must be a mapping")
            capability_id = row.get("capability_id")
            name = row.get("name")
            if not isinstance(capability_id, str) or not capability_id:
                raise ValueError(f"{section}/{name}: missing capability_id")
            if capability_id in seen_ids:
                raise ValueError(f"duplicate matrix ID {capability_id}")
            seen_ids.add(capability_id)
            binding = bindings_by_id.get(capability_id)
            if binding is None:
                raise ValueError(f"{capability_id}: missing crosswalk binding")
            expected_matrix_ref = f"{section}/{name}"
            if binding.get("matrix_ref") != expected_matrix_ref:
                raise ValueError(
                    f"{capability_id}: crosswalk points to {binding.get('matrix_ref')}, "
                    f"expected {expected_matrix_ref}"
                )
            execution_scopes = execution_scopes_for_row(
                section, row, matrix.get("levels", [])
            )
            account_query_scopes = account_query_scopes_for_row(
                section, row, matrix.get("levels", [])
            )

            refs = binding.get("maturity_refs")
            if not isinstance(refs, list) or not refs:
                raise ValueError(f"{capability_id}: maturity_refs must be a non-empty list")
            for scope in execution_scopes + account_query_scopes:
                unknown_refs = sorted(set(scope["maturity_refs"]) - set(refs))
                if unknown_refs:
                    raise ValueError(
                        f"{capability_id}: execution scope references unbound maturity evidence {unknown_refs}"
                    )
            maturity_evidence: list[dict[str, Any]] = []
            for reference in refs:
                record = maturity_records.get(reference)
                if not isinstance(record, dict):
                    raise ValueError(f"{capability_id}: unknown maturity reference {reference}")
                stages = {stage: record.get(stage) for stage in MATURITY_STAGES}
                invalid_stages = {
                    name: value
                    for name, value in stages.items()
                    if not isinstance(value, bool) and not (isinstance(value, str) and value.strip())
                }
                if invalid_stages:
                    raise ValueError(f"{reference}: invalid maturity stages {invalid_stages}")
                evidence = record.get("evidence", [])
                if not isinstance(evidence, list) or any(not isinstance(item, str) for item in evidence):
                    raise ValueError(f"{reference}: evidence must be a list of paths")
                limitations = record.get("limitations", [])
                if not isinstance(limitations, list) or any(not isinstance(item, str) for item in limitations):
                    raise ValueError(f"{reference}: limitations must be a list of strings")
                maturity_evidence.append(
                    {
                        "id": reference,
                        "stages": stages,
                        "unverified_stages": [
                            stage for stage, value in stages.items() if value is not True
                        ],
                        "evidence": evidence,
                        "limitations_count": len(limitations),
                    }
                )
                referenced_maturity.add(reference)

            projected.append(
                {
                    "id": capability_id,
                    "kind": section[:-1],
                    "name": name,
                    "matrix_ref": expected_matrix_ref,
                    "matrix": row,
                    "execution_scopes": execution_scopes,
                    "account_query_scopes": account_query_scopes,
                    "maturity_evidence": maturity_evidence,
                }
            )
            if section == "domains":
                matrix_status = {"level": row.get("level")}
            else:
                field_key = "capability_fields" if section == "venues" else "provider_fields"
                matrix_status = {
                    field: row.get(field)
                    for field in matrix.get(field_key, [])
                }
            index.append(
                {
                    "id": capability_id,
                    "kind": section[:-1],
                    "name": name,
                    "matrix_ref": expected_matrix_ref,
                    "matrix_status": matrix_status,
                    "maturity_refs": refs,
                    "execution_scope_count": len(execution_scopes),
                    "account_query_scope_count": len(account_query_scopes),
                    "evidence_path_count": sum(len(item["evidence"]) for item in maturity_evidence),
                    "limitation_count": len(row.get("limitations", [])),
                }
            )

    if seen_ids != set(bindings_by_id):
        raise ValueError(
            f"crosswalk IDs differ from matrix IDs: "
            f"missing={sorted(seen_ids - set(bindings_by_id))}, "
            f"extra={sorted(set(bindings_by_id) - seen_ids)}"
        )
    if referenced_maturity != set(maturity_records):
        raise ValueError(
            f"maturity references are incomplete: "
            f"unlinked={sorted(set(maturity_records) - referenced_maturity)}"
        )

    all_scopes = [
        scope
        for capability in projected
        for scope in capability["execution_scopes"]
    ]
    scope_facets = {
        field: sorted(
            {
                value
                for scope in all_scopes
                for value in (
                    scope[field]
                    if isinstance(scope[field], list)
                    else [scope[field]]
                )
            }
        )
        for field in (
            "venue",
            "product",
            "account_mode",
            "account_models",
            "margin_modes",
            "position_modes",
            "operations",
        )
    }
    scope_facets["order_types"] = sorted(
        {
            shape["order_type"]
            for scope in all_scopes
            for shape in scope["order_shapes"]
        }
    )
    scope_facets["quantity_units"] = sorted(
        {
            shape["quantity_unit"]
            for scope in all_scopes
            for shape in scope["order_shapes"]
        }
    )
    return {
        "schema_version": 1,
        "kind": "qianxing-capability-catalog",
        "source_updated_at": {
            "matrix": matrix.get("updated_at"),
            "maturity": maturity.get("updated_at"),
            "crosswalk": crosswalk.get("updated_at"),
        },
        "source_sha256": source_digests(),
        "product_levels": matrix.get("levels", []),
        "maturity_stages": list(MATURITY_STAGES),
        "execution_scope_facets": scope_facets,
        "index": index,
        "capabilities": projected,
    }


def render(catalog: dict[str, Any]) -> str:
    return json.dumps(catalog, ensure_ascii=False, indent=2) + "\n"


def render_markdown(catalog: dict[str, Any]) -> str:
    lines = [
        "# 产品能力与验收证据目录",
        "",
        "<!-- Generated by tools/generate_capability_catalog.py; do not edit. -->",
        "<!-- Source SHA-256: "
        + "; ".join(
            f"{path}={digest}" for path, digest in catalog["source_sha256"].items()
        )
        + " -->",
        "",
        f"来源更新时间：矩阵 `{catalog['source_updated_at']['matrix']}`；成熟度登记 `{catalog['source_updated_at']['maturity']}`；关联表 `{catalog['source_updated_at']['crosswalk']}`。",
        "",
        "本页由产品矩阵、内部成熟度登记和稳定 ID 关联表生成。产品状态说明静态支持范围；成熟度证据说明代码与验收登记到哪一档。两者都不代表当前机器已连通供应商、凭据可用或真实沙盒/生产验收通过。详情以 API、CLI 按能力 ID 查询结果及对应来源文件为准。",
        "",
        "| 能力 ID | 类型 | 名称 | 产品状态 | 订单范围 | 账户查询范围 | 成熟度登记 | 限制 | 证据路径 |",
        "|---|---|---|---|---:|---:|---:|---:|---:|",
    ]
    for entry in catalog["index"]:
        status = entry["matrix_status"]
        if isinstance(status, dict):
            status_text = "; ".join(f"{key}={value}" for key, value in status.items())
        else:
            status_text = str(status)
        fields = [
            str(entry["id"]),
            str(entry["kind"]),
            str(entry["name"]),
            status_text or "—",
            str(entry["execution_scope_count"]),
            str(entry["account_query_scope_count"]),
            str(len(entry["maturity_refs"])),
            str(entry["limitation_count"]),
            str(entry["evidence_path_count"]),
        ]
        lines.append("| " + " | ".join(value.replace("|", "\\|") for value in fields) + " |")
    lines.extend(
        [
            "",
            "## 交易所、产品与账户模式范围",
            "",
            "范围行只表示仓内代码、配置样例和离线请求测试覆盖；`implemented-unverified` 不代表已通过真实供应商 sandbox。账户模式采用统一订单口径：`cash`、`one_way`、`hedge`；`account_models` 保留交易所账户路径名，保证金模式单独列出。`operations` 将 submit 与 cancel 分开登记；能提交订单不自动代表能撤单。",
            "",
            "| Capability ID | Venue | Product | Account mode | Account models | Margin modes | Position modes | Operations | Order shapes | 状态 | 成熟度证据 | 代码/fixture 证据 |",
            "|---|---|---|---|---|---|---|---|---|---|---|---|",
        ]
    )
    for capability in catalog["capabilities"]:
        for scope in capability["execution_scopes"]:
            fields = [
                capability["id"],
                scope["venue"],
                scope["product"],
                scope["account_mode"],
                ", ".join(scope["account_models"]),
                ", ".join(scope["margin_modes"]),
                ", ".join(scope["position_modes"]),
                ", ".join(scope["operations"]),
                "; ".join(
                    f"{shape['order_type']}:{shape['quantity_unit']}:{shape['side']}"
                    for shape in scope["order_shapes"]
                ),
                scope["status"],
                ", ".join(scope["maturity_refs"]),
                ", ".join(scope["evidence"]),
            ]
            lines.append("| " + " | ".join(value.replace("|", "\\|") for value in fields) + " |")
    lines.extend(
        [
            "",
            "## 账户私有查询范围",
            "",
            "本范围约束余额、订单、持仓、资金流水和费率等私有查询入口；它只证明代码与离线契约已登记。CCXT 各供应商接口差异和权限仍须逐个 sandbox 验收，未列出的可选查询不会由 worker 调用。",
            "",
            "| Capability ID | Venue | Product | Account models | Operations | 状态 | 成熟度证据 | 代码/fixture 证据 |",
            "|---|---|---|---|---|---|---|---|",
        ]
    )
    for capability in catalog["capabilities"]:
        for scope in capability["account_query_scopes"]:
            for account_model in scope["account_models"]:
                fields = [
                    capability["id"],
                    scope["venue"],
                    scope["product"],
                    account_model,
                    ", ".join(scope["operations"]),
                    scope["status"],
                    ", ".join(scope["maturity_refs"]),
                    ", ".join(scope["evidence"]),
                ]
                lines.append("| " + " | ".join(value.replace("|", "\\|") for value in fields) + " |")
    lines.extend(
        [
            "",
            "## 查询方式",
            "",
            "```text",
            "qx-cli doctor --capabilities",
            "qx-cli doctor --capabilities --capability-id qx.venue.binance_spot --json",
            "qx-cli doctor --capabilities --venue BINANCE --product perpetual --account-mode hedge --json",
            "GET /app/capabilities",
            'POST /app/capabilities/query  {"capability_id":"qx.venue.binance_spot"}',
            'POST /app/capabilities/query  {"venue":"BITGET","product":"spot","account_mode":"cash"}',
            'POST /app/capabilities/query  {"venue":"OKX","product":"perpetual","account_mode":"hedge","operation":"cancel"}',
            'POST /app/capabilities/query  {"venue":"OKX","product":"perpetual","account_model":"unified","operation":"account_settings.read"}',
            'Python: client.query_product_capability(venue="BINANCE", product="perpetual", account_mode="hedge")',
            "C++: client.query_product_capability(query_json) accepts the same JSON body",
            "```",
            "",
            "修改任一来源后，运行 `python tools/generate_capability_catalog.py` 更新本页、API 目录和 Python 原生桥接目录；`--check` 可验证所有生成物是否过期。",
            "",
            "来源：[`maturity/capability-matrix.yaml`](../maturity/capability-matrix.yaml)、[`maturity/capabilities.yaml`](../maturity/capabilities.yaml)、[`maturity/capability-crosswalk.yaml`](../maturity/capability-crosswalk.yaml)。",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
    help="fail if generated API/Python catalogs or Markdown are stale",
    )
    args = parser.parse_args()
    try:
        catalog = build_catalog()
        expected_outputs = {
            MARKDOWN_OUTPUT_PATH: render_markdown(catalog),
        }
        expected_outputs.update({path: render(catalog) for path in OUTPUT_PATHS})
        if args.check:
            stale = [
                path
                for path, expected in expected_outputs.items()
                if not (ROOT / path).is_file()
                or (ROOT / path).read_text(encoding="utf-8") != expected
            ]
            if stale:
                print(
                    f"generated capability outputs are stale: {', '.join(path.as_posix() for path in stale)}; "
                    "run python tools/generate_capability_catalog.py",
                    file=sys.stderr,
                )
                return 1
            print("API/Python JSON catalogs and Markdown capability table match all sources")
            return 0
        for path, content in expected_outputs.items():
            output = ROOT / path
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(content, encoding="utf-8", newline="\n")
            print(f"wrote {path}")
        return 0
    except (OSError, ValueError) as error:
        print(f"generate_capability_catalog: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
