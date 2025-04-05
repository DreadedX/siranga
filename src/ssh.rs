use std::{collections::HashSet, net::SocketAddr, sync::Arc, time::Duration};

use russh::{
    ChannelId,
    keys::PrivateKey,
    server::{Auth, Msg, Server as _, Session},
};
use tokio::{
    net::ToSocketAddrs,
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};
use tracing::{debug, trace, warn};

use crate::tunnel::{Tunnel, Tunnels};

pub struct Handler {
    tx: UnboundedSender<Vec<u8>>,
    rx: Option<UnboundedReceiver<Vec<u8>>>,

    all_tunnels: Tunnels,
    tunnels: HashSet<String>,
}

impl Handler {
    fn send(&self, data: &str) {
        let _ = self.tx.send(data.as_bytes().to_vec());
    }
}

impl russh::server::Handler for Handler {
    type Error = russh::Error;

    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        trace!("channel_open_session");

        let Some(mut rx) = self.rx.take() else {
            return Err(russh::Error::Inconsistent);
        };

        tokio::spawn(async move {
            loop {
                let Some(message) = rx.recv().await else {
                    break;
                };

                trace!("Sending message to client");

                if channel.data(message.as_ref()).await.is_err() {
                    break;
                }
            }
        });

        Ok(true)
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        _public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        debug!("Login from {user}");

        // TODO: Get ssh keys associated with user from ldap
        Ok(Auth::Accept)
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // TODO: Graceful shutdown
        if data == [3] {
            return Err(russh::Error::Disconnect);
        }

        Ok(())
    }

    async fn exec_request(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        trace!(data, "exec_request");

        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        trace!(address, port, "tcpip_forward");

        let tunnel = Tunnel::new(session.handle(), address, *port);
        let Some(address) = self.all_tunnels.add_tunnel(address, tunnel).await else {
            self.send(&format!("FAILED: ({address} already in use)\r\n"));
            return Ok(false);
        };

        // NOTE: The port we receive might not be the port that is getting forwarded from the
        // client, we could include it in the message we send
        self.send(&format!("http://{address}\r\n"));
        self.tunnels.insert(address);

        Ok(true)
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        let tunnels = self.tunnels.clone();
        let mut all_tunnels = self.all_tunnels.clone();

        tokio::spawn(async move {
            all_tunnels.remove_tunnels(tunnels.clone()).await;
        });
    }
}

pub struct Server {
    tunnels: Tunnels,
}

impl Server {
    pub fn new(domain: impl Into<String>) -> Self {
        Server {
            tunnels: Tunnels::new(domain),
        }
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
        let (tx, rx) = unbounded_channel::<Vec<u8>>();

        Handler {
            tx,
            rx: Some(rx),
            all_tunnels: self.tunnels.clone(),
            tunnels: HashSet::new(),
        }
    }

    fn handle_session_error(&mut self, error: <Self::Handler as russh::server::Handler>::Error) {
        warn!("Session error: {error:#?}");
    }
}
