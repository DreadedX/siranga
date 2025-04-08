use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, combinators::BoxBody};
use hyper::{Response, StatusCode};

pub fn response(
    status_code: StatusCode,
    body: impl Into<String>,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    Response::builder()
        .status(status_code)
        .body(Full::new(Bytes::from(body.into())))
        .expect("all configuration should be valid")
        .map(|b| b.map_err(|never| match never {}).boxed())
}
