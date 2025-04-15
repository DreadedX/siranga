use std::{
    collections::{HashMap, hash_map::Entry},
    ops::Deref,
    pin::Pin,
    sync::Arc,
};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty, combinators::BoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::Incoming,
    client::conn::http1::Builder,
    header::{self, HOST},
    service::Service,
};
use tokio::sync::RwLock;
use tracing::{debug, error, trace, warn};

use crate::{
    Tunnel,
    animals::get_animal_name,
    auth::{AuthStatus, ForwardAuth},
    helper::response,
    tunnel::TunnelAccess,
};

use super::TunnelInner;

#[derive(Debug)]
pub struct RegistryEntry {
    registry: Registry,
    name: String,
    address: Option<String>,
}

impl RegistryEntry {
    pub fn new(registry: Registry) -> Self {
        Self {
            registry,
            name: Default::default(),
            address: Default::default(),
        }
    }

    pub fn get_address(&self) -> Option<&String> {
        self.address.as_ref()
    }

    pub fn get_name(&self) -> &str {
        &self.name
    }
}

impl Drop for RegistryEntry {
    fn drop(&mut self) {
        trace!(
            name = self.name,
            address = self.address,
            "Dropping registry entry"
        );

        if let Some(address) = self.address.take() {
            let registry = self.registry.clone();
            tokio::spawn(async move {
                registry.tunnels.write().await.remove(&address);
            });
        }
    }
}

#[derive(Debug, Clone)]
pub struct Registry {
    tunnels: Arc<RwLock<HashMap<String, TunnelInner>>>,
    domain: String,
    auth: ForwardAuth,
}

impl Registry {
    pub fn new(domain: impl Into<String>, auth: ForwardAuth) -> Self {
        Self {
            tunnels: Arc::new(RwLock::new(HashMap::new())),
            domain: domain.into(),
            auth,
        }
    }

    fn address(&self, name: impl AsRef<str>) -> String {
        format!("{}.{}", name.as_ref(), self.domain)
    }

    async fn generate_tunnel_name(&self) -> String {
        // NOTE: It is technically possible to become stuck in this loop.
        // However, that really only becomes a concern if a (very) high
        // number of tunnels is open at the same time.
        loop {
            let name = get_animal_name();
            if !self.tunnels.read().await.contains_key(&self.address(name)) {
                break name.into();
            }
            trace!(name, "Already in use, picking new name");
        }
    }

    pub(super) async fn register(&mut self, tunnel: &mut Tunnel) {
        if tunnel.registry_entry.name.is_empty() {
            if tunnel.inner.internal_address == "localhost" {
                tunnel.registry_entry.name = self.generate_tunnel_name().await;
            } else {
                tunnel.registry_entry.name = tunnel.inner.internal_address.clone();
            }
        }

        trace!(
            name = tunnel.registry_entry.name,
            "Attempting to register tunnel"
        );

        if tunnel.registry_entry.address.is_some() {
            trace!(name = tunnel.registry_entry.name, "Already registered");
            return;
        }

        let address = self.address(&tunnel.registry_entry.name);

        if let Entry::Vacant(e) = self.tunnels.write().await.entry(address.clone()) {
            tunnel.registry_entry.address = Some(address);
            e.insert(tunnel.inner.clone());
        } else {
            trace!(name = tunnel.registry_entry.name, "Address already in use");
            tunnel.registry_entry.address = None;
        }
    }

    pub(super) async fn rename(&mut self, tunnel: &mut Tunnel, name: impl Into<String>) {
        trace!(name = tunnel.registry_entry.name, "Renaming tunnel");

        if let Some(address) = tunnel.registry_entry.address.take() {
            self.tunnels.write().await.remove(&address);
        }

        tunnel.registry_entry.name = name.into();
        self.register(tunnel).await;
    }
}

impl Service<Request<Incoming>> for Registry {
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

        debug!(authority, "Tunnel request");

        let s = self.clone();
        Box::pin(async move {
            let Some(entry) = s.tunnels.read().await.get(&authority).cloned() else {
                debug!(tunnel = authority, "Unknown tunnel");
                let resp = response(StatusCode::NOT_FOUND, "Unknown tunnel");

                return Ok(resp);
            };

            if !matches!(entry.access.read().await.deref(), TunnelAccess::Public) {
                let user = match s.auth.check_auth(req.method(), req.headers()).await {
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

                if let TunnelAccess::Private(owner) = entry.access.read().await.deref() {
                    if !user.is(owner) {
                        let resp = response(
                            StatusCode::FORBIDDEN,
                            "You do not have permission to access this tunnel",
                        );

                        return Ok(resp);
                    }
                }
            }

            let io = match entry.open().await {
                Ok(io) => io,
                Err(err) => {
                    warn!(tunnel = authority, "Failed to open tunnel: {err}");
                    let resp = response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to open tunnel");

                    return Ok(resp);
                }
            };

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
