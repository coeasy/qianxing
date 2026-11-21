//! Optional HTTP transport for the Rust SDK. Plaintext is restricted to IPv4 loopback;
//! HTTPS verifies certificates and hostnames. No request is retried automatically.
//!
//! ```no_run
//! use qx_sdk_http::{HttpRemoteTransport, RemoteApiClient};
//! use std::time::Duration;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let transport = HttpRemoteTransport::loopback("127.0.0.1", 18082, Duration::from_secs(5))?;
//! let client = RemoteApiClient::new(transport);
//! println!("{}", client.builtin_strategy_recipes()?);
//! # Ok(())
//! # }
//! ```

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use qx_adapter::{HttpRequest, HttpTransport, TcpHttpTransport, TlsHttpTransport};
pub use qx_sdk::{
    BacktestDatasetInput, BacktestOutcome, BacktestSpec, BuiltinStrategyKind, BuiltinStrategySpec,
    CallerCapability, Client, DatasetSpec, DatasetVerdict, RemoteApiClient, RemoteApiError,
    RemoteResponse, RemoteTransport, VerificationResult,
};
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::Duration;

enum Transport {
    Loopback(TcpHttpTransport),
    Tls(TlsHttpTransport),
}

/// A single-origin transport. Requests are sent once and never retried by this adapter.
pub struct HttpRemoteTransport {
    host: String,
    port: u16,
    transport: Transport,
}

impl HttpRemoteTransport {
    /// Unencrypted connections are allowed only to a numeric IPv4 loopback address.
    pub fn loopback(host: &str, port: u16, timeout: Duration) -> Result<Self, String> {
        validate_endpoint(host, port, timeout)?;
        let address: Ipv4Addr = host
            .parse()
            .map_err(|_| "loopback requires a numeric IPv4 address")?;
        if !address.is_loopback() {
            return Err("plaintext is restricted to IPv4 loopback".into());
        }
        Ok(Self {
            host: host.into(),
            port,
            transport: Transport::Loopback(TcpHttpTransport::new(timeout)),
        })
    }

    /// HTTPS with the adapter's public trust roots and hostname verification.
    pub fn https(host: &str, port: u16, timeout: Duration) -> Result<Self, String> {
        validate_endpoint(host, port, timeout)?;
        Ok(Self {
            host: host.into(),
            port,
            transport: Transport::Tls(TlsHttpTransport::new(timeout)?),
        })
    }

    /// HTTPS with an explicit server CA and client certificate/key for mTLS.
    pub fn mtls(
        host: &str,
        port: u16,
        timeout: Duration,
        ca: impl AsRef<Path>,
        certificate: impl AsRef<Path>,
        key: impl AsRef<Path>,
    ) -> Result<Self, String> {
        validate_endpoint(host, port, timeout)?;
        let transport = TlsHttpTransport::from_mtls_pem(timeout, ca, certificate, key)?;
        Ok(Self {
            host: host.into(),
            port,
            transport: Transport::Tls(transport),
        })
    }

    fn send(&self, method: &str, path: &str, body: &str) -> Result<RemoteResponse, String> {
        if !path.starts_with('/')
            || path.starts_with("//")
            || !path
                .bytes()
                .all(|b| (0x21..=0x7e).contains(&b) && b != b'\\' && b != b'#')
        {
            return Err("invalid origin-relative request path".into());
        }
        let request = HttpRequest {
            method: method.into(),
            host: self.host.clone(),
            port: self.port,
            path: path.into(),
            body: body.into(),
            headers: BTreeMap::new(),
        };
        let response = match &self.transport {
            Transport::Loopback(transport) => transport.send(request),
            Transport::Tls(transport) => transport.send(request),
        }?;
        Ok(RemoteResponse::new(response.status, response.body))
    }
}

impl RemoteTransport for HttpRemoteTransport {
    fn post(&self, path: &str, body: &str) -> Result<RemoteResponse, String> {
        self.send("POST", path, body)
    }

    fn get(&self, path: &str) -> Result<RemoteResponse, String> {
        self.send("GET", path, "")
    }
}

fn validate_endpoint(host: &str, port: u16, timeout: Duration) -> Result<(), String> {
    if port == 0 || timeout.is_zero() {
        return Err("port and timeout must be nonzero".into());
    }
    if host.is_empty() || host.len() > 253 || !host.is_ascii() {
        return Err("invalid host".into());
    }
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("host must be an IPv4 address or ASCII DNS name without a port".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_untrusted_plaintext_and_invalid_endpoints() {
        let timeout = Duration::from_secs(1);
        assert!(HttpRemoteTransport::loopback("127.0.0.1", 18082, timeout).is_ok());
        for host in [
            "localhost",
            "192.0.2.1",
            "127.0.0.1:80",
            "example.com\r\nX: y",
        ] {
            assert!(HttpRemoteTransport::loopback(host, 80, timeout).is_err());
        }
        assert!(HttpRemoteTransport::https("example.com", 443, timeout).is_ok());
        assert!(HttpRemoteTransport::https("example.com/path", 443, timeout).is_err());
        assert!(HttpRemoteTransport::https("example.com", 0, timeout).is_err());
        assert!(HttpRemoteTransport::https("example.com", 443, Duration::ZERO).is_err());
        let transport = HttpRemoteTransport::loopback("127.0.0.1", 80, timeout).unwrap();
        assert!(transport.get("//other-host/path").is_err());
        assert!(transport.get("/path\r\nX: y").is_err());
    }
}
