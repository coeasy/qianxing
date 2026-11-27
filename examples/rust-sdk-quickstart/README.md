# Rust SDK 独立快速入门

安装 Rust 工具链和 Git 后，在此目录执行：

```bash
cargo run --locked --release
```

这个独立示例直接依赖已发布到 crates.io 的 `qx-sdk 0.4.2`；首次构建需要访问 crates.io 和 Cargo 依赖源。
示例自带 70 根教学用 K 线，程序会校验数据、确认 1 笔 SMA 成交、复核结果并打印产物路径。
数据是示例数据，不代表真实历史行情或投资结论。每次执行在当前目录的 `runs/` 下创建独立目录。

集成到自己的工程时，把 `Cargo.toml` 的依赖项和 `src/main.rs` 中的调用方式复制过去，
并将输入文件、标的、资金单位、策略与撮合假设换成自己验证过的配置。定点数使用 1e-9 标度；
`100_000 * SCALE` 表示 100,000 个结算币单位。本示例提交了 `Cargo.lock` 来固定传递依赖。

CI 会在独立 Cargo home 中从 crates.io 构建并运行本示例；源码 Git 消费路径由 `python tools/check_rust_sdk_git_consumer.py` 单独验收。
将 `Cargo.toml` 依赖和 `src/main.rs` 调用复制到自己的工程，即可从公开 registry 集成当前稳定 SDK。
