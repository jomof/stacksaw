use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::common::centered_rect;
use super::App;
use crate::command::{self, Command};
use crate::theme::RainbowInput;

impl App {
    /// The \`?\` help overlay: every command grouped by category (§8.2).
    pub(crate) fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let ctx = self.ctx();
        let mut lines: Vec<Line> = Vec::new();
        for category in command::Category::ORDER {
            let cmds: Vec<&Command> = command::registry()
                .iter()
                .filter(|c| c.category == category)
                .collect();
            if cmds.is_empty() {
                continue;
            }
            lines.push(Line::from(RSpan::styled(
                category.title(),
                self.theme.style("help_heading", ctx, RainbowInput::None),
            )));
            for cmd in cmds {
                let keys = cmd
                    .keys
                    .iter()
                    .map(|k| k.label())
                    .collect::<Vec<_>>()
                    .join(" / ");
                lines.push(Line::from(vec![
                    RSpan::styled(
                        format!("  {keys:<10}"),
                        self.theme.style("help_key", ctx, RainbowInput::None),
                    ),
                    RSpan::raw(" "),
                    RSpan::raw(cmd.title),
                ]));
            }
            lines.push(Line::from(""));
        }
        lines.push(Line::from(RSpan::styled(
            "any key to close",
            self.theme.style("help_footer", ctx, RainbowInput::None),
        )));

        let popup = centered_rect(48, (lines.len() as u16 + 2).min(area.height), area);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Help — keys")
            .border_style(self.theme.style("overlay_frame", ctx, RainbowInput::None));
        frame.render_widget(Paragraph::new(lines).block(block), popup);
    }

    /// The \`:\` command palette: a fuzzy-filtered list of every command, each
    /// showing its key so the palette teaches shortcuts (§8.2).
    pub(crate) fn draw_palette(&self, frame: &mut Frame, area: Rect) {
        let ctx = self.ctx();
        let results = self.palette_results();
        let popup = centered_rect(52, 16.min(area.height), area);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Command palette")
            .border_style(self.theme.style("overlay_frame", ctx, RainbowInput::None));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);
        // Query line.
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                RSpan::styled(
                    self.theme.glyph("palette_prompt").to_string(),
                    self.theme.style("palette_prompt", ctx, RainbowInput::None),
                ),
                RSpan::raw(self.palette.query.clone()),
                RSpan::styled(
                    self.theme.glyph("palette_cursor").to_string(),
                    self.theme.style("palette_cursor", ctx, RainbowInput::None),
                ),
            ])),
            rows[0],
        );
        // Results, each with its primary key right-aligned.
        let width = rows[1].width as usize;
        let items: Vec<ListItem> = results
            .iter()
            .map(|cmd| {
                let key = cmd.primary_key_label();
                let gap = width
                    .saturating_sub(cmd.title.chars().count() + key.chars().count() + 2)
                    .max(1);
                ListItem::new(Line::from(vec![
                    RSpan::raw(cmd.title),
                    RSpan::raw(" ".repeat(gap)),
                    RSpan::styled(
                        key,
                        self.theme.style("palette_key", ctx, RainbowInput::None),
                    ),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        if !results.is_empty() {
            state.select(Some(self.palette.selected.min(results.len() - 1)));
        }
        let list = List::new(items)
            .highlight_style(self.theme.selection_style(true, self.ctx()))
            .highlight_symbol(self.theme.selection_symbol());
        frame.render_stateful_widget(list, rows[1], &mut state);
    }

    /// The \`>\` command launcher overlay: the command being typed with an inline
    /// history suggestion, and the resolved run context in the frame title.
    pub(crate) fn draw_run_prompt(&self, frame: &mut Frame, area: Rect) {
        let ctx = self.ctx();
        let popup = centered_rect(64, 5.min(area.height), area);
        frame.render_widget(Clear, popup);
        let target = self.exec_target();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!("Run command — {}", target.label))
            .border_style(self.theme.style("overlay_frame", ctx, RainbowInput::None));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);
        let secondary = self.theme.style("secondary", ctx, RainbowInput::None);
        let mut spans = vec![
            RSpan::styled(
                self.theme.glyph("palette_prompt").to_string(),
                self.theme.style("palette_prompt", ctx, RainbowInput::None),
            ),
            RSpan::raw(self.run_prompt.input.clone()),
        ];
        match self.run_prompt_suggestion() {
            // With a suggestion, the cursor rides the first suggested char
            // (reversed) so the completion reads contiguously — no caret cell
            // splitting "ca|rgo test".
            Some(sugg) => {
                let mut tail = sugg[self.run_prompt.input.len()..].chars();
                if let Some(first) = tail.next() {
                    spans.push(RSpan::styled(
                        first.to_string(),
                        secondary.add_modifier(Modifier::REVERSED),
                    ));
                    let rest: String = tail.collect();
                    if !rest.is_empty() {
                        spans.push(RSpan::styled(rest, secondary));
                    }
                }
            }
            // Otherwise the caret sits at the end of the input.
            None => spans.push(RSpan::styled(
                self.theme.glyph("palette_cursor").to_string(),
                self.theme.style("palette_cursor", ctx, RainbowInput::None),
            )),
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), rows[0]);
        frame.render_widget(
            Paragraph::new("enter: run   →/tab: accept   ↑↓: history   esc: cancel")
                .style(self.theme.style("help_footer", ctx, RainbowInput::None)),
            rows[1],
        );
    }
}
