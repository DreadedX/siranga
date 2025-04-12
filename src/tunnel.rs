use bytes::Bytes;
use http_body_util::{BodyExt, Empty, combinators::BoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::Incoming,
    client::conn::http1::Builder,
    header::{self, HOST},
    service::Service,
};
use hyper_util::rt::TokioIo;
use std::{
    collections::{HashMap, hash_map::Entry},
    ops::Deref,
    pin::Pin,
    sync::Arc,
};
use tracing::{debug, error, trace, warn};

use russh::{
    Channel,
    server::{Handle, Msg},
};
use tokio::sync::RwLock;

use crate::{
    animals::get_animal_name,
    auth::{AuthStatus, ForwardAuth},
    helper::response,
};

pub mod tui;

#[derive(Debug, Clone)]
pub enum TunnelAccess {
    Private(String),
    Protected,
    Public,
}

#[derive(Debug, Clone)]
pub struct Tunnel {
    handle: Handle,
    name: String,
    domain: Option<String>,
    port: u32,
    access: Arc<RwLock<TunnelAccess>>,
}

impl Tunnel {
    pub async fn open_tunnel(&self) -> Result<Channel<Msg>, russh::Error> {
        trace!(tunnel = self.name, "Opening tunnel");
        self.handle
            .channel_open_forwarded_tcpip(&self.name, self.port, &self.name, self.port)
            .await
    }

    pub async fn set_access(&self, access: TunnelAccess) {
        *self.access.write().await = access;
    }

    pub async fn is_public(&self) -> bool {
        matches!(*self.access.read().await, TunnelAccess::Public)
    }

    pub fn get_address(&self) -> Option<String> {
        self.domain
            .clone()
            .map(|domain| format!("{}.{domain}", self.name))
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

    pub async fn create_tunnel(
        &mut self,
        handle: Handle,
        name: impl Into<String>,
        port: u32,
        user: impl Into<String>,
    ) -> Tunnel {
        let mut tunnel = Tunnel {
            handle,
            name: name.into(),
            domain: Some(self.domain.clone()),
            port,
            access: Arc::new(RwLock::new(TunnelAccess::Private(user.into()))),
        };

        if tunnel.name == "localhost" {
            // NOTE: It is technically possible to become stuck in this loop.
            // However, that really only becomes a concern if a (very) high
            // number of tunnels is open at the same time.
            loop {
                tunnel.name = get_animal_name().into();
                if !self
                    .tunnels
                    .read()
                    .await
                    .contains_key(&tunnel.get_address().expect("domain is set"))
                {
                    break;
                }
                trace!(tunnel = tunnel.name, "Already in use, picking new name");
            }
        };

        self.add_tunnel(tunnel).await
    }

    async fn add_tunnel(&mut self, mut tunnel: Tunnel) -> Tunnel {
        let address = tunnel.get_address().expect("domain is set");
        if let Entry::Vacant(e) = self.tunnels.write().await.entry(address) {
            trace!(tunnel = tunnel.name, "Adding tunnel");
            e.insert(tunnel.clone());
        } else {
            trace!("Address already in use");
            tunnel.domain = None
        }

        tunnel
    }

    pub async fn remove_tunnel(&mut self, mut tunnel: Tunnel) -> Tunnel {
        let mut all_tunnels = self.tunnels.write().await;
        if let Some(address) = tunnel.get_address() {
            trace!(tunnel.name, "Removing tunnel");
            all_tunnels.remove(&address);
        }
        tunnel.domain = None;
        tunnel
    }

    pub async fn retry_tunnel(&mut self, tunnel: Tunnel) -> Tunnel {
        let mut tunnel = self.remove_tunnel(tunnel).await;
        tunnel.domain = Some(self.domain.clone());

        self.add_tunnel(tunnel).await
    }
}

impl Service<Request<Incoming>> for Tunnels {
    type Response = Response<BoxBody<Bytes, hyper::Error>>;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        trace!("{:#?}", req);

        let Some(authority) = req
            .uri()
            .authority()
            .as_ref()
            .map(|a| a.to_string())
            .or_else(|| {
                req.headers()
                    .get(HOST)
                    .and_then(|h| h.to_str().ok().map(|s| s.to_owned()))
            })
        else {
            let resp = response(
                StatusCode::BAD_REQUEST,
                "Missing or invalid authority or host header",
            );

            return Box::pin(async { Ok(resp) });
        };

        debug!(tunnel = authority, "Tunnel request");

        let s = self.clone();
        Box::pin(async move {
            let tunnels = s.tunnels.read().await;
            let Some(tunnel) = tunnels.get(&authority) else {
                debug!(tunnel = authority, "Unknown tunnel");
                let resp = response(StatusCode::NOT_FOUND, "Unknown tunnel");

                return Ok(resp);
            };

            if !matches!(tunnel.access.read().await.deref(), TunnelAccess::Public) {
                let user = match s.forward_auth.check_auth(req.headers()).await {
                    Ok(AuthStatus::Authenticated(user)) => user,
                    Ok(AuthStatus::Unauthenticated(location)) => {
                        let resp = Response::builder()
                            .status(StatusCode::FOUND)
                            .header(header::LOCATION, location)
                            .body(
                                Empty::new()
                                    // NOTE: I have NO idea why this is able to convert from Innfallible to hyper::Error
                                    .map_err(|never| match never {})
                                    .boxed(),
                            )
                            .expect("configuration should be valid");

                        return Ok(resp);
                    }
                    Ok(AuthStatus::Unauthorized) => {
                        let resp = response(
                            StatusCode::FORBIDDEN,
                            "You do not have permission to access this tunnel",
                        );

                        return Ok(resp);
                    }
                    Err(err) => {
                        error!("Unexpected error during authentication: {err}");
                        let resp = response(
                            StatusCode::FORBIDDEN,
                            "Unexpected error during authentication",
                        );

                        return Ok(resp);
                    }
                };

                trace!("Tunnel is getting accessed by {user:?}");

                if let TunnelAccess::Private(owner) = tunnel.access.read().await.deref() {
                    if !user.is(owner) {
                        let resp = response(
                            StatusCode::FORBIDDEN,
                            "You do not have permission to access this tunnel",
                        );

                        return Ok(resp);
                    }
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

            let resp = sender.send_request(req).await?;
            Ok(resp.map(|b| b.boxed()))
        })
    }
}
