use std::cmp;

use futures::StreamExt;
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Style, Stylize as _},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Cell, HighlightSpacing, Paragraph, Row, Table, TableState},
};
use unicode_width::UnicodeWidthStr;

use crate::tunnel::{self, Tunnel};

#[derive(Default)]
pub struct Renderer {
    table_state: TableState,
    table_rows: Vec<Vec<Span<'static>>>,
}

fn command<'c>(key: &'c str, text: &'c str) -> Vec<Span<'c>> {
    vec![key.bold().light_cyan(), " ".into(), text.dim()]
}

impl Renderer {
    // NOTE: This needs to be a separate function as the render functions can not be async
    pub async fn update(&mut self, tunnels: &[Tunnel], index: Option<usize>) {
        self.table_rows = futures::stream::iter(tunnels)
            .then(tunnel::tui::to_row)
            .collect::<Vec<_>>()
            .await;

        self.table_state.select(index);
    }

    fn compute_footer_text<'a>(&self, rect: Rect) -> (u16, Paragraph<'a>) {
        let width = rect.width as usize - 2;

        let commands = if self.table_state.selected().is_some() {
            vec![
                command("q", "quit"),
                command("esc", "deselect"),
                command("↓/j", "move down"),
                command("↑/k", "move up"),
                vec![],
                command("p", "make private"),
                command("ctrl-p", "make protected"),
                command("shift-p", "make public"),
                command("del", "remove"),
                command("r", "retry"),
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

    pub fn render(&mut self, frame: &mut Frame) {
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
    }

    pub fn render_title(&self, frame: &mut Frame, rect: Rect) {
        let title = format!(
            "{} ({})",
            std::env!("CARGO_PKG_NAME"),
            std::env!("CARGO_PKG_VERSION")
        )
        .bold();
        let title = Line::from(title).centered();
        frame.render_widget(title, rect);
    }

    fn compute_widths(&mut self) -> Vec<Constraint> {
        let table_header = tunnel::tui::header();
        std::iter::once(&table_header)
            .chain(&self.table_rows)
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

        let rows = self.table_rows.iter().map(|row| {
            row.iter()
                .cloned()
                .map(Cell::from)
                .collect::<Row>()
                .style(row_style)
                .height(1)
        });

        let header = tunnel::tui::header()
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

        frame.render_stateful_widget(t, rect, &mut self.table_state);
    }
}
