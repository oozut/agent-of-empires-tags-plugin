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

/// `harness/skills/aoe-activity/SKILL.md` — the skill that tells the agent to
/// keep the session's activity current. A skill is advisory (the harness lists
/// it and the model loads it on demand), which is why it travels with
/// [`ACTIVITY_APPEND`] rather than alone.
pub const ACTIVITY_SKILL: &str = include_str!("../harness/skills/aoe-activity/SKILL.md");

/// `harness/activity.append.md` — the same instruction in the always-loaded
/// slot. One file for every harness: what it says is about this plugin, not
/// about one vendor's config format.
pub const ACTIVITY_APPEND: &str = include_str!("../harness/activity.append.md");

/// The artefact a row names, or `None` when this build does not carry it.
pub fn bytes(path: &str) -> Option<&'static str> {
    match path {
        "harness/pi.ts" => Some(PI),
        "harness/skills/aoe-activity/SKILL.md" => Some(ACTIVITY_SKILL),
        "harness/activity.append.md" => Some(ACTIVITY_APPEND),
        _ => None,
    }
}

/// Every artefact this build carries, as `(path, source)`.
pub fn all() -> &'static [(&'static str, &'static str)] {
    &[
        ("harness/pi.ts", PI),
        ("harness/skills/aoe-activity/SKILL.md", ACTIVITY_SKILL),
        ("harness/activity.append.md", ACTIVITY_APPEND),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hook_is_inert_without_the_live_variable_and_fails_open() {
        assert!(
            PI.contains("AOE_LLM_TAGS_LIVE"),
            "the hook does not look for the live file's path"
        );
        assert!(
            PI.contains("before_provider_headers"),
            "the hook registers no per-request header hook"
        );
        // Fail-open: the read is inside a try/catch and the handler returns
        // early when there is nothing to say.
        assert!(PI.contains("catch"), "the hook is not fail-open");
    }

    /// The two carriers say the same thing: one in the always-loaded slot, one
    /// as a skill. Whichever the agent reads, it has the command, it is told to
    /// ask the session for its vocabulary instead of assuming the built-in nine,
    /// and it knows that vocabulary is closed.
    #[test]
    fn both_activity_carriers_name_the_command_and_ask_for_the_family() {
        for source in [ACTIVITY_SKILL, ACTIVITY_APPEND] {
            assert!(source.contains("activity"), "no `activity` in {source}");
            assert!(source.contains("aoe-llm-tags"), "no command in {source}");
            // A session's words are its family, so the carrier sends the agent to
            // the command rather than to a list of nine it cannot know.
            assert!(
                source.contains("activity --show"),
                "no discovery step in {source}"
            );
            assert!(source.contains("family"), "no family in {source}");
            assert!(
                source.contains("AOE_LLM_TAGS_BIN"),
                "the command must degrade where AoE exports nothing: {source}"
            );
        }
        // The skill still spells out the built-in family, because most sessions
        // have it and a worked example is what makes the mapping usable.
        for word in ["explore", "plan", "test", "idle"] {
            assert!(
                ACTIVITY_SKILL.contains(word),
                "{word} missing from the skill"
            );
        }
        // A skill is only loaded if the harness can parse its frontmatter.
        assert!(ACTIVITY_SKILL.starts_with("---\nname: "));
        assert!(ACTIVITY_SKILL.contains("\ndescription: "));
    }

    #[test]
    fn an_unknown_path_is_not_silently_empty() {
        assert!(bytes("harness/nope.ts").is_none());
    }
}
