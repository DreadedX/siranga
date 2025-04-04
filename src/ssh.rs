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
use tracing::{debug, error};

use crate::{
    animals::get_animal_name,
    tunnel::{self, Tunnel, Tunnels},
};

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

    async fn full_address(&self, address: &str) -> Option<String> {
        let all_tunnels = self.all_tunnels.read().await;

        let address = if address == "localhost" {
            loop {
                let address = get_animal_name();
                if !all_tunnels.contains_key(address) {
                    break address;
                }
            }
        } else {
            if all_tunnels.contains_key(address) {
                return None;
            }
            address
        };

        Some(format!("{address}.tunnel.huizinga.dev"))
    }
}

impl russh::server::Handler for Handler {
    type Error = russh::Error;

    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        debug!("channel_open_session");

        let Some(mut rx) = self.rx.take() else {
            return Err(russh::Error::Inconsistent);
        };

        tokio::spawn(async move {
            debug!("Waiting for message to send to client...");
            loop {
                let message = rx.recv().await;
                debug!("Message!");

                let Some(message) = message else { break };

                if channel.data(message.as_ref()).await.is_err() {
                    break;
                }
            }

            debug!("Ending receive task");
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
        debug!("exec_request data {data:?}");

        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        debug!("{address}:{port}");

        let Some(full_address) = self.full_address(address).await else {
            self.send(&format!("{port} => FAILED ({address} already in use)\r\n"));
            return Ok(false);
        };

        self.tunnels.insert(full_address.clone());
        self.all_tunnels.write().await.insert(
            full_address.clone(),
            Tunnel::new(session.handle(), address, *port),
        );

        self.send(&format!("{port} => https://{full_address}\r\n"));

        Ok(true)
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        let tunnels = self.tunnels.clone();
        let all_tunnels = self.all_tunnels.clone();

        tokio::spawn(async move {
            let mut all_tunnels = all_tunnels.write().await;
            for tunnel in tunnels {
                all_tunnels.remove(&tunnel);
            }

            debug!("{all_tunnels:?}");
        });
    }
}

pub struct Server {
    tunnels: Tunnels,
}

impl Server {
    pub fn new() -> Self {
        Server {
            tunnels: tunnel::new(),
        }
    }

    pub fn tunnels(&self) -> Tunnels {
        self.tunnels.clone()
    }

    pub fn run(
        &mut self,
        key: PrivateKey,
        addr: impl ToSocketAddrs + Send,
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

        async move { self.run_on_address(config, addr).await }
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
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
        error!("Session error: {error:#?}");
    }
}
