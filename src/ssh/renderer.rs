use std::cmp::{self, max};
use std::io::Write as _;

use futures::StreamExt;
use git_version::git_version;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::prelude::CrosstermBackend;
use ratatui::style::{Style, Stylize as _};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, HighlightSpacing, Paragraph, Row, Table, TableState,
};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tracing::error;
use unicode_width::UnicodeWidthStr;

use crate::io::TerminalHandle;
use crate::tunnel::Tunnel;

enum Message {
    Resize { width: u16, height: u16 },
    Redraw,
    Rows(Vec<Vec<Span<'static>>>),
    Select(Option<usize>),
    Rename(Option<String>),
    Help(String),
    Close,
}

struct RendererInner {
    state: TableState,
    rows: Vec<Vec<Span<'static>>>,
    input: Option<String>,
    rx: UnboundedReceiver<Message>,
}

impl RendererInner {
    fn new(rx: UnboundedReceiver<Message>) -> Self {
        Self {
            state: Default::default(),
            rows: Default::default(),
            input: None,
            rx,
        }
    }

    fn compute_footer_text<'a>(&self, rect: Rect) -> (u16, Paragraph<'a>) {
        let width = rect.width as usize - 2;

        fn command<'c>(key: &'c str, text: &'c str) -> Vec<Span<'c>> {
            vec![key.bold().light_cyan(), " ".into(), text.dim()]
        }

        let commands = if self.state.selected().is_some() {
            vec![
                command("q", "quit"),
                command("esc", "deselect"),
                command("↓/j", "move down"),
                command("↑/k", "move up"),
                vec![],
                command("del", "remove"),
                command("r", "rename"),
                command("shift-r", "retry"),
                vec![],
                command("p", "make private"),
                command("ctrl-p", "make protected"),
                command("shift-p", "make public"),
            ]
        } else {
            vec![
                command("q", "quit"),
                command("↓/j", "select first"),
                command("↑/k", "select last"),
                vec![],
                command("p", "make all private"),
                command("ctrl-p", "make all protected"),
                command("shift-p", "make all public"),
            ]
        };

        let mut text = Text::default();
        let mut line = Line::default();
        let sep = " | ";
        for command in commands {
            let command_width: usize = command.iter().map(|span| span.width()).sum();

            if command_width > 0 && line.width() == 0 {
                for span in command {
                    line.push_span(span);
                }
            } else if command_width > 0 && line.width() + sep.width() + command_width <= width {
                line.push_span(sep);
                for span in command {
                    line.push_span(span);
                }
            } else {
                text.push_line(line);
                line = Line::from(command);
            }
        }
        text.push_line(line);

        let height = text.lines.len() + 2;

        let block = Block::bordered().border_type(BorderType::Plain);
        (height as u16, Paragraph::new(text).centered().block(block))
    }

    fn render(&mut self, frame: &mut Frame) {
        self.render_title(frame, frame.area());

        let mut area = frame.area().inner(ratatui::layout::Margin {
            horizontal: 1,
            vertical: 1,
        });
        area.height += 1;
        let (footer_height, footer) = self.compute_footer_text(area);

        let layout = Layout::vertical([Constraint::Min(5), Constraint::Length(footer_height)]);
        let chunks = layout.split(area);

        self.render_table(frame, chunks[0]);
        frame.render_widget(footer, chunks[1]);

        if let Some(input) = &self.input {
            self.render_rename(frame, area, input);
        }
    }

    fn render_rename(&self, frame: &mut Frame, area: Rect, input: &str) {
        let vertical = Layout::vertical([Constraint::Length(3)]).flex(Flex::Center);
        let horizontal = Layout::horizontal([Constraint::Max(max(20, input.width() as u16 + 4))])
            .flex(Flex::Center);
        let [area] = vertical.areas(area);
        let [area] = horizontal.areas(area);

        let title = Line::from("New name").centered();
        let block = Block::bordered().title(title);
        let text = Paragraph::new(format!(" {input}")).block(block);

        frame.render_widget(Clear, area);

        frame.render_widget(text, area);

        frame.set_cursor_position(Position::new(area.x + input.width() as u16 + 2, area.y + 1));
    }

    fn render_title(&self, frame: &mut Frame, rect: Rect) {
        let title = format!("{} ({})", std::env!("CARGO_PKG_NAME"), git_version!()).bold();
        let title = Line::from(title).centered();
        frame.render_widget(title, rect);
    }

    fn compute_widths(&mut self) -> Vec<Constraint> {
        let table_header = Tunnel::header();
        std::iter::once(&table_header)
            .chain(&self.rows)
            .map(|row| row.iter().map(|cell| cell.width() as u16))
            .fold(vec![0; table_header.len()], |acc, row| {
                acc.into_iter()
                    .zip(row)
                    .map(|v| cmp::max(v.0, v.1))
                    .collect()
            })
            .into_iter()
            .map(|c| Constraint::Length(c + 1))
            .collect()
    }

    pub fn render_table(&mut self, frame: &mut Frame<'_>, rect: Rect) {
        let highlight_style = Style::default().bold();
        let header_style = Style::default().bold().reversed();
        let row_style = Style::default();

        let rows = self.rows.iter().map(|row| {
            row.iter()
                .cloned()
                .map(Cell::from)
                .collect::<Row>()
                .style(row_style)
                .height(1)
        });

        let header = Tunnel::header()
            .iter()
            .cloned()
            .map(Cell::from)
            .collect::<Row>()
            .style(header_style)
            .height(1);

        let t = Table::default()
            .header(header)
            .rows(rows)
            .flex(Flex::Start)
            .column_spacing(3)
            .widths(self.compute_widths())
            .row_highlight_style(highlight_style)
            .highlight_symbol(Line::from("> "))
            .highlight_spacing(HighlightSpacing::Always);

        frame.render_stateful_widget(t, rect, &mut self.state);
    }

    pub async fn start(
        &mut self,
        mut terminal: Terminal<CrosstermBackend<TerminalHandle>>,
    ) -> std::io::Result<()> {
        while let Some(message) = self.rx.recv().await {
            match message {
                Message::Resize { width, height } => {
                    let rect = Rect::new(0, 0, width, height);

                    terminal.resize(rect)?;
                }
                Message::Select(selected) => self.state.select(selected),
                Message::Rename(input) => self.input = input,
                Message::Rows(rows) => self.rows = rows,
                Message::Redraw => {
                    terminal.draw(|frame| {
                        self.render(frame);
                    })?;
                }
                Message::Help(message) => {
                    let writer = terminal.backend_mut().writer_mut();
                    writer.leave_alternate_screen()?;
                    writer.write_all(message.as_bytes())?;
                    writer.flush()?;

                    break;
                }
                Message::Close => {
                    break;
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
pub struct Renderer {
    tx: Option<UnboundedSender<Message>>,
}

impl Renderer {
    pub fn start(&mut self, terminal: Terminal<CrosstermBackend<TerminalHandle>>) {
        let (tx, rx) = unbounded_channel();

        let mut inner = RendererInner::new(rx);

        tokio::spawn(async move {
            if let Err(err) = inner.start(terminal).await {
                error!("{err}");
            }
        });

        self.tx = Some(tx)
    }

    pub fn select(&self, selected: Option<usize>) {
        if let Some(tx) = &self.tx {
            tx.send(Message::Select(selected)).ok();
            self.redraw();
        }
    }

    pub fn rename(&self, input: &Option<String>) {
        if let Some(tx) = &self.tx {
            tx.send(Message::Rename(input.clone())).ok();
            self.redraw();
        }
    }

    pub fn help(&self, message: String) {
        if let Some(tx) = &self.tx {
            tx.send(Message::Help(message.replace("\n", "\n\r"))).ok();
        }
    }

    pub fn close(&self) {
        if let Some(tx) = &self.tx {
            tx.send(Message::Close).ok();
        }
    }

    pub fn resize(&self, width: u16, height: u16) {
        if let Some(tx) = &self.tx {
            tx.send(Message::Resize { width, height }).ok();
            self.redraw();
        }
    }

    pub async fn rows(&self, tunnels: &[Tunnel]) {
        if let Some(tx) = &self.tx {
            let rows = futures::stream::iter(tunnels)
                .then(Tunnel::to_row)
                .collect::<Vec<_>>()
                .await;

            tx.send(Message::Rows(rows)).ok();
            self.redraw();
        }
    }

    pub fn redraw(&self) {
        if let Some(tx) = &self.tx {
            tx.send(Message::Redraw).ok();
        }
    }
}
