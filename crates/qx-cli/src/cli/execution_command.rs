//! CLI adapters for venue probes, order submission, Paper execution, and reconciliation.

use super::*;

pub(super) fn dispatch_binance_public_probe(network: String, instrument: String) {
    let testnet = match network.as_str() {
        "testnet" => true,
        "mainnet" => false,
        _ => {
            eprintln!("[qx-cli · CLI] binance-public-probe network 必须是 testnet 或 mainnet");
            std::process::exit(2);
        }
    };
    if let Err(error) = run_binance_public_probe(testnet, &instrument) {
        eprintln!("[qx-cli · CLI] Binance public probe 失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_binance_private_probe(path: PathBuf, worker_id: String) {
    if let Err(error) = run_binance_private_probe(&path, &worker_id) {
        eprintln!("[qx-cli · CLI] Binance private probe 失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_binance_submit_order(
    path: PathBuf,
    worker_id: String,
    command_path: PathBuf,
) {
    if let Err(error) = run_binance_submit_order(&path, &worker_id, &command_path) {
        eprintln!("[qx-cli · CLI] Binance SubmitOrder 执行失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_paper_submit_order(path: PathBuf, command_path: PathBuf) {
    if let Err(error) = run_paper_submit_order(&path, &command_path) {
        eprintln!("[qx-cli · CLI] Paper SubmitOrder 执行失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_ccxt_submit_order(args: crate::ccxt_submit_args::CcxtSubmitArgs) {
    usage_errors::exit_on_failure(
        run_ccxt_submit_order(&args),
        "[qx-cli · CLI] CCXT SubmitOrder 执行失败",
    );
}

pub(super) fn dispatch_paper_e2e(
    path: PathBuf,
    quote_price_raw: Option<i128>,
    quote_qty_raw: Option<i128>,
) {
    let quote = match (quote_price_raw, quote_qty_raw) {
        (Some(price_raw), quantity_raw) => Some(PaperE2eQuoteOverride {
            price_raw,
            quantity_raw: quantity_raw.unwrap_or(1_000_000_000_000),
        }),
        (None, Some(_)) => {
            eprintln!("[qx-cli · CLI] --quote-qty-raw 必须与 --quote-price-raw 同时使用");
            std::process::exit(2);
        }
        (None, None) => None,
    };
    if let Err(error) = run_paper_pipeline_once_with_quote(&path, true, quote) {
        eprintln!("[qx-cli · CLI] Paper 主链路验收失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_paper_check(path: PathBuf, json: bool) {
    if let Err(error) = run_paper_check(&path, json) {
        eprintln!("[qx-cli · CLI] Paper 主链路验收失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_reconcile(path: Option<PathBuf>, worker_id: Option<String>) {
    if let Some(path) = path {
        if let Err(error) = run_binance_reconcile_once(&path, worker_id.as_deref()) {
            eprintln!("[qx-cli · CLI] Binance 对账失败: {error}");
            std::process::exit(2);
        }
    } else {
        eprintln!(
            "reconcile 需要本地与远端两个来源：reconcile <runtime.json> [worker-id]\n  \
             本地来源 = 运行时配置指向的 EventLog 账本\n  \
             远端来源 = 该配置中 Binance worker 的交易所账户（凭据由配置引用，不在命令行传）"
        );
        std::process::exit(2);
    }
}
