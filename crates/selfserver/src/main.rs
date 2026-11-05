use std::{env, net::SocketAddr};

use tokio::net::TcpListener;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let bind: SocketAddr = env::var("SELFTUNNEL_CONTROL_BIND")
        .unwrap_or_else(|_| "0.0.0.0:9000".to_owned())
        .parse()?;

    info!("SelfServer {}", env!("CARGO_PKG_VERSION"));
    info!("Control listener configured at {bind}");

    // Bootstrap milestone only: the authenticated tunnel protocol and public
    // Minecraft listener are implemented in the next milestone.
    let listener = TcpListener::bind(bind).await?;
    info!("Control port is listening; tunnel protocol is not implemented yet.");

    loop {
        let (stream, peer) = listener.accept().await?;
        warn!(%peer, "Received connection, but this bootstrap build does not accept tunnel sessions yet");
        drop(stream);
    }
}
