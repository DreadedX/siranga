use std::{collections::HashSet, net::SocketAddr, pin::Pin, sync::Arc, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, combinators::BoxBody};
use hyper::{
    Request, Response, StatusCode, body::Incoming, client::conn::http1::Builder, header::HOST,
    service::Service,
};
use hyper_util::rt::TokioIo;
use russh::{
    ChannelId,
    keys::PrivateKey,
    server::{Auth, Msg, Server as _, Session},
};
use tokio::{
    net::ToSocketAddrs,
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};
use tracing::{debug, error, trace, warn};

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

        let tunnel = Tunnel::new(session.handle(), address, *port);
        let Some(address) = self.all_tunnels.add_tunnel(address, tunnel).await else {
            self.send(&format!("{port} => FAILED ({address} already in use)\r\n"));
            return Ok(false);
        };
        self.send(&format!("{port} => https://{address}\r\n"));
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
            tunnels: Tunnels::new(),
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

impl Service<Request<Incoming>> for Tunnels {
    type Response = Response<BoxBody<Bytes, hyper::Error>>;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        fn response(
            status_code: StatusCode,
            body: impl Into<String>,
        ) -> Response<BoxBody<Bytes, hyper::Error>> {
            Response::builder()
                .status(status_code)
                .body(Full::new(Bytes::from(body.into())))
                .unwrap()
                .map(|b| b.map_err(|never| match never {}).boxed())
        }

        trace!(?req);

        let Some(authority) = req
            .uri()
            .authority()
            .as_ref()
            .map(|a| a.to_string())
            .or_else(|| {
                req.headers()
                    .get(HOST)
                    .map(|h| h.to_str().unwrap().to_owned())
            })
        else {
            let resp = response(StatusCode::BAD_REQUEST, "Missing authority or host header");

            return Box::pin(async { Ok(resp) });
        };

        debug!("Request for {authority:?}");

        let tunnels = self.clone();
        Box::pin(async move {
            let Some(tunnel) = tunnels.get_tunnel(&authority).await else {
                let resp = response(StatusCode::NOT_FOUND, "Unknown tunnel");

                return Ok::<_, hyper::Error>(resp);
            };

            debug!("Opening channel");
            let channel = match tunnel.open_tunnel().await {
                Ok(channel) => channel,
                Err(err) => {
                    warn!("Failed to open tunnel: {err}");
                    let resp = response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to open tunnel");

                    return Ok::<_, hyper::Error>(resp);
                }
            };
            let io = TokioIo::new(channel.into_stream());

            let (mut sender, conn) = Builder::new()
                .preserve_header_case(true)
                .title_case_headers(true)
                .handshake(io)
                .await?;

            tokio::spawn(async move {
                if let Err(err) = conn.await {
                    warn!("Connection failed: {err}");
                }
            });

            let resp = sender.send_request(req).await.unwrap();
            Ok(resp.map(|b| b.boxed()))
        })
    }
}
