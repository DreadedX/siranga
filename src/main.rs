use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::Path,
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{
    Method, Request, Response, StatusCode,
    client::conn::http1::Builder,
    header::HOST,
    server::conn::http1::{self},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use rand::rngs::OsRng;
use russh::{
    ChannelId,
    server::{self, Handle, Server as _},
};
use tokio::{
    net::TcpListener,
    sync::{
        RwLock,
        mpsc::{self, UnboundedReceiver, UnboundedSender},
    },
};
use tracing::{debug, error, trace, warn};
use tracing_subscriber::{EnvFilter, Registry, layer::SubscriberExt, util::SubscriberInitExt};
use tunnel_rs::animals::get_animal_name;

fn full<T: Into<Bytes>>(chunk: T) -> BoxBody<Bytes, hyper::Error> {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}

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

    let mut sh = Server::new();

    let tunnels = sh.tunnels.clone();
    tokio::spawn(async move { sh.run_on_address(config, ("0.0.0.0", 2222)).await });

    let service = service_fn(move |req: Request<_>| {
        let tunnels = tunnels.clone();
        async move {
            if req.method() == Method::CONNECT {
                let mut resp = Response::new(full("CONNECT not supported"));
                *resp.status_mut() = StatusCode::BAD_REQUEST;

                Ok::<_, hyper::Error>(resp)
            } else {
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
                    let mut resp = Response::new(full("Missing authority or host header"));
                    *resp.status_mut() = StatusCode::BAD_REQUEST;

                    return Ok::<_, hyper::Error>(resp);
                };

                debug!("Request for {authority:?}");

                let Some(tunnel) = tunnels.read().await.get(&authority).cloned() else {
                    let mut resp = Response::new(full(format!("Unknown tunnel: {authority}")));
                    *resp.status_mut() = StatusCode::NOT_FOUND;

                    return Ok::<_, hyper::Error>(resp);
                };

                debug!("Opening channel");
                let channel = match tunnel
                    .handle
                    .channel_open_forwarded_tcpip(
                        &tunnel.address,
                        tunnel.port,
                        &tunnel.address,
                        tunnel.port,
                    )
                    .await
                {
                    Ok(channel) => channel,
                    Err(err) => {
                        warn!("Failed to tunnel: {err}");
                        let mut resp = Response::new(full("Failed to open tunnel"));
                        *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;

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
            }
        }
    });

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = TcpListener::bind(addr).await.unwrap();
    loop {
        let (stream, _) = listener.accept().await.unwrap();
        let io = TokioIo::new(stream);

        let service = service.clone();
        tokio::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .preserve_header_case(true)
                .title_case_headers(true)
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                warn!("Failed to serve connection: {err:?}");
            }
        });
    }
}

#[derive(Debug, Clone)]
struct Tunnel {
    handle: Handle,
    address: String,
    port: u32,
}

type Tunnels = Arc<RwLock<HashMap<String, Tunnel>>>;

struct Server {
    tunnels: Tunnels,
}

impl Server {
    fn new() -> Self {
        Server {
            tunnels: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl server::Server for Server {
    type Handler = Handler;

    fn new_client(&mut self, _peer_addr: Option<std::net::SocketAddr>) -> Self::Handler {
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();

        Handler {
            tx,
            rx: Some(rx),
            all_tunnels: self.tunnels.clone(),
            tunnels: HashSet::new(),
        }
    }

    fn handle_session_error(&mut self, error: <Self::Handler as server::Handler>::Error) {
        error!("Session error: {error:#?}");
    }
}

struct Handler {
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

impl server::Handler for Handler {
    type Error = russh::Error;

    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<server::Msg>,
        _session: &mut server::Session,
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
    ) -> Result<server::Auth, Self::Error> {
        debug!("Login from {user}");

        // TODO: Get ssh keys associated with user from ldap
        Ok(server::Auth::Accept)
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut server::Session,
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
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        debug!("exec_request data {data:?}");

        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut server::Session,
    ) -> Result<bool, Self::Error> {
        debug!("{address}:{port}");

        let Some(full_address) = self.full_address(address).await else {
            self.send(&format!("{port} => FAILED ({address} already in use)\r\n"));
            return Ok(false);
        };

        self.tunnels.insert(full_address.clone());
        self.all_tunnels.write().await.insert(
            full_address.clone(),
            Tunnel {
                handle: session.handle(),
                address: address.into(),
                port: *port,
            },
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
