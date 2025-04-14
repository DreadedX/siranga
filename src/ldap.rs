use ldap3::{LdapConnAsync, SearchEntry};
use russh::keys::PublicKey;

#[derive(Debug, Clone)]
pub struct Ldap {
    base: String,
    ldap: ldap3::Ldap,
}

#[derive(Debug, thiserror::Error)]
pub enum LdapError {
    #[error(transparent)]
    Ldap(#[from] ldap3::LdapError),
    #[error("Key error: {0}")]
    FailedToParseKey(#[from] russh::Error),
    #[error("Mising environment variable: {0}")]
    MissingEnvironmentVariable(&'static str),
    #[error("Mising environment variable: {0}")]
    CouldNotReadPasswordFile(#[from] std::io::Error),
}

impl Ldap {
    pub async fn start_from_env() -> Result<Ldap, LdapError> {
        let address = std::env::var("LDAP_ADDRESS")
            .map_err(|_| LdapError::MissingEnvironmentVariable("LDAP_ADDRESS"))?;
        let base = std::env::var("LDAP_BASE")
            .map_err(|_| LdapError::MissingEnvironmentVariable("LDAP_BASE"))?;
        let bind_dn = std::env::var("LDAP_BIND_DN")
            .map_err(|_| LdapError::MissingEnvironmentVariable("LDAP_BIND_DN"))?;

        let password = std::env::var("LDAP_PASSWORD_FILE").map_or_else(
            |_| {
                std::env::var("LDAP_PASSWORD")
                    .map_err(|_| LdapError::MissingEnvironmentVariable("LDAP_PASSWORD"))
            },
            |path| std::fs::read_to_string(path).map_err(|err| err.into()),
        )?;

        let (conn, mut ldap) = LdapConnAsync::new(&address).await?;
        ldap3::drive!(conn);

        ldap.simple_bind(&bind_dn, &password).await?.success()?;

        Ok(Self { base, ldap })
    }

    pub async fn get_ssh_keys(
        &mut self,
        user: impl AsRef<str>,
    ) -> Result<Vec<PublicKey>, LdapError> {
        Ok(self
            .ldap
            .search(
                &self.base,
                ldap3::Scope::Subtree,
                // TODO: Make this not hardcoded
                &format!("(uid={})", user.as_ref()),
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
