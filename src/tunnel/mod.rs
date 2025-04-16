mod registry;
mod tui;

use registry::RegistryEntry;
use std::sync::Arc;
use tracing::trace;

use russh::server::Handle;
use tokio::sync::{RwLock, RwLockReadGuard};

use crate::{stats::Stats, wrapper::Wrapper};

pub use registry::Registry;

#[derive(Debug, Clone)]
pub enum TunnelAccess {
    Private(String),
    Protected,
    Public,
}

#[derive(Debug, Clone)]
pub struct TunnelInner {
    handle: Handle,
    internal_address: String,
    port: u32,
    access: Arc<RwLock<TunnelAccess>>,
    stats: Arc<Stats>,
}

impl TunnelInner {
    pub async fn open(&self) -> Result<Wrapper, russh::Error> {
        trace!("Opening tunnel");
        self.stats.add_connection();
        let channel = self
            .handle
            .channel_open_forwarded_tcpip(
                &self.internal_address,
                self.port,
                &self.internal_address,
                self.port,
            )
            .await?;

        Ok(Wrapper::new(channel.into_stream(), self.stats.clone()))
    }

    pub async fn is_public(&self) -> bool {
        matches!(*self.access.read().await, TunnelAccess::Public)
    }

    pub async fn get_access(&self) -> RwLockReadGuard<'_, TunnelAccess> {
        self.access.read().await
    }
}

#[derive(Debug)]
pub struct Tunnel {
    inner: TunnelInner,

    registry: Registry,
    registry_entry: RegistryEntry,
}

impl Tunnel {
    pub async fn create(
        registry: &mut Registry,
        handle: Handle,
        internal_address: impl Into<String>,
        port: u32,
        access: TunnelAccess,
    ) -> Self {
        let mut tunnel = Self {
            inner: TunnelInner {
                handle,
                internal_address: internal_address.into(),
                port,
                access: Arc::new(RwLock::new(access)),
                stats: Default::default(),
            },
            registry: registry.clone(),
            registry_entry: RegistryEntry::new(registry.clone()),
        };

        registry.register(&mut tunnel).await;

        tunnel
    }

    pub async fn set_access(&self, access: TunnelAccess) {
        *self.inner.access.write().await = access;
    }

    pub fn get_address(&self) -> Option<&String> {
        self.registry_entry.get_address()
    }

    pub async fn set_name(&mut self, name: impl Into<String>) {
        let mut registry = self.registry.clone();
        registry.rename(self, name).await;
    }

    pub async fn retry(&mut self) {
        let mut registry = self.registry.clone();
        registry.register(self).await;
    }
}
