use std::{cmp::min, io::Write, iter::once};

use clap::Parser as _;
use ratatui::{Terminal, TerminalOptions, Viewport, layout::Rect, prelude::CrosstermBackend};
use russh::{
    ChannelId,
    keys::ssh_key::PublicKey,
    server::{Auth, Msg, Session},
};
use tracing::{debug, trace, warn};

use crate::{
    Ldap, cli,
    input::Input,
    io::TerminalHandle,
    ldap::LdapError,
    tui::Renderer,
    tunnel::{Tunnel, TunnelAccess, Tunnels},
};

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

    all_tunnels: Tunnels,
    tunnels: Vec<Tunnel>,

    user: Option<String>,
    pty_channel: Option<ChannelId>,

    terminal: Option<Terminal<CrosstermBackend<TerminalHandle>>>,
    renderer: Renderer,
    selected: Option<usize>,

    rename_buffer: Option<String>,
}

impl Handler {
    pub fn new(ldap: Ldap, all_tunnels: Tunnels) -> Self {
        Self {
            ldap,
            all_tunnels,
            tunnels: Default::default(),
            user: None,
            pty_channel: None,
            terminal: None,
            renderer: Default::default(),
            selected: None,
            rename_buffer: None,
        }
    }

    async fn set_access_all(&mut self, access: TunnelAccess) {
        for tunnel in &self.tunnels {
            tunnel.set_access(access.clone()).await;
        }
    }

    async fn resize(&mut self, width: u32, height: u32) -> std::io::Result<()> {
        if let Some(terminal) = &mut self.terminal {
            let rect = Rect {
                x: 0,
                y: 0,
                width: width as u16,
                height: height as u16,
            };

            terminal.resize(rect)?;
            self.redraw().await?;
        } else {
            warn!("Resize called without valid terminal");
        }

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
            self.renderer.update(&self.tunnels, self.selected).await;
            terminal.draw(|frame| {
                self.renderer.render(frame, &self.rename_buffer);
            })?;
        } else {
            warn!("Redraw called without valid terminal");
        }

        Ok(())
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

    async fn handle_input(&mut self, input: Input) -> std::io::Result<bool> {
        if self.rename_buffer.is_some() {
            match input {
                Input::Char(c) if c.is_alphanumeric() => {
                    self.rename_buffer
                        .as_mut()
                        .expect("input buffer should be some")
                        .push(c.to_ascii_lowercase());
                }
                Input::Backspace => {
                    self.rename_buffer
                        .as_mut()
                        .expect("input buffer should be some")
                        .pop();
                }
                Input::Enter => {
                    debug!("Input accepted");
                    if let Some(selected) = self.selected
                        && let Some(tunnel) = self.tunnels.get_mut(selected)
                        && let Some(buffer) = self.rename_buffer.take()
                    {
                        *tunnel = self.all_tunnels.rename_tunnel(tunnel.clone(), buffer).await;
                    } else {
                        warn!("Trying to rename invalid tunnel");
                    }
                }
                Input::Esc => {
                    debug!("Input rejected");
                    self.rename_buffer = None;
                }
                _ => return Ok(false),
            }
            debug!("Input: {:?}", self.rename_buffer);
        } else {
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
                Input::Char('R') => {
                    let Some(selected) = self.selected else {
                        return Ok(false);
                    };

                    let Some(tunnel) = self.tunnels.get_mut(selected) else {
                        warn!("Trying to retry invalid tunnel");
                        return Ok(false);
                    };

                    *tunnel = self.all_tunnels.retry_tunnel(tunnel.clone()).await;
                }
                Input::Char('r') => {
                    if self.selected.is_some() {
                        trace!("Renaming tunnel");
                        self.rename_buffer = Some(String::new());
                    }
                }
                Input::Delete => {
                    let Some(selected) = self.selected else {
                        return Ok(false);
                    };

                    if selected >= self.tunnels.len() {
                        warn!("Trying to delete tunnel out of bounds");
                        return Ok(false);
                    }

                    let tunnel = self.tunnels.remove(selected);
                    self.all_tunnels.remove_tunnel(tunnel).await;

                    if self.tunnels.is_empty() {
                        self.selected = None;
                    } else {
                        self.selected = Some(min(self.tunnels.len() - 1, selected));
                    }
                }
                Input::CtrlP => {
                    self.set_access_selection(TunnelAccess::Protected).await;
                }
                _ => {
                    return Ok(false);
                }
            };
        }

        Ok(true)
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

            if self.handle_input(input).await? {
                self.redraw().await?;
            }
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
        match cli::Args::try_parse_from(cmd) {
            Ok(args) => {
                debug!("{args:?}");
                if args.make_public() {
                    trace!("Making tunnels public");
                    self.set_access_all(TunnelAccess::Public).await;
                    self.redraw().await?;
                } else if args.make_protected() {
                    trace!("Making tunnels protected");
                    self.set_access_all(TunnelAccess::Protected).await;
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

        let tunnel = self
            .all_tunnels
            .create_tunnel(session.handle(), address, *port, user)
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
        self.terminal = Some(Terminal::with_options(backend, options)?);
        self.redraw().await?;

        self.pty_channel = Some(channel);

        session.channel_success(channel)?;

        Ok(())
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        let tunnels = self.tunnels.clone();
        let mut all_tunnels = self.all_tunnels.clone();

        tokio::spawn(async move {
            for tunnel in tunnels {
                all_tunnels.remove_tunnel(tunnel).await;
            }
        });
    }
}
