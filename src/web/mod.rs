mod auth;
mod response;

use std::ops::Deref;
use std::pin::Pin;

use auth::AuthStatus;
pub use auth::ForwardAuth;
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt as _, Empty};
use hyper::body::Incoming;
use hyper::client::conn::http1::Builder;
use hyper::header::{self, HOST};
use hyper::{Request, Response, StatusCode};
use response::response;
use tracing::{debug, error, trace, warn};

use crate::tunnel::{Registry, TunnelAccess};

#[derive(Debug, Clone)]
pub struct Service {
    registry: Registry,
    auth: ForwardAuth,
}

impl Service {
    pub fn new(registry: Registry, auth: ForwardAuth) -> Self {
        Self { registry, auth }
    }
}

impl hyper::service::Service<Request<Incoming>> for Service {
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

        let registry = self.registry.clone();
        let auth = self.auth.clone();
        Box::pin(async move {
            let Some(entry) = registry.get(&authority).await else {
                debug!(tunnel = authority, "Unknown tunnel");
                let resp = response(StatusCode::NOT_FOUND, "Unknown tunnel");

                return Ok(resp);
            };

            if !entry.is_public().await {
                let user = match auth.check(req.method(), req.headers()).await {
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

                if let TunnelAccess::Private(owner) = entry.get_access().await.deref() {
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
