use std::{collections::HashMap, sync::Arc};

use russh::{
    Channel,
    server::{Handle, Msg},
};
use tokio::sync::RwLock;

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

pub type Tunnels = Arc<RwLock<HashMap<String, Tunnel>>>;

pub fn new() -> Tunnels {
    Arc::new(RwLock::new(HashMap::new()))
}
