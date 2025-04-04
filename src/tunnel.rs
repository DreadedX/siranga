use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use russh::{
    Channel,
    server::{Handle, Msg},
};
use tokio::sync::RwLock;

use crate::animals::get_animal_name;

#[derive(Debug, Clone)]
pub struct Tunnel {
    handle: Handle,
    address: String,
    port: u32,
}

impl Tunnel {
    pub fn new(handle: Handle, address: impl Into<String>, port: u32) -> Self {
        Self {
            handle,
            address: address.into(),
            port,
        }
    }

    pub async fn open_tunnel(&self) -> Result<Channel<Msg>, russh::Error> {
        self.handle
            .channel_open_forwarded_tcpip(&self.address, self.port, &self.address, self.port)
            .await
    }
}

#[derive(Debug, Clone)]
pub struct Tunnels(Arc<RwLock<HashMap<String, Tunnel>>>);

impl Tunnels {
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(HashMap::new())))
    }

    pub async fn add_tunnel(&mut self, address: &str, tunnel: Tunnel) -> Option<String> {
        let mut all_tunnels = self.0.write().await;

        let address = if address == "localhost" {
            loop {
                let address = get_animal_name();
                if !all_tunnels.contains_key(address) {
                    break address;
                }
            }
        } else {
            if all_tunnels.contains_key(address) {
                return None;
            }
            address
        };

        let address = format!("{address}.tunnel.huizinga.dev");

        all_tunnels.insert(address.clone(), tunnel);

        Some(address)
    }

    pub async fn remove_tunnels(&mut self, tunnels: HashSet<String>) {
        let mut all_tunnels = self.0.write().await;
        for tunnel in tunnels {
            all_tunnels.remove(&tunnel);
        }
    }

    pub async fn get_tunnel(&self, address: &str) -> Option<Tunnel> {
        self.0.read().await.get(address).cloned()
    }
}

impl Default for Tunnels {
    fn default() -> Self {
        Self::new()
    }
}
