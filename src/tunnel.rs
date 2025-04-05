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

use crate::animals::get_animal_name;

#[derive(Debug, Clone)]
pub struct Tunnel {
    handle: Handle,
    address: String,
    port: u32,
}

impl Tunnel {
    pub fn new(handle: Handle, address: impl Into<String>, port: u32) -> Self {
        Self {
            handle,
            address: address.into(),
            port,
        }
    }

    pub async fn open_tunnel(&self) -> Result<Channel<Msg>, russh::Error> {
        self.handle
            .channel_open_forwarded_tcpip(&self.address, self.port, &self.address, self.port)
            .await
    }
}

#[derive(Debug, Clone)]
pub struct Tunnels(Arc<RwLock<HashMap<String, Tunnel>>>);

impl Tunnels {
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(HashMap::new())))
    }

    pub async fn add_tunnel(&mut self, address: &str, tunnel: Tunnel) -> Option<String> {
        let mut all_tunnels = self.0.write().await;

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

        let address = format!("{address}.tunnel.huizinga.dev");

        all_tunnels.insert(address.clone(), tunnel);

        Some(address)
    }

    pub async fn remove_tunnels(&mut self, tunnels: HashSet<String>) {
        let mut all_tunnels = self.0.write().await;
        for tunnel in tunnels {
            all_tunnels.remove(&tunnel);
        }
    }

    pub async fn get_tunnel(&self, address: &str) -> Option<Tunnel> {
        self.0.read().await.get(address).cloned()
    }
}

impl Default for Tunnels {
    fn default() -> Self {
        Self::new()
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
