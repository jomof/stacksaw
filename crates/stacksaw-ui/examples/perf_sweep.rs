//! Draw/layout performance sweep.
//!
//! Measures the cost of a single `App::draw` (scene build + ratatui layout +
//! buffer diff) across input dimensions, so we can locate what dominates a
//! redraw and compare machines (notably a fast laptop vs. a remote tmux/ssh
//! dev box). It renders against a `TestBackend`, isolating CPU (scene build +
//! layout) from terminal I/O.
//!
//! Results are written to `perf/sweep/<hostname>.txt` at the workspace root,
//! one file per host, overwritten on each run (never appended) so the checked-in
//! numbers always reflect the latest sweep. Run it on any machine and commit the
//! file to compare.
//!
//! Release only: an unoptimized build is ~15x slower and would poison the
//! comparison, so a debug build refuses to run.
//!
//! Run: `cargo sweep` (alias), or
//!      `cargo run --release --example perf_sweep -p stacksaw-ui`

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::process::{self, Command};
use std::thread;
use std::time::Instant;

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use stacksaw_ssp::git_ref::GitRef;
use stacksaw_ssp::types::{
    CommitSummary, FileEntry, FileStatus, FindingCounts, Segment, Snapshot, Staircase,
    SCHEMA_VERSION,
};
use stacksaw_ui::{
    App, HoverThrottle, RedrawGate, HOVER_MAX_WAIT_MS, HOVER_SETTLE_MS, REDRAW_MIN_INTERVAL_MS,
};

/// Draws per timed configuration. High enough to average out scheduler noise
/// while keeping the whole sweep to a couple of seconds on a release build.
const FRAMES: usize = 2000;

fn commit(short: &str) -> CommitSummary {
    CommitSummary {
        oid: format!("{short:0<40}"),
        short: short.into(),
        subject: "Wire the proto codec end to end".into(),
        author: "Ada Lovelace".into(),
        author_time: 1_780_000_000,
        parents: vec![],
        change_id: None,
        patch_id: None,
        finding_counts: FindingCounts::default(),
        twins: vec![],
        added: 12,
        deleted: 4,
    }
}

/// A snapshot with `stairs` staircases, each `segments` deep with `commits`
/// commits per segment.
fn snapshot(stairs: usize, segments: usize, commits: usize) -> Snapshot {
    let staircases = (0..stairs)
        .map(|s| Staircase {
            id: None,
            name: format!("feat/topic-{s}"),
            upstream: "origin/main".into(),
            ahead: 2,
            behind: 3,
            dirty: s % 2 == 0,
            rebase: Default::default(),
            conflict: None,
            segments: (0..segments)
                .map(|g| Segment {
                    branch: GitRef::new(format!("feat/topic-{s}-part-{g}")),
                    parent: g.checked_sub(1),
                    stale: false,
                    commits: (0..commits)
                        .map(|c| commit(&format!("{s:x}{g:x}{c:x}a")))
                        .collect(),
                })
                .collect(),
        })
        .collect();
    Snapshot {
        schema_version: SCHEMA_VERSION,
        generation: 1,
        head: Some("feat/topic-0".into()),
        detached: false,
        staircases,
    }
}

/// A unified-diff patch body of roughly `lines` rows (mixed context/add/del).
fn patch(lines: usize) -> String {
    let mut s = String::from("diff --git a/src/lib.rs b/src/lib.rs\n@@ -1,1 +1,1 @@\n");
    for i in 0..lines {
        match i % 5 {
            0 => s.push_str(&format!("+    let value_{i} = compute(input, {i});\n")),
            1 => s.push_str(&format!("-    let old_{i} = legacy(input);\n")),
            _ => s.push_str(&format!("     ctx.record(value_{i}, {i});\n")),
        }
    }
    s
}

fn load_diff(app: &mut App, lines: usize) {
    let oid = app.selected_commit_oid().unwrap_or_else(|| "0".repeat(40));
    app.set_files(
        oid.clone(),
        vec![FileEntry {
            status: FileStatus::Modified,
            path: "src/lib.rs".into(),
            ..Default::default()
        }],
    );
    app.set_diff(oid, "src/lib.rs".into(), &patch(lines), false);
}

/// Time `FRAMES` draws of `app` at `w x h`, returning milliseconds per frame.
fn bench(app: &App, w: u16, h: u16) -> f64 {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    // Warm up (fill caches: ratatui layout LRU, allocator, etc.).
    for _ in 0..50 {
        terminal.draw(|f| app.draw(f)).unwrap();
    }
    let start = Instant::now();
    for _ in 0..FRAMES {
        terminal.draw(|f| app.draw(f)).unwrap();
    }
    start.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64
}

/// Pointer moves in one rapid sweep across the commit list (a fast mouse flick).
const HOVER_MOVES: usize = 256;

/// Simulated spacing between pointer-move events during that sweep (~250 Hz, a
/// fast flick). Drives how many moves fall inside one frame budget.
const HOVER_MOVE_INTERVAL_MS: u64 = 4;

/// Locate the commit rows (screen `y`s) and an `x` column inside the Commits
/// column, by rendering once (which populates the hit map) and finding the
/// commit subject text. Subjects are ASCII, so the subject's char offset is its
/// screen column.
fn hover_targets(app: &App, w: u16, h: u16) -> (u16, Vec<u16>) {
    let lines = stacksaw_ui::render_to_lines(app, w, h);
    let needle = "Wire the proto";
    let ys: Vec<u16> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains(needle))
        .map(|(y, _)| y as u16)
        .collect();
    let x = lines
        .iter()
        .find_map(|l| l.find(needle).map(|b| l[..b].chars().count() as u16))
        .expect("a commit subject is rendered");
    assert!(ys.len() >= 2, "need several commit rows to sweep across");
    (x, ys)
}

/// The realistic hover path: the pointer sweeps across `HOVER_MOVES` commits,
/// events arriving `HOVER_MOVE_INTERVAL_MS` apart. Hover changes flow through
/// the same [`HoverThrottle`] (and [`RedrawGate`]) the event loop uses, so a
/// rapid drag paints only coarse steps and a final settle frame rather than one
/// per commit crossed. Returns `(redraws, draw_ms)`. Redraw count is what
/// matters over a remote link — each redraw is a flush.
fn bench_hover_sweep(app: &mut App, w: u16, h: u16) -> (usize, f64) {
    let (x, ys) = hover_targets(app, w, h);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    for _ in 0..50 {
        terminal.draw(|f| app.draw(f)).unwrap();
    }
    let mut gate = RedrawGate::new(REDRAW_MIN_INTERVAL_MS);
    let mut hover = HoverThrottle::new(HOVER_SETTLE_MS, HOVER_MAX_WAIT_MS);
    let mut now_ms = 0u64;
    let mut redraws = 0usize;
    let start = Instant::now();
    for i in 0..HOVER_MOVES {
        now_ms += HOVER_MOVE_INTERVAL_MS;
        if app.on_mouse_move(x, ys[i % ys.len()]) {
            hover.touched(now_ms);
        }
        // A hover change is painted only once it's due and the budget allows.
        if hover.due(now_ms) && gate.ready(now_ms) {
            terminal.draw(|f| app.draw(f)).unwrap();
            hover.drawn(now_ms);
            redraws += 1;
        }
    }
    // Motion stops; after the settle window the final hovered commit is painted.
    now_ms += HOVER_SETTLE_MS;
    if hover.due(now_ms) {
        terminal.draw(|f| app.draw(f)).unwrap();
        hover.drawn(now_ms);
        redraws += 1;
    }
    (redraws, start.elapsed().as_secs_f64() * 1000.0)
}

/// One sweep row: a labelled configuration and its per-frame cost.
fn row(out: &mut String, label: &str, app: &App, w: u16, h: u16) {
    let ms = bench(app, w, h);
    writeln!(out, "{label:<44} {:>4}x{:<4} {ms:8.3} ms/frame", w, h).unwrap();
}

fn sweep() -> String {
    let mut out = String::new();

    // Baseline: a modest repo with a small diff.
    let mut small = App::new(snapshot(1, 2, 1));
    load_diff(&mut small, 20);
    row(
        &mut out,
        "baseline (1 stair, 20-line diff)",
        &small,
        220,
        60,
    );

    // Diff scaling: only the diff length changes. skip/take should keep this
    // ~flat; any growth points at per-row work that ignores the viewport.
    for d in [200usize, 2000, 20000] {
        let mut a = App::new(snapshot(1, 2, 1));
        load_diff(&mut a, d);
        row(&mut out, &format!("diff {d} lines"), &a, 220, 60);
    }

    // Column volume scaling: many staircases/commits, small diff. Growth here
    // points at the Stacks/Commits/Files column builders.
    for (st, sg, cm) in [(5usize, 3usize, 3usize), (25, 4, 4), (100, 4, 5)] {
        let mut a = App::new(snapshot(st, sg, cm));
        load_diff(&mut a, 20);
        row(
            &mut out,
            &format!("columns {st} stairs x {sg} seg x {cm} commits"),
            &a,
            220,
            60,
        );
    }

    // Terminal-size scaling: more cells to lay out and diff (draw is O(w*h)).
    let mut big = App::new(snapshot(25, 4, 4));
    load_diff(&mut big, 2000);
    for (w, h) in [(120u16, 40u16), (220, 60), (320, 100)] {
        row(
            &mut out,
            "size sweep (25 stairs, 2000-line diff)",
            &big,
            w,
            h,
        );
    }

    // Interaction: a rapid mouse sweep across the commit list, redrawing on
    // every hover change. Reports the redraw count (= terminal flushes) and the
    // total draw time for the sweep.
    let mut hover = App::new(snapshot(1, 10, 8));
    load_diff(&mut hover, 20);
    let (redraws, ms) = bench_hover_sweep(&mut hover, 220, 60);
    writeln!(
        out,
        "{:<44} {:>4}x{:<4} {redraws:>4} redraws  {ms:8.3} ms total  ({HOVER_MOVES} moves)",
        "hover sweep across commit list", 220, 60
    )
    .unwrap();

    out
}

/// `<workspace-root>/perf/sweep/<hostname>.txt`. The workspace root is derived
/// from this crate's manifest dir (`crates/stacksaw-ui`), baked in at compile
/// time, so it resolves regardless of the current working directory.
fn output_path(host: &str) -> PathBuf {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .and_then(|p| p.parent()) // workspace root
        .expect("manifest dir has a workspace root")
        .to_path_buf();
    workspace_root
        .join("perf")
        .join("sweep")
        .join(format!("{host}.txt"))
}

/// A filesystem-safe host identifier (e.g. `jomos-macbook-pro`). Falls back to
/// `unknown-host` when the `hostname` command is unavailable.
fn hostname() -> String {
    let raw = Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string());
    // Strip a trailing `.local`/domain and normalize to a safe file stem.
    let stem = raw.split('.').next().unwrap_or(&raw);
    stem.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// Best-effort short commit SHA for provenance, or `unknown` outside a checkout.
fn git_sha() -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn main() {
    // A debug build is ~15x slower and would make cross-host numbers
    // meaningless. Refuse rather than write misleading results.
    if cfg!(debug_assertions) {
        eprintln!(
            "perf_sweep must run as a release build (a debug build is ~15x slower).\n\
             Use `cargo sweep` or `cargo run --release --example perf_sweep -p stacksaw-ui`."
        );
        process::exit(1);
    }

    let host = hostname();
    let cpus = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let table = sweep();

    let mut report = String::new();
    writeln!(report, "stacksaw draw performance sweep").unwrap();
    writeln!(report, "host:    {host}").unwrap();
    writeln!(report, "os/arch: {}/{}", env::consts::OS, env::consts::ARCH).unwrap();
    writeln!(report, "cpus:    {cpus}").unwrap();
    writeln!(report, "profile: release").unwrap();
    writeln!(report, "commit:  {}", git_sha()).unwrap();
    writeln!(report, "frames:  {FRAMES} per config").unwrap();
    writeln!(report).unwrap();
    report.push_str(&table);

    print!("{report}");

    let path = output_path(&host);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create perf/sweep dir");
    }
    // `create` truncates: a new run replaces the old one, never grows it.
    fs::write(&path, report.as_bytes()).expect("write sweep report");
    eprintln!("\nwrote {}", path.display());
}
