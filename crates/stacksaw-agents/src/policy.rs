//! Agent tool-permission policy (§9.3).
//!
//! Requests are matched against ordered `deny` → `allow` → `ask` rules; the
//! first match wins, defaulting to `ask` when nothing matches (fail-safe).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

/// Policy config as declared under `[agents.policy]` (§9.3).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

impl Policy {
    /// Resolve a decision for a request string like `"git push origin main"`
    /// or `"write:src/lib.rs"`. Deny takes precedence, then allow, then ask.
    pub fn decide(&self, request: &str) -> Decision {
        if self.deny.iter().any(|p| glob_match(p, request)) {
            return Decision::Deny;
        }
        if self.allow.iter().any(|p| glob_match(p, request)) {
            return Decision::Allow;
        }
        if self.ask.iter().any(|p| glob_match(p, request)) {
            return Decision::Ask;
        }
        // Nothing matched: fail safe by asking the human.
        Decision::Ask
    }
}

/// Trailing/embedded `*` glob matching. `*` matches any run of characters.
fn glob_match(pattern: &str, text: &str) -> bool {
    // Split on '*' and require the fixed parts to appear in order.
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut idx = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !text[idx..].starts_with(part) {
                return false;
            }
            idx += part.len();
        } else if let Some(pos) = text[idx..].find(part) {
            idx += pos + part.len();
        } else {
            return false;
        }
    }
    // If the pattern does not end with '*', the last part must reach the end.
    if !pattern.ends_with('*') {
        if let Some(last) = parts.last() {
            return text.ends_with(last);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            allow: vec!["git add".into(), "git rebase --continue".into(), "read:**".into()],
            ask: vec!["git *".into(), "write:**".into()],
            deny: vec!["git push*".into(), "network:*".into()],
        }
    }

    #[test]
    fn deny_takes_precedence() {
        assert_eq!(policy().decide("git push origin main"), Decision::Deny);
        assert_eq!(policy().decide("network:fetch"), Decision::Deny);
    }

    #[test]
    fn explicit_allow() {
        assert_eq!(policy().decide("git add"), Decision::Allow);
        assert_eq!(policy().decide("git rebase --continue"), Decision::Allow);
    }

    #[test]
    fn falls_through_to_ask() {
        assert_eq!(policy().decide("git rebase --abort"), Decision::Ask);
        assert_eq!(policy().decide("write:src/lib.rs"), Decision::Ask);
    }

    #[test]
    fn unmatched_defaults_to_ask() {
        assert_eq!(policy().decide("rm -rf /"), Decision::Ask);
    }
}
