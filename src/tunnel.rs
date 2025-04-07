use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, combinators::BoxBody};
use hyper::{
    Request, Response, StatusCode, body::Incoming, client::conn::http1::Builder, header::HOST,
    service::Service,
};
use hyper_util::rt::TokioIo;
use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::Arc,
};
use tracing::{debug, trace, warn};

use russh::{
    Channel,
    server::{Handle, Msg},
};
use tokio::sync::RwLock;

use crate::{
    animals::get_animal_name,
    auth::{
        AuthStatus::{Authenticated, Unauthenticated},
        ForwardAuth,
    },
};

#[derive(Debug, Clone)]
pub enum TunnelAccess {
    Private(String),
    Public,
}

#[derive(Debug, Clone)]
pub struct Tunnel {
    handle: Handle,
    address: String,
    port: u32,
    access: TunnelAccess,
}

impl Tunnel {
    pub fn new(
        handle: Handle,
        address: impl Into<String>,
        port: u32,
        access: TunnelAccess,
    ) -> Self {
        Self {
            handle,
            address: address.into(),
            port,
            access,
        }
    }

    pub async fn open_tunnel(&self) -> Result<Channel<Msg>, russh::Error> {
        trace!(tunnel = self.address, "Opening tunnel");
        self.handle
            .channel_open_forwarded_tcpip(&self.address, self.port, &self.address, self.port)
            .await
    }
}

#[derive(Debug, Clone)]
pub struct Tunnels {
    tunnels: Arc<RwLock<HashMap<String, Tunnel>>>,
    domain: String,
    forward_auth: ForwardAuth,
}

impl Tunnels {
    pub fn new(domain: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            tunnels: Arc::new(RwLock::new(HashMap::new())),
            domain: domain.into(),
            forward_auth: ForwardAuth::new(endpoint),
        }
    }

    pub async fn add_tunnel(&mut self, address: &str, tunnel: Tunnel) -> Option<String> {
        let mut all_tunnels = self.tunnels.write().await;

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

        let address = format!("{address}.{}", self.domain);

        trace!(tunnel = address, "Adding tunnel");
        all_tunnels.insert(address.clone(), tunnel);

        Some(address)
    }

    pub async fn remove_tunnels(&mut self, tunnels: HashSet<String>) {
        let mut all_tunnels = self.tunnels.write().await;
        for tunnel in tunnels {
            trace!(tunnel, "Removing tunnel");
            all_tunnels.remove(&tunnel);
        }
    }

    pub async fn set_access(&mut self, tunnel: &str, access: TunnelAccess) {
        if let Some(tunnel) = self.tunnels.write().await.get_mut(tunnel) {
            tunnel.access = access;
        };
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

        debug!(tunnel = authority, "Request");

        let s = self.clone();
        Box::pin(async move {
            let tunnels = s.tunnels.read().await;
            let Some(tunnel) = tunnels.get(&authority) else {
                debug!(tunnel = authority, "Unknown tunnel");
                let resp = response(StatusCode::NOT_FOUND, "Unknown tunnel");

                return Ok(resp);
            };

            if let TunnelAccess::Private(owner) = &tunnel.access {
                let user = match s.forward_auth.check_auth(req.headers()).await {
                    Authenticated(user) => user,
                    Unauthenticated(response) => return Ok(response),
                };

                trace!("Tunnel owned by {owner} is getting accessed by {user}");

                if !user.eq(owner) {
                    let resp = response(
                        StatusCode::FORBIDDEN,
                        "You do not have permission to access this tunnel",
                    );

                    return Ok(resp);
                }
            }

            let channel = match tunnel.open_tunnel().await {
                Ok(channel) => channel,
                Err(err) => {
                    warn!(tunnel = authority, "Failed to open tunnel: {err}");
                    let resp = response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to open tunnel");

                    return Ok(resp);
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
                    warn!(runnel = authority, "Connection failed: {err}");
                }
            });

            let resp = sender.send_request(req).await.unwrap();
            Ok(resp.map(|b| b.boxed()))
        })
    }
}
