# Dataset Snapshot v1

`DatasetSnapshotV1` freezes the context needed to interpret market data and to decide whether a requested interval is usable. It wraps the existing `DatasetManifestV2` content identity and adds:

- explicit instrument role, capture time and frequency;
- timestamp meaning (`bar_open`, `bar_close` or `event_time`) and timezone inherited from the manifest;
- price and quantity precision and units;
- actual coverage bounds, calendar identity/version and ordered gap intervals;
- explicit corporate-action policy.

`validate_for_execution` rejects index/unknown/non-tradable roles, unknown corporate-action handling, stale or future-dated captures, intervals outside the frozen coverage, and any requested interval intersecting a declared gap. It also requires the gap list to agree with the manifest's missing-interval count. `validate_for_market_spec` additionally checks the exact instrument and declared role/product compatibility. The snapshot's embedded content hash must be a SHA-256 hex digest.

`validate_for_replay` checks a supplied snapshot against the exact replay instrument, rejects an unknown role and duplicate or out-of-order observations, and still permits research-only roles such as `index`. Multi-market Bar and Depth backtests apply this qualification both when creating evidence and when verifying report readback. The snapshot remains optional for research specifications; when present, its digest is bound into the canonical input identity.

Live Non-Paper Execution requires each market entry to include a `dataset_instrument_role_catalog` conforming to [dataset-instrument-role-catalog-v1.json](../schemas/dataset-instrument-role-catalog-v1.json). The catalog signature covers compact UTF-8 JSON with fields in this order: `schema_version`, `catalog_id`, `supplier_id`, `source_version`, `issued_at_ms`, `valid_from_ms`, `valid_until_ms`, `roles` (keys sorted lexically), and `key_id`. The key ID is the lowercase SHA-256 hex digest of the raw 32-byte public key. Operators pin trusted keys in `QX_DATASET_ROLE_TRUSTED_PUBLIC_KEYS`, a JSON object mapping each key ID to `{ "supplier_id": "…", "public_key_hex": "…" }`. Live startup rejects missing keys/catalogs, invalid signatures, supplier mismatches, stale catalogs and role mismatches. Public keys are trust roots and must be provisioned through the operator's protected deployment configuration; market specs cannot establish their own trust.

The wire contract is [dataset-snapshot-v1.json](../schemas/dataset-snapshot-v1.json); Rust validation lives in `qx-data::DatasetSnapshotV1`. Existing `DatasetManifestV2` remains readable for research and compatibility. It does not silently gain assumed values for missing snapshot metadata. Callers must produce and validate a complete snapshot before using the stricter execution admission API.

To publish a snapshot, create a `DatasetSnapshotContextV1` JSON file matching [dataset-snapshot-context-v1.json](../schemas/dataset-snapshot-context-v1.json), then run:

```sh
qx-cli data-snapshot bars.json dataset-manifest.json snapshot-context.json dataset-snapshot.json
```

The command re-parses the frozen BarFrame or DepthFrame, verifies that its bytes match the manifest SHA-256, checks the manifest identity and exact gap intervals, and publishes the snapshot idempotently. Bar, DepthFrame and multi-market quality-manifest inputs can use this snapshot envelope in place of the bare `DatasetManifestV2`; report recomputation rechecks the snapshot object, digest and exact gaps against the source frame.

Complete local Bar strategy project runs now require a `dataset_snapshot_path`; preflight binds its instrument, fingerprint, and exact source-byte hash to the validated BarFrame and records the snapshot digest in the managed run plan. Provider Bar and Depth backtests require owner-supplied snapshot context, publish immutable manifest/snapshot sidecars, and include the snapshot identity in verified run evidence and report readback. DatasetBundle bars now require a matching snapshot when persisted and when used by strategy backtests; older snapshot-less JSON stays readable but is not execution-eligible. Multi-market Bar/Depth and multi-leg backtests include any supplied snapshot digest in their canonical input identity, and report verification recomputes that identity. Multi-market Bar/Depth also qualify any supplied snapshot for replay at creation and report readback, while retaining manifest-only research inputs. Non-Paper Execution workers require a versioned DatasetInstrumentRoleCatalogV1 whose Ed25519 signature verifies against the operator-pinned `QX_DATASET_ROLE_TRUSTED_PUBLIC_KEYS` JSON map; each trusted key is bound to a supplier ID. The catalog binds canonical instrument IDs to known roles and has a bounded validity interval. A reproducible Binance Vision BTCUSDT Spot daily capture is documented in [Binance historical data acceptance](binance-history-source-acceptance.md); its CC BY-NC-SA 4.0 terms restrict it to non-commercial research and it does not authorize commercial/live use. Licensed HS300 history and calendar acceptance, and supplier catalog/trust-root provisioning, remain external work. Live strategy bars use provider lineage and reconciliation evidence rather than a historical DatasetSnapshotV1.
