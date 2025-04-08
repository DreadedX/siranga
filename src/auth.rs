use hyper::{
    HeaderMap, StatusCode,
    header::{self, HeaderName, HeaderValue, ToStrError},
};
use reqwest::redirect::Policy;
use tracing::{debug, error};

#[derive(Debug, Clone)]
pub struct ForwardAuth {
    address: String,
}

#[derive(Debug)]
pub struct User {
    username: String,
}

impl User {
    pub fn is(&self, username: impl AsRef<str>) -> bool {
        self.username.eq(username.as_ref())
    }
}

#[derive(Debug)]
pub enum AuthStatus {
    // Contains the value of the location header that will redirect the user to the login page
    Unauthenticated(HeaderValue),
    Authenticated(User),
    Unauthorized,
}

const REMOTE_USER: HeaderName = HeaderName::from_static("remote-user");

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("Http error: {0}")]
    Http(#[from] hyper::http::Error),
    #[error("Header '{0}' is missing from auth endpoint response")]
    MissingHeader(HeaderName),
    #[error("Header '{0}' received from auth endpoint is invalid: {1}")]
    InvalidHeader(HeaderName, ToStrError),
    #[error("Unexpected response from auth endpoint: {0:?}")]
    UnexpectedResponse(reqwest::Response),
}

impl ForwardAuth {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            address: endpoint.into(),
        }
    }

    pub async fn check_auth(
        &self,
        headers: &HeaderMap<HeaderValue>,
    ) -> Result<AuthStatus, AuthError> {
        let client = reqwest::ClientBuilder::new()
            .redirect(Policy::none())
            .build()?;

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

        let resp = client.get(&self.address).headers(headers).send().await?;

        let status_code = resp.status();
        if status_code == StatusCode::FOUND {
            let location = resp
                .headers()
                .get(header::LOCATION)
                .cloned()
                .ok_or(AuthError::MissingHeader(header::LOCATION))?;

            return Ok(AuthStatus::Unauthenticated(location));
        } else if status_code == StatusCode::FORBIDDEN {
            return Ok(AuthStatus::Unauthorized);
        } else if !status_code.is_success() {
            return Err(AuthError::UnexpectedResponse(resp));
        }

        let username = resp
            .headers()
            .get(REMOTE_USER)
            .ok_or(AuthError::MissingHeader(REMOTE_USER))?
            .to_str()
            .map_err(|err| AuthError::InvalidHeader(REMOTE_USER, err))?
            .to_owned();

        debug!("Connected user is: {username}");

        Ok(AuthStatus::Authenticated(User { username }))
    }
}
