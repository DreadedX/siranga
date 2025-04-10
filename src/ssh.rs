use std::{io::Write, iter::once, net::SocketAddr, sync::Arc, time::Duration};

use clap::Parser;
use indexmap::IndexMap;
use ratatui::{Terminal, TerminalOptions, Viewport, layout::Rect, prelude::CrosstermBackend};
use russh::{
    ChannelId,
    keys::PrivateKey,
    server::{Auth, Msg, Server as _, Session},
};
use tokio::net::ToSocketAddrs;
use tracing::{debug, trace, warn};

use crate::{
    keys::Input,
    terminal::TerminalHandle,
    tui::Renderer,
    tunnel::{Tunnel, TunnelAccess, Tunnels},
};

pub struct Handler {
    all_tunnels: Tunnels,
    tunnels: IndexMap<String, Option<Tunnel>>,

    user: Option<String>,

    terminal: Option<Terminal<CrosstermBackend<TerminalHandle>>>,
    renderer: Renderer,
    selected: Option<usize>,
}

impl Handler {
    fn new(all_tunnels: Tunnels) -> Self {
        Self {
            all_tunnels,
            tunnels: IndexMap::new(),
            user: None,
            terminal: None,
            renderer: Default::default(),
            selected: None,
        }
    }

    async fn set_access_all(&mut self, access: TunnelAccess) {
        for (_address, tunnel) in &self.tunnels {
            if let Some(tunnel) = tunnel {
                tunnel.set_access(access.clone()).await;
            }
        }
    }

    async fn resize(&mut self, width: u32, height: u32) -> std::io::Result<()> {
        let rect = Rect {
            x: 0,
            y: 0,
            width: width as u16,
            height: height as u16,
        };

        if let Some(terminal) = &mut self.terminal {
            terminal.resize(rect)?;
        } else {
            todo!()
        }

        self.redraw().await?;

        Ok(())
    }

    pub fn close(&mut self) -> std::io::Result<()> {
        if let Some(terminal) = self.terminal.take() {
            drop(terminal);
        }

        Ok(())
    }

    async fn redraw(&mut self) -> std::io::Result<()> {
        if let Some(terminal) = &mut self.terminal {
            trace!("redraw");
            self.renderer.update_table(&self.tunnels).await;
            self.renderer.select(self.selected);
            terminal.draw(|frame| {
                self.renderer.render(frame);
            })?;
        } else {
            todo!()
        }

        Ok(())
    }

    async fn set_access_selection(&mut self, access: TunnelAccess) {
        if let Some(selected) = self.selected {
            if let Some((_, Some(tunnel))) = self.tunnels.get_index_mut(selected) {
                tunnel.set_access(access).await;
            } else {
                warn!("Selection was invalid");
            }
        } else {
            self.set_access_all(access).await;
        }
    }

    async fn handle_input(&mut self, input: Input) -> std::io::Result<bool> {
        match input {
            Input::Char('q') => {
                self.close()?;
                return Ok(false);
            }
            Input::Char('k') | Input::Up => self.previous_row(),
            Input::Char('j') | Input::Down => self.next_row(),
            Input::Esc => self.selected = None,
            Input::Char('P') => {
                self.set_access_selection(TunnelAccess::Public).await;
            }
            Input::Char('p') => {
                if let Some(user) = self.user.clone() {
                    self.set_access_selection(TunnelAccess::Private(user)).await;
                } else {
                    warn!("User not set");
                }
            }
            _ => {
                return Ok(false);
            }
        };

        Ok(true)
    }

    fn next_row(&mut self) {
        let i = match self.selected {
            Some(i) => {
                if i < self.tunnels.len() - 1 {
                    i + 1
                } else {
                    i
                }
            }
            None => 0,
        };
        self.selected = Some(i);
    }

    fn previous_row(&mut self) {
        let i = match self.selected {
            Some(i) => {
                if i > 0 {
                    i - 1
                } else {
                    i
                }
            }
            None => self.tunnels.len() - 1,
        };
        self.selected = Some(i);
    }
}

/// Quickly create http tunnels for development
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Make all tunnels public by default instead of private
    #[arg(short, long)]
    public: bool,
}

impl russh::server::Handler for Handler {
    type Error = russh::Error;

    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<Msg>,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        trace!("channel_open_session");

        let terminal_handle = TerminalHandle::start(session.handle(), channel.id()).await?;
        let backend = CrosstermBackend::new(terminal_handle);
        let options = TerminalOptions {
            viewport: Viewport::Fixed(Rect::default()),
        };
        self.terminal = Some(Terminal::with_options(backend, options)?);

        Ok(true)
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        _public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        debug!("Login from {user}");

        self.user = Some(user.into());

        // TODO: Get ssh keys associated with user from ldap
        Ok(Auth::Accept)
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let input: Input = data.into();
        trace!(?input, "data");

        if self.handle_input(input).await? {
            self.redraw().await?;
        }

        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let cmd = String::from_utf8_lossy(data);

        trace!(?cmd, "exec_request");

        let cmd = once("<ssh command> --").chain(cmd.split_whitespace());
        match Args::try_parse_from(cmd) {
            Ok(args) => {
                debug!("{args:?}");
                if args.public {
                    trace!("Making tunnels public");
                    self.set_access_all(TunnelAccess::Public).await;
                    self.redraw().await?;
                }
            }
            Err(err) => {
                trace!("Sending help message and disconnecting");

                if let Some(terminal) = &mut self.terminal {
                    let writer = terminal.backend_mut().writer_mut();

                    writer.leave_alternate_screen()?;
                    writer.write_all(err.to_string().replace('\n', "\n\r").as_bytes())?;
                    writer.flush()?;
                }

                self.close()?;
            }
        }

        session.channel_success(channel)
    }

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        trace!(address, port, "tcpip_forward");

        let Some(user) = self.user.clone() else {
            return Err(russh::Error::Inconsistent);
        };

        let tunnel = Tunnel::new(
            session.handle(),
            address,
            *port,
            TunnelAccess::Private(user),
        );
        let (success, address) = self.all_tunnels.add_tunnel(address, tunnel.clone()).await;

        let tunnel = if success { Some(tunnel) } else { None };
        self.tunnels.insert(address, tunnel);

        Ok(success)
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        trace!(col_width, row_height, "window_change_request");

        self.resize(col_width, row_height).await?;

        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        trace!(col_width, row_height, "pty_request");

        self.resize(col_width, row_height).await?;

        session.channel_success(channel)?;

        Ok(())
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        let tunnels = self.tunnels.clone();
        let mut all_tunnels = self.all_tunnels.clone();

        tokio::spawn(async move {
            all_tunnels.remove_tunnels(&tunnels).await;
        });
    }
}

pub struct Server {
    tunnels: Tunnels,
}

impl Server {
    pub fn new(tunnels: Tunnels) -> Self {
        Server { tunnels }
    }

    pub fn tunnels(&self) -> Tunnels {
        self.tunnels.clone()
    }

    pub fn run(
        &mut self,
        key: PrivateKey,
        addr: impl ToSocketAddrs + Send + std::fmt::Debug,
    ) -> impl Future<Output = Result<(), std::io::Error>> + Send {
        let config = russh::server::Config {
            inactivity_timeout: Some(Duration::from_secs(3600)),
            auth_rejection_time: Duration::from_secs(3),
            auth_rejection_time_initial: Some(Duration::from_secs(0)),
            keys: vec![key],
            preferred: russh::Preferred {
                ..Default::default()
            },
            nodelay: true,
            ..Default::default()
        };
        let config = Arc::new(config);

        debug!(?addr, "Running ssh");

        async move { self.run_on_address(config, addr).await }
    }
}

impl russh::server::Server for Server {
    type Handler = Handler;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self::Handler {
        Handler::new(self.tunnels.clone())
    }

    fn handle_session_error(&mut self, error: <Self::Handler as russh::server::Handler>::Error) {
        warn!("Session error: {error:#?}");
    }
}
