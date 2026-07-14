//! Canonical `git staircase` command strings for contextual UI actions.
//!
//! Selectors are always typed (`--id`, `--structural-key`, `--record`) — never
//! interpolated display names.

use stacksaw_ssp::types::{RepresentationKind, Staircase};

pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".into();
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':'))
    {
        return value.into();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Flags that uniquely identify the selected staircase for canonical CLI resolution.
pub fn selector_flags(stair: &Staircase) -> Option<String> {
    if let Some(id) = &stair.selector.lineage_id {
        return Some(format!("--id {}", shell_quote(id)));
    }
    if let Some(record) = &stair.record_revision {
        return Some(format!("--record {}", shell_quote(record)));
    }
    let key = stair
        .selector
        .path_id
        .as_ref()
        .or(stair.selector.structural_key.as_ref())?;
    Some(format!("--structural-key {}", shell_quote(key)))
}

pub fn can_adopt(stair: &Staircase) -> bool {
    stair.selector.lineage_id.is_none()
        && matches!(
            stair.representation,
            RepresentationKind::Implicit | RepresentationKind::FamilyPath
        )
}

pub fn adopt_command(stair: &Staircase) -> Option<String> {
    if !can_adopt(stair) {
        return None;
    }
    let branches: Vec<String> = stair
        .segments
        .iter()
        .map(|segment| segment.branch.short().to_string())
        .collect();
    if branches.is_empty() {
        return None;
    }
    let onto = shell_quote(stair.integration.target.as_str());
    let name = shell_quote(stair.name.as_str());
    let branch_args = branches
        .iter()
        .map(|branch| shell_quote(branch))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!(
        "git staircase adopt {name} --onto {onto} {branch_args}"
    ))
}

pub fn show_command(stair: &Staircase) -> Option<String> {
    let flags = selector_flags(stair)?;
    Some(format!("git staircase show {flags}"))
}

pub fn verify_command(stair: &Staircase) -> Option<String> {
    let flags = selector_flags(stair)?;
    Some(format!("git staircase verify {flags}"))
}

pub fn land_command(stair: &Staircase) -> Option<String> {
    let flags = selector_flags(stair)?;
    Some(format!("git staircase land {flags}"))
}

pub fn review_upload_command(stair: &Staircase) -> Option<String> {
    let flags = selector_flags(stair)?;
    Some(format!("git staircase review upload {flags}"))
}

pub fn review_reconcile_command(stair: &Staircase) -> Option<String> {
    let flags = selector_flags(stair)?;
    Some(format!("git staircase review reconcile {flags}"))
}

pub fn restack_command(stair: &Staircase) -> Option<String> {
    if !stair.segments.iter().any(|segment| segment.stale) {
        return None;
    }
    let flags = selector_flags(stair)?;
    Some(format!("git staircase restack {flags}"))
}

pub fn rebase_command(stair: &Staircase) -> Option<String> {
    if stair.behind == 0 || stair.segments.iter().any(|segment| segment.stale) {
        return None;
    }
    let flags = selector_flags(stair)?;
    let onto = shell_quote(stair.integration.target.as_str());
    Some(format!("git staircase rebase {flags} --onto {onto}"))
}

pub fn sync_command(stair: &Staircase) -> Option<String> {
    restack_command(stair).or_else(|| rebase_command(stair))
}

pub fn materialize_command(stair: &Staircase) -> Option<String> {
    if !stair.dirty {
        return None;
    }
    Some(format!(
        "git staircase draft materialize {}",
        shell_quote(stair.name.as_str())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use stacksaw_ssp::types::{CanonicalSelector, Segment, Staircase};

    fn implicit_fixture() -> Staircase {
        Staircase {
            name: "feat/use-proto".into(),
            selector: CanonicalSelector {
                structural_key: Some("implicit@fixture".into()),
                ..Default::default()
            },
            integration: stacksaw_ssp::types::IntegrationContext {
                target: "refs/heads/main".into(),
                oid: "main".into(),
            },
            segments: vec![
                Segment {
                    branch: "feat/wire-proto".into(),
                    ..Default::default()
                },
                Segment {
                    branch: "feat/use-proto".into(),
                    parent: Some(0),
                    step_id: Some("step-2".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn selector_flags_prefer_lineage_id() {
        let mut stair = implicit_fixture();
        stair.selector.lineage_id = Some("lineage-1".into());
        assert_eq!(
            selector_flags(&stair).as_deref(),
            Some("--id lineage-1")
        );
    }

    #[test]
    fn adopt_uses_branch_list_not_display_name() {
        let command = adopt_command(&implicit_fixture()).unwrap();
        assert!(command.contains("git staircase adopt"));
        assert!(command.contains("feat/wire-proto"));
        assert!(command.contains("feat/use-proto"));
        assert!(command.contains("--onto refs/heads/main"));
    }

    #[test]
    fn materialize_targets_dirty_staircase_name() {
        let mut stair = implicit_fixture();
        stair.dirty = true;
        let command = materialize_command(&stair).unwrap();
        assert_eq!(
            command,
            "git staircase draft materialize feat/use-proto"
        );
    }
}
