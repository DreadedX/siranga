use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, combinators::BoxBody};
use hyper::{
    HeaderMap, Response,
    header::{self, HeaderValue},
};
use reqwest::redirect::Policy;
use tracing::debug;

pub enum AuthStatus {
    Authenticated(String),
    Unauthenticated(Response<BoxBody<Bytes, hyper::Error>>),
}

#[derive(Debug, Clone)]
pub struct ForwardAuth {
    address: String,
}

impl ForwardAuth {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            address: endpoint.into(),
        }
    }

    pub async fn check_auth(&self, headers: &HeaderMap<HeaderValue>) -> AuthStatus {
        let client = reqwest::ClientBuilder::new()
            .redirect(Policy::none())
            .build()
            .unwrap();

        let headers = headers
            .clone()
            .into_iter()
            .filter_map(|(key, value)| {
                if let Some(key) = key
                    && key != header::CONTENT_LENGTH
                    && key != header::HOST
                {
                    Some((key, value))
                } else {
                    None
                }
            })
            .collect();

        debug!("{headers:#?}");

        let resp = client
            .get(&self.address)
            .headers(headers)
            .send()
            .await
            .unwrap();

        let status_code = resp.status();
        if !status_code.is_success() {
            debug!("{:#?}", resp.headers());
            let location = resp.headers().get(header::LOCATION).unwrap().clone();
            let body = resp.bytes().await.unwrap();
            let resp = Response::builder()
                .status(status_code)
                .header(header::LOCATION, location)
                .body(Full::new(body))
                .unwrap()
                .map(|b| b.map_err(|never| match never {}).boxed());

            return AuthStatus::Unauthenticated(resp);
        }

        debug!("{:#?}", resp.headers());
        let user = resp
            .headers()
            .get("remote-user")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        debug!("{}", resp.text().await.unwrap());

        debug!("Logged in as user: {user}");

        AuthStatus::Authenticated(user)
    }
}
