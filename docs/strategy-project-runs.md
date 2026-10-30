# Project run lifecycle

The Python project CLI records local executions under
`<project>/.qianxing/runs/<project_run_id>/`. The project run ID identifies one
invocation; `engine_run_id` is the separate identity returned by Rust.

```sh
qianxing project doctor ./my-project
qianxing project doctor ./my-project --dynamic --timeout 30
qianxing project run ./my-project
qianxing project runs ./my-project --limit 20
qianxing project status ./my-project PROJECT_RUN_ID
qianxing project logs ./my-project PROJECT_RUN_ID --tail 100
qianxing project artifacts ./my-project PROJECT_RUN_ID
qianxing project cancel ./my-project PROJECT_RUN_ID
```

`run` executes in the foreground and prints its project run ID to stderr. Use a
second terminal for `status`, `logs`, or `cancel`. Stdout retains the existing
engine outcome JSON. Both directory paths and manifest paths are accepted, and
input paths are resolved from the project root, including native preflight
dataset reads. Paths containing spaces are supported.

Complete Bar strategy runs must declare `dataset_snapshot_path` beside each
market's `bars_path`. The snapshot must validate and bind to that exact BarFrame
instrument and content fingerprint. Managed runs freeze both files into the
run input snapshot, and native preflight records the snapshot SHA-256 in each
checked market. Search and diagnose preflights retain their research-only
compatibility behavior.

Complete L1/L2 depth strategy runs must provide a `DatasetSnapshotV1` through
`quality_manifest_path`. The project runner resolves and freezes both that file
and `depth_path`; native preflight verifies the snapshot tier, event-time
semantics, instrument, DepthFrame fingerprint, and exact source-byte SHA-256.
The resulting snapshot digest and depth fingerprint are recorded in the checked
market evidence. The depth engine then independently recomputes the manifest's
quality and gap counts when execution starts.

Each invocation has its own output directory, even when the input uses the same
engine run ID. The original input file is left intact. Before preflight, the
managed runner resolves its recognized local file references from the project
root, snapshots those bytes under `input-files/`, and preflights and executes
the resulting `preflight-input.json`. The engine's effective payload, including
the managed output directory, is kept separately as `execution-input.json`.
The strategy file and its declared/eagerly imported local Python dependencies
are copied under `strategy-bundle/`; preflight and execution both load that
copy. Declare lazy local imports with `STRATEGY_DEPENDENCIES`, using `.py`
paths relative to the strategy file's directory. `artifacts` verifies the
strategy bundle digest, both input documents, every copied strategy source,
and every copied data file. The run receipt records the Python interpreter and
installed distribution name/version inventory, plus loaded runtime module files,
and rechecks these identities during artifact verification. Projects may include
a `requirements.lock` at the project root. Package lines must use exact
`name==version` pins, optionally with pip `--hash=sha256:...`; index URL, extra
index URL, trusted-host, and find-links options are allowed. Includes, markers,
editable installs, and direct URLs are rejected because their effective package
set cannot be proven by this local check. Before project code runs, every pin
must match an installed distribution name and version. When hashes are present,
provide matching `.whl` files under the project-root `wheelhouse/`; the runner
checks complete wheel SHA-256 values against the lock, snapshots matching
wheels, and requires loaded source/native module files for those packages to
match the wheel's `RECORD`. Bytecode and other tracked files are still hashed
and checked against installed `RECORD` entries when available. Managed
runs bind the lock, normalized pin, and wheelhouse digests into the receipt and
preflight plan; report status carries the same identities. `project doctor`
reports lock pins and installed versions without executing strategy code, and
flags a missing required wheelhouse. A missing lock remains an explicit absence.
Lazy local
imports must be listed in `STRATEGY_DEPENDENCIES`; dynamic imports are not
discovered automatically. The managed run also hashes regular source, bytecode,
and native-extension files for non-standard-library modules loaded at preflight
and run completion, binds the preflight digest into the plan and runtime identity,
checks them against the owning distribution's `RECORD` SHA-256 when one is
available, and rechecks those file bytes during artifact verification. A bounded,
run-scoped Python import audit also records module names and origins, including
file-backed modules removed from `sys.modules` before completion. A tracked
`PathFinder` proxy records resolved `ModuleSpec.origin` values because Python's
audit event runs before resolution and often supplies no filename. The proxy
delegates every other `PathFinder` entry point, including `find_distributions`,
so `importlib.metadata` still discovers the installed distribution inventory the
receipt records. Imports from
ZIP, wheel, and egg containers bind the container bytes; ordinary strategy
`.py` files remain covered by `strategy-bundle`. Origins that cannot be
resolved to a file, container, stdlib module, or strategy bundle are retained
as unresolved/opaque observations, and artifact verification reports the
runtime inventory as incomplete. A no-origin audit event with no resolved
`PathFinder` origin and no module present at completion is retained as
`attempt`: the inventory cannot distinguish a rejected optional import from a
non-file module removed before completion. Attempt-only observations do not
alone invalidate the run. This records observed import provenance; it does not
archive arbitrary in-memory importer contents. The lock digest also does not
prove every lock entry was installed from its declared wheel hash.
The process working directory is not changed.

The HS300 evidence runner additionally copies its runner and acceptance helper
into the run's input directory and executes those frozen copies. Its archived
`python-runtime.json` records the interpreter path and executable digest,
Python version/implementation/platform, and both script digests; the canonical
runtime identity digest is also included in `status.json`, the run identity,
and the archive manifest. The helper retains its repository data-root settings
while its executable bytes come from the archive snapshot. This freezes the
scripts and interpreter binary identity, but does not claim a complete OS,
shared-library, or non-file-importer inventory.
`run.json` records the local profile, process ID and available
host/process-start identity, timestamps, validated plan and identity
digests, engine run ID, terminal state, structured errors, and artifact paths and
SHA-256 digests. `events.jsonl` contains Rust lifecycle events. Successful runs
also save `result.json`. Files are replaced atomically after flushing.

While the engine phase runs, project output is tee'd into `stdout.log` and
`stderr.log` next to the receipt; console output is unchanged. Each log stops at
4 MiB, then ends with an explicit truncation marker, and `run.json` records every
log under `output_logs` with its path, byte count, SHA-256, and `truncated` flag.
Structured events stay in `events.jsonl` and are never mixed into these logs. A
second concurrent run in the same process skips capture rather than
interleaving both engines' output.

Terminal failures use the plan §8.3 error envelope: `code`, `category`,
`retryable`, `correlation_id`, `stage`, `project_run_id`, `cause`, and
`remediation`. Envelope values come from the failure itself where one exists —
an SDK `AppError` payload keeps its own category, action code, and retry policy —
and otherwise from the exception type; unclassified exceptions keep their class
name in `code` and are labeled by `type`. Because SDK payloads expose `action` as
a code such as `FIX_INPUT`, remediation falls back to a per-category instruction
rather than echoing that code. The envelope also carries the older
`type`/`message` keys so existing readers keep working. `correlation_id` is the
project run ID for run- and report-stage failures.

`project doctor` is static by default: it reads project/runtime/input metadata,
checks referenced local files, and parses Python syntax without importing the
strategy. `--dynamic` runs strategy preflight in a trusted local Python
subprocess with a 1–300 second timeout. The child runs in a separate process
group on POSIX or a kill-on-close Windows Job Object; timeout and normal return
clean up descendants that remain in that owned group. The strategy still runs
with the local user's permissions and this is not a security sandbox. A child
that deliberately escapes its process group/job is outside the cleanup boundary.
Windows 11 acceptance starts a child process, then verifies that both normal
preflight return and timeout reap the descendant; the run is recorded in
`maturity/evidence/strategy-preflight-process-tree-windows-2026-10-29.json`.
Linux/POSIX process-group behavior has not yet been measured.

Code-executing entry points are explicit: `project doctor --dynamic` imports and
preflights the strategy; `project plan --profile local` imports it to derive its
manifest and validate parameters; and `project run` executes it through the
managed run worker. Outside the project command group, `inspect-strategy`,
`diagnose-strategy`, `run-strategy`, and `search-strategy` also import or execute
the supplied strategy file; `remote-research` submits user strategy code to its
configured remote execution provider. Static `project doctor`, `project show`,
`project plan --profile server`, and the top-level `doctor` command do not import
strategy code.

The server plan includes the Rust-selected host capability catalog and the result
of checking `strategy.required_capabilities`. C ABI export negotiation is marked
deferred until verified library load. Python source-declared requirements are
reported as `not_imported`; the plan keeps trading disabled and lists capability
negotiation among its readiness gates until the worker validates the manifest.

Project-local imports during strategy loading are serialized. The loader captures
modules imported from that strategy's project directory on the strategy module,
then removes those modules from process-wide `sys.modules` and restores the
original import path. Strategy bundle hashing reads this captured module list,
so one project's dependency scan does not include another project's imports.
Imports performed lazily after module loading still require the project directory
to be available to Python's import system; managed runs should declare such source
files in `STRATEGY_DEPENDENCIES`.

States progress from `preparing` to `running`, optionally `cancelling`, then
`succeeded`, `failed`, or `cancelled`. A zero exit from the execution callback
is recorded as success only after the result has a nonempty artifact index and
all indexed artifacts, frozen inputs, strategy files, and recorded runtime
identities reverify. Missing, changed, unreadable, or inconsistent artifacts
turn the run into `failed` with an `ARTIFACT_INTEGRITY_INCOMPLETE` error and
per-artifact checks. Missing or malformed `run.json` is reported as a `corrupt`
status rather than crashing status/list queries. Validation failures also retain a failed
receipt. A cancellation request is a separate idempotent marker, so it cannot
overwrite a terminal receipt. Rust decides whether success or cancellation wins
a completion race. Ctrl+C requests the same cooperative engine cancellation;
cancelled project commands exit 130. A request during preparation is checked
before starting the engine. Cancellation is cooperative: trusted Python setup
code and a callback that never returns cannot be forcibly interrupted by this
command. No operating system kill signal is sent by `project cancel`.

If the owner process no longer exists or its recorded start identity differs,
status reports `interrupted`; inability to observe the process reports
`unknown`. Windows and Linux receipts include a process-start identity; macOS
currently uses `ps lstart` with one-second precision. Platforms where identity
cannot be read may only expose PID liveness. Such a receipt is not automatically
converted to success, and it cannot be resumed as a durable strategy checkpoint.

`artifacts` checks recorded artifact bytes for missing or changed files. It does
not rerun Rust accounting/EventLog verification; use `qianxing verify RESULT_JSON` with the
saved result JSON for the existing Bar replay verifier, or the corresponding
depth/multi-market verifier for that outcome type. `logs` exposes bounded
lifecycle event tails, not captured strategy stdout/stderr or server logs.

The managed path supports the existing single-market Bar, L1/L2, and multi-market
Bar strategy routes through Rust RunHandles. The ordinary `run-strategy`
command retains its synchronous behavior. This local workflow does not start a
server deployment or establish live trading readiness.

Validation includes real native Bar execution and replay verification, repeated
run isolation, artifact mutation detection, native cancellation, project paths
with spaces, and a native server static plan that never imports strategy code.
Focused contract tests also exercise Ctrl+C, preparation cancellation, failures,
event tails, and context restoration. The current input snapshot and process
identity changes have not yet received a new runtime acceptance pass. Native tests use
`QX_TEST_NATIVE_LIBRARY` to select the current build.

## Generate a verified report

Use `qianxing project report PROJECT PROJECT_RUN_ID --cli PATH_TO_QX_CLI` to generate the completed run report. It checks indexed artifact bytes before invoking Rust. The default HTML, `report.log` and `report-status.json` live beside the run receipt; `-o` chooses an HTML path relative to the project root. See [report layout and details](backtest-report-layout.md).

`--format html,xlsx,json` selects one or more outputs. `html` renders the document, `xlsx` writes the workbook, and `json` persists the Rust report projection that `qx-cli report --json` prints to stdout. XLSX and JSON targets take the HTML target's stem, so `-o reports/run.html` also produces `reports/run.xlsx` and `reports/run.json`. Requested formats that resolve to the same path are rejected before Rust starts, and every target is compared with the protected path set (run artifacts, receipts, plan, strategy sources, input snapshots) so a report cannot overwrite evidence. JSON is written through a temporary file and `os.replace`, because the Rust renderer only streams it on stdout. `report-status.json` records `formats`, per-format `path`/`present`/`sha256`/`bytes`, and the HTML digest in `report_sha256`; a successful run also requires every requested artifact to exist and every indexed engine artifact to reverify after the renderer exits. Missing outputs, invalid JSON, an unwritable target, a nonzero exit, or changed source evidence produce a `failed` status with a `reason` and a `REPORT_FAILED` error envelope; the engine run itself keeps its own terminal state.
