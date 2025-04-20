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
use hyper::header::{self, HOST, UPGRADE};
use hyper::{Request, Response, StatusCode, client, server};
use hyper_util::rt::TokioIo;
use response::response;
use tokio::net::TcpListener;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, trace, warn};

use crate::tunnel::{Registry, TunnelAccess};

#[derive(Debug, Clone)]
pub struct Service {
    registry: Registry,
    auth: ForwardAuth,
    task_tracker: TaskTracker,
}

pub fn empty() -> BoxBody<Bytes, hyper::Error> {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

fn copy_request_parts<T>(req: Request<T>) -> (Request<T>, Request<BoxBody<Bytes, hyper::Error>>) {
    let (parts, body) = req.into_parts();
    let req = Request::from_parts(parts.clone(), body);
    let forwarded_req = Request::from_parts(parts, empty());

    (req, forwarded_req)
}

fn copy_response_parts<T>(
    resp: Response<T>,
) -> (Response<T>, Response<BoxBody<Bytes, hyper::Error>>) {
    let (parts, body) = resp.into_parts();
    let resp = Response::from_parts(parts.clone(), body);
    let forwarded_resp = Response::from_parts(parts, empty());

    (resp, forwarded_resp)
}

impl Service {
    pub fn new(registry: Registry, auth: ForwardAuth) -> Self {
        Self {
            registry,
            auth,
            task_tracker: Default::default(),
        }
    }

    pub async fn handle_connection(&self, listener: &TcpListener) -> std::io::Result<()> {
        let (stream, _) = listener.accept().await?;

        let io = TokioIo::new(stream);
        let connection = server::conn::http1::Builder::new()
            .preserve_header_case(true)
            .title_case_headers(true)
            .serve_connection(io, self.clone())
            .with_upgrades();

        self.task_tracker.spawn(async move {
            if let Err(err) = connection.await {
                error!("Failed to serve connection: {err:?}");
            }
        });

        Ok(())
    }

    pub async fn serve(self, listener: TcpListener, token: CancellationToken) {
        loop {
            select! {
                res = self.handle_connection(&listener) => {
                    if let Err(err) = res {
                        error!("Failed to accept connection: {err}")
                    }
                }
                _ = token.cancelled() => {
                    break;
                }
            }
        }

        debug!(
            "Waiting for {} connections to close",
            self.task_tracker.len()
        );
        self.task_tracker.close();
        self.task_tracker.wait().await;

        debug!("Graceful shutdown");
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

        let s = self.clone();
        Box::pin(async move {
            let Some(entry) = s.registry.get(&authority).await else {
                debug!(tunnel = authority, "Unknown tunnel");
                let resp = response(StatusCode::NOT_FOUND, "Unknown tunnel");

                return Ok(resp);
            };

            if !entry.is_public().await {
                let user = match s.auth.check(req.method(), req.headers()).await {
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

            let (mut sender, conn) = client::conn::http1::Builder::new()
                .preserve_header_case(true)
                .title_case_headers(true)
                .handshake(io)
                .await?;

            let conn = conn.with_upgrades();
            s.task_tracker.spawn(async move {
                if let Err(err) = conn.await {
                    warn!(runnel = authority, "Connection failed: {err}");
                }
            });

            let (mut req, forwarded_req) = copy_request_parts(req);

            let resp = sender.send_request(forwarded_req).await?;

            if req.headers().contains_key(UPGRADE)
                && req.headers().get(UPGRADE) == resp.headers().get(UPGRADE)
            {
                let (mut resp, forwarded_resp) = copy_response_parts(resp);

                debug!("UPGRADE established");
                match hyper::upgrade::on(&mut resp).await {
                    Ok(upgraded_resp) => {
                        s.task_tracker.spawn(async move {
                            match hyper::upgrade::on(&mut req).await {
                                Ok(upgraded_req) => {
                                    let mut upgraded_req = TokioIo::new(upgraded_req);
                                    let mut upgraded_resp = TokioIo::new(upgraded_resp);

                                    match tokio::io::copy_bidirectional(
                                        &mut upgraded_req,
                                        &mut upgraded_resp,
                                    )
                                    .await
                                    {
                                        Ok((rx, tx)) => {
                                            debug!(
                                                "Received {rx} bytes and send {tx} bytes over upgraded tunnel"
                                            );
                                        }
                                        Err(err) => {
                                            // Likely due to channel being closed
                                            // TODO: Show warning if not channel closed, otherwise ignore
                                            debug!("Upgraded connection error: {err:?}");
                                        }
                                    }
                                }
                                Err(err) => {
                                    error!("Failed to upgrade: {err}");
                                }
                            }
                        });

                        return Ok(forwarded_resp.map(|b| b.boxed()));
                    }
                    Err(err) => {
                        error!("Failed to upgrade req: {err}");
                        return Ok(response(StatusCode::BAD_REQUEST, "Failed to upgrade"));
                    }
                }
            }

            trace!("{resp:#?}");

            Ok(resp.map(|b| b.boxed()))
        })
    }
}
