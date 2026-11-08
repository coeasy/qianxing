import hashlib
import io
import json
import os
import sys
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from contextlib import redirect_stderr, redirect_stdout
from dataclasses import asdict
from pathlib import Path
from threading import Barrier, Lock
from types import SimpleNamespace
from unittest.mock import Mock, call, patch
from zoneinfo import ZoneInfoNotFoundError

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_ashare import (  # noqa: E402
    AkShareProvider,
    AshareActionManifest,
    AshareActionQuery,
    AshareManifest,
    AsharePITRecord,
    AshareProviderError,
    AshareQuery,
    AshareTradingCalendar,
    AshareCorporateAction,
    BarFrame,
    MootdxProvider,
    build_dataset_bundle_manifest,
    create_provider,
    fetch_actions,
    fetch_bars,
    normalize_bar_rows,
    normalize_corporate_action_rows,
    normalize_instrument,
    reconcile_corporate_actions,
    screen_bar_frames,
    SCALE,
)
import qianxing_ashare  # noqa: E402  只有用例替换模块属性时才需要包对象。
from qianxing_ashare import cache, sources  # noqa: E402
from qianxing_bridge import native  # noqa: E402

#: 与 Rust `crates/qx-cli/tests/unit/calendar_component_fingerprint.rs` 共用的那一对夹具。
CALENDAR_FIXTURES = Path(__file__).resolve().parent / "fixtures"


class _FakeAkShare:
    __version__ = "test"

    @staticmethod
    def stock_zh_a_hist(**kwargs):
        assert kwargs["symbol"] == "000001"
        return [
            {"日期": "2024-01-03", "开盘": "10", "最高": "11", "最低": "9.5", "收盘": "10.5", "成交量": "100"},
            {"日期": "2024-01-02", "开盘": "9", "最高": "10", "最低": "8.5", "收盘": "9.5", "成交量": "90"},
            # 同一时间重复返回时，保留最后一条，避免回测出现重复 bar。
            {"日期": "2024-01-03", "开盘": "10.1", "最高": "11.1", "最低": "9.6", "收盘": "10.6", "成交量": "101"},
        ]

    @staticmethod
    def stock_dividend_cninfo(**kwargs):
        assert kwargs["symbol"] == "000001"
        return [
            {
                "除权除息日": "2024-06-03",
                "公告日期": "2024-05-20",
                "股权登记日": "2024-05-31",
                "派息": "0.10",
                "送股": "0",
            }
        ]


class AshareTest(unittest.TestCase):
    def test_corporate_action_business_validation_delegates_to_rust(self):
        valid = AshareCorporateAction(
            "000001.SZSE",
            "2024-06-03",
            "2024-06-01T00:00:00+08:00",
            action_type="rights_issue",
            rights_instrument="000001.SZSE-R",
            rights_issue_price_raw=500_000_000,
            rights_issue_ratio_num=1,
        )
        valid.validate()
        invalid = AshareCorporateAction(
            "000001.SZSE",
            "2024-06-03",
            "2024-06-01T00:00:00+08:00",
            action_type="cash_dividend",
            cash_dividend_raw=-1,
        )
        with self.assertRaises(ValueError):
            invalid.validate()

    def test_raw_integer_and_ratio_fields_reject_fractional_values(self):
        self.assertEqual(native.decimal_to_integer_exact("1e3"), 1_000)
        with self.assertRaises(AshareProviderError):
            normalize_corporate_action_rows(
                [
                    {
                        "除权除息日": "2024-06-03",
                        "action_type": "dividend",
                        "cash_dividend_raw": "100000000.5",
                    }
                ],
                code="000001",
                source="test",
            )
        with self.assertRaises(AshareProviderError):
            normalize_corporate_action_rows(
                [
                    {
                        "除权除息日": "2024-06-03",
                        "action_type": "dividend",
                        "share_ratio_num": "1.5",
                        "share_ratio_den": "1",
                    }
                ],
                code="000001",
                source="test",
            )

    def test_instrument_normalization(self):
        self.assertEqual(normalize_instrument("sh.600000"), "600000.SSE")
        self.assertEqual(normalize_instrument("000001"), "000001.SZSE")
        self.assertEqual(normalize_instrument("430047.BJSE"), "430047.BJSE")

    def test_normalize_rows_sorts_deduplicates_and_scales(self):
        frame = normalize_bar_rows(
            [
                {"date": "2024-01-02", "open": 1, "high": 2, "low": 0.9, "close": 1.5, "volume": 10},
                {"date": "2024-01-01", "open": 1, "high": 2, "low": 0.9, "close": 1.2, "volume": 8},
                {"date": "2024-01-02", "open": 1, "high": 2, "low": 0.9, "close": 1.6, "volume": 11},
            ],
            code="sz.000001",
            source="test",
        )
        self.assertEqual(frame.instrument, "000001.SZSE")
        self.assertEqual(len(frame.ts), 2)
        self.assertEqual(frame.close_raw[-1], 1_600_000_000)
        self.assertLess(frame.ts[0], frame.ts[1])
        self.assertIsNotNone(frame.exact_ohlcv)

    def test_normalize_rows_preserves_exact_decimal_ohlcv_beyond_raw_scale(self):
        frame = normalize_bar_rows(
            [
                {
                    "date": "2024-01-02",
                    "open": "0.0000000101",
                    "high": "0.0000000105",
                    "low": "0.0000000095",
                    "close": "0.0000000102",
                    "volume": "0.0000000015",
                }
            ],
            code="sz.000001",
            source="test-exact",
        )

        self.assertEqual(frame.open_raw, (10,))
        self.assertEqual(frame.low_raw, (9,))
        self.assertEqual(frame.volume_raw, (1,))
        self.assertEqual(
            frame.exact_ohlcv,
            (
                {
                    "open": {"coefficient": "101", "scale": 10},
                    "high": {"coefficient": "105", "scale": 10},
                    "low": {"coefficient": "95", "scale": 10},
                    "close": {"coefficient": "102", "scale": 10},
                    "volume": {"coefficient": "15", "scale": 10},
                },
            ),
        )
        encoded = json.loads(frame.to_json())
        self.assertEqual(encoded["schema_version"], 2)
        self.assertEqual(encoded["exact_ohlcv"][0]["close"]["coefficient"], "102")

    def test_normalize_rows_rejects_exact_ohlcv_scale_outside_contract(self):
        with self.assertRaisesRegex(AshareProviderError, "18 decimal places"):
            normalize_bar_rows(
                [
                    {
                        "date": "2024-01-02",
                        "open": "1.0000000000000000001",
                        "high": "2",
                        "low": "1",
                        "close": "1.1",
                        "volume": "1",
                    }
                ],
                code="sz.000001",
                source="test-exact",
            )

    def test_intraday_date_and_time_are_not_collapsed(self):
        frame = normalize_bar_rows(
            [
                {"date": "2024-01-02", "time": "09:30:00", "open": 1, "high": 1, "low": 1, "close": 1, "volume": 1},
                {"date": "2024-01-02", "time": "09:35:00", "open": 1, "high": 1, "low": 1, "close": 1, "volume": 1},
            ],
            code="600000", source="test", frequency="5m",
        )
        self.assertEqual(len(frame.ts), 2)
        self.assertGreater(frame.ts[1] - frame.ts[0], 0)

    def test_provider_and_manifest_are_offline_testable(self):
        query = AshareQuery("000001.SZSE", "20240101", "20240131", adjustment="qfq")
        frame, manifest = AkShareProvider(_FakeAkShare()).fetch(query)
        self.assertEqual(frame.instrument, "000001.SZSE")
        self.assertEqual(manifest.provider, "akshare")
        self.assertEqual(
            manifest.source_hash,
            native.fingerprint_bars(frame.to_json()),
            "A 股清单必须复用 Rust qx-data 的规范 BarFrame 指纹",
        )
        restored = AshareManifest.from_json(manifest.to_json())
        self.assertEqual(restored.source_hash, manifest.source_hash)

    def test_source_resolution_has_four_levels_and_explicit_auto_wins(self):
        env = {sources.DATA_SOURCE_ENV: "baostock"}
        with patch.object(sources, "module_available", side_effect=lambda name: name in {"akshare", "baostock"}), patch(
            "qianxing_ashare.importlib.import_module"
        ) as imports:
            for options, name, origin in (
                ({"explicit": "easy-tdx", "manifest": "mootdx", "environ": env}, "easy_tdx", "flag"),
                ({"manifest": "mootdx", "environ": env}, "mootdx", "manifest"),
                ({"environ": env}, "baostock", "env"),
                ({"environ": {}, "default": "mootdx"}, "mootdx", "default"),
                ({"explicit": "auto", "manifest": "mootdx", "environ": env}, "akshare", "flag"),
                ({"manifest": "auto", "environ": env}, "akshare", "manifest"),
                ({"environ": {sources.DATA_SOURCE_ENV: "auto"}}, "akshare", "env"),
                ({"environ": {}}, "akshare", "default"),
            ):
                with self.subTest(options=options):
                    resolution = sources.resolve_source(**options)
                    self.assertEqual((resolution.name, resolution.origin), (name, origin))
            self.assertEqual(create_provider("auto", environ=env).name, "akshare")
            self.assertEqual(create_provider(environ=env).name, "baostock")
            resolution = qianxing_ashare._resolution_for("auto", env)
            self.assertEqual((resolution.name, resolution.origin, resolution.requested), ("akshare", "flag", "auto"))
            self.assertEqual(resolution.candidates, ("akshare", "baostock"))
            imports.assert_not_called()
        with patch.object(sources, "module_available", return_value=False):
            with self.assertRaises(AshareProviderError) as raised:
                sources.resolve_source(explicit="auto", environ=env)
            self.assertEqual(raised.exception.category, "missing-extra")
            self.assertIn("pip install 'qianxing[", raised.exception.suggestion)
            self.assertEqual(raised.exception.exit_code, sources.DATA_ERROR_EXIT_CODES["missing-extra"])
        with self.assertRaises(AshareProviderError) as raised:
            sources.resolve_source(explicit="unknown", environ=env)
        self.assertEqual(raised.exception.category, "invalid-request")

    def test_fetch_auto_selection_never_fails_over(self):
        for fetch, query, method in (
            (fetch_bars, AshareQuery("000001", "20240101", "20240131"), "fetch"),
            (fetch_actions, AshareActionQuery("000001", "20240101", "20241231"), "fetch_corporate_actions"),
        ):
            with self.subTest(kind=method), tempfile.TemporaryDirectory() as directory:
                provider = Mock()
                failure = AshareProviderError("stop", source="akshare", category="provider")
                getattr(provider, method).side_effect = failure
                with patch.object(sources, "module_available", return_value=True), patch.object(
                    qianxing_ashare, "AkShareProvider", return_value=provider
                ) as first, patch.object(qianxing_ashare, "BaoStockProvider") as fallback:
                    with self.assertRaises(AshareProviderError) as raised:
                        fetch(query, source_name="auto", environ={sources.DATA_SOURCE_ENV: "baostock"},
                              cache_directory=directory, retries=9)
                    self.assertIs(raised.exception, failure)
                    first.assert_called_once()
                    getattr(provider, method).assert_called_once_with(query)
                    fallback.assert_not_called()
                self.assertEqual(list(Path(directory).iterdir()), [])

    def test_fetch_cache_hit_corruption_refresh_and_offline(self):
        for fetch, query, method in (
            (fetch_bars, AshareQuery("000001", "20240101", "20240131"), "fetch"),
            (fetch_actions, AshareActionQuery("000001", "20240101", "20241231", "2024-05-31T23:59:59+08:00"),
             "fetch_corporate_actions"),
        ):
            with self.subTest(kind=method), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / "cache"
                options = {"source_name": "akshare", "cache_directory": root, "environ": {}}
                provider = Mock(wraps=AkShareProvider(_FakeAkShare()))
                with patch.object(qianxing_ashare, "create_provider", return_value=provider) as factory:
                    with self.assertRaises(AshareProviderError) as raised:
                        fetch(query, offline=True, **options)
                    self.assertEqual(raised.exception.category, "cache-miss")
                    self.assertIn("000001.SZSE", str(raised.exception))
                    self.assertFalse(root.exists())
                    factory.assert_not_called()
                    first = fetch(query, **options)
                    for offline in (False, True):
                        hit = fetch(query, offline=offline, **options)
                        if method == "fetch":
                            self.assertFalse(first.cached)
                            self.assertTrue(hit.cached)
                            self.assertEqual(hit.bars_json, first.bars_json)
                            self.assertEqual(hit.manifest_json, first.manifest_json)
                        else:
                            self.assertFalse(first[2])
                            self.assertTrue(hit[2])
                            self.assertEqual(hit[:2], first[:2])
                    factory.assert_called_once()
                    self.assertEqual(getattr(provider, method).call_count, 1)
                    path = (first.cache_path if method == "fetch" else first[3]) / "record.json"
                    for field in ("payload_json", "manifest_json"):
                        record = json.loads(path.read_text(encoding="utf-8"))
                        record[field] += " "  # Still valid JSON, but not the cached bytes.
                        path.write_text(json.dumps(record), encoding="utf-8")
                        before = path.read_bytes()
                        calls = factory.call_count
                        with self.assertRaises(AshareProviderError) as raised:
                            fetch(query, offline=True, **options)
                        self.assertEqual(raised.exception.category, "cache-miss")
                        self.assertEqual(path.read_bytes(), before)
                        self.assertEqual(factory.call_count, calls)
                        fetch(query, **options)  # An online miss repairs the corrupt record.
                        self.assertEqual(factory.call_count, calls + 1)
                    with patch.object(cache, "read_cached") as read:
                        fetch(query, refresh=True, **options)
                        read.assert_not_called()
                    self.assertEqual(getattr(provider, method).call_count, 4)
                    intact = path.read_bytes()
                    getattr(provider, method).side_effect = AshareProviderError("refresh failed")
                    with self.assertRaises(AshareProviderError):
                        fetch(query, refresh=True, **options)
                    self.assertEqual(path.read_bytes(), intact)
                    fetch(query, offline=True, **options)
                    # Even checksum-valid records must satisfy the document schema.
                    record = json.loads(intact)
                    record["manifest_json"] = "{}"
                    record["manifest_sha256"] = hashlib.sha256(b"{}").hexdigest()
                    path.write_text(json.dumps(record), encoding="utf-8")
                    calls = factory.call_count
                    with self.assertRaises(AshareProviderError) as raised:
                        fetch(query, offline=True, **options)
                    self.assertEqual(raised.exception.category, "cache-miss")
                    self.assertEqual(factory.call_count, calls)

    def test_fetch_rejects_conflicting_flags_and_invalid_retries_before_io(self):
        for fetch, query in (
            (fetch_bars, AshareQuery("000001", "20240101", "20240131")),
            (fetch_actions, AshareActionQuery("000001", "20240101", "20241231")),
        ):
            for options in (
                {"offline": True, "refresh": True},
                {"offline": True, "use_cache": False},
                *({"retries": value} for value in (-1, 10, 1.5, "2", True)),
                *({"environ": {sources.DATA_RETRIES_ENV: value}} for value in ("-1", "10", "bad")),
            ):
                with self.subTest(fetch=fetch.__name__, options=options), patch.object(
                    qianxing_ashare, "_resolution_for"
                ) as resolve, patch.object(cache, "read_cached") as read, patch.object(
                    cache, "write_cached"
                ) as write, patch.object(qianxing_ashare, "create_provider") as factory:
                    with self.assertRaises(AshareProviderError) as raised:
                        fetch(query, **options)
                    self.assertEqual(raised.exception.category, "invalid-request")
                    for operation in (resolve, read, write, factory):
                        operation.assert_not_called()

    def test_fetch_retries_are_bounded_for_bars_and_actions(self):
        for fetch, query, method in (
            (fetch_bars, AshareQuery("000001", "20240101", "20240131"), "fetch"),
            (fetch_actions, AshareActionQuery("000001", "20240101", "20241231"), "fetch_corporate_actions"),
        ):
            for limit in (0, 2, 9):
                with self.subTest(fetch=fetch.__name__, limit=limit), tempfile.TemporaryDirectory() as directory:
                    provider = Mock()
                    failure = AshareProviderError("timeout", category="network", retryable=True)
                    getattr(provider, method).side_effect = failure
                    pause = Mock()
                    with patch.object(qianxing_ashare, "create_provider", return_value=provider) as factory:
                        with self.assertRaises(AshareProviderError) as raised:
                            fetch(query, source_name="akshare", cache_directory=directory, retries=limit,
                                  environ={sources.DATA_RETRIES_ENV: "invalid-but-overridden"}, sleep=pause)
                        self.assertIs(raised.exception, failure)
                        factory.assert_called_once()
                    self.assertEqual(getattr(provider, method).call_count, limit + 1)
                    self.assertEqual(pause.call_count, limit)
                    self.assertTrue(all(0 < args.args[0] <= 2 for args in pause.call_args_list))
                    self.assertEqual(list(Path(directory).iterdir()), [])
        # Exercise the default sleeper, which must not resolve to datetime.time.
        operation = Mock(side_effect=[AshareProviderError("timeout", retryable=True), "ok"])
        with patch.object(qianxing_ashare._time, "sleep") as pause:
            self.assertEqual(qianxing_ashare._retry_provider(operation, retries=1, sleep=None), "ok")
            pause.assert_called_once()
        self.assertEqual(qianxing_ashare._retry_limit({}), 2)
        self.assertEqual(qianxing_ashare._retry_limit({sources.DATA_RETRIES_ENV: "9"}), 9)

    def test_cache_record_binds_key_source_kind_and_rejects_corruption(self):
        descriptor = cache.request_descriptor("akshare", instrument="000001.SZSE", start="20240101",
                                              end="20240131", frequency="daily", adjustment="none")
        key = cache.request_key(descriptor)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            entry = cache.write_cached(root, "akshare", key, descriptor, '{"bars":1}', '{"manifest":1}')
            path = entry / "record.json"
            original = path.read_bytes()
            self.assertEqual(cache.read_cached(root, "akshare", key), ('{"bars":1}', '{"manifest":1}'))
            for field, value in (
                ("schema_version", 99), ("request_key", "wrong"), ("payload_sha256", "wrong"),
                ("manifest_sha256", "wrong"), ("payload_json", {}),
                ("request", {**descriptor, "start": "20240102"}),
                ("request", {**descriptor, "source": "mootdx"}),
            ):
                with self.subTest(field=field, value=value):
                    record = json.loads(original)
                    record[field] = value
                    path.write_text(json.dumps(record), encoding="utf-8")
                    self.assertIsNone(cache.read_cached(root, "akshare", key))
            for invalid in (b'{', b'[]', b'\xff'):
                path.write_bytes(invalid)
                self.assertIsNone(cache.read_cached(root, "akshare", key))
            path.write_bytes(original)
            self.assertIsNone(cache.read_cached(root, "akshare", key, kind="actions"))
            for source, other_key in (("mootdx", key), ("akshare", "0" * 64)):
                other = cache.entry_directory(root, source, other_key)
                other.mkdir(parents=True)
                (other / "record.json").write_bytes(original)
                self.assertIsNone(cache.read_cached(root, source, other_key))
            with self.assertRaises(ValueError):
                cache.write_cached(root, "akshare", "wrong", descriptor, "{}", "{}")
            self.assertFalse(cache.entry_directory(root, "akshare", "wrong").exists())
            path.unlink()
            # The old split-file format cannot be accepted without integrity evidence.
            (entry / "bars.json").write_text('{"bars":1}', encoding="utf-8")
            (entry / "manifest.json").write_text('{"manifest":1}', encoding="utf-8")
            self.assertIsNone(cache.read_cached(root, "akshare", key))

    def test_cache_atomic_writers_and_failed_replace_keep_coherent_generations(self):
        descriptor = cache.request_descriptor("akshare", instrument="000001.SZSE", start="20240101",
                                              end="20240131", frequency="daily", adjustment="none")
        key = cache.request_key(descriptor)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def write(generation):
                text = json.dumps({"generation": generation})
                return cache.write_cached(root, "akshare", key, descriptor, text, text)
            entry = write(0)
            original = cache.read_cached(root, "akshare", key)
            replace_file = cache.os.replace
            barrier, lock = Barrier(2), Lock()
            temporary_paths = []
            def replace_together(src, dst):
                with lock:
                    first_attempt = src not in temporary_paths
                    if first_attempt:
                        temporary_paths.append(src)
                if first_attempt:
                    self.assertEqual(cache.read_cached(root, "akshare", key), original)
                    barrier.wait(timeout=10)
                replace_file(src, dst)
            with patch.object(cache.os, "replace", side_effect=replace_together), ThreadPoolExecutor(max_workers=2) as pool:
                list(pool.map(write, (1, 2)))
            self.assertEqual(len(set(temporary_paths)), 2)
            payload, manifest = cache.read_cached(root, "akshare", key)
            self.assertEqual(payload, manifest)
            self.assertIn(json.loads(payload)["generation"], (1, 2))
            self.assertEqual(list(entry.glob("*.tmp")), [])
            before = (entry / "record.json").read_bytes()
            with patch.object(cache.os, "replace", side_effect=OSError("interrupted")):
                with self.assertRaises(OSError):
                    write(3)
            self.assertEqual((entry / "record.json").read_bytes(), before)
            self.assertEqual(list(entry.glob("*.tmp")), [])
            with patch.object(cache.os, "replace", side_effect=PermissionError("busy")) as replace_mock, patch.object(
                cache.time, "sleep"
            ) as pause:
                with self.assertRaises(PermissionError):
                    write(4)
                attempts = 5 if cache.os.name == "nt" else 1
                self.assertEqual(replace_mock.call_count, attempts)
                self.assertEqual(pause.call_count, attempts - 1)
            self.assertEqual((entry / "record.json").read_bytes(), before)
            with patch.object(cache.os, "fsync", side_effect=OSError("flush interrupted")):
                with self.assertRaises(OSError):
                    write(5)
            self.assertEqual((entry / "record.json").read_bytes(), before)
            self.assertEqual(list(entry.glob("*.tmp")), [])

    @staticmethod
    def _mootdx_row(day):
        return {"datetime": f"2024-01-{day:02d} 15:00:00", "open": 10, "high": 11,
                "low": 9, "close": 10.5, "vol": 100}

    def test_mootdx_lazily_imports_quotes_and_normalizes_frame(self):
        rows = [self._mootdx_row(day) for day in (3, 2, 1)]
        frame_like = SimpleNamespace(to_dict=Mock(return_value=rows))
        client = SimpleNamespace(bars=Mock(return_value=frame_like))
        factory = Mock(return_value=client)
        root = SimpleNamespace(__version__="test")
        submodule = SimpleNamespace(Quotes=SimpleNamespace(factory=factory))
        query = AshareQuery("000001", "20240102", "20240103")
        with patch("qianxing_ashare.importlib.import_module", side_effect=[root, submodule]) as imports:
            provider = MootdxProvider()
            imports.assert_not_called()
            frame, manifest = provider.fetch(query)
            imports.assert_has_calls([call("mootdx"), call("mootdx.quotes")])
            self.assertEqual(imports.call_count, 2)
            provider.fetch(query)
            self.assertEqual(imports.call_count, 2)
        factory.assert_called_once_with(market="std")
        client.bars.assert_called_with(symbol="000001", frequency=9, start=0, offset=800)
        frame_like.to_dict.assert_called_with("records")
        self.assertEqual(len(frame.ts), 2)
        self.assertLess(frame.ts[0], frame.ts[1])
        self.assertEqual(frame.volume_raw, (100 * SCALE, 100 * SCALE))
        self.assertEqual((manifest.provider, manifest.provider_version), ("mootdx", "test"))
        self.assertEqual(manifest.source_hash, native.fingerprint_bars(frame.to_json()))
        for module in (submodule, SimpleNamespace(quotes=submodule)):
            with patch("qianxing_ashare.importlib.import_module") as imports:
                MootdxProvider(module=module).fetch(query)
                imports.assert_not_called()
        with patch("qianxing_ashare.importlib.import_module") as imports:
            for frequency, code in qianxing_ashare._MOOTDX_FREQUENCIES.items():
                MootdxProvider(client=client).fetch(AshareQuery("000001", "20240102", "20240103", frequency))
                client.bars.assert_called_with(symbol="000001", frequency=code, start=0, offset=800)
            imports.assert_not_called()

    def test_mootdx_schema_empty_and_version_failures_are_categorized(self):
        query = AshareQuery("000001", "20240102", "20240103")
        good = self._mootdx_row(2)
        for missing in ("datetime", "open", "high", "low", "close", "vol"):
            with self.subTest(missing=missing):
                row = {key: value for key, value in good.items() if key != missing}
                client = SimpleNamespace(bars=Mock(return_value=[row]))
                with self.assertRaisesRegex(AshareProviderError, "missing") as raised:
                    MootdxProvider(client=client).fetch(query)
                self.assertEqual((raised.exception.category, raised.exception.source), ("provider", "mootdx"))
                self.assertFalse(raised.exception.retryable)
        for page in ([], SimpleNamespace(to_dict=Mock(return_value=[]))):
            with self.subTest(page=page):
                with self.assertRaisesRegex(AshareProviderError, "no bars") as raised:
                    MootdxProvider(client=SimpleNamespace(bars=Mock(return_value=page))).fetch(query)
                self.assertEqual(raised.exception.category, "provider")
        for error, category in ((TypeError("bad signature"), "provider"),
                                (TimeoutError("timed out"), "network"), (ValueError("bad value"), "provider")):
            with self.subTest(error=error):
                client = SimpleNamespace(bars=Mock(side_effect=error))
                with self.assertRaises(AshareProviderError) as raised:
                    MootdxProvider(client=client).fetch(query)
                self.assertEqual(raised.exception.category, category)
                self.assertEqual(raised.exception.retryable, category == "network")
                self.assertEqual(raised.exception.source, "mootdx")
                client.bars.assert_called_once()  # No positional retry on TypeError.
        for client in (SimpleNamespace(), SimpleNamespace(k=Mock())):
            with self.assertRaisesRegex(AshareProviderError, "version incompatible"):
                MootdxProvider(client=client).fetch(query)
        for module in (SimpleNamespace(), SimpleNamespace(Quotes=SimpleNamespace(factory=42))):
            with patch("qianxing_ashare.importlib.import_module") as imports:
                with self.assertRaisesRegex(AshareProviderError, "version incompatible"):
                    MootdxProvider(module=module).fetch(query)
                imports.assert_not_called()
        for error, category in ((TypeError("factory signature"), "provider"), (ConnectionError("offline"), "network")):
            with self.subTest(factory_error=error):
                factory = Mock(side_effect=error)
                with self.assertRaises(AshareProviderError) as raised:
                    MootdxProvider(module=SimpleNamespace(Quotes=SimpleNamespace(factory=factory))).fetch(query)
                self.assertEqual(raised.exception.category, category)
                factory.assert_called_once()
        for imports, category in (([ImportError("not installed")], "missing-extra"),
                                  ([SimpleNamespace(), ImportError("missing submodule")], "provider")):
            with patch("qianxing_ashare.importlib.import_module", side_effect=imports):
                with self.assertRaises(AshareProviderError) as raised:
                    MootdxProvider().fetch(query)
                self.assertEqual(raised.exception.category, category)

    def test_mootdx_partial_history_is_not_reported_as_complete(self):
        row = self._mootdx_row
        with patch.object(qianxing_ashare, "_MOOTDX_PAGE", 2), patch.object(qianxing_ashare, "_MOOTDX_MAX_PAGES", 2):
            client = SimpleNamespace(bars=Mock(side_effect=[[row(5), row(4)], [row(3), row(2)]]))
            frame, _ = MootdxProvider(client=client).fetch(AshareQuery("000001", "20240102", "20240105"))
            self.assertEqual(len(frame.ts), 4)
            self.assertEqual(client.bars.call_args_list, [
                call(symbol="000001", frequency=9, start=0, offset=2),
                call(symbol="000001", frequency=9, start=2, offset=2),
            ])
            for pages, start, end in (
                ([[row(4)]], "20240101", "20240104"),
                ([[row(5), row(4)], []], "20240101", "20240105"),
                ([[row(5), row(4)], [row(5), row(4)]], "20240101", "20240105"),
                ([[row(5), row(4)], [row(3), row(2)]], "20240101", "20240105"),
                ([[row(2), row(1)]], "20240101", "20240103"),
            ):
                with self.subTest(pages=pages, start=start, end=end):
                    client = SimpleNamespace(bars=Mock(side_effect=pages))
                    with self.assertRaises(AshareProviderError) as raised:
                        MootdxProvider(client=client).fetch(AshareQuery("000001", start, end))
                    self.assertEqual(raised.exception.category, "partial")
                    self.assertFalse(raised.exception.retryable)
                    self.assertLessEqual(client.bars.call_count, 2)

    def test_mootdx_unsupported_requests_never_load_provider(self):
        with patch("qianxing_ashare.importlib.import_module") as imports:
            for adjustment, frequency, capability, remedy in (
                ("qfq", "daily", "能力位 adjustments=none", "--source akshare"),
                ("hfq", "daily", "能力位 adjustments=none", "--source akshare"),
                ("none", "unsupported", "不支持频率 unsupported", "可用频率："),
            ):
                with self.subTest(adjustment=adjustment, frequency=frequency):
                    with self.assertRaises(AshareProviderError) as raised:
                        MootdxProvider().fetch(AshareQuery("000001", "20240101", "20240131", frequency, adjustment))
                    self.assertEqual(raised.exception.category, "unsupported")
                    self.assertEqual(raised.exception.exit_code,
                                     sources.DATA_ERROR_EXIT_CODES["unsupported"])
                    self.assertIn(capability, str(raised.exception))
                    self.assertIn(remedy, raised.exception.suggestion)
            with self.assertRaises(AshareProviderError) as raised:
                MootdxProvider().fetch_corporate_actions(AshareActionQuery("000001", "20240101", "20240131"))
            self.assertEqual(raised.exception.category, "unsupported")
            self.assertIn("能力位 corporate_actions=false", str(raised.exception))
            self.assertIn("--source baostock", raised.exception.suggestion)
            imports.assert_not_called()
        entry = sources.source("mootdx")
        self.assertEqual(entry.adjustments, ("none",))
        self.assertFalse(entry.corporate_actions)

    def test_corporate_action_normalization_and_akshare_provider(self):
        query = AshareActionQuery("000001", "20240101", "20241231", "2024-05-31T23:59:59+08:00")
        actions, manifest = AkShareProvider(_FakeAkShare()).fetch_corporate_actions(
            query
        )
        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].action_type, "cash_dividend")
        self.assertEqual(actions[0].cash_dividend_raw, 100_000_000)
        self.assertEqual(actions[0].record_date, "2024-05-31")
        payload = json.dumps(
            [asdict(action) for action in actions],
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
        self.assertEqual(manifest.source_hash, hashlib.sha256(payload).hexdigest())
        self.assertIsInstance(AshareActionManifest.from_json(manifest.to_json()), AshareActionManifest)

    def test_bonus_and_transfer_share_ratio_composition_uses_rust_core(self):
        action = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-03",
                    "送股比例": "0.1",
                    "转增比例": "0.2",
                }
            ],
            code="000001",
            source="test",
        )[0]
        self.assertEqual(action.share_ratio_num, 1_320_000_000)
        self.assertEqual(action.share_ratio_den, SCALE)

    def test_corporate_action_ratio_precedence_uses_rust_core(self):
        action = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-03",
                    "action_type": "cash_dividend",
                    "share_ratio_num": "3",
                    "share_ratio_den": "2",
                    "rights_issue_ratio_num": "4",
                    "rights_issue_ratio_den": "7",
                    "conversion_ratio_num": "5",
                    "conversion_ratio_den": "9",
                }
            ],
            code="000001",
            source="test",
        )[0]
        self.assertEqual((action.share_ratio_num, action.share_ratio_den), (3, 2))
        self.assertEqual((action.rights_issue_ratio_num, action.rights_issue_ratio_den), (4, 7))
        self.assertEqual((action.conversion_ratio_num, action.conversion_ratio_den), (5, 9))

    def test_corporate_action_pit_filter_does_not_leak_future_event(self):
        actions, _ = AkShareProvider(_FakeAkShare()).fetch_corporate_actions(
            AshareActionQuery("000001", "20240101", "20241231", "2024-05-19T23:59:59+08:00")
        )
        self.assertEqual(actions, ())

    def test_corporate_action_reconciliation_reports_conflicts(self):
        first = normalize_corporate_action_rows(
            [{"除权除息日": "2024-06-03", "公告日期": "2024-05-20", "派息": "0.10"}],
            code="000001", source="baostock",
        )
        second = normalize_corporate_action_rows(
            [{"除权除息日": "2024-06-03", "公告日期": "2024-05-21", "派息": "0.12"}],
            code="000001", source="akshare",
        )
        actions, conflicts = reconcile_corporate_actions(first, second)
        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].source, "baostock+akshare")
        self.assertEqual(len(conflicts), 1)
        self.assertEqual(conflicts[0].field, "cash_dividend_raw")

    def test_complex_corporate_action_fields_are_normalized_for_rust_ledger(self):
        actions = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-03",
                    "公告日期": "2024-05-20",
                    "action_type": "rights_issue",
                    "配股权代码": "000001.SZSE",
                    "配股价": "5",
                    "配股比例": "0.1",
                    "认购数量": "10",
                }
            ],
            code="000001",
            source="test",
        )
        action = actions[0]
        self.assertEqual(action.rights_instrument, "000001.SZSE")
        self.assertEqual(action.subscription_qty_raw, 10 * 1_000_000_000)
        restored = AshareCorporateAction.from_json(action.to_json())
        self.assertEqual(restored, action)

    def test_capital_change_requires_and_preserves_absolute_issuer_supply(self):
        action = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-03",
                    "公告日期": "2024-05-20",
                    "action_type": "capital_change",
                    "issuer_total_shares_raw": "1000000000000",
                    "issuer_free_float_shares_raw": "700000000000",
                }
            ],
            code="000001",
            source="test",
        )[0]
        self.assertEqual(action.issuer_total_shares_raw, 1_000_000_000_000)
        self.assertEqual(action.issuer_free_float_shares_raw, 700_000_000_000)
        self.assertEqual(AshareCorporateAction.from_json(action.to_json()), action)

        with self.assertRaises(ValueError):
            normalize_corporate_action_rows(
                [
                    {
                        "除权除息日": "2024-06-03",
                        "公告日期": "2024-05-20",
                        "action_type": "capital_change",
                    }
                ],
                code="000001",
                source="test",
            )

        human_units = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-03",
                    "公告日期": "2024-05-20",
                    "action_type": "除权除息",
                    "总股本": "1000",
                    "流通股本": "700",
                }
            ],
            code="000001",
            source="test",
        )[0]
        self.assertEqual(human_units.issuer_total_shares_raw, 1000 * SCALE)
        self.assertEqual(human_units.issuer_free_float_shares_raw, 700 * SCALE)

    def test_rights_expiry_is_a_separate_explicit_corporate_action(self):
        actions = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-10",
                    "公告日期": "2024-05-20",
                    "action_type": "rights_issue_expiry",
                    "配股权代码": "700001.SZSE",
                    "配股失效数量": "10",
                }
            ],
            code="000001",
            source="test",
        )
        self.assertEqual(actions[0].action_type, "rights_issue_expiry")
        self.assertEqual(actions[0].rights_expiry_qty_raw, 10 * 1_000_000_000)

    def test_convertible_bond_lifecycle_fields_are_normalized_and_round_trip(self):
        actions = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-10",
                    "公告日期": "2024-05-20",
                    "action_type": "convertible_bond_interest",
                    "可转债代码": "123001.SZSE",
                    "每张利息": "5",
                    "股权登记日": "2024-06-10",
                    "派息日": "2024-06-20",
                }
            ],
            code="000001",
            source="test",
        )
        interest = actions[0]
        self.assertEqual(interest.action_type, "convertible_bond_interest")
        self.assertEqual(interest.convertible_bond_instrument, "123001.SZSE")
        self.assertEqual(interest.interest_per_bond_raw, 5 * 1_000_000_000)
        self.assertEqual(AshareCorporateAction.from_json(interest.to_json()), interest)

        redemption = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-30",
                    "公告日期": "2024-06-01",
                    "action_type": "可转债赎回",
                    "可转债代码": "123001.SZSE",
                    "赎回数量": "100",
                    "赎回价": "105",
                }
            ],
            code="000001",
            source="test",
        )[0]
        self.assertEqual(redemption.action_type, "convertible_bond_redemption")
        self.assertEqual(redemption.settlement_qty_raw, 100 * 1_000_000_000)
        self.assertEqual(redemption.settlement_price_raw, 105 * 1_000_000_000)

        call = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-07-31",
                    "公告日期": "2024-07-01",
                    "action_type": "可转债强赎",
                    "可转债代码": "123001.SZSE",
                    "赎回价": "106",
                }
            ],
            code="000001",
            source="test",
        )[0]
        self.assertEqual(call.action_type, "convertible_bond_call")
        self.assertEqual(call.settlement_qty_raw, 0)
        self.assertEqual(call.settlement_price_raw, 106 * 1_000_000_000)
        self.assertEqual(AshareCorporateAction.from_json(call.to_json()), call)

    def test_screening_delegates_calculation_to_rust(self):
        first = normalize_bar_rows(
            [{"date": "2024-01-01", "open": 1, "high": 1, "low": 1, "close": 1, "volume": 10},
             {"date": "2024-01-02", "open": 1, "high": 1.2, "low": 1, "close": 1.2, "volume": 20}],
            code="000001", source="test",
        )
        second = normalize_bar_rows(
            [{"date": "2024-01-01", "open": 1, "high": 1, "low": 1, "close": 1, "volume": 10},
             {"date": "2024-01-02", "open": 1, "high": 1.1, "low": 1, "close": 1.1, "volume": 20}],
            code="600000", source="test",
        )
        rust_rows = [{"instrument": "000001.SZSE", "total_return_bps": 2000}]
        with patch(
            "qianxing_bridge.native.app_screen_ashare_bars",
            return_value=json.dumps(rust_rows),
        ) as rust_screen:
            rows = screen_bar_frames([first, second], min_return_bps=500, limit=1)
        self.assertEqual(rows, rust_rows)
        request = json.loads(rust_screen.call_args.args[0])
        self.assertEqual(request["min_return_bps"], 500)
        self.assertEqual(request["limit"], 1)
        self.assertEqual(
            [json.loads(item["bars_json"])["instrument"] for item in request["frames"]],
            ["000001.SZSE", "600000.SSE"],
        )
        self.assertEqual(json.loads(first.to_json())["instrument"], "000001.SZSE")

    def test_calendar_corporate_action_and_pit_are_as_of_bounded(self):
        calendar = AshareTradingCalendar(
            "sse-2024",
            ("2024-01-02", "2024-01-03"),
            (("09:30", "11:30"),),
            source="akshare",
        )
        restored = AshareTradingCalendar.from_json(calendar.to_json())
        self.assertTrue(restored.contains("2024-01-02"))
        self.assertEqual(restored.source, "akshare")
        action = AshareCorporateAction("000001", "2024-01-03", "2024-01-04T09:00:00+08:00", source="test")
        self.assertFalse(action.visible_at("2024-01-03T15:00:00+08:00"))
        self.assertTrue(action.visible_at("2024-01-04T09:00:00+08:00"))
        record = AsharePITRecord("000001", "2023-12-31", "2024-01-05T09:00:00+08:00", {"pe": 10})
        self.assertFalse(record.visible_at("2024-01-04T15:00:00+08:00"))

    def test_calendar_component_digest_is_the_shared_cross_language_pair(self):
        # Rust CLI 在回测启动前对同一份文件重算指纹；两侧比的是夹具旁边那一份摘要，谁都不抄谁。
        for document, digest in (
            ("calendar-component-v1.json", "calendar-component-v1.fingerprint"),
            ("calendar-component-legacy.json", "calendar-component-legacy.fingerprint"),
        ):
            with self.subTest(document=document):
                text = (CALENDAR_FIXTURES / document).read_text(encoding="utf-8")
                pinned = (CALENDAR_FIXTURES / digest).read_text(encoding="utf-8").strip()
                self.assertEqual(
                    AshareTradingCalendar.from_json(text).component_fingerprint(),
                    pinned,
                    f"{document} 的指纹与仓库里钉住的那一格不符：改了指纹口径就得同时重产摘要，"
                    "而 Rust 侧那三条用例会立刻指出对面没跟上",
                )
        text = (CALENDAR_FIXTURES / "calendar-component-v1.json").read_text(encoding="utf-8")
        self.assertEqual(
            AshareTradingCalendar.from_json(text).to_json() + "\n",
            text,
            "夹具必须由写侧原样产出：改了编码就重新产出夹具，不要手抄一份近似形状",
        )

    def test_calendar_model_validation_delegates_to_rust(self):
        AshareTradingCalendar(
            "cn-shanghai",
            ("2024-01-02", "2024-01-03"),
            (("09:30", "11:30"), ("13:00", "15:00")),
        ).validate()
        with self.assertRaisesRegex(ValueError, "trading_days must be strictly sorted"):
            AshareTradingCalendar("cn", ("2024-01-03", "2024-01-02")).validate()
        with self.assertRaisesRegex(ValueError, "trading_days must be strictly sorted"):
            AshareTradingCalendar("cn", ("2024-01-02", "2024-01-02")).validate()
        with self.assertRaisesRegex(ValueError, "calendar session start must be before end"):
            AshareTradingCalendar("cn", ("2024-01-02",), (("15:00", "09:30"),)).validate()

    def test_calendar_component_fingerprint_moves_only_with_canonical_fields(self):
        text = (CALENDAR_FIXTURES / "calendar-component-v1.json").read_text(encoding="utf-8")
        calendar = AshareTradingCalendar.from_json(text)
        base = calendar.component_fingerprint()
        # 顶层新增或改动的非规范字段不进指纹：换 `source` 标签不能让已登记的 bundle 失效。
        self.assertEqual(
            AshareTradingCalendar(
                calendar.calendar_id,
                calendar.trading_days,
                calendar.sessions,
                source="another-provider",
            ).component_fingerprint(),
            base,
        )
        for label, mutated in (
            (
                "calendar_id",
                AshareTradingCalendar(
                    "cn-shanghai-other", calendar.trading_days, calendar.sessions, source=calendar.source
                ),
            ),
            (
                "trading_days",
                AshareTradingCalendar(
                    calendar.calendar_id,
                    calendar.trading_days + ("2024-02-20",),
                    calendar.sessions,
                    source=calendar.source,
                ),
            ),
            (
                "sessions",
                AshareTradingCalendar(
                    calendar.calendar_id, calendar.trading_days, (), source=calendar.source
                ),
            ),
        ):
            with self.subTest(field=label):
                self.assertNotEqual(
                    mutated.component_fingerprint(),
                    base,
                    f"{label} 改了摘要却没变，说明这一列根本没进 canonical 字节",
                )

    def test_python_bundle_manifest_matches_rust_component_schema(self):
        bars = AshareManifest(
            schema_version=1,
            provider="akshare",
            provider_version="test",
            instrument="000001.SZSE",
            frequency="daily",
            adjustment="none",
            start="2024-01-02",
            end="2024-01-03",
            row_count=2,
            source_hash="0123456789abcdef",
            received_at="2024-01-04T09:00:00+08:00",
        )
        actions = AshareActionManifest(
            schema_version=1,
            provider="akshare",
            provider_version="test",
            instrument="000001.SZSE",
            start="2024-01-01",
            end="2024-12-31",
            row_count=1,
            source_hash="fedcba9876543210",
            received_at="2024-01-04T09:00:00+08:00",
        )
        calendar = AshareTradingCalendar(
            "cn-2024", ("2024-01-02", "2024-01-03"), (("09:30", "11:30"),)
        )
        frame = BarFrame(
            "000001.SZSE",
            "akshare",
            (1704159000000, 1704245400000),
            (1_000_000_000, 1_100_000_000),
            (1_200_000_000, 1_300_000_000),
            (900_000_000, 1_000_000_000),
            (1_100_000_000, 1_200_000_000),
            (100, 120),
        )
        bundle = build_dataset_bundle_manifest(
            "ashare.000001",
            "snapshot-v1",
            "akshare+calendar",
            bars,
            actions,
            calendar,
            frame,
        )
        self.assertEqual(bundle["components"]["bars"]["kind"], "bars")
        self.assertEqual(len(bundle["components"]["bars"]["fingerprint"]), 16)
        self.assertEqual(bundle["components"]["corporate_actions"]["row_count"], 1)
        self.assertEqual(bundle["components"]["calendar"]["version"], "cn-2024")
        self.assertLess(
            bundle["components"]["bars"]["start_timestamp"],
            bundle["components"]["bars"]["end_timestamp"],
        )


    def test_naive_dates_are_stamped_at_shanghai_midnight(self):
        #: 时区口径固定为 UTC+08:00；换成 UTC 或本机时区会让跨语言指纹静默漂移。
        frame = normalize_bar_rows(
            [{"date": "2024-01-02", "open": 1, "high": 2, "low": 0.9, "close": 1.5, "volume": 10}],
            code="sz.000001",
            source="test",
        )
        self.assertEqual(frame.ts[0], 1_704_124_800_000)

    def test_missing_tzdata_fails_at_first_use_and_names_the_remedy(self):
        #: Windows 的 zoneinfo 不自带时区库：缺 tzdata 只许让取时间的那一步失败，
        #: 不许让 import qianxing_ashare 整体失败（本模块对外承诺核心包不装数据源依赖）。
        def refuse(_key):
            raise ZoneInfoNotFoundError("No time zone found with key Asia/Shanghai")

        original = qianxing_ashare.ZoneInfo
        qianxing_ashare.ZoneInfo = refuse
        try:
            with self.assertRaises(AshareProviderError) as raised:
                normalize_bar_rows(
                    [
                        {
                            "date": "2024-01-02",
                            "open": 1,
                            "high": 2,
                            "low": 0.9,
                            "close": 1.5,
                            "volume": 10,
                        }
                    ],
                    code="sz.000001",
                    source="test",
                )
        finally:
            qianxing_ashare.ZoneInfo = original
        self.assertIn("pip install tzdata", str(raised.exception))
        self.assertTrue(callable(qianxing_ashare.normalize_bar_rows))


class DataConfigTest(unittest.TestCase):
    def test_config_priority_and_explanation(self):
        env = {sources.DATA_SOURCE_ENV: "mootdx", sources.DATA_CACHE_ENV: "env-cache",
               sources.DATA_RETRIES_ENV: "7"}
        data = {"source": "baostock", "cache_root": "project-cache", "retries": 4}
        project = Path.cwd() / "project"
        for flags, manifest, environment, name, origin, retries, root in (
            ({"explicit": "easy-tdx", "cache_directory": "flag-cache", "retries": 0},
             data, env, "easy_tdx", "flag", 0, Path.cwd() / "flag-cache"),
            ({}, data, env, "baostock", "manifest", 4, project / "project-cache"),
            ({}, {}, env, "mootdx", "env", 7, Path.cwd() / "env-cache"),
            ({}, {}, {}, "sample", "default", 2, cache.cache_root({}).absolute()),
        ):
            with self.subTest(origin=origin), patch.object(sources, "resolve_source", wraps=sources.resolve_source) as resolve:
                result = sources.resolve_data_config(
                    **flags, manifest=manifest, project_root=project, environ=environment,
                )
                resolve.assert_called_once()
                self.assertEqual((result["source"].name, result["source"].origin), (name, origin))
                self.assertEqual((result["cache_directory"], result["retries"]), (root, retries))
                explanation = result["explanation"]
                self.assertEqual(set(explanation), {"data.source", "data.cache_root", "data.retries"})
                for key, value in (("data.source", name), ("data.cache_root", str(root)), ("data.retries", retries)):
                    self.assertEqual(explanation[key]["value"], value)
                    self.assertEqual(explanation[key]["source"], origin)
                json.dumps(explanation)
                if origin == "flag":
                    self.assertEqual(explanation["data.source"]["overridden"], [
                        {"value": "baostock", "source": "manifest"},
                        {"value": "mootdx", "source": "env"},
                        {"value": "sample", "source": "default"},
                    ])
                    self.assertEqual(explanation["data.retries"]["overridden"], [
                        {"value": 4, "source": "manifest"}, {"value": "7", "source": "env"},
                        {"value": 2, "source": "default"},
                    ])
                    self.assertEqual(explanation["data.cache_root"]["overridden"][0],
                                     {"value": "project-cache", "source": "manifest"})

    def test_zero_retries_masks_invalid_lower_values_and_rejects_chosen_invalid(self):
        for value in (True, False, -1, 10, 1.5, "bad", "2"):
            with self.subTest(value=value):
                config = sources.resolve_data_config(retries=0, manifest={"retries": value},
                                                     environ={sources.DATA_RETRIES_ENV: "bad"})
                self.assertEqual(config["retries"], 0)
                self.assertEqual(config["explanation"]["data.retries"]["overridden"][0]["value"], value)
                for options in ({"retries": value}, {"manifest": {"retries": value}}):
                    with self.assertRaises(AshareProviderError):
                        sources.resolve_data_config(**options, environ={})
        config = sources.resolve_data_config(manifest={"retries": 0},
                                             environ={sources.DATA_RETRIES_ENV: "bad"})
        self.assertEqual(config["retries"], 0)
        self.assertEqual(sources.resolve_data_config(environ={sources.DATA_RETRIES_ENV: "0"})["retries"], 0)
        for value in ("bad", "10", "-1", True, False):
            with self.assertRaises(AshareProviderError):
                sources.resolve_data_config(environ={sources.DATA_RETRIES_ENV: value})

    def test_auto_is_resolved_once_and_passive_checks_never_import_providers(self):
        with patch.object(sources, "module_available", side_effect=lambda name: name in {"akshare", "baostock"}), patch.object(
            sources, "resolve_source", wraps=sources.resolve_source
        ) as resolve, patch("importlib.import_module") as imports:
            config = sources.resolve_data_config(explicit="auto", manifest={"source": "mootdx"},
                                                 environ={sources.DATA_SOURCE_ENV: "baostock"})
            resolve.assert_called_once()
            self.assertEqual(config["source"].candidates, ("akshare", "baostock"))
            self.assertEqual(config["explanation"]["data.source"]["value"], "akshare")
            with patch.object(sources.importlib.metadata, "version", side_effect=sources.importlib.metadata.PackageNotFoundError):
                self.assertEqual(sources.provider_version("akshare"), "unknown")
                self.assertIsNone(sources.provider_version("mootdx"))
                self.assertEqual(sources.provider_status("akshare")["version"], "unknown")
            imports.assert_not_called()

    def test_default_cache_uses_platform_cache_root_without_creating_it(self):
        with tempfile.TemporaryDirectory() as directory:
            env = {"XDG_CACHE_HOME": str(Path(directory) / "xdg")}
            config = sources.resolve_data_config(environ=env)
            self.assertEqual(config["cache_directory"], cache.cache_root(env))
            self.assertFalse(config["cache_directory"].exists())

    def test_no_cache_disables_reads_and_writes_for_bars_and_actions(self):
        for fetch, query, method in (
            (fetch_bars, AshareQuery("000001", "20240101", "20240131"), "fetch"),
            (fetch_actions, AshareActionQuery("000001", "20240101", "20241231"), "fetch_corporate_actions"),
        ):
            with self.subTest(method=method), tempfile.TemporaryDirectory() as directory:
                options = {"source_name": "akshare", "cache_directory": directory, "environ": {}}
                provider = Mock(wraps=AkShareProvider(_FakeAkShare()))
                with patch.object(qianxing_ashare, "create_provider", return_value=provider):
                    cached = fetch(query, **options)
                    root = cached.cache_path if method == "fetch" else cached[3]
                    original = (root / "record.json").read_bytes()
                    for refresh in (False, True):
                        with patch.object(cache, "read_cached") as read, patch.object(cache, "write_cached") as write:
                            result = fetch(query, use_cache=False, refresh=refresh, **options)
                            read.assert_not_called()
                            write.assert_not_called()
                            self.assertEqual((result.cached, result.cache_path) if method == "fetch" else result[2:],
                                             (False, None))
                    self.assertEqual(getattr(provider, method).call_count, 3)
                    self.assertEqual((root / "record.json").read_bytes(), original)


class DataCliTest(unittest.TestCase):
    def setUp(self):
        from qianxing_ashare import cli

        self.cli = cli
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        environment = patch.dict(os.environ, {sources.DATA_SOURCE_ENV: "", sources.DATA_CACHE_ENV: "",
                                              sources.DATA_RETRIES_ENV: ""})
        environment.start()
        self.addCleanup(environment.stop)

    def invoke(self, *args):
        stdout, stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            code = self.cli.main([str(arg) for arg in args])
        return code, stdout.getvalue(), stderr.getvalue()

    def bars_args(self, *extra):
        return ("fetch", "--code", "000001", "--start", "20240101", "--end", "20240131",
                "--output", self.root / "out.json", "--cache-dir", self.root / "cache", *extra)

    @staticmethod
    def fake_result(query, **options):
        frame = normalize_bar_rows(_FakeAkShare.stock_zh_a_hist(symbol="000001"), code=query.code,
                                   source="akshare:test")
        manifest = qianxing_ashare._manifest(query, frame, "akshare", "test")
        return qianxing_ashare.FetchResult(frame, manifest, "akshare", False, None,
                                          frame.to_json(), manifest.to_json())

    def test_default_source_is_sample_and_absence_preserves_environment(self):
        from qianxing import strategy_project

        with patch.object(strategy_project, "load_strategy_project") as load, patch.object(
            self.cli, "fetch_bars", side_effect=self.fake_result
        ) as fetch:
            for env, expected, origin in (("", "sample", "default"), ("baostock", "baostock", "env")):
                with patch.dict(os.environ, {sources.DATA_SOURCE_ENV: env}):
                    code, stdout, stderr = self.invoke(*self.bars_args("--no-cache"))
                self.assertEqual(code, 0, stderr)
                selected = fetch.call_args.kwargs["source_name"]
                self.assertEqual((selected.name, selected.origin), (expected, origin))
                self.assertFalse(fetch.call_args.kwargs["use_cache"])
                self.assertEqual(json.loads(stdout)["explanation"]["data.source"]["source"], origin)
            load.assert_not_called()
        # With no mock, sample cannot accidentally become a network provider.
        with patch("qianxing_ashare.importlib.import_module") as imports:
            code, _, stderr = self.invoke(*self.bars_args("--no-cache"))
            self.assertEqual(code, sources.DATA_ERROR_EXIT_CODES["unsupported"])
            self.assertEqual(json.loads(stderr)["source"], "sample")
            imports.assert_not_called()

    def test_source_aliases_conflicts_and_parse_errors(self):
        with patch.object(self.cli, "fetch_bars", side_effect=self.fake_result) as fetch:
            for flags in (("--provider", "easy-tdx"),
                          ("--source", "easy_tdx", "--provider", "easy-tdx")):
                code, _, stderr = self.invoke(*self.bars_args(*flags))
                self.assertEqual(code, 0, stderr)
                self.assertEqual(fetch.call_args.kwargs["source_name"].name, "easy_tdx")
            fetch.reset_mock()
            for command in (self.bars_args(), ("doctor", "--json")):
                code, _, stderr = self.invoke(*command, "--source", "akshare", "--provider", "mootdx")
                self.assertEqual(code, 2)
                self.assertEqual(json.loads(stderr)["category"], "invalid-request")
            fetch.assert_not_called()
        with redirect_stderr(io.StringIO()) as stderr, self.assertRaises(SystemExit) as raised:
            self.cli.main(["doctor", "--source", "does-not-exist"])
        self.assertEqual(raised.exception.code, 2)
        self.assertNotIn("Traceback", stderr.getvalue())

    def test_project_fetch_actions_and_doctor_share_data_section_and_root(self):
        from qianxing import strategy_project

        project = self.root / "project"
        project.mkdir()
        manifest = {"schema_version": 1, "kind": "qianxing-strategy-project", "project_id": "offline",
                    "strategy_file": "unused.py", "input_spec": None,
                    "data": {"source": "akshare", "cache_root": "local-cache", "retries": 0}}
        path = project / strategy_project.PROJECT_FILE
        path.write_text(json.dumps(manifest), encoding="utf-8")
        with patch.object(strategy_project, "load_strategy_project", wraps=strategy_project.load_strategy_project) as load, patch.object(
            sources, "module_available", return_value=False
        ), patch.dict(os.environ, {sources.DATA_SOURCE_ENV: "baostock", sources.DATA_RETRIES_ENV: "7"}):
            # No --cache-dir: the manifest's relative root must be project-relative.
            with patch.object(self.cli, "fetch_bars", side_effect=self.fake_result) as fetch:
                code, _, stderr = self.invoke("fetch", "--code", "000001", "--start", "20240101",
                                              "--end", "20240131", "--output", self.root / "bars.json",
                                              "--project", path, "--no-cache")
                self.assertEqual(code, 0, stderr)
                self.assertEqual(fetch.call_args.kwargs["cache_directory"], project / "local-cache")
                self.assertEqual(fetch.call_args.kwargs["retries"], 0)
                self.assertEqual(fetch.call_args.kwargs["source_name"].origin, "manifest")
            actions_manifest = AshareActionManifest(1, "akshare", "test", "000001.SZSE", "2024-01-01",
                                                    "2024-12-31", 0, "digest", "now")
            with patch.object(self.cli, "fetch_actions", return_value=((), actions_manifest, False, None)) as fetch:
                custom = self.root / "nested" / "metadata" / "actions.json"
                code, _, stderr = self.invoke("actions", "--code", "000001", "--start", "20240101",
                                              "--end", "20241231", "--output", self.root / "actions.json",
                                              "--manifest", custom, "--project", project, "--no-cache")
                self.assertEqual(code, 0, stderr)
                self.assertTrue(custom.is_file())
                self.assertEqual(fetch.call_args.kwargs["cache_directory"], project / "local-cache")
                self.assertFalse(fetch.call_args.kwargs["use_cache"])
            code, stdout, stderr = self.invoke("doctor", "--project", project, "--json")
            self.assertEqual(code, 0, stderr)
            document = json.loads(stdout)
            self.assertEqual(document["explanation"]["data.retries"]["value"], 0)
            self.assertEqual(document["resolution"]["data.source"], "akshare")
            self.assertFalse(document["resolution"]["available"])
            self.assertIn("reason", document["resolution"])
            self.assertEqual(load.call_count, 3)
            with patch.object(self.cli, "fetch_bars", side_effect=self.fake_result) as fetch, patch.dict(
                os.environ, {sources.DATA_RETRIES_ENV: "invalid-but-overridden"}
            ):
                code, stdout, stderr = self.invoke(*self.bars_args(
                    "--project", project, "--provider", "easy-tdx", "--cache-dir", "flag-cache",
                    "--retries", "9", "--no-cache",
                ))
                self.assertEqual(code, 0, stderr)
                self.assertEqual(fetch.call_args.kwargs["source_name"].name, "easy_tdx")
                self.assertEqual(fetch.call_args.kwargs["cache_directory"], Path.cwd() / "flag-cache")
                self.assertEqual(fetch.call_args.kwargs["retries"], 9)
                self.assertEqual(json.loads(stdout)["explanation"]["data.source"]["overridden"][0],
                                 {"value": "akshare", "source": "manifest"})
        self.assertFalse((project / "local-cache").exists())

    def test_offline_no_cache_rejected_before_project_or_fetch_io(self):
        from qianxing import strategy_project

        with patch.object(strategy_project, "load_strategy_project") as load, patch.object(
            sources, "resolve_data_config"
        ) as resolve, patch.object(self.cli, "fetch_bars") as fetch:
            code, _, stderr = self.invoke(*self.bars_args("--offline", "--no-cache", "--project", self.root))
            self.assertEqual(code, 2, stderr)
            for operation in (load, resolve, fetch):
                operation.assert_not_called()

    def test_doctor_auto_reports_real_candidates_without_provider_imports(self):
        for installed in (False, True):
            with self.subTest(installed=installed), patch.object(sources, "module_available", return_value=installed), patch.object(
                sources.importlib.metadata, "version", side_effect=sources.importlib.metadata.PackageNotFoundError
            ), patch("importlib.import_module") as imports:
                code, stdout, stderr = self.invoke("doctor", "--source", "auto", "--json")
                self.assertEqual(code, 0, stderr)
                document = json.loads(stdout)
                self.assertEqual([row["name"] for row in document["sources"]], list(sources.AUTO_ORDER))
                self.assertEqual(document["resolution"]["available"], installed)
                if installed:
                    self.assertEqual(document["resolution"]["data.source"], "akshare")
                    self.assertEqual(document["resolution"]["candidates"], list(sources.AUTO_ORDER))
                imports.assert_not_called()

    def test_batch_resolves_once_and_failed_batch_has_no_outputs(self):
        resolutions = []
        def fetch(query, **options):
            resolutions.append(options["source_name"])
            if query.code == "600000":
                raise AshareProviderError("stop", category="network", source="akshare")
            return self.fake_result(query, **options)
        with patch.object(sources, "module_available", return_value=True), patch.object(
            sources, "resolve_source", wraps=sources.resolve_source
        ) as resolve, patch.object(self.cli, "fetch_bars", side_effect=fetch):
            code, _, stderr = self.invoke(*self.bars_args("--source", "auto", "--code", "600000"))
            self.assertEqual(code, sources.DATA_ERROR_EXIT_CODES["partial"])
            self.assertEqual(json.loads(stderr)["category"], "partial")
            resolve.assert_called_once()
            self.assertIs(resolutions[0], resolutions[1])
            self.assertEqual(resolutions[0].name, "akshare")
            self.assertFalse((self.root / "out.json").exists())
            resolve.reset_mock()
            code, stdout, stderr = self.invoke(*self.bars_args("--source", "auto", "--code", "600000", "--allow-partial"))
            self.assertEqual(code, 0, stderr)
            resolve.assert_called_once()
            self.assertEqual(json.loads(stdout)["status"], "partial")
            self.assertTrue((self.root / "out.json" / "000001.SZSE.bars.json").is_file())

    def test_batch_cache_is_published_only_with_successful_outputs(self):
        provider = Mock()
        def fetch(query):
            if query.code == "600000":
                raise AshareProviderError("failed second symbol", source="akshare")
            result = self.fake_result(query)
            return result.frame, result.manifest
        provider.fetch.side_effect = fetch
        with patch.object(qianxing_ashare, "create_provider", return_value=provider) as factory:
            code, _, stderr = self.invoke(*self.bars_args("--source", "akshare", "--code", "600000"))
            self.assertEqual(code, 5, stderr)
            self.assertFalse((self.root / "cache").exists())
            self.assertFalse((self.root / "out.json").exists())
            code, _, stderr = self.invoke(*self.bars_args("--source", "akshare"))
            self.assertEqual(code, 0, stderr)
            records = list((self.root / "cache").rglob("record.json"))
            self.assertEqual(len(records), 1)
            before = records[0].read_bytes()
            output = (self.root / "out.json").read_bytes()
            code, _, stderr = self.invoke(*self.bars_args("--source", "akshare", "--code", "600000", "--refresh"))
            self.assertEqual(code, 5, stderr)
            self.assertEqual(records[0].read_bytes(), before)
            self.assertEqual((self.root / "out.json").read_bytes(), output)
            factory.reset_mock()
            code, stdout, stderr = self.invoke(*self.bars_args("--source", "akshare", "--offline"))
            self.assertEqual(code, 0, stderr)
            self.assertTrue(json.loads(stdout)["results"][0]["cached"])
            factory.assert_not_called()

    def test_bundle_provenance_excludes_structural_keys_and_verifies(self):
        from qianxing import dataset_bundle

        with patch.object(self.cli, "fetch_bars", side_effect=self.fake_result):
            code, _, stderr = self.invoke(*self.bars_args("--source", "akshare", "--no-cache",
                                                         "--bundle", self.root / "bundle"))
        self.assertEqual(code, 0, stderr)
        bundle = dataset_bundle.verify_bundle(self.root / "bundle")
        self.assertFalse(bundle["is_sample"])
        self.assertEqual(bundle["instrument"], "000001.SZSE")
        self.assertEqual(bundle["provider"], "akshare")

    def test_late_bundle_failure_and_destination_conflict_leave_no_partial_outputs(self):
        with patch.object(self.cli, "fetch_bars", side_effect=self.fake_result), patch.object(
            self.cli, "_write_bundle", side_effect=[{}, ValueError("invalid second bundle")]
        ):
            code, _, stderr = self.invoke(*self.bars_args("--source", "akshare", "--code", "600000",
                                                         "--bundle", self.root / "bundle"))
            self.assertEqual(code, 2, stderr)
            self.assertFalse((self.root / "out.json").exists())
            self.assertFalse((self.root / "bundle").exists())
        collision = self.root / "metadata"
        collision.mkdir()
        with patch.object(self.cli, "fetch_bars", side_effect=self.fake_result):
            code, _, stderr = self.invoke(*self.bars_args("--source", "akshare", "--manifest", collision))
            self.assertEqual(code, 2, stderr)
            self.assertFalse((self.root / "out.json").exists())

    def test_failed_publish_restores_existing_files(self):
        first, second = self.root / "first", self.root / "second"
        first.write_bytes(b"original")
        replace_bytes = self.cli._replace_bytes
        def write(path, content):
            if path == second and content == b"fail":
                raise OSError("write failed")
            return replace_bytes(path, content)
        with patch.object(self.cli, "_replace_bytes", write):
            with self.assertRaises(OSError):
                self.cli._publish_files([(first, b"replacement"), (second, b"fail")])
        self.assertEqual(first.read_bytes(), b"original")
        self.assertFalse(second.exists())

    def test_install_protects_unindexed_and_tampered_targets_and_preserves_extras(self):
        from qianxing import dataset_bundle, sample_data

        incoming, target = self.root / "incoming", self.root / "target"
        sample_data.write_sample_bundle(incoming, rows=4)
        target.mkdir()
        extra = target / "keep.txt"
        extra.write_text("keep", encoding="utf-8")
        code, _, stderr = self.invoke("install", incoming, "--target", target, "--json")
        self.assertEqual(code, 2, stderr)
        self.assertEqual(list(target.iterdir()), [extra])
        code, _, stderr = self.invoke("install", incoming, "--target", target, "--force", "--json")
        self.assertEqual(code, 0, stderr)
        self.assertEqual(extra.read_text(encoding="utf-8"), "keep")
        before = {path.name: path.read_bytes() for path in target.iterdir()}
        code, _, stderr = self.invoke("install", incoming, "--target", target, "--json")
        self.assertEqual(code, 0, stderr)
        self.assertEqual(before, {path.name: path.read_bytes() for path in target.iterdir()})
        different = self.root / "different"
        sample_data.write_sample_bundle(different, rows=4, seed=123)
        code, _, stderr = self.invoke("install", different, "--target", target, "--json")
        self.assertEqual(code, 2, stderr)
        self.assertEqual(before, {path.name: path.read_bytes() for path in target.iterdir()})
        bars = target / dataset_bundle.BARS_FILE
        bars.write_bytes(b"tampered destination")
        code, _, stderr = self.invoke("install", incoming, "--target", target, "--json")
        self.assertEqual(code, 2, stderr)
        self.assertEqual(bars.read_bytes(), b"tampered destination")
        (incoming / dataset_bundle.BARS_FILE).write_bytes(b"tampered incoming")
        code, _, stderr = self.invoke("install", incoming, "--target", target, "--force", "--json")
        self.assertEqual(code, 2, stderr)
        self.assertEqual(bars.read_bytes(), b"tampered destination")

    def test_sample_install_is_idempotent(self):
        target = self.root / "sample"
        code, _, stderr = self.invoke("install", "--sample", "--target", target, "--json")
        self.assertEqual(code, 0, stderr)
        before = {path.name: path.read_bytes() for path in target.iterdir()}
        code, _, stderr = self.invoke("install", "--sample", "--target", target, "--json")
        self.assertEqual(code, 0, stderr)
        self.assertEqual(before, {path.name: path.read_bytes() for path in target.iterdir()})

    def test_sample_install_cleans_staging_on_generation_and_preflight_errors(self):
        from qianxing import sample_data

        for fail_generation in (True, False):
            staging = self.root / ("staging-fail" if fail_generation else "staging-ok")
            staging.mkdir()
            temporary = Mock(name=str(staging))
            temporary.name = str(staging)
            target = self.root / "unindexed"
            target.mkdir(exist_ok=True)
            (target / "keep").write_text("keep", encoding="utf-8")
            def generate(directory):
                if fail_generation:
                    raise ValueError("generation failed")
                return original(directory, rows=4)
            original = sample_data.write_sample_bundle
            with patch("tempfile.TemporaryDirectory", return_value=temporary), patch.object(
                sample_data, "write_sample_bundle", side_effect=generate
            ):
                code, _, stderr = self.invoke("install", "--sample", "--target", target, "--json")
                self.assertEqual(code, 2, stderr)
                temporary.cleanup.assert_called_once()

    def test_data_error_ladder_is_the_only_source_of_data_exit_codes(self):
        import inspect

        ladder = sources.DATA_ERROR_EXIT_CODES
        self.assertEqual(
            ladder,
            {
                "invalid-request": 2,
                "credentials": 3,
                "missing-extra": 3,
                "network": 4,
                "partial": 5,
                "unsupported": 6,
                "cache-miss": 7,
                "provider": 8,
            },
        )
        for category, code in ladder.items():
            self.assertEqual(sources.exit_code_for(category), code, category)
            self.assertEqual(AshareProviderError("x", category=category).exit_code, code, category)
        self.assertEqual(sources.exit_code_for("not-a-category"), ladder["provider"])
        self.assertEqual(AshareProviderError("x").exit_code, ladder["provider"])

        # 结构性判据：category 词汇表从产出方源码里读出，表多一行、少一行、写错一行都会在这里定价。
        declared = {AshareProviderError.category}
        for module in (sources, qianxing_ashare, self.cli):
            for chunk in inspect.getsource(module).split('category="')[1:]:
                declared.add(chunk.split('"', 1)[0])
        self.assertEqual(declared - set(ladder), set())
        # `credentials` 是保留行：错误信封已能表达它，但 token 类源还没接入，没有任何一处抛出。
        self.assertEqual(set(ladder) - declared, {"credentials"})

        expected = "".join((" / ".join("%s %d" % item for item in ladder.items())).split())
        docs_root = Path(__file__).resolve().parents[2] / "docs"
        plan_docs = sorted(docs_root.glob("*易用性对照RQAlpha*方案-2026-10-30.md"))
        self.assertEqual(len(plan_docs), 1, [str(path) for path in plan_docs])
        for document in [docs_root / "first-run.md", *plan_docs]:
            self.assertIn(expected, "".join(document.read_text(encoding="utf-8").replace("`", "").split()),
                          str(document))

        code, _, stderr = self.invoke(*self.bars_args("--source", "akshare", "--provider", "mootdx"))
        envelope = json.loads(stderr)
        self.assertEqual((envelope["category"], code), ("invalid-request", ladder["invalid-request"]))
        with patch("qianxing_ashare.importlib.import_module") as imports:
            code, _, stderr = self.invoke(*self.bars_args("--no-cache"))
        envelope = json.loads(stderr)
        self.assertEqual((envelope["category"], code), ("unsupported", ladder["unsupported"]))
        self.assertEqual(code, sources.exit_code_for(envelope["category"]))
        imports.assert_not_called()


if __name__ == "__main__":
    unittest.main()
