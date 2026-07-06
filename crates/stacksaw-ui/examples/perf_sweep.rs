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

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use stacksaw_ssp::types::{
    CommitSummary, FileEntry, FindingCounts, Segment, Snapshot, Staircase, SCHEMA_VERSION,
};
use stacksaw_ui::App;

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
            name: format!("feat/topic-{s}"),
            upstream: "origin/main".into(),
            ahead: 2,
            behind: 3,
            dirty: s % 2 == 0,
            segments: (0..segments)
                .map(|g| Segment {
                    branch: format!("feat/topic-{s}-part-{g}"),
                    parent: g.checked_sub(1),
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
            status: "M".into(),
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

/// Moves in one simulated rapid sweep before the queue drains and we redraw.
/// Models flicking the pointer across the commit list faster than the loop
/// redraws: the real event loop coalesces the whole burst into a single draw.
const HOVER_BURST: usize = 256;

/// Sweeps to average for the hover-burst measurement.
const HOVER_SWEEPS: usize = 1000;

/// Time the realistic hover path: a burst of `HOVER_BURST` pointer moves over
/// different commits arrives back-to-back (no redraw in between), then the
/// queue drains and a single redraw paints the final hovered commit — exactly
/// how the coalescing event loop handles a fast mouse sweep. Returns the
/// milliseconds for one full sweep (burst handling + the one redraw).
///
/// CPU only: the felt lag over tmux/ssh also pays one terminal flush per sweep,
/// which `TestBackend` excludes — but this shows a sweep costs *one* redraw, not
/// one per commit crossed.
fn bench_hover_burst(app: &mut App, w: u16, h: u16) -> f64 {
    // A first render populates the hit map so we can locate the commit rows and
    // an x-column inside the Commits column (subjects are ASCII, so the char
    // offset of the subject text is its screen column).
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

    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    for _ in 0..50 {
        terminal.draw(|f| app.draw(f)).unwrap();
    }
    let start = Instant::now();
    for _ in 0..HOVER_SWEEPS {
        // Drain a burst of motion over different commits without redrawing...
        for i in 0..HOVER_BURST {
            app.on_mouse_move(x, ys[i % ys.len()]);
        }
        // ...then a single redraw for the final hovered commit.
        terminal.draw(|f| app.draw(f)).unwrap();
    }
    start.elapsed().as_secs_f64() * 1000.0 / HOVER_SWEEPS as f64
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
    row(&mut out, "baseline (1 stair, 20-line diff)", &small, 220, 60);

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
        row(&mut out, "size sweep (25 stairs, 2000-line diff)", &big, w, h);
    }

    // Interaction: a rapid mouse sweep across the commit list. The event loop
    // coalesces the burst, so the cost is one redraw per sweep, not per commit.
    let mut hover = App::new(snapshot(1, 10, 8));
    load_diff(&mut hover, 20);
    let ms = bench_hover_burst(&mut hover, 220, 60);
    writeln!(
        out,
        "{:<44} {:>4}x{:<4} {ms:8.3} ms/sweep ({HOVER_BURST} moves -> 1 redraw)",
        "hover burst across commit list", 220, 60
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
    let raw = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string());
    // Strip a trailing `.local`/domain and normalize to a safe file stem.
    let stem = raw.split('.').next().unwrap_or(&raw);
    stem.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect()
}

/// Best-effort short commit SHA for provenance, or `unknown` outside a checkout.
fn git_sha() -> String {
    std::process::Command::new("git")
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
        std::process::exit(1);
    }

    let host = hostname();
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let table = sweep();

    let mut report = String::new();
    writeln!(report, "stacksaw draw performance sweep").unwrap();
    writeln!(report, "host:    {host}").unwrap();
    writeln!(report, "os/arch: {}/{}", std::env::consts::OS, std::env::consts::ARCH).unwrap();
    writeln!(report, "cpus:    {cpus}").unwrap();
    writeln!(report, "profile: release").unwrap();
    writeln!(report, "commit:  {}", git_sha()).unwrap();
    writeln!(report, "frames:  {FRAMES} per config").unwrap();
    writeln!(report).unwrap();
    report.push_str(&table);

    print!("{report}");

    let path = output_path(&host);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create perf/sweep dir");
    }
    // `create` truncates: a new run replaces the old one, never grows it.
    std::fs::write(&path, report.as_bytes()).expect("write sweep report");
    eprintln!("\nwrote {}", path.display());
}
