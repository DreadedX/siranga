use std::ops::Deref;

use ratatui::style::Stylize;
use ratatui::text::Span;

use super::{Tunnel, TunnelAccess};

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

pub async fn to_row(tunnel: &Tunnel) -> Vec<Span<'static>> {
    let access = match tunnel.access.read().await.deref() {
        TunnelAccess::Private(owner) => owner.clone().yellow(),
        TunnelAccess::Protected => "PROTECTED".blue(),
        TunnelAccess::Public => "PUBLIC".green(),
    };

    let address = tunnel
        .get_address()
        .map(|address| format!("http://{address}").into())
        .unwrap_or("FAILED".red());

    vec![
        tunnel.name.clone().into(),
        access,
        tunnel.port.to_string().into(),
        address,
        tunnel.stats.connections().to_string().into(),
        tunnel.stats.rx().to_string().into(),
        tunnel.stats.tx().to_string().into(),
    ]
}
