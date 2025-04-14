use std::{net::SocketAddr, path::Path};

use color_eyre::eyre::Context;
use dotenvy::dotenv;
use hyper::server::conn::http1::{self};
use hyper_util::rt::TokioIo;
use rand::rngs::OsRng;
use tokio::net::TcpListener;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, Registry, layer::SubscriberExt, util::SubscriberInitExt};
use tunnel_rs::{Ldap, Server, Tunnels};

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    dotenv().ok();

    let env_filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info"))?;

    let logger = tracing_subscriber::fmt::layer().compact();
    Registry::default().with(logger).with(env_filter).init();

    let key = if let Ok(path) = std::env::var("PRIVATE_KEY_FILE") {
        russh::keys::PrivateKey::read_openssh_file(Path::new(&path))
            .wrap_err_with(|| format!("failed to read ssh key: {path}"))?
    } else {
        russh::keys::PrivateKey::random(&mut OsRng, russh::keys::Algorithm::Ed25519)?
    };

    let port = 3000;
    let domain = std::env::var("TUNNEL_DOMAIN").unwrap_or_else(|_| format!("localhost:{port}"));
    let authz_address = std::env::var("AUTHZ_ENDPOINT")
        .unwrap_or("http://localhost:9091/api/authz/forward-auth".into());

    let ldap = Ldap::start_from_env().await?;

    let tunnels = Tunnels::new(domain, authz_address);
    let mut ssh = Server::new(ldap, tunnels.clone());
    let addr = SocketAddr::from(([0, 0, 0, 0], 2222));
    tokio::spawn(async move { ssh.run(key, addr).await });
    info!("SSH is available on {addr}");

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    info!("HTTP is available on {addr}");

    // TODO: Graceful shutdown
    loop {
        let (stream, _) = listener.accept().await?;
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
                error!("Failed to serve connection: {err:?}");
            }
        });
    }
}
