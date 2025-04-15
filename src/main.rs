use std::{net::SocketAddr, path::Path};

use color_eyre::eyre::Context;
use dotenvy::dotenv;
use hyper::server::conn::http1::{self};
use hyper_util::rt::TokioIo;
use rand::rngs::OsRng;
use tokio::net::TcpListener;
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use tunnel_rs::{Ldap, Registry, Server, auth::ForwardAuth};

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    dotenv().ok();

    let env_filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info"))?;

    let logger = tracing_subscriber::fmt::layer().compact();
    tracing_subscriber::Registry::default()
        .with(logger)
        .with(env_filter)
        .init();

    let key = if let Ok(path) = std::env::var("PRIVATE_KEY_FILE") {
        russh::keys::PrivateKey::read_openssh_file(Path::new(&path))
            .wrap_err_with(|| format!("failed to read ssh key: {path}"))?
    } else {
        warn!("No private key file specified, generating a new key");
        russh::keys::PrivateKey::random(&mut OsRng, russh::keys::Algorithm::Ed25519)?
    };

    let http_port = std::env::var("HTTP_PORT")
        .map(|port| port.parse())
        .unwrap_or(Ok(3000))?;
    let ssh_port = std::env::var("SSH_PORT")
        .map(|port| port.parse())
        .unwrap_or(Ok(2222))?;

    let domain =
        std::env::var("TUNNEL_DOMAIN").unwrap_or_else(|_| format!("localhost:{http_port}"));
    let authz_address = std::env::var("AUTHZ_ENDPOINT").wrap_err("AUTHZ_ENDPOINT is not set")?;

    let ldap = Ldap::start_from_env().await?;

    let auth = ForwardAuth::new(authz_address);
    let tunnels = Registry::new(domain, auth);
    let mut ssh = Server::new(ldap, tunnels.clone());
    let addr = SocketAddr::from(([0, 0, 0, 0], ssh_port));
    tokio::spawn(async move { ssh.run(key, addr).await });
    info!("SSH is available on {addr}");

    let addr = SocketAddr::from(([0, 0, 0, 0], http_port));
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
