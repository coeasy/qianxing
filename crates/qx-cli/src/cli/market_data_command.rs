//! CLI adapters for CCXT market metadata and historical bar acquisition.

use super::*;

pub(super) fn dispatch_market_spec(config: PathBuf, instrument: String, output: PathBuf) {
    if let Err(error) = run_ccxt_market_spec(&config, &instrument, &output) {
        eprintln!("[qx-cli · CLI] CCXT market spec 下载失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_fetch_ohlcv(
    ccxt_config: PathBuf,
    instrument: String,
    start_ms: u64,
    end_ms: u64,
    output: PathBuf,
    timeframe: String,
    manifest: Option<PathBuf>,
) {
    if let Err(error) = run_ccxt_fetch_ohlcv(
        &ccxt_config,
        &instrument,
        &timeframe,
        start_ms,
        end_ms,
        &output,
        manifest.as_deref(),
    ) {
        eprintln!("[qx-cli · CLI] CCXT OHLCV 下载失败: {error}");
        std::process::exit(2);
    }
}
