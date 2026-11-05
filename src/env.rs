//! The environment, injected rather than read.
//!
//! Every module takes an [`Env`] so a test never depends on the ambient
//! process: the hook reads `std::env`, a test reads a map. This mirrors the
//! plan's rule that the resolver is pure apart from one injectable subprocess.

use std::collections::BTreeMap;
use std::ffi::OsString;

/// Process environment, or a test's stand-in for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    map: BTreeMap<String, String>,
}

impl Env {
    /// The real process environment. Non-UTF-8 values are replaced, never fatal
    /// (the hook must not abort a session start over an exotic byte).
    pub fn current() -> Self {
        let mut map = BTreeMap::new();
        for (key, value) in std::env::vars_os() {
            map.insert(lossy(key), lossy(value));
        }
        Self { map }
    }

    pub fn new<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            map: pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    /// `Some(value)` only when the variable is set and non-empty.
    pub fn non_empty(&self, key: &str) -> Option<&str> {
        self.get(key).filter(|value| !value.is_empty())
    }

    pub fn home(&self) -> Option<&str> {
        self.non_empty("HOME")
    }

    /// Every name in this environment, for a subprocess that must not inherit all
    /// of it (a `GIT_DIR` in the launch environment would point `git -C <session>`
    /// at another repository).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }
}

fn lossy(value: OsString) -> String {
    value.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_set_variables_only() {
        let env = Env::new([("AOE_TOOL", "pi"), ("EMPTY", "")]);
        assert_eq!(env.get("AOE_TOOL"), Some("pi"));
        assert_eq!(env.non_empty("EMPTY"), None);
        assert_eq!(env.get("MISSING"), None);
    }
}
