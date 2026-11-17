# Strategy refactor acceptance records

`maturity/strategy-run-acceptance-v1.json` assigns one stable `acceptance_id` to each R01–R23 work package and keeps planned scenarios separate from execution evidence. A scenario remains `planned` until its acceptance criteria have been reviewed and satisfied; a successful command exit alone does not mark it accepted.

Run an acceptance command through the recorder so its exact arguments, source revision, source-tree digest and file manifest, declared input digests, platform/Python identity, exit-derived outcome, and combined stdout/stderr artifact are captured:

```text
python tools/record_strategy_acceptance.py --acceptance-id R01-AC01 --input maturity/schema-registry.json -- python tools/check_strategy_refactor_ledger.py
```

Repeat `--input` for every repository file that supplies scenario data. Before starting the command, the recorder snapshots its inputs, `git rev-parse HEAD`, platform/Python identity, and source-tree manifest. It then writes a command log and manifest under `maturity/acceptance-artifacts/` and appends a UUID-keyed record to the catalog under a cross-platform file lock. The source-tree digest covers sorted tracked and non-ignored untracked repository files using each relative path and file digest; it excludes the catalog itself and generated acceptance artifacts to avoid a recursive digest.

`python tools/check_strategy_refactor_ledger.py` checks the catalog shape, scenario IDs, repository-contained artifact paths, recorded input/output SHA-256 values, and recomputes source-tree digests from saved manifests. Legacy V1 records without a source manifest retain their input digest as historical metadata; new records include manifests so inputs can be independently tied to that exact source snapshot. The checker rejects an `accepted` scenario unless a passed run record exists. These records make command evidence reviewable and detect artifact drift; environment-specific acceptance still has to be run in the environment named by the work package.
