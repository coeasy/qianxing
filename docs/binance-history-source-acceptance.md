# Binance Spot Historical Data Acceptance

## Accepted capture

`BTCUSDT` Spot daily klines from Binance Vision were acquired for UTC dates 2017-08-17 through 2026-09-30. The capture contains 3,332 consecutive UTC daily bars with no duplicates or missing intervals. Each monthly ZIP was checked against its companion SHA-256 `.CHECKSUM` file before parsing. The raw archive timestamp convention was verified as milliseconds before 2025-01-01 and microseconds from 2025-01-01; timestamps in the canonical BarFrame and DatasetSnapshot are normalized to milliseconds.

The acquisition and snapshot evidence is recorded in [binance-spot-btcusdt-2017-2026-09.json](../maturity/evidence/binance-spot-btcusdt-2017-2026-09.json). The local, ignored capture is under `target/binance-history-btcusdt-2017-2026-v2/`; it retains the downloaded archive bytes and checksum files for offline inspection. The licensed data is deliberately not included in the repository. Binance Vision's [Dataset Terms](https://github.com/binance/binance-public-data/blob/master/TERMS_AND_CONDITIONS.md) specify CC BY-NC-SA 4.0. Clause 4.1 limits use to its enumerated non-commercial cases, including academic research, non-monetized education, algorithmic backtesting for purely personal non-production research, and non-commercial open-source data-science evaluation. The terms require attribution and the identical license for redistributed derivatives, exclude commercial utilization without a separate written enterprise license, and expressly prohibit live proprietary trading and commercial order generation. This capture is evidence only; each proposed use must fit an enumerated case. Any future redistribution must satisfy the license and attribution conditions, and commercial/live use requires a separate written data license.

## Reproduce and qualify

Use a new output directory; the downloader refuses to overwrite existing captures. The expected-acquisition file pins every official monthly archive SHA-256, so source revisions fail closed:

```powershell
python tools/acquire_binance_history.py `
  --symbol BTCUSDT `
  --start 2017-08-17 `
  --end 2026-09-30 `
  --expected-acquisition maturity/evidence/binance-spot-btcusdt-2017-2026-09.json `
  --output target/binance-history-btcusdt-2017-2026-reproduction
```

The expected evidence also freezes `snapshot_captured_at_ms`; the downloader reuses it so that the DatasetSnapshot digest can be reproduced byte-for-byte. From the repository root, run:

```powershell
qx-cli data-validate target/binance-history-btcusdt-2017-2026-reproduction/bars.json binance-spot-BTCUSDT-1d-20170817-20260930 binance-public-data-spot-monthly-v1 86400000 target/binance-history-btcusdt-2017-2026-reproduction/manifest.json --timezone UTC
qx-cli data-snapshot target/binance-history-btcusdt-2017-2026-reproduction/bars.json target/binance-history-btcusdt-2017-2026-reproduction/manifest.json target/binance-history-btcusdt-2017-2026-reproduction/snapshot-context.json target/binance-history-btcusdt-2017-2026-reproduction/snapshot.json
```

Acceptance requires the reproduced BarFrame hash, row count, coverage, Qianxing snapshot digest, and snapshot file hash to equal the versioned evidence. The initial capture passed and was reproduced byte-for-byte: BarFrame SHA-256 `fa21e9081e498bde9e35ef78b789f41773fde007718758e28926afebb84f061c`, 3,332 rows, zero duplicates/out-of-order/missing intervals, Qianxing snapshot digest `106301d497c484e8b3f3afbd1fa69675d2d9d3149d5db7d7ac72dae16719f74c`, snapshot JSON file SHA-256 `6944d550101ebc20dd706791fee2f2a136e474322d309833687fb362f10a9982`.

## Scope and remaining R15 work

This closes reproducible real-history acceptance for one Binance Spot instrument and daily interval only. It does not establish HS300 historical-data rights or completeness, Binance data rights for commercial/live use, a supplier-issued role catalog, or deployment trust-root provisioning. Those external dependencies remain explicit R15 blockers.
