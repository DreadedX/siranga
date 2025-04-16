use crossterm::execute;
use crossterm::terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use russh::ChannelId;
use russh::server::Handle;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tracing::error;

pub struct TerminalHandle {
    sender: UnboundedSender<Vec<u8>>,
    sink: Vec<u8>,
}

impl TerminalHandle {
    pub async fn start(handle: Handle, channel_id: ChannelId) -> std::io::Result<Self> {
        let (sender, mut receiver) = unbounded_channel::<Vec<u8>>();

        tokio::spawn(async move {
            while let Some(data) = receiver.recv().await {
                let result = handle.data(channel_id, data.into()).await;

                if let Err(e) = result {
                    error!("Failed to send data: {e:?}");
                };
            }

            if let Err(e) = handle.close(channel_id).await {
                error!("Failed to close session: {e:?}");
            }
        });

        let mut terminal_handle = Self {
            sender,
            sink: Vec::new(),
        };

        execute!(terminal_handle, EnterAlternateScreen)?;
        execute!(terminal_handle, Clear(ClearType::All))?;

        Ok(terminal_handle)
    }

    pub fn leave_alternate_screen(&mut self) -> std::io::Result<()> {
        execute!(self, LeaveAlternateScreen)
    }
}

impl Drop for TerminalHandle {
    fn drop(&mut self) {
        self.leave_alternate_screen().ok();
    }
}

impl std::io::Write for TerminalHandle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.sink.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let result = self.sender.send(self.sink.clone());
        if let Err(e) = result {
            return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e));
        }

        self.sink.clear();
        Ok(())
    }
}
