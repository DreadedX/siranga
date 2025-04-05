use std::{net::SocketAddr, path::Path};

use hyper::server::conn::http1::{self};
use hyper_util::rt::TokioIo;
use rand::rngs::OsRng;
use tokio::net::TcpListener;
use tracing::warn;
use tracing_subscriber::{EnvFilter, Registry, layer::SubscriberExt, util::SubscriberInitExt};
use tunnel_rs::ssh::Server;

#[tokio::main]
async fn main() {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .expect("Fallback should be valid");

    let logger = tracing_subscriber::fmt::layer().compact();
    Registry::default().with(logger).with(env_filter).init();

    let key = if let Ok(path) = std::env::var("PRIVATE_KEY_FILE") {
        russh::keys::PrivateKey::read_openssh_file(Path::new(&path)).unwrap()
    } else {
        russh::keys::PrivateKey::random(&mut OsRng, russh::keys::Algorithm::Ed25519).unwrap()
    };

    let port = 3000;
    let domain = std::env::var("TUNNEL_DOMAIN").unwrap_or_else(|_| format!("localhost:{port}"));

    let mut ssh = Server::new(domain);

    let tunnels = ssh.tunnels();
    tokio::spawn(async move { ssh.run(key, ("0.0.0.0", 2222)).await });

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await.unwrap();
    loop {
        let (stream, _) = listener.accept().await.unwrap();
        let io = TokioIo::new(stream);

        let tunnels = tunnels.clone();
        tokio::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .preserve_header_case(true)
                .title_case_headers(true)
                .serve_connection(io, tunnels)
                .with_upgrades()
                .await
            {
                warn!("Failed to serve connection: {err:?}");
            }
        });
    }
}
