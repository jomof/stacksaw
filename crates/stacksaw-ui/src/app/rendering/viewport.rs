use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use stacksaw_ssp::types::WORKTREE_OID;

use super::App;
use crate::app::{Mode, RunButton};
use crate::layout::ColumnKind;
use crate::theme::RainbowInput;
use crate::viewport::{RunView, Tab, TabStatus};

impl App {
    /// Draw the tabbed bottom pane: the tab buttons ride the first row, with the
    /// active contributor's content filling the rest. Borderless — the top band's
    /// bottom borders already separate it, so the space is given to the body.
    pub(crate) fn draw_viewport(&self, frame: &mut Frame, area: Rect) {
        self.hit
            .borrow_mut()
            .columns
            .push((ColumnKind::Viewport, area));
        if area.height == 0 {
            return;
        }
        let bar = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        self.draw_viewport_tabs(frame, bar);
        let body = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height - 1,
        };
        self.draw_viewport_active(frame, body);
    }

    /// Render the active tab's content into \`area\` and record the content size
    /// (so command terminals can be sized to match).
    pub(crate) fn draw_viewport_active(&self, frame: &mut Frame, area: Rect) {
        // Command terminals reserve the top row for a fixed context header, so
        // every run terminal is sized to the area below it (Diff uses the full
        // area and ignores the content size). Sizing all runs alike — regardless
        // of which tab is active — keeps a backgrounded run's grid stable.
        let run_area = Rect {
            x: area.x,
            y: area.y.saturating_add(1),
            width: area.width,
            height: area.height.saturating_sub(1),
        };
        self.viewport_content_size
            .set((run_area.width, run_area.height));
        // All tabs can be closed (Diff included); with none left, show a hint
        // rather than indexing into an empty tab list.
        if self.viewport.tabs.is_empty() {
            frame.render_widget(
                Paragraph::new(
                    "(no tabs — select a file to open Diff, or press > to run a command)",
                )
                .style(self.theme.style(
                    "diff_placeholder",
                    self.ctx(),
                    RainbowInput::None,
                )),
                area,
            );
            return;
        }
        let run_idx = match self.viewport.active_tab() {
            Tab::Diff(_) => None,
            Tab::Run(_) => Some(self.viewport.active),
        };
        match run_idx {
            None => self.draw_diff(frame, area),
            Some(i) => {
                if let Some(Tab::Run(run)) = self.viewport.tabs.get(i) {
                    let header = Rect { height: 1, ..area };
                    self.draw_run_header(frame, header, run);
                    run.render(frame, run_area);
                    if !run.is_running() {
                        self.draw_run_buttons(frame, run_area, run);
                    }
                }
            }
        }
    }

    /// Draw the finished-command action buttons (Run Again / Close Tab) side by
    /// side, left-aligned just past the command's output, and record their click
    /// regions. Both share one uniform style, distinct from the tab pills. When
    /// output fills the pane, the strip pins to the last row (over that line).
    pub(crate) fn draw_run_buttons(&self, frame: &mut Frame, run_area: Rect, run: &RunView) {
        if run_area.height == 0 {
            return;
        }
        let ctx = self.ctx();
        // Leave one blank row between the output and the buttons (none after).
        let row = (run.content_height() + 1).min(run_area.height - 1);
        let strip = Rect {
            x: run_area.x,
            y: run_area.y + row,
            width: run_area.width,
            height: 1,
        };
        // A clean strip keeps the buttons legible even over a row of output.
        frame.render_widget(Clear, strip);
        let style = self.theme.style("action_button", ctx, RainbowInput::None);
        let cap = match style.bg {
            Some(bg) => Style::default().fg(bg),
            None => Style::default(),
        };
        let lead = self.theme.lead("action_button");
        let trail = self.theme.trail("action_button");
        let mut spans: Vec<RSpan> = Vec::new();
        let mut x = strip.x;
        let mut rects: Vec<(Rect, RunButton)> = Vec::new();
        for (glyph_role, text, action) in [
            ("run_rerun", "Run Again", RunButton::Rerun),
            ("run_close", "Close Tab", RunButton::Close),
        ] {
            let start = x;
            if !lead.is_empty() {
                spans.push(RSpan::styled(lead.to_string(), cap));
                x += lead.chars().count() as u16;
            }
            let glyph = self.theme.glyph(glyph_role);
            let body = if glyph.is_empty() {
                format!(" {text} ")
            } else {
                format!(" {glyph} {text} ")
            };
            x += body.chars().count() as u16;
            spans.push(RSpan::styled(body, style));
            if !trail.is_empty() {
                spans.push(RSpan::styled(trail.to_string(), cap));
                x += trail.chars().count() as u16;
            }
            rects.push((
                Rect {
                    x: start,
                    y: strip.y,
                    width: x - start,
                    height: 1,
                },
                action,
            ));
            spans.push(RSpan::raw("  "));
            x += 2;
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), strip);
        self.hit.borrow_mut().viewport_run_buttons = rects;
    }

    /// A fixed, one-line context header at the top of a command tab, styled like
    /// the context header rows in Stacks/Commits: the command, the commit/branch
    /// it runs against, and — once finished — its exit code (color is never the
    /// sole carrier of the pass/fail state, per P6).
    pub(crate) fn draw_run_header(&self, frame: &mut Frame, area: Rect, run: &RunView) {
        let ctx = self.ctx();
        let glyph = self.theme.glyph("run_header");
        let lead = if glyph.is_empty() {
            String::new()
        } else {
            format!("{glyph} ")
        };
        // "{command}   {repo} ({git}) @ {target}": the action, then the repo
        // root, .git folder, and branch/commit it ran against.
        let mut text = format!("{lead}{}", run.command);
        let mut whence = String::new();
        if !run.context.repo_root.is_empty() {
            whence.push_str(&run.context.repo_root);
        }
        if !run.context.git_dir.is_empty() {
            whence.push_str(&format!(" ({})", run.context.git_dir));
        }
        // Name the target by its label (branch or short oid); when the label is a
        // branch, also pin the exact commit it resolved to.
        let label = self.run_display_label(run);
        let short: Option<String> = run
            .target_oid
            .as_ref()
            .filter(|o| o.as_str() != WORKTREE_OID)
            .map(|o| o.chars().take(7).collect());
        match &short {
            Some(s) if *s != run.label => whence.push_str(&format!(" @ {} · {}", label, s)),
            _ => whence.push_str(&format!(" @ {}", label)),
        }
        text.push_str(&format!("   {}", whence.trim_start()));
        if let TabStatus::Exited(code) = run.status() {
            text.push_str(&format!("   · exited {code}"));
        }
        frame.render_widget(
            Paragraph::new(text).style(self.theme.style("run_header", ctx, RainbowInput::None)),
            area,
        );
    }

    /// Render the tab buttons (\`[badge] label x\`) on the pane's top border and
    /// record their clickable regions.
    pub(crate) fn draw_viewport_tabs(&self, frame: &mut Frame, area: Rect) {
        let ctx = self.ctx();
        let capture = self.mode == Mode::Terminal;
        let close_glyph = self.theme.glyph("tab_close").to_string();
        let mut spans: Vec<RSpan> = Vec::new();
        let mut tabs: Vec<(Rect, usize)> = Vec::new();
        let mut closes: Vec<(Rect, usize)> = Vec::new();
        let mut badges: Vec<(Rect, usize)> = Vec::new();
        let mut x = area.x;
        let end = area.x + area.width;
        for (i, tab) in self.viewport.tabs.iter().enumerate() {
            if x >= end {
                break;
            }
            let active = i == self.viewport.active;
            let role = if active { "tab_active" } else { "tab" };
            // The button surface (with its background) styles the whole pill; the
            // caps borrow that background as their foreground so the rounded ends
            // blend into the surface.
            let btn = self.theme.style(role, ctx, RainbowInput::None);
            let on_btn = |s: Style| match btn.bg {
                Some(bg) => s.bg(bg),
                None => s,
            };
            let cap = match btn.bg {
                Some(bg) => Style::default().fg(bg),
                None => Style::default(),
            };
            let lead = self.theme.lead(role);
            let trail = self.theme.trail(role);
            let start = x;
            // A one-cell gap separates adjacent pills (not before the first).
            if i > 0 {
                spans.push(RSpan::raw(" "));
                x += 1;
            }
            if !lead.is_empty() {
                spans.push(RSpan::styled(lead.to_string(), cap));
                x += lead.chars().count() as u16;
            }
            spans.push(RSpan::styled(" ", btn));
            x += 1;
            if let Some(badge) = tab.badge() {
                let g = self.theme.glyph(badge.role);
                if !g.is_empty() {
                    let bx = x;
                    let w = g.chars().count() as u16 + 1;
                    spans.push(RSpan::styled(
                        format!("{g} "),
                        on_btn(self.theme.style(badge.role, ctx, RainbowInput::None)),
                    ));
                    if badge.cancel {
                        badges.push((
                            Rect {
                                x: bx,
                                y: area.y,
                                width: w,
                                height: 1,
                            },
                            i,
                        ));
                    }
                    x += w;
                }
            }
            // A per-kind type glyph leads the label (code for Diff, terminal for
            // Run), styled with the button surface. Skipped when the theme
            // defines no glyph (e.g. Unicode mode may leave it blank).
            let type_glyph = match tab {
                Tab::Diff(_) => self.theme.glyph("tab_diff"),
                Tab::Run(_) => self.theme.glyph("tab_run"),
            };
            if !type_glyph.is_empty() {
                spans.push(RSpan::styled(format!("{type_glyph} "), btn));
                x += type_glyph.chars().count() as u16 + 1;
            }
            let label = match tab {
                Tab::Run(r) => self.run_display_label(r),
                // Once the diff theme is switched, name it on the tab so the
                // choice is visible (default stays a plain "Diff").
                Tab::Diff(_) if self.syntax_theme_override.is_some() => {
                    format!("{} · {}", tab.label(), self.effective_syntax_theme())
                }
                _ => tab.label(),
            };
            spans.push(RSpan::styled(label.clone(), btn));
            x += label.chars().count() as u16;
            if !close_glyph.is_empty() {
                spans.push(RSpan::styled(" ", btn));
                x += 1;
                let cx = x;
                let cw = close_glyph.chars().count() as u16;
                let close_role = if active {
                    "tab_close_active"
                } else {
                    "tab_close"
                };
                spans.push(RSpan::styled(
                    close_glyph.clone(),
                    on_btn(self.theme.style(close_role, ctx, RainbowInput::None)),
                ));
                closes.push((
                    Rect {
                        x: cx,
                        y: area.y,
                        width: cw,
                        height: 1,
                    },
                    i,
                ));
                x += cw;
            }
            spans.push(RSpan::styled(" ", btn));
            x += 1;
            if !trail.is_empty() {
                spans.push(RSpan::styled(trail.to_string(), cap));
                x += trail.chars().count() as u16;
            }
            tabs.push((
                Rect {
                    x: start,
                    y: area.y,
                    width: x - start,
                    height: 1,
                },
                i,
            ));
        }
        if capture {
            let g = self.theme.glyph("tab_capture");
            let text = if g.is_empty() {
                " [capture]".to_string()
            } else {
                format!(" {g} capture")
            };
            spans.push(RSpan::styled(
                text,
                self.theme.style("tab_capture", ctx, RainbowInput::None),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        let mut hit = self.hit.borrow_mut();
        hit.viewport_tabs = tabs;
        hit.viewport_closes = closes;
        hit.viewport_badges = badges;
    }
}
