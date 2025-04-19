mod handler;
mod renderer;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use handler::Handler;
use renderer::Renderer;
use russh::MethodKind;
use russh::keys::PrivateKey;
use russh::server::Server as _;
use tokio::net::ToSocketAddrs;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::ldap::Ldap;
use crate::tunnel::Registry;

pub struct Server {
    ldap: Ldap,
    registry: Registry,
    token: CancellationToken,
}

async fn graceful_shutdown(token: CancellationToken) {
    token.cancelled().await;
    let duration = 1;
    // All pty sessions will close once the token is cancelled, but to properly allow the sessions
    // to close the ssh server still needs to be driven, so we let it run a little bit longer.
    // TODO: Figure out a way to wait for all connections to be closed, would require also closing
    // non-pty sessions somehow
    debug!("Waiting for {duration}s before stopping");
    tokio::time::sleep(Duration::from_secs(duration)).await;
}

impl Server {
    pub fn new(ldap: Ldap, registry: Registry, token: CancellationToken) -> Self {
        Server {
            ldap,
            registry,
            token,
        }
    }

    pub async fn run(mut self, key: PrivateKey, addr: impl ToSocketAddrs + Send + std::fmt::Debug) {
        let config = russh::server::Config {
            inactivity_timeout: Some(Duration::from_secs(3600)),
            auth_rejection_time: Duration::from_secs(1),
            auth_rejection_time_initial: Some(Duration::from_secs(0)),
            keys: vec![key],
            preferred: russh::Preferred {
                ..Default::default()
            },
            nodelay: true,
            methods: [MethodKind::PublicKey].as_slice().into(),
            ..Default::default()
        };
        let config = Arc::new(config);

        debug!(?addr, "Running ssh");

        let token = self.token.clone();
        select! {
            res = self.run_on_address(config, addr) => {
                if let Err(err) = res {
                    error!("SSH Server error: {err}");
                }
            }
            _ = graceful_shutdown(token) => {
                debug!("Graceful shutdown");
            }
        }
    }
}

impl russh::server::Server for Server {
    type Handler = Handler;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self::Handler {
        Handler::new(self.ldap.clone(), self.registry.clone(), self.token.clone())
    }

    fn handle_session_error(&mut self, error: <Self::Handler as russh::server::Handler>::Error) {
        warn!("Session error: {error:#?}");
    }
}
