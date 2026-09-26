"""R7 变异取证（第 2 版）：每一颗变异都必须被它自己的判据打死，且按 sha 逐字节还原。

档位：ASSERT-RED（用例红，最强）/ GATE-RED（仅门禁红）/ COMPILE-RED（不算证据）/ SURVIVED（判据是假的）。
锚点按二进制比较；文件若是 CRLF，含 \\n 的锚点会自动换成 \\r\\n 再数一次命中。
"""
import hashlib
import os
import subprocess
import sys

ROOT = r"D:/aex_work/qianxing"
SNAP1 = os.path.join(ROOT, "scratch_r7", "snap1")  # R7-4 删除前的 11 份文件


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def read(path: str) -> bytes:
    with open(path, "rb") as handle:
        return handle.read()


def write(path: str, data: bytes) -> None:
    with open(path, "wb") as handle:
        handle.write(data)


def run(cmd, timeout=2400):
    proc = subprocess.run(
        cmd, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace",
        timeout=timeout, shell=False,
    )
    return proc.returncode, proc.stdout + proc.stderr


STORAGE_OUTBOX = ["cargo", "test", "-p", "qx-storage", "--features", "sqlite",
                  "--test", "outbox_backend_semantics"]
STORAGE_QUEUE = ["cargo", "test", "-p", "qx-storage", "--features", "sqlite",
                 "--test", "queue_backend_semantics"]
PROTOCOL = ["cargo", "test", "-p", "qx-protocol", "--test", "snapshot_single_source"]
SCHEDULER = ["cargo", "test", "-p", "qx-scheduler"]

CASES = [
    {
        "name": "R7-1-M1 file 死信点查 max_by_key→min_by_key（取最旧一次入账）",
        "cmd": STORAGE_OUTBOX,
        "edits": [("crates/qx-storage/src/file/consumers.rs",
                   "max_by_key(|record| record.attempts)",
                   "min_by_key(|record| record.attempts)")],
    },
    {
        "name": "R7-1-M2 sqlite 死信点查 DESC→ASC",
        "cmd": STORAGE_OUTBOX,
        "edits": [("crates/qx-storage/src/sqlite.rs",
                   "ORDER BY attempts DESC LIMIT 1",
                   "ORDER BY attempts ASC LIMIT 1")],
    },
    {
        "name": "R7-2-M3 sqlite outbox 排序退回字典序",
        "cmd": STORAGE_OUTBOX,
        "edits": [("crates/qx-storage/src/sqlite.rs",
                   "ORDER BY CAST(e.created_ts AS INTEGER), CAST(e.sequence AS INTEGER), e.event_id",
                   "ORDER BY e.created_ts, e.sequence, e.event_id")],
    },
    {
        "name": "R7-2-M3b file outbox 排序把 event_id 提到 sequence 之前",
        "cmd": STORAGE_OUTBOX,
        "edits": [("crates/qx-storage/src/file/outbox.rs",
                   "events.sort_by_key(|event| (event.created_ts, event.sequence, event.event_id.clone()));",
                   "events.sort_by_key(|event| (event.created_ts, event.event_id.clone()));")],
    },
    {
        "name": "R7-2-M4 sqlite 控制队列退回字典序",
        "cmd": STORAGE_QUEUE,
        "edits": [("crates/qx-storage/src/sqlite.rs",
                   "ORDER BY CAST(c.enqueued_ts AS INTEGER), CAST(c.command_id AS INTEGER)",
                   "ORDER BY c.enqueued_ts, c.command_id")],
    },
    {
        "name": "R7-2-M4b file 控制队列按 command_id 的字符串序外发",
        "cmd": STORAGE_QUEUE,
        "edits": [("crates/qx-storage/src/lib.rs",
                   "commands.sort_by_key(|queued| (queued.enqueued_ts, queued.command.command_id));",
                   "commands.sort_by_key(|queued| (queued.enqueued_ts, queued.command.command_id.to_string()));")],
    },
    {
        "name": "R7-2-M5b sqlite 作业队列按 run_id 的字符串序外发（真正的定序处）",
        "cmd": STORAGE_QUEUE,
        "edits": [("crates/qx-storage/src/sqlite.rs",
                   "jobs.sort_by_key(|job| (job.run.trading_day.clone(), job.run.run_id));",
                   "jobs.sort_by_key(|job| (job.run.trading_day.clone(), job.run.run_id.to_string()));")],
    },
    {
        "name": "R7-3-M6a validate 漏掉 fills 表",
        "cmd": PROTOCOL,
        "edits": [("crates/qx-protocol/src/lib.rs",
                   '        check_keys(\n            "fill",\n'
                   '            self.fills.iter().map(|(key, row)| (*key, row.fill_id)),\n'
                   '        )?;\n',
                   "")],
    },
    {
        "name": "R7-3-M6b validate 的 orders 半边改问外键 client_order_id",
        "cmd": PROTOCOL,
        "edits": [("crates/qx-protocol/src/lib.rs",
                   '            self.orders.iter().map(|(key, row)| (*key, row.order_id)),',
                   '            self.orders.iter().map(|(key, row)| (*key, row.client_order_id)),')],
    },
    {
        "name": "R7-3-M6c check_keys 的判据反向（key==id 才报错）",
        "cmd": PROTOCOL,
        "edits": [("crates/qx-protocol/src/lib.rs",
                   "find(|(key, id)| key != id)",
                   "find(|(key, id)| key == id)")],
    },
    {
        "name": "R7-4-M7 JobSpec 改 deny_unknown_fields（升级读崩旧状态）",
        "cmd": SCHEDULER,
        "edits": [("crates/qx-scheduler/src/job_spec.rs",
                   "#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]\npub struct JobSpec {",
                   "#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]\n#[serde(deny_unknown_fields)]\npub struct JobSpec {")],
    },
    {
        "name": "R7-4-M8 示例里再抄进一格没人读的键",
        "gate": True,
        "edits": [("deploy/qianxing.scheduler.jobs.smoke.json",
                   '    "depends_on": [],',
                   '    "depends_on": [],\n    "input_refs": ["market:BTCUSDT.BINANCE"],')],
    },
]


def anchored(text: str, old: str):
    """返回 (命中数, 实际锚点)：CRLF 文件里的含 \\n 锚点自动换形。"""
    hits = text.count(old)
    if hits == 0 and "\n" in old:
        crlf = old.replace("\n", "\r\n")
        return text.count(crlf), crlf
    return hits, old


def apply(edits):
    originals, staged = [], {}
    for path, old, new in edits:
        abs_path = os.path.join(ROOT, path.replace("/", os.sep))
        base = staged.get(abs_path, read(abs_path))
        text = base.decode("utf-8")
        hits, anchor = anchored(text, old)
        if hits != 1:
            raise AssertionError(f"锚点命中 {hits} 次（预期 1）：{path} :: {anchor[:70]}")
        staged[abs_path] = text.replace(anchor, new, 1).encode("utf-8")
    for abs_path, data in staged.items():
        originals.append((abs_path, read(abs_path)))
        write(abs_path, data)
    return originals


def restore(originals):
    for abs_path, data in originals:
        write(abs_path, data)
    for abs_path, data in originals:
        if sha(read(abs_path)) != sha(data):
            raise AssertionError(f"还原失败：{abs_path}")


def classify(output: str) -> str:
    if "error[E0" in output or "could not compile" in output:
        return "COMPILE-RED(不算证据)"
    if "test result: FAILED" in output or "panicked at" in output:
        return "ASSERT-RED"
    return "SURVIVED"


def gate_reds(output: str):
    return sorted({line.split(" — ")[0].strip(" ✗") for line in output.splitlines()
                   if line.startswith("  ✗")})


def gate_run():
    rc, out = run([sys.executable, "-X", "utf8", "tools/check_architecture.py"])
    return rc, out


def revert_r74():
    """M9：把 R7-4 整颗回滚（11 份文件回到删除前 + JobSpec 的三格字段回来）。"""
    originals = []
    for rel in os.listdir(SNAP1):
        abs_path = os.path.join(ROOT, rel.replace("__", "/").replace("/", os.sep))
        originals.append((abs_path, read(abs_path)))
        write(abs_path, read(os.path.join(SNAP1, rel)))
    spec = os.path.join(ROOT, "crates", "qx-scheduler", "src", "job_spec.rs")
    originals.append((spec, read(spec)))
    text = read(spec).decode("utf-8")
    for old, new in (
        ("    pub depends_on: Vec<String>,",
         "    pub depends_on: Vec<String>,\n    pub input_refs: Vec<String>,\n    pub output_refs: Vec<String>,"),
        ("    pub idempotency_key: String,",
         "    pub idempotency_key: String,\n    pub permission_scope: String,"),
    ):
        assert text.count(old) == 1, old
        text = text.replace(old, new, 1)
    write(spec, text.encode("utf-8"))
    return originals


def main():
    args = sys.argv[1:]
    only_m9 = "m9" in args
    # 选择器：`python mutate_r7.py R7-1 R7-2` 只跑这两批，`m9` 只跑整颗回滚。
    # 分批跑是因为单批的 cargo 重编译会顶满工具的单次调用上限，而变异套件不能后台跑。
    prefixes = [a for a in args if a.startswith("R7-")]
    cases = [c for c in CASES if not prefixes or any(c["name"].startswith(p) for p in prefixes)]
    print("== 基线：门禁红名单 ==")
    rc, out = gate_run()
    baseline = gate_reds(out)
    print("gate rc", rc, "红", len(baseline))
    for name in baseline:
        print("   -", name)

    print("== 基线：受影响用例必须先全绿 ==")
    seen = []
    for case in cases:
        if case.get("gate"):
            continue
        key = " ".join(case["cmd"])
        if key in seen:
            continue
        seen.append(key)
        rc, out = run(case["cmd"])
        results = [line.strip() for line in out.splitlines() if "test result:" in line]
        print("  ", key, "rc", rc, results)
        if rc != 0:
            raise SystemExit("基线不是绿的，先停下")

    for case in cases:
        originals = apply(case["edits"])
        try:
            if case.get("gate"):
                rc, out = gate_run()
                reds = [n for n in gate_reds(out) if n not in baseline]
                hit = [line.strip() for line in out.splitlines() if "没人读的键" in line]
                print(f"[{case['name']}] {'GATE-RED' if reds else 'SURVIVED'} rc={rc} 新增红={reds}")
                for line in hit[:2]:
                    print("      ", line[:220])
            else:
                rc, out = run(case["cmd"])
                failures = [line.strip() for line in out.splitlines() if "test result: FAILED" in line]
                print(f"[{case['name']}] {classify(out)} rc={rc} {failures[:1]}")
        finally:
            restore(originals)
        print("      还原 sha: OK")

    if prefixes and not only_m9:
        print("== 本批未跑 M9（整颗回滚），按选择器跳过 ==")
        return

    print("== R7-4-M9 整颗回滚 R7-4（含字段、构造点与示例） ==")
    originals = revert_r74()
    try:
        rc, out = gate_run()
        reds = [n for n in gate_reds(out) if n not in baseline]
        hit = [line.strip() for line in out.splitlines() if "退役键" in line or "没人读的键" in line]
        print(f"   {'GATE-RED' if reds else 'SURVIVED'} rc={rc} 新增红={reds}")
        for line in hit[:4]:
            print("      ", line[:240])
    finally:
        restore(originals)
    print("      还原 sha: OK")


if __name__ == "__main__":
    main()
