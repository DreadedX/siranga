use std::{net::SocketAddr, path::Path};

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
use tokio::net::TcpListener;
use tracing::{debug, trace, warn};
use tracing_subscriber::{EnvFilter, Registry, layer::SubscriberExt, util::SubscriberInitExt};
use tunnel_rs::ssh::Server;

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

    let mut ssh = Server::new();
    let tunnels = ssh.tunnels();
    tokio::spawn(async move { ssh.run(key, ("0.0.0.0", 2222)).await });

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
                let channel = match tunnel.open_tunnel().await {
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
