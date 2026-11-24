# Hosted performance baseline — 2026-11-23

This note preserves the performance artifact produced by CI for source commit
`9fa5dab42ef150c0281b3553641ea92d5f19b60b`. The original GitHub artifact is
identified by its ID and SHA-256 in
[`manifest.json`](../maturity/evidence/performance-baseline-2026-11-23/manifest.json);
the raw JSON files beside it are the measurements, not this summary.

## Measured results

The run used a single Ubuntu x86_64 GitHub-hosted runner with an AMD EPYC 9V74,
4 logical CPUs, 2 physical cores, and about 16 GiB RAM.

| Workload | 1,000 events | 8,000 events | 64,000 events |
| --- | ---: | ---: | ---: |
| Full replay median | 0.729 ms | 6.172 ms | 102.961 ms |
| Steady-state pipeline ingest p50 | 1.551 ms | 2.886 ms | 14.871 ms |
| Segmented append, warm store | 1.270 ms | 1.290 ms | 1.427 ms |
| Segmented append, reopened store | 2.985 ms | 15.934 ms | 122.875 ms |

At 1,000 events, incremental refresh measured 0.701 ms versus 6.282 ms for full
replay, and both paths produced equal digests. The 64,000-event samples show a
remaining history-size-dependent cost in steady-state ingestion and a much
larger one-time cost when a segmented store is reopened before append. These
are investigation signals; this note does not claim either path is constant
time or already meets a product target.

The synthetic A-share CLI workload processed 64,000 bars at a p50 of 1.852 s
(34,561 bars/s) with a maximum process RSS of 275,116,032 bytes. The synthetic
Paper lifecycle completed in a median of 710.28 ms and verified one submitted
order, one fill, and four ledger entries per independent root.

## How to use this baseline

- Compare only runs with the same benchmark profile, source fingerprint,
  hardware class, and toolchain; this single hosted sample is not comparable to
  the local Ryzen target or to competitor measurements from different machines.
- Prioritize profiling the history-size-dependent pipeline ingest and the
  reopened segmented append path before changing storage integrity checks.
- Keep full replay as the correctness reference. Any incremental-path change
  must preserve the recorded digest-equivalence contract.
- Do not update the nine profile readiness entries in `maturity/targets.yaml`
  from this bundle. Its machine-readable evidence explicitly says
  `profile_coverage_claim: false` and `profile_target_status_updated: false`.

## Scope limits

The artifact is a reproducible CI baseline, not an apples-to-apples comparison
with RQAlpha, NautilusTrader, or another engine. Synthetic backtest throughput
does not prove market-data coverage, trading-rule completeness, or profitability.
The Paper sample does not exercise a live exchange connection. The full raw
records, including source hashes, commands, repeated samples, and limitations,
are in [`maturity/evidence/performance-baseline-2026-11-23/`](../maturity/evidence/performance-baseline-2026-11-23/).
