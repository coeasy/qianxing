"""``qianxing data`` 与 ``python -m qianxing_ashare`` 共用的取数命令行（方案 D11）。

同一份实现、两份入口：``qianxing data …`` 是产品命令，``python -m qianxing_ashare …``
保留为兼容别名。取数、标准化、缓存与失败分类都在 :mod:`qianxing_ashare` 里，本模块
只做"选源 → 取数 → 校验 → 落盘 → 登记"的分派，不引入第二套取数逻辑。

失败一律按 :mod:`qianxing_ashare.sources` 的固定 category 输出（D16），退出码由
``exit_code_for`` 决定；多标的取数默认整批失败、零产物，``--allow-partial`` 才落盘。
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any, Sequence

from qianxing_bridge import BarFrame

from . import (
    AshareActionManifest,
    AshareActionQuery,
    AshareManifest,
    AshareQuery,
    AshareTradingCalendar,
    build_dataset_bundle_manifest,
    fetch_actions,
    fetch_bars,
    screen_bar_frames,
    serialize_corporate_actions,
)
from . import sources as source_registry
from .sources import (
    AUTO_SOURCE,
    DATA_ENVIRONMENT,
    AshareProviderError,
    describe_chain,
    provider_status,
)

_FETCH_SOURCE_CHOICES = (AUTO_SOURCE, *source_registry.source_names())


def _data_source(value: str | None) -> str | None:
    if value is None:
        return None
    normalized = value.strip().lower().replace("-", "_")
    if normalized == AUTO_SOURCE:
        return AUTO_SOURCE
    try:
        source_registry.source(normalized)
    except AshareProviderError as error:
        raise argparse.ArgumentTypeError(str(error)) from error
    return normalized


def _config_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--project", type=Path, help="读取项目清单的 data 配置")
    parser.add_argument("--cache-dir", type=Path, help="覆盖 QIANXING_DATA_CACHE")
    parser.add_argument("--retries", type=int, help="网络类失败重试次数（0..9，默认 2）")
    parser.add_argument("--source", type=_data_source, help="数据源名或 auto（缺省 sample，不自动联网）")
    parser.add_argument("--provider", type=_data_source, help="--source 的旧名（二者等价）")


def _cache_arguments(parser: argparse.ArgumentParser) -> None:
    _config_arguments(parser)
    parser.add_argument("--offline", action="store_true", help="只读内容寻址缓存，不触网")
    parser.add_argument("--refresh", action="store_true", help="忽略缓存并强制重取")
    parser.add_argument("--no-cache", action="store_true", help="禁用缓存读取和写入")


def _source_argument(args: argparse.Namespace) -> str | None:
    if args.source and args.provider and args.source != args.provider:
        raise AshareProviderError(
            f"--source {args.source!r} 与 --provider {args.provider!r} 冲突",
            category="invalid-request",
            suggestion="只保留一个；--provider 是 --source 的旧名",
        )
    return args.source or args.provider


def _data_config(args: argparse.Namespace) -> dict[str, Any]:
    explicit = _source_argument(args)
    if getattr(args, "offline", False) and (
        getattr(args, "refresh", False) or getattr(args, "no_cache", False)
    ):
        raise AshareProviderError(
            "--offline cannot be used with --refresh or --no-cache", category="invalid-request"
        )
    root, data = None, None
    if args.project is not None:
        from qianxing.strategy_project import load_strategy_project

        root, project = load_strategy_project(args.project)
        data = project.get("data")
    return source_registry.resolve_data_config(
        explicit=explicit, manifest=data, project_root=root,
        cache_directory=args.cache_dir, retries=args.retries,
    )


def build_parser(prog: str = "qianxing data") -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog=prog, description="选源、取数、登记血缘并落盘")
    sub = parser.add_subparsers(dest="command", required=True)

    listing = sub.add_parser("sources", help="打印数据源注册表与能力矩阵")
    listing.add_argument("--json", action="store_true", help="输出机器可读 JSON")
    doctor = sub.add_parser("doctor", help="打印本机每个数据源的就绪状态")
    doctor.add_argument("--json", action="store_true", help="输出机器可读 JSON")
    _config_arguments(doctor)
    environment = sub.add_parser("env", help="打印参与取数决策的环境变量表")
    environment.add_argument("--json", action="store_true")

    fetch = sub.add_parser("fetch", help="获取并标准化 A 股 K 线")
    fetch.add_argument("--code", action="append", required=True, help="标的代码，可重复")
    fetch.add_argument("--start", required=True)
    fetch.add_argument("--end", required=True)
    fetch.add_argument("--frequency", default="daily")
    fetch.add_argument("--adjustment", default="none", choices=("none", "qfq", "hfq"))
    fetch.add_argument("--output", type=Path, help="单标的的输出文件，或多标的的输出目录")
    fetch.add_argument("--manifest", type=Path, help="单标的的清单输出路径")
    fetch.add_argument("--bundle", type=Path, help="写入数据集包目录（每标的一个子目录）")
    fetch.add_argument("--bundle-id", help="数据集包 ID（默认按标的与频率生成）")
    fetch.add_argument("--bundle-version", default="1")
    fetch.add_argument("--allow-partial", action="store_true", help="部分标的失败时仍落盘成功的部分")
    _cache_arguments(fetch)

    actions = sub.add_parser("actions", help="获取并标准化 A 股公司行为")
    actions.add_argument("--code", required=True)
    actions.add_argument("--start", required=True)
    actions.add_argument("--end", required=True)
    actions.add_argument("--as-of", help="PIT 截止时间；只保留已公开可见事件")
    actions.add_argument("--output", type=Path, required=True)
    actions.add_argument("--manifest", type=Path)
    _cache_arguments(actions)

    bundle = sub.add_parser("bundle", help="将 A 股组件 manifest 绑定为 DatasetBundleManifest")
    bundle.add_argument("--bundle-id", required=True)
    bundle.add_argument("--version", required=True)
    bundle.add_argument("--source", required=True)
    bundle.add_argument("--bars-manifest", required=True, type=Path)
    bundle.add_argument("--bars-frame", type=Path, help="可选 BarFrame；提供后使用 Rust qx-data fingerprint")
    bundle.add_argument("--actions-manifest", type=Path)
    bundle.add_argument("--calendar", type=Path)
    bundle.add_argument("--output", required=True, type=Path)

    screen = sub.add_parser("screen", help="对目录中的 BarFrame 做快速初筛")
    screen.add_argument("--bars-dir", required=True, type=Path)
    screen.add_argument("--output", required=True, type=Path)
    screen.add_argument("--min-return-bps", type=int, default=-10_000)
    screen.add_argument("--min-avg-volume-raw", type=int, default=0)
    screen.add_argument("--limit", type=int)
    screen.add_argument("--backtest-manifest", type=Path, help="可选：按筛选结果生成 fast-backtest manifest")
    screen.add_argument("--runtime", type=Path, help="manifest 中每个 job 使用的 runtime")
    screen.add_argument("--market-spec", type=Path, help="manifest 中每个 job 使用的 market spec")

    install = sub.add_parser("install", help="校验数据集包并落入项目数据目录（默认不联网）")
    install.add_argument("source_bundle", nargs="?", help="待安装的数据集包目录")
    install.add_argument("--sample", action="store_true", help="生成并安装内置合成样例包")
    install.add_argument("--target", required=True, type=Path, help="项目内数据目录")
    install.add_argument("--force", action="store_true", help="目标目录已有不同内容时覆盖")
    install.add_argument("--json", action="store_true")
    return parser


def _print(document: Any) -> None:
    print(json.dumps(document, ensure_ascii=False))


def _fail(error: AshareProviderError) -> int:
    print(error.to_json(), file=sys.stderr)
    return error.exit_code


def _sources(console: bool) -> int:
    rows = [entry.capability_row() for entry in source_registry.registry()]
    if not console:
        _print(rows)
        return 0
    for row in rows:
        extra = row["extra"] or "-"
        print(
            "{name:<10} {markets:<24} {freq:<28} adj={adj:<14} actions={actions:<5} "
            "calendar={calendar:<5} creds={creds:<9} extra={extra:<18} {license}".format(
                name=row["name"],
                markets=",".join(row["markets"]),
                freq=",".join(row["frequencies"]),
                adj=",".join(row["adjustments"]) or "-",
                actions=str(row["corporate_actions"]).lower(),
                calendar=str(row["trading_calendar"]).lower(),
                creds=row["credentials"],
                extra=extra,
                license=row["license"],
            )
        )
    return 0


def _doctor_entries(names: Sequence[str]) -> list[dict[str, Any]]:
    rows = []
    for name in names:
        row = provider_status(name)
        rows.append(row)
    return rows


def _doctor(args: argparse.Namespace, console: bool) -> int:
    explanation: dict[str, Any] = {}
    try:
        config = _data_config(args)
    except AshareProviderError as error:
        if error.category != "missing-extra":
            raise
        # auto has no installed candidate; report each real candidate, not 'auto'.
        rows = _doctor_entries(source_registry.AUTO_ORDER)
        resolution = {"data.source": None, "data.source_origin": "unavailable",
                      "data.source_requested": AUTO_SOURCE, "available": False,
                      "reason": str(error), "suggestion": error.suggestion}
    else:
        selected = config["source"]
        names = (source_registry.AUTO_ORDER if selected.requested.strip().lower() == AUTO_SOURCE
                 else [selected.name] if args.source or args.provider
                 else source_registry.source_names())
        rows = _doctor_entries(names)
        row = next(row for row in rows if row["name"] == selected.name)
        available = row["builtin"] or row["ready"]
        resolution = {**describe_chain(selected), "available": available,
                      "candidates": list(selected.candidates)}
        if not available:
            resolution.update(reason=f"{selected.name} is not ready for data fetch",
                              suggestion=row["action"] or row["description"])
        explanation = config["explanation"]
    document = {"sources": rows, "resolution": resolution, "explanation": explanation}
    if not console:
        _print(document)
        return 0
    for row in rows:
        if row["builtin"]:
            state = "builtin"
        else:
            state = "installed" if row["installed"] else "not-installed"
        version = row["version"] or "-"
        action = row["action"] or ""
        print(f"{row['name']:<10} {state:<14} version={version:<12} {action}".rstrip())
    print(f"data.source={resolution.get('data.source')} origin={resolution.get('data.source_origin')}")
    if resolution.get("reason"):
        print(f"  {resolution['reason']}")
    if resolution.get("suggestion"):
        print(f"  hint: {resolution['suggestion']}")
    return 0


def _env(console: bool) -> int:
    rows = [
        {"env": name, "key": key, "description": description}
        for name, key, description in DATA_ENVIRONMENT
    ]
    if not console:
        _print(rows)
        return 0
    for row in rows:
        print(f"{row['env']:<24} {row['key']:<20} {row['description']}")
    return 0


def _provenance(result: Any, entry: Any) -> dict[str, Any]:
    manifest = result.manifest
    return {
        "is_sample": False,
        "source": manifest.provider,
        "source_kind": "provider",
        "provider": manifest.provider,
        "provider_version": manifest.provider_version,
        "source_hash": manifest.source_hash,
        "received_at": manifest.received_at,
        "frequency": manifest.frequency,
        "adjustment": manifest.adjustment,
        "start": manifest.start,
        "end": manifest.end,
        "cached": result.cached,
        "license": entry.license,
    }


def _write_bundle(result: Any, target: Path, args: argparse.Namespace, entry: Any) -> dict[str, Any]:
    from qianxing import dataset_bundle

    frame = result.frame
    context = dataset_bundle.source_context(
        frame.instrument,
        frequency=result.manifest.frequency,
        adjustment=result.manifest.adjustment,
        gaps=dataset_bundle.frame_gaps(frame, result.manifest.frequency),
    )
    dataset_id = args.bundle_id or f"ashare-{frame.instrument}-{result.manifest.frequency}"
    return dataset_bundle.write_dataset_bundle(
        target,
        frame,
        context,
        _provenance(result, entry),
        dataset_id=dataset_id,
        version=args.bundle_version,
    )


def _replace_bytes(path: Path, payload: bytes) -> None:
    import tempfile

    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(dir=path.parent, prefix=f".{path.name}.", delete=False) as stream:
            temporary = Path(stream.name)
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def _publish_files(files: list[tuple[Path, bytes]]) -> None:
    """Preflight all destinations, and restore replaced files on a write failure."""
    previous: dict[Path, bytes | None] = {}
    for destination, _payload in files:
        path = destination.resolve()
        if path in previous:
            raise ValueError(f"duplicate output destination: {destination}")
        if path.exists() and not path.is_file():
            raise ValueError(f"output destination is not a file: {destination}")
        for parent in path.parents:
            if parent.exists() and not parent.is_dir():
                raise ValueError(f"output parent is not a directory: {parent}")
        previous[path] = path.read_bytes() if path.exists() else None
    written: list[Path] = []
    try:
        for destination, payload in files:
            path = destination.resolve()
            path.parent.mkdir(parents=True, exist_ok=True)
            _replace_bytes(path, payload)
            written.append(path)
    except OSError:
        for path in reversed(written):
            original = previous[path]
            if original is None:
                path.unlink(missing_ok=True)
            else:
                _replace_bytes(path, original)
        raise


def _fetch(args: argparse.Namespace) -> int:
    # 先校验落点，避免取完才报"无处落地"。
    if args.output is None and args.bundle is None:
        raise AshareProviderError(
            "fetch 需要 --output 或 --bundle，否则取到的数据无处落地",
            category="invalid-request",
            suggestion="加 --output <文件|目录> 或 --bundle <项目数据目录>",
        )
    config = _data_config(args)
    if len(args.code) > 1 and args.manifest is not None:
        raise AshareProviderError("--manifest requires a single --code", category="invalid-request")
    import tempfile

    from . import cache

    with tempfile.TemporaryDirectory(prefix="qianxing-fetch-cache-") as temporary:
        staging = Path(temporary)
        cache_files: list[tuple[Path, bytes]] = []

        def stage_cache(root: Path, *arguments: Any, **options: Any) -> Path:
            # Reads still use the real cache. Only writes are redirected, using
            # the core record encoder rather than duplicating its layout/digests.
            entry = cache.write_cached(staging, *arguments, **options)
            cache_files.extend((root / path.relative_to(staging), path.read_bytes())
                               for path in entry.iterdir())
            return root / entry.relative_to(staging)

        return _fetch_batch(args, config, stage_cache, cache_files)


def _fetch_batch(
    args: argparse.Namespace, config: dict[str, Any], cache_writer: Any,
    cache_files: list[tuple[Path, bytes]],
) -> int:
    source = config["source"]
    entry = source_registry.source(source.name)
    failures: list[tuple[str, AshareProviderError]] = []
    results: list[tuple[str, Any]] = []
    for code in args.code:
        try:
            result = fetch_bars(
                AshareQuery(code, args.start, args.end, args.frequency, args.adjustment),
                source_name=source,
                offline=args.offline,
                refresh=args.refresh,
                use_cache=not args.no_cache,
                cache_directory=config["cache_directory"],
                retries=config["retries"],
                _cache_writer=cache_writer,
            )
        except (AshareProviderError, ValueError) as error:
            failure = error if isinstance(error, AshareProviderError) else AshareProviderError(
                str(error), category="invalid-request", source=source.name
            )
            failures.append((code, failure))
            continue
        results.append((code, result))
    if not results:
        # 全部失败时保留原始 category 与安装建议，而不是包成笼统的 partial。
        raise failures[0][1]
    if failures and not args.allow_partial:
        raise AshareProviderError(
            f"{len(failures)}/{len(args.code)} 个标的取数失败，整批不落盘",
            category="partial",
            suggestion="修正失败的标的，或加 --allow-partial 只落盘成功的部分",
        )
    failure_rows = [
        {"instrument": code, **json.loads(error.to_json())} for code, error in failures
    ]
    written: list[dict[str, Any]] = []
    files: list[tuple[Path, bytes]] = []
    multiple = len(args.code) > 1
    for code, result in results:
        record: dict[str, Any] = {
            "instrument": result.frame.instrument,
            "bars": len(result.frame.ts),
            "source": result.source,
            "cached": result.cached,
            "source_hash": result.manifest.source_hash,
        }
        if args.bundle is not None:
            import tempfile

            target = Path(args.bundle) / result.frame.instrument if multiple else Path(args.bundle)
            record["bundle"] = str(target)
            record["snapshot"] = str(target / "snapshot.json")
            # Native validation/publication must finish for every symbol before
            # the first user-visible output is replaced.
            with tempfile.TemporaryDirectory(prefix="qianxing-fetch-") as temporary:
                staging = Path(temporary)
                _write_bundle(result, staging, args, entry)
                files.extend((target / path.name, path.read_bytes()) for path in staging.iterdir())
        if args.output is not None:
            if multiple or (args.output.exists() and args.output.is_dir()):
                directory = Path(args.output)
                bars_path = directory / f"{result.frame.instrument}.bars.json"
                manifest_path = directory / f"{result.frame.instrument}.manifest.json"
            else:
                bars_path = Path(args.output)
                manifest_path = args.manifest or bars_path.with_suffix(".manifest.json")
            files.extend(((bars_path, result.bars_json.encode("utf-8")),
                          (manifest_path, result.manifest_json.encode("utf-8"))))
            record["output"] = str(bars_path)
            record["manifest"] = str(manifest_path)
        written.append(record)
    _publish_files([*files, *cache_files])
    _print(
        {
            "status": "partial" if failures else "ok",
            "source": results[0][1].source,
            "explanation": config["explanation"],
            "results": written,
            "failures": failure_rows,
        }
    )
    return 0


def _actions(args: argparse.Namespace) -> int:
    config = _data_config(args)
    query = AshareActionQuery(args.code, args.start, args.end, args.as_of)
    actions, manifest, cached, _cache_path = fetch_actions(
        query,
        source_name=config["source"],
        offline=args.offline,
        refresh=args.refresh,
        use_cache=not args.no_cache,
        cache_directory=config["cache_directory"],
        retries=config["retries"],
    )
    # v1 信封：顶层携带 schema_version/source/as_of，Rust 侧据此启用严格模式
    # 并按 published_at <= as_of 复核 PIT 可见性；actions 行本身保持兼容。
    payload = serialize_corporate_actions(
        actions, source=manifest.provider, instrument=manifest.instrument, as_of=query.as_of,
    )
    manifest_path = args.manifest or args.output.with_suffix(".manifest.json")
    _publish_files([(args.output, payload.encode("utf-8")),
                    (manifest_path, manifest.to_json().encode("utf-8"))])
    _print(
        {
            "instrument": manifest.instrument,
            "actions": len(actions),
            "cached": cached,
            "explanation": config["explanation"],
            "output": str(args.output),
            "manifest": str(manifest_path),
        }
    )
    return 0


def _bundle(args: argparse.Namespace) -> int:
    bars_manifest = AshareManifest.from_json(args.bars_manifest.read_text(encoding="utf-8"))
    actions_manifest = None
    if args.actions_manifest:
        actions_manifest = AshareActionManifest.from_json(
            args.actions_manifest.read_text(encoding="utf-8")
        )
    calendar = None
    if args.calendar:
        calendar = AshareTradingCalendar.from_json(args.calendar.read_text(encoding="utf-8"))
    bars_frame = None
    if args.bars_frame:
        bars_frame = BarFrame.from_json(args.bars_frame.read_text(encoding="utf-8"))
    bundle = build_dataset_bundle_manifest(
        args.bundle_id,
        args.version,
        args.source,
        bars_manifest,
        actions_manifest,
        calendar,
        bars_frame,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(bundle, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n"
    )
    _print(
        {
            "bundle_id": args.bundle_id,
            "components": sorted(bundle["components"]),
            "output": str(args.output),
        }
    )
    return 0


def _screen(args: argparse.Namespace) -> int:
    args.output.parent.mkdir(parents=True, exist_ok=True)
    paths = []
    skipped = 0
    required = {"instrument", "source", "ts", "open_raw", "high_raw", "low_raw", "close_raw", "volume_raw"}
    for path in sorted(args.bars_dir.glob("*.json")):
        if path.name.endswith(".manifest.json"):
            continue
        try:
            value = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            skipped += 1
            continue
        if not isinstance(value, dict) or not required.issubset(value):
            skipped += 1
            continue
        paths.append(path)
    if not paths:
        raise AshareProviderError(
            f"目录里没有可识别的 BarFrame JSON：{args.bars_dir}",
            category="invalid-request",
            suggestion="先运行 qianxing data fetch --output <目录>",
        )
    rows = screen_bar_frames(
        paths,
        min_return_bps=args.min_return_bps,
        min_avg_volume_raw=args.min_avg_volume_raw,
        limit=args.limit,
    )
    args.output.write_text(
        json.dumps(rows, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n"
    )
    manifest_path = None
    if args.backtest_manifest:
        if not args.runtime:
            raise AshareProviderError(
                "--backtest-manifest requires --runtime",
                category="invalid-request",
                suggestion="补 --runtime <runtime.json>",
            )
        manifest_path = args.backtest_manifest
        manifest_path.parent.mkdir(parents=True, exist_ok=True)
        base = manifest_path.parent.resolve()
        runtime = os.path.relpath(args.runtime.resolve(), base)
        jobs = []
        for row in rows:
            bars_path = row.get("bars_path")
            if not bars_path:
                continue
            job = {"runtime": runtime, "bars": os.path.relpath(Path(bars_path).resolve(), base)}
            if args.market_spec:
                job["market_spec"] = os.path.relpath(args.market_spec.resolve(), base)
            jobs.append(job)
        manifest_path.write_text(
            json.dumps({"schema_version": 1, "jobs": jobs}, ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
            newline="\n",
        )
    _print(
        {
            "candidates": len(rows),
            "scanned": len(paths),
            "skipped": skipped,
            "output": str(args.output),
            "backtest_manifest": str(manifest_path) if manifest_path else None,
        }
    )
    return 0


def _install(args: argparse.Namespace) -> int:
    import tempfile

    from qianxing import dataset_bundle, sample_data

    target = Path(args.target)
    if args.sample and args.source_bundle:
        raise AshareProviderError(
            "--sample 与显式数据包目录只能二选一",
            category="invalid-request",
            suggestion="去掉 --sample，或省略数据包目录",
        )
    if not args.sample and not args.source_bundle:
        raise AshareProviderError(
            "install 需要数据包目录或 --sample",
            category="invalid-request",
            suggestion="qianxing data install <bundle> --target data 或 --sample --target data",
        )
    staging: tempfile.TemporaryDirectory[str] | None = None
    try:
        if args.sample:
            staging = tempfile.TemporaryDirectory(prefix="qianxing-sample-")
            source_directory = Path(staging.name)
            sample_data.write_sample_bundle(source_directory)
        else:
            source_directory = Path(args.source_bundle)
        incoming = dataset_bundle.verify_bundle(source_directory)
        if target.exists() and not target.is_dir():
            raise AshareProviderError("--target must be a directory", category="invalid-request")
        if target.exists() and any(target.iterdir()) and not args.force:
            existing = target / dataset_bundle.BUNDLE_FILE
            if not existing.is_file():
                raise AshareProviderError(
                    f"目标目录非空且没有数据集索引：{target}", category="invalid-request",
                    suggestion="换一个 --target，或加 --force 覆盖",
                )
            # Matching indexes alone do not prove an idempotent, intact install.
            current = dataset_bundle.verify_bundle(target)
            if current != incoming:
                raise AshareProviderError(
                    f"目标目录已有不同数据集包：{target}",
                    category="invalid-request",
                    suggestion="换一个 --target，或加 --force 覆盖",
                )
        # The installer writes only indexed files; unrelated files stay untouched.
        result = dataset_bundle.install_bundle(source_directory, target)
    finally:
        if staging is not None:
            staging.cleanup()
    if args.sample:
        result["license"] = sample_data.SAMPLE_LICENSE
    if args.json:
        _print(result)
    else:
        label = "sample" if result["is_sample"] else result.get("source")
        print(f"installed {result['instrument']} rows={result['rows']} source={label} -> {result['target']}")
    return 0


def main(argv: Sequence[str] | None = None, *, prog: str = "qianxing data") -> int:
    parser = build_parser(prog)
    args = parser.parse_args(argv)
    try:
        if args.command == "sources":
            return _sources(not args.json)
        if args.command == "doctor":
            return _doctor(args, not args.json)
        if args.command == "env":
            return _env(not args.json)
        if args.command == "fetch":
            return _fetch(args)
        if args.command == "actions":
            return _actions(args)
        if args.command == "bundle":
            return _bundle(args)
        if args.command == "screen":
            return _screen(args)
        if args.command == "install":
            return _install(args)
        raise AshareProviderError(f"未知子命令 {args.command!r}", category="invalid-request")
    except AshareProviderError as error:
        return _fail(error)
    except (OSError, ValueError) as error:
        failure = AshareProviderError(str(error), category="invalid-request")
        return _fail(failure)


if __name__ == "__main__":  # pragma: no cover - 兼容 python -m qianxing_ashare.cli
    raise SystemExit(main(prog="python -m qianxing_ashare"))
