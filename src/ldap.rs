use ldap3::{LdapConnAsync, SearchEntry};
use leon::{Template, vals};
use russh::keys::PublicKey;
use tokio::select;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

#[derive(Debug, Clone)]
pub struct Ldap {
    base: String,
    ldap: ldap3::Ldap,
    search_filter: String,
}

#[derive(Debug, thiserror::Error)]
pub enum LdapError {
    #[error(transparent)]
    Ldap(#[from] ldap3::LdapError),
    #[error("Key error: {0}")]
    FailedToParseKey(#[from] russh::Error),
    #[error("Missing environment variable: {0}")]
    MissingEnvironmentVariable(&'static str),
    #[error("Could not read password file: {0}")]
    CouldNotReadPasswordFile(#[from] std::io::Error),
    #[error("Failed to parse search filter: {0}")]
    FailedToParseSearchFilter(#[from] leon::ParseError),
    #[error("Failed to render search filter: {0}")]
    FailedToRenderSearchFilter(#[from] leon::RenderError),
}

impl Ldap {
    pub async fn start_from_env(
        token: CancellationToken,
    ) -> Result<(Ldap, JoinHandle<()>), LdapError> {
        let address = std::env::var("LDAP_ADDRESS")
            .map_err(|_| LdapError::MissingEnvironmentVariable("LDAP_ADDRESS"))?;
        let base = std::env::var("LDAP_BASE")
            .map_err(|_| LdapError::MissingEnvironmentVariable("LDAP_BASE"))?;
        let bind_dn = std::env::var("LDAP_BIND_DN")
            .map_err(|_| LdapError::MissingEnvironmentVariable("LDAP_BIND_DN"))?;
        let search_filter = std::env::var("LDAP_SEARCH_FILTER")
            .map_err(|_| LdapError::MissingEnvironmentVariable("LDAP_SEARCH_FILTER"))?;

        let password = std::env::var("LDAP_PASSWORD_FILE").map_or_else(
            |_| {
                std::env::var("LDAP_PASSWORD").map_err(|_| {
                    LdapError::MissingEnvironmentVariable("LDAP_PASSWORD or LDAP_PASSWORD_FILE")
                })
            },
            |path| {
                std::fs::read_to_string(path)
                    .map(|v| v.trim().into())
                    .map_err(|err| err.into())
            },
        )?;

        let (conn, mut ldap) = LdapConnAsync::new(&address).await?;
        let handle = tokio::spawn(async move {
            select! {
                res = conn.drive() => {
                    if let Err(err) = res {
                        error!("LDAP connection error: {}", err);
                    } else {
                        error!("LDAP connection lost");
                        token.cancel();
                    }
                }
                _ = token.cancelled() => {
                    debug!("Graceful shutdown");
                }
            }
        });

        ldap.simple_bind(&bind_dn, &password).await?.success()?;

        Ok((
            Self {
                base,
                ldap,
                search_filter,
            },
            handle,
        ))
    }

    pub async fn get_ssh_keys(
        &mut self,
        user: impl AsRef<str>,
    ) -> Result<Vec<PublicKey>, LdapError> {
        let search_filter = Template::parse(&self.search_filter)?;

        let search_filter = search_filter.render(&&vals(|key| {
            if key == "username" {
                Some(user.as_ref().to_string().into())
            } else {
                None
            }
        }))?;

        debug!("search_filter = {search_filter}");

        Ok(self
            .ldap
            .search(
                &self.base,
                ldap3::Scope::Subtree,
                // TODO: Make this not hardcoded
                &search_filter,
                vec!["sshkeys"],
            )
            .await?
            .success()?
            .0
            .into_iter()
            .map(SearchEntry::construct)
            .flat_map(|entry| {
                entry
                    .attrs
                    .into_values()
                    .flat_map(|keys| keys.into_iter().map(|key| PublicKey::from_openssh(&key)))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(russh::Error::from)?)
    }
}
