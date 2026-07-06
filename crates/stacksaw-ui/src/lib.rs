//! `stacksaw-ui` — the ratatui TUI (an SSP client). Column layout, Rainbox
//! rendering, and the staircase view (§8), behind a `RenderSurface` seam so a
//! future `--gui` renderer can reuse the same scene (§12).

pub mod app;
pub mod command;
pub mod highlight;
pub mod layout;
pub mod surface;
pub mod theme;

pub use app::{
    render_to_lines, App, RecentRowView, RecentsView, ViewState,
};
pub use command::{Action, Command};
pub use layout::{ColumnKind, LayoutPlan};
pub use surface::{Span, SurfaceRow};
