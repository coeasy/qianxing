"""R7-7 / R7-8 / R7-9 三颗的变异取证：每一颗都必须被它自己的判据打死，且按 sha 逐字节还原。

档位：ASSERT-RED（用例红，最强）/ GATE-RED（仅门禁红）/ COMPILE-RED（不算证据）/ SURVIVED（判据是假的）。
锚点按二进制比较；命中数不等于预期就停下，不留"看起来改了"的第二种可能。
"""
import hashlib
import json
import os
import subprocess
import sys

ROOT = r"D:/aex_work/qianxing"

STORAGE_LIB = ["cargo", "test", "-p", "qx-storage", "--lib"]
CLI_AUDIT = ["cargo", "test", "-p", "qx-cli", "control_audit_reads_the_store"]


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def read(path: str) -> bytes:
    with open(path, "rb") as handle:
        return handle.read()


def write(path: str, data: bytes) -> None:
    with open(path, "wb") as handle:
        handle.write(data)
        handle.flush()
        os.fsync(handle.fileno())


def run(cmd, timeout=2400):
    proc = subprocess.run(
        cmd, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace",
        timeout=timeout, shell=False,
    )
    return proc.returncode, proc.stdout + proc.stderr


def anchored(text: str, old: str):
    hits = text.count(old)
    if hits == 0 and "\n" in old:
        crlf = old.replace("\n", "\r\n")
        return text.count(crlf), crlf
    return hits, old


def apply(edits):
    """edits: [(rel, old, new, expect_hits)]；expect_hits 默认 1。"""
    staged, originals = {}, []
    for edit in edits:
        rel, old, new = edit[0], edit[1], edit[2]
        expect = edit[3] if len(edit) > 3 else 1
        abs_path = os.path.join(ROOT, rel.replace("/", os.sep))
        base = staged.get(abs_path, read(abs_path))
        text = base.decode("utf-8")
        hits, anchor = anchored(text, old)
        if hits != expect:
            raise AssertionError(f"锚点命中 {hits} 次（预期 {expect}）：{rel} :: {anchor[:70]!r}")
        staged[abs_path] = text.replace(anchor, new).encode("utf-8")
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
    if "error[E0" in output or "could not compile" in output or "error: expected" in output:
        return "COMPILE-RED(不算证据)"
    if "test result: FAILED" in output or "panicked at" in output:
        return "ASSERT-RED"
    return "SURVIVED"


def gate_reds(output: str):
    return sorted({line.split(" — ")[0].strip(" ✗") for line in output.splitlines()
                   if line.startswith("  ✗")})


def gate_run():
    return run([sys.executable, "-X", "utf-8", "tools/check_architecture.py"])


# —— R7-7 M1：把 `Failed` 整颗改名（改名才是这颗缺陷的真实形状）——
FAILED_SITES = [
    "crates/qx-control/src/lib.rs",
    "crates/qx-cli/src/spread.rs",
    "crates/qx-cli/src/tests/live_submit_fail_closed.rs",
    "crates/qx-cli/src/tests/paper_and_strategy_worker.rs",
    "crates/qx-cli/src/venue_runtime/binance_submit.rs",
    "crates/qx-cli/src/venue_runtime/paper_submit.rs",
    "crates/qx-storage/src/tests.rs",
]

API = "crates/qx-api/src/lib.rs"
LOCK = "crates/qx-core/src/file_lock.rs"
AUDIT_CASE = "crates/qx-cli/src/tests/api_control_audit_live.rs"
HASH = "crates/qx-storage/src/lib.rs"

CASES = [
    {
        "name": "R7-7-M1 CommandStatus::Failed 改名成 Errored（全仓 8 处一起改）",
        "cmd": STORAGE_LIB,
        "rename_failed": True,
    },
    {
        "name": "R7-7-M2 只动 serde 那一半（#[serde(rename_all = \"snake_case\")]）",
        "cmd": STORAGE_LIB,
        "edits": [(
            "crates/qx-control/src/lib.rs",
            "#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]\npub enum CommandStatus {",
            "#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]\n"
            "#[serde(rename_all = \"snake_case\")]\npub enum CommandStatus {",
        )],
    },
    {
        "name": "R7-7-M3 把 `{:?}` 换成看着更稳的小写编码（作废存量链的那手法）",
        "cmd": STORAGE_LIB,
        "edits": [(
            HASH,
            'hash.write_text(&format!("{:?}", record.status));',
            'hash.write_text(&format!("{:?}", record.status).to_lowercase());',
        )],
    },
    {
        "name": "R7-8-M4 摘要面退回进程内副本（撤同源，只留流水同源）",
        "cmd": CLI_AUDIT,
        "gate": True,
        "edits": [(
            API,
            "        self.control_plane_last_known().retirement()",
            "        self.state.lock().expect(\"api state mutex poisoned\").control.retirement()",
        )],
    },
    {
        "name": "R7-8-M5 两半各写一遍降级（行为等价，只有判据看得出）",
        "cmd": CLI_AUDIT,
        "gate": True,
        "edits": [(
            API,
            "        self.control_plane_last_known().audit().to_vec()",
            "        self.control_plane().map(|plane| plane.audit().to_vec()).unwrap_or_default()",
        )],
    },
    {
        "name": "R7-8-M6 摘掉用例读摘要的那半（判据第三格有没有牙齿）",
        "cmd": CLI_AUDIT,
        "gate": True,
        "edits": [
            (
                AUDIT_CASE,
                "    let cold_retirement = service.query_port().control_retirement();\n",
                "",
            ),
            (
                AUDIT_CASE,
                "    assert_eq!(\n"
                "        cold_retirement,\n"
                "        retirement,\n"
                "        \"退场累计摘要必须与流水同源：两个读面不能各说一份总量\"\n"
                "    );\n",
                "",
            ),
        ],
    },
    {
        "name": "R7-9-M7 Takeover 支自己再 age_of 一次（年龄两种写法）",
        "cmd": ["cargo", "test", "-p", "qx-core", "file_lock"],
        "gate": True,
        "edits": [(
            LOCK,
            "                        LockDecision::Takeover { age } => {\n"
            "                            last_age = Some(age);",
            "                        LockDecision::Takeover { .. } => {\n"
            "                            last_age = age_of(&path);",
        )],
    },
    {
        "name": "R7-9-M8 Wait 支自己再 age_of 一次",
        "cmd": ["cargo", "test", "-p", "qx-core", "file_lock"],
        "gate": True,
        "edits": [(
            LOCK,
            "                        LockDecision::Wait { age } => {\n"
            "                            last_age = age;",
            "                        LockDecision::Wait { .. } => {\n"
            "                            last_age = age_of(&path);",
        )],
    },
]


def failed_rename_edits():
    edits = []
    for rel in FAILED_SITES:
        text = read(os.path.join(ROOT, rel.replace("/", os.sep))).decode("utf-8")
        for old, new in (
            ("CommandStatus::Failed", "CommandStatus::Errored"),
            ("Self::Failed", "Self::Errored"),
            ("pub enum CommandStatus {\n    Accepted,\n    Executed,\n    Failed,\n}",
             "pub enum CommandStatus {\n    Accepted,\n    Executed,\n    Errored,\n}"),
        ):
            hits = anchored(text, old)[0]
            if hits:
                edits.append((rel, old, new, hits))
    if not edits:
        raise AssertionError("改名锚点一颗都没命中")
    return edits


def main():
    print("== 基线：门禁红名单 ==")
    rc, out = gate_run()
    baseline = gate_reds(out)
    print("gate rc", rc, "红", len(baseline))
    for name in baseline:
        print("   -", name[:120])

    print("== 基线：受影响用例必须先全绿 ==")
    for cmd in (STORAGE_LIB, CLI_AUDIT, ["cargo", "test", "-p", "qx-core", "file_lock"]):
        rc, out = run(cmd)
        results = [line.strip() for line in out.splitlines() if "test result:" in line]
        print("  ", " ".join(cmd[2:]), "rc", rc, results)
        if rc != 0:
            raise SystemExit("基线不是绿的，先停下")

    only = [a for a in sys.argv[1:] if a.startswith("only=")]
    wanted = set()
    if only:
        wanted = set(only[0][5:].split(","))

    for case in CASES:
        if wanted and case["name"].split()[0] not in wanted:
            continue
        edits = failed_rename_edits() if case.get("rename_failed") else case["edits"]
        originals = apply(edits)
        try:
            rc, out = run(case["cmd"])
            tier = classify(out)
            failed = [line.strip() for line in out.splitlines()
                      if "test result: FAILED" in line or line.strip().startswith("---- ")]
            print(f"[{case['name']}] 用例档={tier} rc={rc} {failed[:2]}")
            if case.get("gate"):
                grc, gout = gate_run()
                reds = [n for n in gate_reds(gout) if n not in baseline]
                print("        门禁档=", "GATE-RED" if reds else "SURVIVED", "rc", grc)
                for line in reds[:3]:
                    print("          红:", line[:180])
                if tier == "SURVIVED" and not reds:
                    print("        >>> SURVIVED：判据是假的，得补")
        finally:
            restore(originals)
        print("      还原 sha: OK")


if __name__ == "__main__":
    main()
