use std::cmp;

use futures::StreamExt;
use indexmap::IndexMap;
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Rect},
    style::{Style, Stylize as _},
    text::{Line, Span},
    widgets::{Cell, HighlightSpacing, Row, Table, TableState},
};

use crate::tunnel::{self, Tunnel};

pub struct Renderer {
    table_state: TableState,
    table_rows: Vec<Vec<Span<'static>>>,
    table_header: Vec<Span<'static>>,
    table_widths: Vec<Constraint>,
}

impl Default for Renderer {
    fn default() -> Self {
        let mut renderer = Self {
            table_state: Default::default(),
            table_rows: Default::default(),
            table_header: tunnel::tui::header(),
            table_widths: Default::default(),
        };

        renderer.update_widths();

        renderer
    }
}

impl Renderer {
    // NOTE: This needs to be a separate function as the render functions can not be async
    pub async fn update(
        &mut self,
        tunnels: &IndexMap<String, Option<Tunnel>>,
        index: Option<usize>,
    ) {
        self.table_rows = futures::stream::iter(tunnels.iter())
            .then(tunnel::tui::to_row)
            .collect::<Vec<_>>()
            .await;

        self.update_widths();

        self.table_state.select(index);
    }

    pub fn render(&mut self, frame: &mut Frame) {
        self.render_title(frame, frame.area());

        let area = frame.area().inner(ratatui::layout::Margin {
            horizontal: 1,
            vertical: 1,
        });

        self.render_table(frame, area);
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

    fn update_widths(&mut self) {
        self.table_widths = std::iter::once(&self.table_header)
            .chain(&self.table_rows)
            .map(|row| row.iter().map(|cell| cell.width() as u16))
            .fold(vec![0; self.table_header.len()], |acc, row| {
                acc.into_iter()
                    .zip(row)
                    .map(|v| cmp::max(v.0, v.1))
                    .collect()
            })
            .into_iter()
            .map(|c| Constraint::Length(c + 1))
            .collect();
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

        let header = self
            .table_header
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
            .widths(&self.table_widths)
            .row_highlight_style(highlight_style)
            .highlight_symbol(Line::from("> "))
            .highlight_spacing(HighlightSpacing::Always);

        frame.render_stateful_widget(t, rect, &mut self.table_state);
    }
}
