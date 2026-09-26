"""§55 的引用表先按今天的代码量一遍：每个 (文件, 行号, 该格里必须出现的名字) 都要成立。
写章节前先跑这颗，等于"用自己的规则量自己要写的那段文本"。"""
import sys
from pathlib import Path

WANTED = [
    # R7-1 死信重放
    ("crates/qx-storage/src/file/consumers.rs", "max_by_key(|record| record.attempts)"),
    ("crates/qx-storage/src/sqlite.rs", "ORDER BY attempts DESC LIMIT 1"),
    ("crates/qx-storage/tests/outbox_backend_semantics.rs",
     "fn file_outbox_relay_unblocks_the_tail("),
    ("crates/qx-storage/tests/outbox_backend_semantics.rs",
     "fn sqlite_outbox_relay_unblocks_the_tail("),
    ("crates/qx-storage/tests/outbox_backend_semantics.rs",
     "fn file_outbox_relay_parks_events_after_the_attempt_budget("),
    ("crates/qx-storage/tests/outbox_backend_semantics.rs",
     "fn outbox_attempt_budget_boundaries("),
    # R7-2 定序
    ("crates/qx-storage/src/sqlite.rs", "ORDER BY CAST(e.created_ts AS INTEGER)"),
    ("crates/qx-storage/src/sqlite.rs", "ORDER BY CAST(c.enqueued_ts AS INTEGER)"),
    ("crates/qx-storage/src/sqlite.rs", "jobs.sort_by_key("),
    ("crates/qx-storage/src/file/outbox.rs", "events.sort_by_key("),
    ("crates/qx-storage/src/lib.rs", "commands.sort_by_key("),
    ("crates/qx-storage/src/postgres.rs", "CREATE UNIQUE INDEX"),
    ("crates/qx-storage/tests/queue_backend_semantics.rs",
     "fn sqlite_backends_share_the_persistent_queue_contract("),
    ("crates/qx-storage/tests/queue_backend_semantics.rs",
     "fn file_backends_share_the_persistent_queue_contract("),
    # R7-3 行内自指 id
    ("crates/qx-protocol/src/lib.rs", "fn check_keys("),
    ("crates/qx-protocol/src/lib.rs", 'check_keys(\n            "order"'.replace("\n", "")),
    ("crates/qx-protocol/tests/snapshot_single_source.rs",
     "fn key_tables_reject_rows_whose_inline_id_disagrees_with_their_key("),
    # R7-4 JobSpec 退役键
    ("crates/qx-scheduler/src/job_spec.rs", "pub struct JobSpec {"),
    ("crates/qx-scheduler/src/lib.rs", "fn retired_job_spec_keys_are_ignored_by_the_loader("),
    # R7-7 / R7-7c 词表与金色字面量
    ("crates/qx-storage/src/tests.rs",
     "fn audit_chain_status_vocabulary_is_pinned_by_literal_words("),
    ("crates/qx-storage/src/tests.rs",
     "fn audit_chain_digest_inputs_are_pinned_by_a_golden_record("),
    # R7-8 流水与摘要同源
    ("crates/qx-api/src/lib.rs", "fn control_plane("),
    ("crates/qx-cli/src/tests/api_control_audit_live.rs",
     "fn control_audit_reads_the_store_the_workers_write_into("),
    # R7-9 锁年龄
    ("crates/qx-core/src/file_lock.rs", "fn decide_lock("),
    ("crates/qx-core/src/file_lock.rs", "fn second_stale_sighting_in_one_competition_waits_instead_of_deleting("),
    ("crates/qx-core/src/file_lock.rs", "fn takeover_happens_at_most_once_per_competition("),
    # R7-6 登记的其中两颗 + R7-5 那颗未接线的读侧
    ("crates/qx-zhenlu/src/lib.rs", "pub fn fee_descriptor"),
    ("crates/qx-runtime/src/pipeline.rs", "pub primary_seq"),
]

GATE = [
    "窗口与累计摘要在两个读面上同源",
    "控制面进 HTTP 只经 control_plane 这一个现读出口",
    "锁的『同一场竞争只接管一次』住在 decide_lock 里",
    "接管动手前复读年龄、只在仍然够老时才删",
    "JobSpec 字段名单可解析，且不含三格只有写侧的退役键",
    "deploy 里 3 份作业清单示例的每一格顶层键都在 JobSpec 名单里",
    "判据搬进 SQL 就要在 file/sqlite/postgres 三处同步",
    "M4 留下的零调用公开面与它的活替代路径，行号逐条仍指到定义本身",
    "单文件行数预算只降不升、无未登记的超大文件",
    "V11 方案书点名某颗东西时，被引那一格里就是那颗",
    "变更日志点名某颗东西时，被引那一格里就是那颗",
    "安全白皮书指过去的那一行不是空行",
    "台账指过去的那一行不是空行",
]

out = []
for rel, needle in WANTED:
    body = Path(rel).read_bytes().decode("utf-8", errors="replace").splitlines()
    hits = [i for i, line in enumerate(body, 1) if needle.replace("\n", "") in line]
    out.append(
        f"{rel}  共 {len(body)} 行   {':'.join(map(str, hits[:3])) if hits else 'MISS'}"
        f"   [{len(hits)} 命中] {needle[:56]}"
    )

gate_src = Path("tools/check_architecture.py").read_bytes().decode("utf-8").splitlines()
out.append("")
for title in GATE:
    hits = [i for i, line in enumerate(gate_src, 1) if title in line]
    out.append(f"gate {hits[:3]} [{len(hits)}] {title[:56]}")

Path("scratch_r7/r7_cite_table.txt").write_bytes(("\n".join(out) + "\n").encode("utf-8"))
print("rows", len(out))
