use crate::*;

pub(crate) fn is_binance_testnet(worker: &WorkerConfig) -> bool {
    worker
        .venue_id
        .as_deref()
        .map(|venue| venue.to_ascii_lowercase().contains("testnet"))
        .unwrap_or(false)
        || worker
            .endpoint
            .as_deref()
            .map(|endpoint| endpoint.to_ascii_lowercase().contains("testnet"))
            .unwrap_or(false)
}

pub(crate) fn load_binance_worker_auth(worker: &WorkerConfig) -> Result<BinanceSpotAuth, String> {
    if let Some(files) = worker.credential_files.as_ref() {
        return BinanceSpotCredentials::from_files(&files.api_key, &files.secret)?.into_auth();
    }
    let env = worker.credential_env.as_ref().ok_or_else(|| {
        format!(
            "worker {} 缺少 credential_env 或 credential_files",
            worker.id
        )
    })?;
    BinanceSpotCredentials::from_env(&env.api_key, &env.secret)?.into_auth()
}

pub(crate) fn new_binance_venue(
    worker: &WorkerConfig,
    auth: BinanceSpotAuth,
) -> Result<BinanceSpotVenue, String> {
    let transport: Arc<dyn HttpTransport> =
        Arc::new(TlsHttpTransport::new(Duration::from_secs(10))?);
    Ok(if is_binance_testnet(worker) {
        BinanceSpotVenue::testnet(worker.id.clone(), auth, transport)
    } else {
        BinanceSpotVenue::new(worker.id.clone(), auth, transport)
    })
}

pub(crate) fn configured_ws_endpoint(worker: &WorkerConfig) -> Result<(String, u16), String> {
    let endpoint = worker
        .endpoint
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 WebSocket endpoint", worker.id))?;
    let remainder = endpoint
        .strip_prefix("wss://")
        .or_else(|| endpoint.strip_prefix("ws://"))
        .ok_or_else(|| format!("worker {} endpoint 必须使用 ws:// 或 wss://", worker.id))?;
    let authority = remainder
        .split('/')
        .next()
        .filter(|authority| !authority.is_empty())
        .ok_or_else(|| format!("worker {} endpoint 缺少主机", worker.id))?;
    let (host, port) = authority
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()))
        .map(|(host, port)| (host, port.parse::<u16>().unwrap_or_default()))
        .unwrap_or((authority, 443));
    if host.is_empty()
        || host.contains(':')
        || host.contains('\r')
        || host.contains('\n')
        || port == 0
    {
        return Err(format!("worker {} endpoint 主机非法", worker.id));
    }
    Ok((host.to_string(), port))
}

pub(crate) fn run_binance_public_probe(testnet: bool, instrument: &str) -> Result<(), String> {
    let instrument = InstrumentId::parse(instrument)
        .ok_or_else(|| format!("Binance public probe instrument 非法: {instrument}"))?;
    if !instrument.venue.as_str().eq_ignore_ascii_case("BINANCE") {
        return Err("Binance public probe 只接受 *.BINANCE InstrumentId".into());
    }
    let transport: Arc<dyn HttpTransport> =
        Arc::new(TlsHttpTransport::new(Duration::from_secs(10))?);
    let mut market = if testnet {
        BinanceSpotMarketData::testnet(transport)
    } else {
        BinanceSpotMarketData::new(transport)
    };
    let quote = market
        .fetch_book_ticker(&instrument, runtime_timestamp_ms())
        .map_err(|error| format!("Binance public bookTicker 失败: {error:?}"))?;
    println!(
        "[Binance · PublicProbe] network={} instrument={} bid_raw={} ask_raw={} bid_qty_raw={} ask_qty_raw={} ts={}",
        if testnet { "testnet" } else { "mainnet" },
        instrument,
        quote.bid.raw(),
        quote.ask.raw(),
        quote.bid_qty.raw(),
        quote.ask_qty.raw(),
        quote.ts
    );
    Ok(())
}

pub(crate) fn run_binance_private_probe(
    runtime_path: &Path,
    worker_id: &str,
) -> Result<(), String> {
    let config = read_runtime_config(runtime_path)?;
    let mut worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 Binance private probe worker: {worker_id}"))?;
    if !worker.enabled
        || !matches!(
            worker.role,
            WorkerRole::UserStream | WorkerRole::Execution | WorkerRole::Reconciler
        )
    {
        return Err(format!("worker {worker_id} 不是启用的 Binance 私有 worker"));
    }
    if !worker
        .venue_id
        .as_deref()
        .is_some_and(|venue| venue.to_ascii_lowercase().contains("binance"))
    {
        return Err(format!("worker {worker_id} 不是 Binance worker"));
    }
    resolve_worker_runtime_paths(&mut worker, runtime_path);
    let auth = load_binance_worker_auth(&worker)?;
    let transport: Arc<dyn HttpTransport> =
        Arc::new(TlsHttpTransport::new(Duration::from_secs(10))?);
    let mut venue = if is_binance_testnet(&worker) {
        BinanceSpotVenue::testnet(worker.id.clone(), auth, transport)
    } else {
        BinanceSpotVenue::new(worker.id.clone(), auth, transport)
    };
    let balances = venue
        .fetch_account_balances(runtime_timestamp_ms())
        .map_err(|error| format!("Binance private account probe 失败: {error:?}"))?;
    println!(
        "[Binance · PrivateProbe] network={} worker={} balances={}",
        if is_binance_testnet(&worker) {
            "testnet"
        } else {
            "mainnet"
        },
        worker.id,
        balances.len()
    );
    Ok(())
}

/// Binance worker 的 EventLog 身份：行情按 worker 各自成册，账户侧角色一律走
/// 账户身份的唯一构造点，拿不到身份就是配置错误。
pub(crate) fn binance_event_log_name(worker: &WorkerConfig) -> Result<String, String> {
    match worker.role {
        WorkerRole::MarketData => Ok(worker_scoped_event_log_name("", &worker.id)),
        _ => required_account_event_log(worker),
    }
}
