use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::Deref;

/// A git reference (branch, tag, or full ref).
///
/// Encapsulates the logic for switching between short names (e.g. `main`) and
/// full ref names (e.g. `refs/heads/main`), and identifying the ref type.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct GitRef(String);

impl GitRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The full ref name, e.g. `refs/heads/main`.
    pub fn full(&self) -> &str {
        &self.0
    }

    /// The short name, e.g. `main` for `refs/heads/main`.
    pub fn short(&self) -> &str {
        if let Some(rest) = self.0.strip_prefix("refs/heads/") {
            return rest;
        }
        if let Some(rest) = self.0.strip_prefix("refs/tags/") {
            return rest;
        }
        if let Some(rest) = self.0.strip_prefix("refs/remotes/") {
            return rest;
        }
        &self.0
    }

    pub fn is_local_branch(&self) -> bool {
        self.0.starts_with("refs/heads/")
    }

    pub fn is_remote_branch(&self) -> bool {
        self.0.starts_with("refs/remotes/")
    }

    pub fn is_tag(&self) -> bool {
        self.0.starts_with("refs/tags/")
    }
}

impl From<String> for GitRef {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for GitRef {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl Deref for GitRef {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for GitRef {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<String> for GitRef {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

impl PartialEq<&str> for GitRef {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<GitRef> for String {
    fn eq(&self, other: &GitRef) -> bool {
        self == &other.0
    }
}

impl PartialEq<GitRef> for &str {
    fn eq(&self, other: &GitRef) -> bool {
        *self == other.0
    }
}

impl fmt::Display for GitRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn normalization_examples() {
        let r = GitRef::new("refs/heads/main");
        assert_eq!(r.short(), "main");
        assert!(r.is_local_branch());

        let r = GitRef::new("refs/remotes/origin/main");
        assert_eq!(r.short(), "origin/main");
        assert!(r.is_remote_branch());

        let r = GitRef::new("refs/tags/v1.0");
        assert_eq!(r.short(), "v1.0");
        assert!(r.is_tag());

        let r = GitRef::new("main");
        assert_eq!(r.short(), "main");
        assert!(!r.is_local_branch());
    }

    #[test]
    fn ergonomics() {
        let r = GitRef::new("refs/heads/main");
        assert!(r.starts_with("refs/"));
        assert_eq!(r, "refs/heads/main");
        assert_eq!("refs/heads/main", r);
    }

    proptest! {
        #[test]
        fn short_name_never_includes_prefix(ref_name in "refs/(heads|tags|remotes)/[a-z0-9/]+") {
            let r = GitRef::new(ref_name);
            let short = r.short();
            prop_assert!(!short.starts_with("refs/heads/"));
            prop_assert!(!short.starts_with("refs/tags/"));
            prop_assert!(!short.starts_with("refs/remotes/"));
        }

        #[test]
        fn full_name_is_always_original(ref_name in "[a-z0-9/]+") {
            let r = GitRef::new(&ref_name);
            prop_assert_eq!(r.full(), ref_name);
        }
    }
}
