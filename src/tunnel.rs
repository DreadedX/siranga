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
use indexmap::IndexMap;
use std::{collections::HashMap, ops::Deref, pin::Pin, sync::Arc};
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
    port: u32,
    access: Arc<RwLock<TunnelAccess>>,
}

impl Tunnel {
    pub fn new(handle: Handle, name: impl Into<String>, port: u32, access: TunnelAccess) -> Self {
        Self {
            handle,
            name: name.into(),
            port,
            access: Arc::new(RwLock::new(access)),
        }
    }

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

    pub async fn add_tunnel(&mut self, address: &str, tunnel: Tunnel) -> (bool, String) {
        let mut all_tunnels = self.tunnels.write().await;

        let address = if address == "localhost" {
            // NOTE: It is technically possible to become stuck in this loop.
            // However, that really only becomes a concern if a (very) high
            // number of tunnels is open at the same time.
            loop {
                let address = get_animal_name();
                let address = format!("{address}.{}", self.domain);
                if !all_tunnels.contains_key(&address) {
                    break address;
                }
            }
        } else {
            let address = format!("{address}.{}", self.domain);
            if all_tunnels.contains_key(&address) {
                return (false, address);
            }
            address
        };

        trace!(tunnel = address, "Adding tunnel");
        all_tunnels.insert(address.clone(), tunnel);

        (true, address)
    }

    pub async fn remove_tunnels(&mut self, tunnels: &IndexMap<String, Option<Tunnel>>) {
        let mut all_tunnels = self.tunnels.write().await;
        for (address, tunnel) in tunnels {
            if tunnel.is_some() {
                trace!(address, "Removing tunnel");
                all_tunnels.remove(address);
            }
        }
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
