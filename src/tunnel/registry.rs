use std::{
    collections::{HashMap, hash_map::Entry},
    sync::Arc,
};

use tokio::sync::RwLock;
use tracing::trace;

use crate::{Tunnel, animals::get_animal_name};

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
}

impl Registry {
    pub fn new(domain: impl Into<String>) -> Self {
        Self {
            tunnels: Arc::new(RwLock::new(HashMap::new())),
            domain: domain.into(),
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

    pub async fn get(&self, address: &str) -> Option<TunnelInner> {
        self.tunnels.read().await.get(address).cloned()
    }
}
