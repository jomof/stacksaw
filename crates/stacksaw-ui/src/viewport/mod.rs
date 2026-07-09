//! The tabbed Viewport (the generalized Diff pane).
//!
//! The bottom pane is a host for *contributors*: the singleton `Diff` view and
//! any number of `Run` command terminals. Each tab renders itself into its pane
//! rect and owns its own live badge on the tab button (a green dot while a
//! command executes). Diff is a data-only contributor (its rows are rendered by
//! `app::draw_diff`, which needs the app's theme/selection context); Run is a
//! fully self-contained terminal emulator backed by `vt100`.
//!
//! Each contributor lives in its own module ([`diff`], [`run`]); this module
//! holds the core [`Tab`]/[`Viewport`] machinery that hosts them.

mod diff;
mod run;

pub use diff::{DiffKind, DiffRow, DiffView};
pub use run::{RunContext, RunView, TabBadge, TabStatus};

/// One tab in the viewport. Diff is a singleton; Run tabs are the executed
/// commands.
pub enum Tab {
    Diff(DiffView),
    // Boxed: a `RunView` (its vt100 grid) dwarfs a `DiffView`, so keep the
    // enum — and the `Vec<Tab>` of tabs — small.
    Run(Box<RunView>),
}

impl Tab {
    pub fn label(&self) -> String {
        match self {
            Tab::Diff(_) => "Diff".to_string(),
            Tab::Run(r) => r.label.clone(),
        }
    }

    /// A live badge for this tab's button (contributor-owned).
    pub fn badge(&self) -> Option<TabBadge> {
        match self {
            Tab::Diff(_) => None,
            Tab::Run(r) => r.badge(),
        }
    }

    pub fn is_diff(&self) -> bool {
        matches!(self, Tab::Diff(_))
    }
}

/// The tabbed bottom pane. Always holds exactly one Diff contributor, either as
/// an open tab or stashed (so closing and reopening Diff preserves its scroll
/// and loaded file).
pub struct Viewport {
    pub tabs: Vec<Tab>,
    pub active: usize,
    diff_stash: Option<DiffView>,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            tabs: vec![Tab::Diff(DiffView::default())],
            active: 0,
            diff_stash: None,
        }
    }
}

impl Viewport {
    /// A shared reference to the Diff contributor, whether open or stashed.
    pub fn diff(&self) -> &DiffView {
        self.tabs
            .iter()
            .find_map(|t| match t {
                Tab::Diff(d) => Some(d),
                _ => None,
            })
            .or(self.diff_stash.as_ref())
            .expect("diff contributor always exists")
    }

    /// The Diff contributor as a mutable reference, whether it is an open tab or
    /// stashed, without opening it or changing the active tab (unlike
    /// [`diff_mut_open`](Self::diff_mut_open)). For in-place restyling.
    pub fn diff_mut(&mut self) -> &mut DiffView {
        if let Some(i) = self.tabs.iter().position(Tab::is_diff) {
            if let Tab::Diff(d) = &mut self.tabs[i] {
                return d;
            }
            unreachable!("position matched a Diff tab");
        }
        self.diff_stash
            .as_mut()
            .expect("diff contributor always exists")
    }

    /// The Diff contributor as a mutable reference, reopening it as the leftmost
    /// tab if it was closed (matches "reopened as leftmost tab if a file is
    /// clicked"). Makes the Diff tab active.
    pub fn diff_mut_open(&mut self) -> &mut DiffView {
        if !self.tabs.iter().any(Tab::is_diff) {
            let view = self.diff_stash.take().unwrap_or_default();
            self.tabs.insert(0, Tab::Diff(view));
            self.active = 0;
        } else {
            self.active = self.tabs.iter().position(Tab::is_diff).unwrap_or(0);
        }
        match &mut self.tabs[self.active] {
            Tab::Diff(d) => d,
            _ => unreachable!("just ensured a Diff tab is active"),
        }
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active.min(self.tabs.len().saturating_sub(1))]
    }

    /// Open a new command tab and focus it.
    pub fn open_run(&mut self, run: RunView) {
        self.tabs.push(Tab::Run(Box::new(run)));
        self.active = self.tabs.len() - 1;
    }

    pub fn find_run_mut(&mut self, id: u64) -> Option<&mut RunView> {
        self.tabs.iter_mut().find_map(|t| match t {
            Tab::Run(r) if r.id == id => Some(&mut **r),
            _ => None,
        })
    }

    pub fn active_run_mut(&mut self) -> Option<&mut RunView> {
        match self.tabs.get_mut(self.active) {
            Some(Tab::Run(r)) => Some(&mut **r),
            _ => None,
        }
    }

    pub fn active_run(&self) -> Option<&RunView> {
        match self.tabs.get(self.active) {
            Some(Tab::Run(r)) => Some(&**r),
            _ => None,
        }
    }

    pub fn next(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
    }

    /// Scroll the active tab's content.
    pub fn scroll_active(&mut self, down: bool) {
        match self.tabs.get_mut(self.active) {
            Some(Tab::Diff(d)) => d.on_scroll(down),
            Some(Tab::Run(r)) => r.on_scroll(down),
            None => {}
        }
    }

    /// Close the tab at `index`. Diff is stashed (never destroyed) so it can
    /// reopen with its state. Returns the run id if a command tab was closed,
    /// so the host can kill the process and reclaim its worktree.
    pub fn close(&mut self, index: usize) -> Option<u64> {
        if index >= self.tabs.len() {
            return None;
        }
        let closed = self.tabs.remove(index);
        let run_id = match closed {
            Tab::Diff(view) => {
                self.diff_stash = Some(view);
                None
            }
            Tab::Run(r) => Some(r.id),
        };
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if index < self.active {
            self.active -= 1;
        }
        run_id
    }

    /// The index of the active tab if it is a running command (for capture).
    pub fn active_is_running(&self) -> bool {
        matches!(self.active_run(), Some(r) if r.is_running())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: u64) -> RunView {
        RunView::new(
            id,
            "cmd".into(),
            "cmd".into(),
            None,
            RunContext::default(),
            10,
            40,
        )
    }

    #[test]
    fn viewport_starts_with_a_diff_tab() {
        let vp = Viewport::default();
        assert_eq!(vp.tabs.len(), 1);
        assert!(vp.active_tab().is_diff());
    }

    #[test]
    fn diff_is_stashed_and_reopens_leftmost() {
        let mut vp = Viewport::default();
        vp.open_run(run(1));
        assert_eq!(vp.tabs.len(), 2);
        // Closing the Diff tab (index 0) stashes it (no run id returned).
        assert_eq!(vp.close(0), None);
        assert!(!vp.tabs.iter().any(Tab::is_diff));
        // Selecting a file reopens Diff as the leftmost tab, made active.
        vp.diff_mut_open();
        assert!(vp.tabs[0].is_diff());
        assert_eq!(vp.active, 0);
    }

    #[test]
    fn closing_a_run_tab_returns_its_id() {
        let mut vp = Viewport::default();
        vp.open_run(run(7));
        assert_eq!(vp.close(1), Some(7));
        assert_eq!(vp.tabs.len(), 1);
    }

    #[test]
    fn tab_navigation_wraps() {
        let mut vp = Viewport::default();
        vp.open_run(run(1));
        vp.active = 0;
        vp.next();
        assert_eq!(vp.active, 1);
        vp.next();
        assert_eq!(vp.active, 0);
        vp.prev();
        assert_eq!(vp.active, 1);
    }
}
