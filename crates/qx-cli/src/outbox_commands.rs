//! Outbox 与事件消费命令入口。
//!
//! 处理函数本身不受 nats feature 门控，只有内部分支需要 `--features nats`：
//! 缺少特性时仍打印可操作的报错并以 2 号退出码失败，而不是让命令从 CLI 表面上消失。

#[cfg(feature = "nats")]
use super::*;

#[cfg_attr(not(feature = "nats"), allow(unused_variables))]
pub(crate) fn outbox_relay_command(argv: &[String]) {
    #[cfg(feature = "nats")]
    {
        let root = match argv.get(2).cloned() {
            Some(value) => PathBuf::from(value),
            None => {
                eprintln!("outbox-relay 需要 data-root nats-url subject-prefix [limit]");
                std::process::exit(2);
            }
        };
        let url = match argv.get(3).cloned() {
            Some(value) => value,
            None => {
                eprintln!("outbox-relay 缺少 nats-url");
                std::process::exit(2);
            }
        };
        let prefix = match argv.get(4).cloned() {
            Some(value) => value,
            None => {
                eprintln!("outbox-relay 缺少 subject-prefix");
                std::process::exit(2);
            }
        };
        let limit = argv
            .get(5)
            .cloned()
            .map(|value| value.parse::<usize>())
            .transpose()
            .unwrap_or_else(|_| {
                eprintln!("outbox-relay limit 非法");
                std::process::exit(2);
            })
            .unwrap_or(100);
        if let Err(error) = run_file_outbox_relay(&root, &url, &prefix, limit) {
            eprintln!("Outbox relay 失败: {error}");
            std::process::exit(2);
        }
    }
    #[cfg(not(feature = "nats"))]
    {
        eprintln!("outbox-relay 需要使用 --features nats 构建 qx-cli");
        std::process::exit(2);
    }
}

#[cfg_attr(
    not(all(feature = "nats", feature = "postgres")),
    allow(unused_variables)
)]
pub(crate) fn outbox_relay_postgres_command(argv: &[String]) {
    #[cfg(all(feature = "nats", feature = "postgres"))]
    {
        let runtime_path = match argv.get(2).cloned() {
            Some(value) => PathBuf::from(value),
            None => {
                eprintln!(
                    "outbox-relay-postgres 需要 runtime.json nats-url subject-prefix [limit]"
                );
                std::process::exit(2);
            }
        };
        let url = match argv.get(3).cloned() {
            Some(value) => value,
            None => {
                eprintln!("outbox-relay-postgres 缺少 nats-url");
                std::process::exit(2);
            }
        };
        let prefix = match argv.get(4).cloned() {
            Some(value) => value,
            None => {
                eprintln!("outbox-relay-postgres 缺少 subject-prefix");
                std::process::exit(2);
            }
        };
        let limit = argv
            .get(5)
            .cloned()
            .map(|value| value.parse::<usize>())
            .transpose()
            .unwrap_or_else(|_| {
                eprintln!("outbox-relay-postgres limit 非法");
                std::process::exit(2);
            })
            .unwrap_or(100);
        if let Err(error) = run_postgres_outbox_relay(&runtime_path, &url, &prefix, limit) {
            eprintln!("PostgreSQL Outbox relay 失败: {error}");
            std::process::exit(2);
        }
    }
    #[cfg(not(all(feature = "nats", feature = "postgres")))]
    {
        eprintln!("outbox-relay-postgres 需要使用 --features 'nats postgres' 构建 qx-cli");
        std::process::exit(2);
    }
}

#[cfg_attr(not(feature = "nats"), allow(unused_variables))]
pub(crate) fn outbox_relay_worker_command(argv: &[String]) {
    #[cfg(feature = "nats")]
    {
        let path = argv
            .get(2)
            .cloned()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("deploy/qianxing.runtime.example.json"));
        let worker_id = match argv.get(3).cloned() {
            Some(value) => value,
            None => {
                eprintln!("outbox-relay-worker 需要 runtime.json worker-id [--once]");
                std::process::exit(2);
            }
        };
        let once = argv.iter().any(|argument| argument == "--once");
        if let Err(error) = run_outbox_relay_worker(&path, &worker_id, once) {
            eprintln!("Outbox relay worker 失败: {error}");
            std::process::exit(2);
        }
    }
    #[cfg(not(feature = "nats"))]
    {
        eprintln!("outbox-relay-worker 需要使用 --features nats 构建 qx-cli");
        std::process::exit(2);
    }
}

#[cfg_attr(not(feature = "nats"), allow(unused_variables))]
pub(crate) fn event_consumer_worker_command(argv: &[String]) {
    #[cfg(feature = "nats")]
    {
        let path = argv
            .get(2)
            .cloned()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("deploy/qianxing.runtime.example.json"));
        let worker_id = match argv.get(3).cloned() {
            Some(value) => value,
            None => {
                eprintln!("event-consumer-worker 需要 runtime.json worker-id [--once]");
                std::process::exit(2);
            }
        };
        let once = argv.iter().any(|argument| argument == "--once");
        if let Err(error) = run_event_consumer_worker(&path, &worker_id, once) {
            eprintln!("Event consumer worker 失败: {error}");
            std::process::exit(2);
        }
    }
    #[cfg(not(feature = "nats"))]
    {
        eprintln!("event-consumer-worker 需要使用 --features nats 构建 qx-cli");
        std::process::exit(2);
    }
}

#[cfg_attr(not(feature = "nats"), allow(unused_variables))]
pub(crate) fn consumer_dlq_replay_command(argv: &[String]) {
    #[cfg(feature = "nats")]
    {
        let path = match argv.get(2).cloned() {
            Some(value) => PathBuf::from(value),
            None => {
                eprintln!("consumer-dlq-replay 需要 runtime.json group-id event-id");
                std::process::exit(2);
            }
        };
        let group_id = match argv.get(3).cloned() {
            Some(value) => value,
            None => {
                eprintln!("consumer-dlq-replay 缺少 group-id");
                std::process::exit(2);
            }
        };
        let event_id = match argv.get(4).cloned() {
            Some(value) => value,
            None => {
                eprintln!("consumer-dlq-replay 缺少 event-id");
                std::process::exit(2);
            }
        };
        if let Err(error) = run_dead_letter_replay(&path, &group_id, &event_id) {
            eprintln!("DLQ 重放失败: {error}");
            std::process::exit(2);
        }
    }
    #[cfg(not(feature = "nats"))]
    {
        eprintln!("consumer-dlq-replay 需要使用 --features nats 构建 qx-cli");
        std::process::exit(2);
    }
}
