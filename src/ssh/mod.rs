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
use tracing::{debug, warn};

use crate::ldap::Ldap;
use crate::tunnel::Registry;

pub struct Server {
    ldap: Ldap,
    registry: Registry,
}

impl Server {
    pub fn new(ldap: Ldap, registry: Registry) -> Self {
        Server { ldap, registry }
    }

    pub fn run(
        &mut self,
        key: PrivateKey,
        addr: impl ToSocketAddrs + Send + std::fmt::Debug,
    ) -> impl Future<Output = Result<(), std::io::Error>> + Send {
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

        async move { self.run_on_address(config, addr).await }
    }
}

impl russh::server::Server for Server {
    type Handler = Handler;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self::Handler {
        Handler::new(self.ldap.clone(), self.registry.clone())
    }

    fn handle_session_error(&mut self, error: <Self::Handler as russh::server::Handler>::Error) {
        warn!("Session error: {error:#?}");
    }
}
