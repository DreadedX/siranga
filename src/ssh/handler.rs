use std::cmp::min;
use std::iter::once;

use clap::Parser;
use git_version::git_version;
use ratatui::layout::Rect;
use ratatui::prelude::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};
use russh::ChannelId;
use russh::keys::ssh_key::PublicKey;
use russh::server::{Auth, Msg, Session};
use tracing::{debug, trace, warn};

use crate::io::{Input, TerminalHandle};
use crate::ldap::{Ldap, LdapError};
use crate::tunnel::{Registry, Tunnel, TunnelAccess};

/// Quickly create http tunnels for development
#[derive(Parser, Debug)]
#[command(version = git_version!(), about, long_about = None)]
pub struct Args {
    /// Make all tunnels public by default instead of private
    #[arg(long, group = "access")]
    public: bool,

    #[arg(long, group = "access")]
    protected: bool,
}

impl Args {
    pub fn make_public(&self) -> bool {
        self.public
    }

    pub fn make_protected(&self) -> bool {
        self.protected
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HandlerError {
    #[error(transparent)]
    Russh(#[from] russh::Error),
    #[error(transparent)]
    Ldap(#[from] LdapError),
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

pub struct Handler {
    ldap: Ldap,

    registry: Registry,
    tunnels: Vec<Tunnel>,

    user: Option<String>,
    pty_channel: Option<ChannelId>,

    renderer: super::Renderer,
    selected: Option<usize>,
    rename_input: Option<String>,
}

impl Handler {
    pub fn new(ldap: Ldap, registry: Registry) -> Self {
        Self {
            ldap,
            registry,
            tunnels: Default::default(),
            user: None,
            pty_channel: None,

            renderer: Default::default(),
            selected: None,
            rename_input: None,
        }
    }

    async fn set_access_all(&mut self, access: TunnelAccess) {
        for tunnel in &self.tunnels {
            tunnel.set_access(access.clone()).await;
        }
    }

    async fn set_access_selection(&mut self, access: TunnelAccess) {
        if let Some(selected) = self.selected {
            if let Some(tunnel) = self.tunnels.get_mut(selected) {
                tunnel.set_access(access).await;
            }
        } else {
            self.set_access_all(access).await;
        }
    }

    async fn handle_input(&mut self, input: Input) -> std::io::Result<()> {
        if self.rename_input.is_some() {
            match input {
                Input::Char(c) if c.is_alphanumeric() => {
                    self.rename_input
                        .as_mut()
                        .expect("input buffer should be some")
                        .push(c.to_ascii_lowercase());
                }
                Input::Backspace => {
                    self.rename_input
                        .as_mut()
                        .expect("input buffer should be some")
                        .pop();
                }
                Input::Enter => {
                    debug!("Input accepted");
                    if let Some(selected) = self.selected
                        && let Some(tunnel) = self.tunnels.get_mut(selected)
                        && let Some(buffer) = self.rename_input.take()
                    {
                        tunnel.set_name(buffer).await;
                        self.renderer.rows(&self.tunnels).await;
                    } else {
                        warn!("Trying to rename invalid tunnel");
                    }
                }
                Input::Esc => {
                    debug!("Input rejected");
                    self.rename_input = None;
                }
                _ => return Ok(()),
            }
            debug!("Input: {:?}", self.rename_input);
            self.renderer.rename(&self.rename_input);
        } else {
            match input {
                Input::Char('q') => {
                    self.renderer.close();
                }
                Input::Char('k') | Input::Up => {
                    self.previous_row();
                    self.renderer.select(self.selected);
                }
                Input::Char('j') | Input::Down => {
                    self.next_row();
                    self.renderer.select(self.selected);
                }
                Input::Esc => {
                    self.selected = None;
                    self.renderer.select(self.selected);
                }
                Input::Char('P') => {
                    self.set_access_selection(TunnelAccess::Public).await;
                    self.renderer.rows(&self.tunnels).await;
                }
                Input::Char('p') => {
                    if let Some(user) = self.user.clone() {
                        self.set_access_selection(TunnelAccess::Private(user)).await;
                        self.renderer.rows(&self.tunnels).await;
                    } else {
                        warn!("User not set");
                    }
                }
                Input::Char('R') => {
                    let Some(selected) = self.selected else {
                        return Ok(());
                    };

                    let Some(tunnel) = self.tunnels.get_mut(selected) else {
                        warn!("Trying to retry invalid tunnel");
                        return Ok(());
                    };

                    tunnel.retry().await;
                    self.renderer.rows(&self.tunnels).await;
                }
                Input::Char('r') => {
                    if self.selected.is_some() {
                        trace!("Renaming tunnel");
                        self.rename_input = Some(String::new());
                        self.renderer.rename(&self.rename_input);
                    }
                }
                Input::Delete => {
                    let Some(selected) = self.selected else {
                        return Ok(());
                    };

                    if selected >= self.tunnels.len() {
                        warn!("Trying to delete tunnel out of bounds");
                        return Ok(());
                    }

                    self.tunnels.remove(selected);
                    self.renderer.rows(&self.tunnels).await;

                    if self.tunnels.is_empty() {
                        self.selected = None;
                    } else {
                        self.selected = Some(min(self.tunnels.len() - 1, selected));
                    }
                    self.renderer.select(self.selected);
                }
                Input::CtrlP => {
                    self.set_access_selection(TunnelAccess::Protected).await;
                    self.renderer.rows(&self.tunnels).await;
                }
                _ => {}
            };
        }

        Ok(())
    }

    fn next_row(&mut self) {
        if self.tunnels.is_empty() {
            return;
        }
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
        if self.tunnels.is_empty() {
            return;
        }
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

impl russh::server::Handler for Handler {
    type Error = HandlerError;

    async fn channel_open_session(
        &mut self,
        _channel: russh::Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        trace!("channel_open_session");

        Ok(true)
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        debug!("Login from {user}");
        trace!("{public_key:?}");

        self.user = Some(user.into());

        for key in self.ldap.get_ssh_keys(user).await? {
            trace!("{key:?}");
            if key.key_data() == public_key.key_data() {
                return Ok(Auth::Accept);
            }
        }

        Ok(Auth::reject())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Make sure we only handle user input, and not other data send over ssh
        if let Some(pty_channel) = self.pty_channel
            && pty_channel == channel
        {
            let input: Input = data.into();
            trace!(?input, "input");

            self.handle_input(input).await?;
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
                if args.make_public() {
                    trace!("Making tunnels public");
                    self.set_access_all(TunnelAccess::Public).await;
                    self.renderer.rows(&self.tunnels).await;
                } else if args.make_protected() {
                    trace!("Making tunnels protected");
                    self.set_access_all(TunnelAccess::Protected).await;
                    self.renderer.rows(&self.tunnels).await;
                }
            }
            Err(err) => {
                trace!("Sending help message and disconnecting");

                self.renderer.help(err.render().ansi().to_string());
            }
        }

        Ok(session.channel_success(channel)?)
    }

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        trace!(address, port, "tcpip_forward");

        let Some(user) = self.user.clone() else {
            return Err(russh::Error::Inconsistent.into());
        };

        let tunnel = Tunnel::create(
            &mut self.registry,
            session.handle(),
            address,
            *port,
            TunnelAccess::Private(user),
        )
        .await;

        self.tunnels.push(tunnel);

        // Technically forwarding has failed if tunnel.domain = None, however by lying to the ssh
        // client we can retry in the future
        Ok(true)
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

        self.renderer.resize(col_width as u16, row_height as u16);

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
        trace!(col_width, row_height, ?channel, "pty_request");

        let rect = Rect {
            x: 0,
            y: 0,
            width: col_width as u16,
            height: row_height as u16,
        };
        let terminal_handle = TerminalHandle::start(session.handle(), channel).await?;
        let backend = CrosstermBackend::new(terminal_handle);
        let options = TerminalOptions {
            viewport: Viewport::Fixed(rect),
        };
        let terminal = Terminal::with_options(backend, options)?;
        self.renderer.start(terminal);

        self.renderer.rows(&self.tunnels).await;

        self.pty_channel = Some(channel);

        session.channel_success(channel)?;

        Ok(())
    }
}
