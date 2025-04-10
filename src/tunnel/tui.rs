use std::ops::Deref;

use ratatui::style::Stylize;
use ratatui::text::Span;

use super::{Tunnel, TunnelAccess};

pub fn header() -> Vec<Span<'static>> {
    vec!["Access".into(), "Port".into(), "Address".into()]
}

pub async fn to_row((address, tunnel): (&String, &Option<Tunnel>)) -> Vec<Span<'static>> {
    let (access, port) = if let Some(tunnel) = tunnel {
        let access = match tunnel.access.read().await.deref() {
            TunnelAccess::Private(owner) => owner.clone().yellow(),
            TunnelAccess::Public => "PUBLIC".green(),
        };

        (access, tunnel.port.to_string().into())
    } else {
        ("FAILED".red(), "".into())
    };
    let address = format!("http://{address}").into();

    vec![access, port, address]
}
