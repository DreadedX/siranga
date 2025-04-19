#![feature(future_join)]
use std::future::join;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use color_eyre::eyre::Context;
use dotenvy::dotenv;
use rand::rngs::OsRng;
use siranga::VERSION;
use siranga::ldap::Ldap;
use siranga::ssh::Server;
use siranga::tunnel::Registry;
use siranga::web::{ForwardAuth, Service};
use tokio::net::TcpListener;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

async fn shutdown_task(token: CancellationToken) {
    select! {
        _ = tokio::signal::ctrl_c() => {
            debug!("Received SIGINT");
        }
        _ = token.cancelled() => {
            debug!("Application called for graceful shutdown");
        }
    }
    info!("Starting graceful shutdown");
    token.cancel();
    select! {
        _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    dotenv().ok();

    let env_filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info"))?;

    if std::env::var("CARGO").is_ok() {
        let logger = tracing_subscriber::fmt::layer().compact();
        tracing_subscriber::Registry::default()
            .with(logger)
            .with(env_filter)
            .init();
    } else {
        let logger = tracing_subscriber::fmt::layer().json();
        tracing_subscriber::Registry::default()
            .with(logger)
            .with(env_filter)
            .init();
    }

    info!(version = VERSION, "Starting",);

    let key = if let Ok(path) = std::env::var("PRIVATE_KEY_FILE") {
        russh::keys::PrivateKey::read_openssh_file(Path::new(&path))
            .wrap_err_with(|| format!("failed to read ssh key: {path}"))?
    } else {
        warn!("No private key file specified, generating a new key");
        russh::keys::PrivateKey::random(&mut OsRng, russh::keys::Algorithm::Ed25519)?
    };

    let http_port = std::env::var("HTTP_PORT")
        .map(|port| port.parse().wrap_err_with(|| format!("HTTP_PORT={port}")))
        .unwrap_or(Ok(3000))?;
    let ssh_port = std::env::var("SSH_PORT")
        .map(|port| port.parse().wrap_err_with(|| format!("SSH_PORT={port}")))
        .unwrap_or(Ok(2222))?;

    let domain =
        std::env::var("TUNNEL_DOMAIN").unwrap_or_else(|_| format!("localhost:{http_port}"));
    let authz_address = std::env::var("AUTHZ_ENDPOINT").wrap_err("AUTHZ_ENDPOINT is not set")?;

    let registry = Registry::new(domain);

    let token = CancellationToken::new();

    let (ldap, ldap_handle) = Ldap::start_from_env(token.clone()).await?;

    let ssh = Server::new(ldap, registry.clone(), token.clone());
    let ssh_addr = SocketAddr::from(([0, 0, 0, 0], ssh_port));
    let ssh_task = ssh.run(key, ssh_addr);
    info!("SSH is available on {ssh_addr}");

    let auth = ForwardAuth::new(authz_address);
    let service = Service::new(registry, auth);
    let http_addr = SocketAddr::from(([0, 0, 0, 0], http_port));
    let http_listener = TcpListener::bind(http_addr).await?;
    let http_task = service.serve(http_listener, token.clone());
    info!("HTTP is available on {http_addr}");

    select! {
        _ = join!(ldap_handle, ssh_task, http_task) => {
            info!("Shutdown gracefully");
        }
        _ = shutdown_task(token.clone()) => {
            error!("Failed to shut down gracefully");
        }
    };

    Ok(())
}
