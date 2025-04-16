use std::ops::Deref;
use std::sync::Arc;

use ratatui::style::Stylize;
use ratatui::text::Span;

use super::{Tunnel, TunnelAccess};
use crate::io::Stats;

pub struct TunnelRow {
    name: Span<'static>,
    access: Span<'static>,
    port: Span<'static>,
    address: Span<'static>,
    stats: Arc<Stats>,
}

impl From<&TunnelRow> for Vec<Span<'static>> {
    fn from(row: &TunnelRow) -> Self {
        vec![
            row.name.clone(),
            row.access.clone(),
            row.port.clone(),
            row.address.clone(),
            row.stats.connections().to_string().into(),
            row.stats.rx().to_string().into(),
            row.stats.tx().to_string().into(),
        ]
    }
}

impl Tunnel {
    pub fn header() -> Vec<Span<'static>> {
        vec![
            "Name".into(),
            "Access".into(),
            "Port".into(),
            "Address".into(),
            "Conn".into(),
            "Rx".into(),
            "Tx".into(),
        ]
    }

    pub async fn to_row(tunnel: &Tunnel) -> TunnelRow {
        let access = match tunnel.inner.access.read().await.deref() {
            TunnelAccess::Private(owner) => owner.clone().yellow(),
            TunnelAccess::Protected => "PROTECTED".blue(),
            TunnelAccess::Public => "PUBLIC".green(),
        };

        let address = tunnel
            .get_address()
            .map(|address| format!("http://{address}").into())
            .unwrap_or("FAILED".red());

        TunnelRow {
            name: tunnel.registry_entry.get_name().to_string().into(),
            access,
            port: tunnel.inner.port.to_string().into(),
            address,
            stats: tunnel.inner.stats.clone(),
        }
    }
}
