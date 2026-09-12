"""最小事件 Consumer handler 示例。

qx-cli event-consumer-worker 会把一条 Outbox envelope 作为一行 JSON 写入 stdin。
真实 reducer 应在这里执行幂等业务事务，并在成功提交后退出 0；失败退出非 0，
由 JetStream NAK 重试，达到阈值后进入消费者 DLQ。
"""

from __future__ import annotations

import json
import sys


def main() -> int:
    line = sys.stdin.readline()
    if not line:
        return 2
    event = json.loads(line)
    required = ("event_id", "topic", "partition_key", "payload")
    if any(not event.get(field) for field in required):
        return 3
    # 这里替换成真实的幂等 reducer/数据库事务；不要把凭证写入 stdout。
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
