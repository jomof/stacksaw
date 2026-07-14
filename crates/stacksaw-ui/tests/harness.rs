use stacksaw_ssp::types::{
    CommitSummary, ConflictInfo, FindingCounts, RebaseStatus, Segment, Snapshot, Staircase,
    SCHEMA_VERSION,
};
use stacksaw_ui::{render_to_lines, App};

pub struct TuiTestHarness {
    pub app: App,
    pub width: u16,
    pub height: u16,
}

impl TuiTestHarness {
    pub fn new() -> Self {
        Self {
            app: App::new(Snapshot {
                schema_version: SCHEMA_VERSION,
                generation: 1,
                head: None,
                detached: false,
                staircases: vec![],
                ..Default::default()
            }),
            width: 220,
            height: 60,
        }
    }

    pub fn snapshot(mut self, f: impl FnOnce(SnapshotBuilder) -> SnapshotBuilder) -> Self {
        let builder = SnapshotBuilder::new();
        self.app = App::new(f(builder).build());
        self
    }

    pub fn render(&self) -> RenderedFrame {
        let lines = render_to_lines(&self.app, self.width, self.height);
        RenderedFrame { lines }
    }
}

pub struct SnapshotBuilder {
    snapshot: Snapshot,
}

impl SnapshotBuilder {
    pub fn new() -> Self {
        Self {
            snapshot: Snapshot {
                schema_version: SCHEMA_VERSION,
                generation: 1,
                head: None,
                detached: false,
                staircases: vec![],
                ..Default::default()
            },
        }
    }

    pub fn head(mut self, head: impl Into<String>) -> Self {
        self.snapshot.head = Some(head.into().into());
        self
    }

    pub fn staircase(
        mut self,
        name: impl Into<String>,
        f: impl FnOnce(StaircaseBuilder) -> StaircaseBuilder,
    ) -> Self {
        let builder = StaircaseBuilder::new(name.into());
        self.snapshot.staircases.push(f(builder).build());
        self
    }

    pub fn build(self) -> Snapshot {
        self.snapshot
    }
}

pub struct StaircaseBuilder {
    staircase: Staircase,
}

impl StaircaseBuilder {
    pub fn new(name: String) -> Self {
        Self {
            staircase: Staircase {
                id: None,
                selector: stacksaw_ssp::types::CanonicalSelector {
                    structural_key: Some("implicit@fixture".into()),
                    ..Default::default()
                },
                name,
                upstream: "origin/main".into(),
                ahead: 0,
                behind: 0,
                dirty: false,
                rebase: RebaseStatus::Unknown,
                conflict: None,
                segments: vec![],
                ..Default::default()
            },
        }
    }

    pub fn upstream(mut self, upstream: impl Into<String>) -> Self {
        self.staircase.upstream = upstream.into().into();
        self
    }

    pub fn ahead(mut self, ahead: u32) -> Self {
        self.staircase.ahead = ahead;
        self
    }

    pub fn behind(mut self, behind: u32) -> Self {
        self.staircase.behind = behind;
        self
    }

    pub fn dirty(mut self, dirty: bool) -> Self {
        self.staircase.dirty = dirty;
        self
    }

    pub fn rebase(mut self, status: RebaseStatus) -> Self {
        self.staircase.rebase = status;
        self
    }

    pub fn conflict(mut self, commit: impl Into<String>, paths: Vec<impl Into<String>>) -> Self {
        self.staircase.conflict = Some(ConflictInfo {
            commit: commit.into(),
            paths: paths.into_iter().map(|p| p.into()).collect(),
        });
        self
    }

    pub fn segment(
        mut self,
        branch: impl Into<String>,
        f: impl FnOnce(SegmentBuilder) -> SegmentBuilder,
    ) -> Self {
        let builder = SegmentBuilder::new(branch.into());
        self.staircase.segments.push(f(builder).build());
        self
    }

    pub fn build(self) -> Staircase {
        self.staircase
    }
}

pub struct SegmentBuilder {
    segment: Segment,
}

impl SegmentBuilder {
    pub fn new(branch: String) -> Self {
        Self {
            segment: Segment {
                branch: branch.into(),
                parent: None,
                stale: false,
                commits: vec![],
                ..Default::default()
            },
        }
    }

    pub fn parent(mut self, index: usize) -> Self {
        self.segment.parent = Some(index);
        self
    }

    pub fn stale(mut self, stale: bool) -> Self {
        self.segment.stale = stale;
        self
    }

    pub fn commit(mut self, short: impl Into<String>, subject: impl Into<String>) -> Self {
        let short = short.into();
        self.segment.commits.push(CommitSummary {
            oid: format!("{}0000000000000000000000000000000000", short),
            short: short.into(),
            subject: subject.into().into(),
            author: "Ada".into(),
            author_time: 1_780_000_000,
            parents: vec![],
            change_id: None,
            patch_id: None,
            finding_counts: FindingCounts::default(),
            twins: vec![],
            added: 0,
            deleted: 0,
        });
        self
    }

    pub fn build(self) -> Segment {
        self.segment
    }
}

pub struct RenderedFrame {
    pub lines: Vec<String>,
}

impl RenderedFrame {
    pub fn assert_contains(&self, needle: &str) -> &Self {
        let joined = self.lines.join("\n");
        assert!(
            joined.contains(needle),
            "Frame should contain '{}':\n{}",
            needle,
            joined
        );
        self
    }

    pub fn assert_not_contains(&self, needle: &str) -> &Self {
        let joined = self.lines.join("\n");
        assert!(
            !joined.contains(needle),
            "Frame should NOT contain '{}':\n{}",
            needle,
            joined
        );
        self
    }

    pub fn row(&self, needle: &str) -> &str {
        self.lines
            .iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| {
                panic!(
                    "Missing row containing '{}' in:\n{}",
                    needle,
                    self.lines.join("\n")
                )
            })
    }
}
