use qx_sdk_http::{HttpRemoteTransport, RemoteApiClient};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let timeout = Duration::from_secs(5);
    let transport =
        match args.as_slice() {
            [mode, host, port] if mode == "loopback" => {
                HttpRemoteTransport::loopback(host, port.parse()?, timeout)?
            }
            [mode, host, port] if mode == "https" => {
                HttpRemoteTransport::https(host, port.parse()?, timeout)?
            }
            [mode, host, port, ca, cert, key] if mode == "mtls" => {
                HttpRemoteTransport::mtls(host, port.parse()?, timeout, ca, cert, key)?
            }
            _ => return Err(
                "usage: discover loopback|https HOST PORT, or discover mtls HOST PORT CA CERT KEY"
                    .into(),
            ),
        };
    let recipes = RemoteApiClient::new(transport).builtin_strategy_recipes()?;
    println!("{}", recipes);
    Ok(())
}
