//! Harness-side artefacts, compiled into the binary.
//!
//! The table below pairs the path an `adapters.toml` row names
//! (`extension_artifact`) with the source of the file this build ships. It is
//! compile-time data on purpose: the digest a user approves is then a property
//! of the binary, not of whatever happens to sit next to it at runtime, and a
//! session copy can be materialised with no filesystem lookup — which is also
//! what makes the behaviour testable and sandbox-safe.
//!
//! The keys are file paths, not harness names: a harness id appears only in
//! `adapters.toml`, so adding one adds a row and (if it has an in-process hook)
//! one line here.

/// `harness/pi.ts` — the per-request header hook pi loads in-process.
pub const PI: &str = include_str!("../harness/pi.ts");

/// The artefact a row names, or `None` when this build does not carry it.
pub fn bytes(path: &str) -> Option<&'static str> {
    match path {
        "harness/pi.ts" => Some(PI),
        _ => None,
    }
}

/// Every artefact this build carries, as `(path, source)`.
pub fn all() -> &'static [(&'static str, &'static str)] {
    &[("harness/pi.ts", PI)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_carried_artefact_is_inert_without_the_live_variable() {
        for (path, source) in all() {
            assert!(
                source.contains("AOE_LLM_TAGS_LIVE"),
                "{path} does not look for the live file's path"
            );
            assert!(
                source.contains("before_provider_headers"),
                "{path} registers no per-request header hook"
            );
            // Fail-open: the read is inside a try/catch and the handler returns
            // early when there is nothing to say.
            assert!(source.contains("catch"), "{path} is not fail-open");
        }
    }

    #[test]
    fn an_unknown_path_is_not_silently_empty() {
        assert!(bytes("harness/nope.ts").is_none());
    }
}
