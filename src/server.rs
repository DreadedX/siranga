use std::{net::SocketAddr, sync::Arc, time::Duration};

use russh::{keys::PrivateKey, server::Server as _};
use tokio::net::ToSocketAddrs;
use tracing::{debug, warn};

use crate::{handler::Handler, tunnel::Tunnels};

pub struct Server {
    tunnels: Tunnels,
}

impl Server {
    pub fn new(tunnels: Tunnels) -> Self {
        Server { tunnels }
    }

    pub fn tunnels(&self) -> Tunnels {
        self.tunnels.clone()
    }

    pub fn run(
        &mut self,
        key: PrivateKey,
        addr: impl ToSocketAddrs + Send + std::fmt::Debug,
    ) -> impl Future<Output = Result<(), std::io::Error>> + Send {
        let config = russh::server::Config {
            inactivity_timeout: Some(Duration::from_secs(3600)),
            auth_rejection_time: Duration::from_secs(3),
            auth_rejection_time_initial: Some(Duration::from_secs(0)),
            keys: vec![key],
            preferred: russh::Preferred {
                ..Default::default()
            },
            nodelay: true,
            ..Default::default()
        };
        let config = Arc::new(config);

        debug!(?addr, "Running ssh");

        async move { self.run_on_address(config, addr).await }
    }
}

impl russh::server::Server for Server {
    type Handler = Handler;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self::Handler {
        Handler::new(self.tunnels.clone())
    }

    fn handle_session_error(&mut self, error: <Self::Handler as russh::server::Handler>::Error) {
        warn!("Session error: {error:#?}");
    }
}
