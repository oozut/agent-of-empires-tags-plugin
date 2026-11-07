//! The reserved `activity` key: what the session is *doing*, in a closed vocabulary.
//!
//! A tag says what a session *is* — `feature:billing`, `team:core`. Those are stable for a
//! launch, so the launch hook can deliver them. An activity changes while the session runs
//! (plan, code, test, debug…), which is exactly what the harness extension exists to carry
//! (`plan/m5.md`).
//!
//! The vocabulary is closed on purpose: the icon and the tone are chosen here, by the plugin,
//! so every surface agrees and nobody has to spell a lucide name in a settings file. An
//! activity outside the list is a warning rather than a new icon, because an open vocabulary
//! would make the badge an accident of spelling (`testing`, `test`, `Test`).
//!
//! Presentation only. The tag itself is `activity` = one of [`KEYS`], and that is what travels
//! in the payload; the icon never does. A terminal UI shows text and tone, so [`Activity::label`]
//! is what a badge falls back to.

use crate::resolver::Tag;

/// The reserved tag key. A session has at most one activity.
pub const KEY: &str = "activity";

/// One activity: its tag value, its lucide icon and its tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Activity {
    /// The value half of `activity:<this>` — also the whole vocabulary.
    pub key: &'static str,
    /// What a badge says where an icon cannot render.
    pub label: &'static str,
    /// A lucide kebab-case name, the only icon vocabulary the host renders.
    pub icon: &'static str,
    /// One of `neutral` / `info` / `success` / `warn` / `danger`.
    pub tone: &'static str,
}

/// Every activity, in the order a legend should show them: the work a session moves through.
pub const ALL: [Activity; 9] = [
    Activity {
        key: "explore",
        label: "exploring",
        icon: "compass",
        tone: "neutral",
    },
    Activity {
        key: "plan",
        label: "planning",
        icon: "map",
        tone: "info",
    },
    Activity {
        key: "code",
        label: "coding",
        icon: "code",
        tone: "neutral",
    },
    Activity {
        key: "test",
        label: "testing",
        icon: "flask-conical",
        tone: "warn",
    },
    Activity {
        key: "debug",
        label: "debugging",
        icon: "bug",
        tone: "danger",
    },
    Activity {
        key: "review",
        label: "reviewing",
        icon: "git-pull-request",
        tone: "info",
    },
    Activity {
        key: "docs",
        label: "writing docs",
        icon: "book-open",
        tone: "neutral",
    },
    Activity {
        key: "ops",
        label: "ops",
        icon: "terminal",
        tone: "warn",
    },
    Activity {
        key: "idle",
        label: "idle",
        icon: "pause",
        tone: "neutral",
    },
];

/// The activity a value names, accepting `test`, `activity:test`, or a value with
/// stray case/whitespace — all three mean the same badge.
pub fn find(value: &str) -> Option<&'static Activity> {
    let value = value.trim().to_ascii_lowercase();
    let value = value.strip_prefix("activity:").unwrap_or(&value);
    // The key is canonical, but a person (and an agent copying the word out of
    // the badge) says "testing", so the label is accepted too. It is never
    // stored: the tag is always `activity:<key>`.
    ALL.iter()
        .find(|activity| activity.key == value || activity.label == value)
}

/// This session's activity: the last `activity` tag wins, because a session's
/// own `overrides.toml` is applied after the inherited tags.
pub fn of(tags: &[Tag]) -> Option<&'static Activity> {
    tags.iter()
        .rev()
        .filter(|tag| tag.key == KEY)
        .find_map(|tag| match &tag.value {
            serde_json::Value::String(value) => find(value),
            // A bare `activity` marker is not an activity: which one would it be?
            _ => None,
        })
}

/// Activities a tag set asks for but the vocabulary does not have.
///
/// Reported once per launch rather than rendered: a typo must be visible, and a
/// guessed icon would hide it.
pub fn unknown(tags: &[Tag]) -> Vec<String> {
    tags.iter()
        .filter(|tag| tag.key == KEY)
        .filter_map(|tag| match &tag.value {
            serde_json::Value::String(value) if find(value).is_none() => Some(value.clone()),
            _ => None,
        })
        .collect()
}

/// The whole vocabulary as the page's legend, and the keys a settings field can offer.
pub fn legend() -> Vec<serde_json::Value> {
    ALL.iter()
        .map(|activity| {
            serde_json::json!({
                "key": activity.key,
                "label": activity.label,
                "icon": activity.icon,
                "tone": activity.tone,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_activity_has_a_key_that_finds_it_back() {
        for activity in ALL {
            assert_eq!(find(activity.key), Some(&activity), "{}", activity.key);
            assert_eq!(
                find(&format!("activity:{}", activity.key)),
                Some(&activity),
                "a value written as a whole tag still reads"
            );
        }
    }

    #[test]
    fn the_vocabulary_is_unique_and_lowercase() {
        for (index, activity) in ALL.iter().enumerate() {
            assert!(activity
                .key
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-'));
            assert!(
                !activity
                    .icon
                    .chars()
                    .any(|c| c.is_ascii_uppercase() || c == '_'),
                "{} is not a lucide kebab-case name",
                activity.icon
            );
            assert!(
                ["neutral", "info", "success", "warn", "danger"].contains(&activity.tone),
                "{} has an unknown tone",
                activity.tone
            );
            assert!(
                ALL.iter()
                    .skip(index + 1)
                    .all(|other| other.key != activity.key),
                "{} is listed twice",
                activity.key
            );
            assert!(!activity.label.is_empty());
        }
    }

    #[test]
    fn case_around_a_value_does_not_make_a_new_activity() {
        assert_eq!(find("TEST").map(|a| a.key), Some("test"));
        assert_eq!(find("  activity:Debug ").map(|a| a.key), Some("debug"));
        // The label is a convenience for a human or an agent quoting the badge;
        // the tag it produces is still the key.
        assert_eq!(find("Testing").map(|a| a.key), Some("test"));
        assert_eq!(find("writing docs").map(|a| a.key), Some("docs"));
        assert_eq!(find("testing!"), None);
    }

    fn tag(value: &str) -> Tag {
        Tag {
            key: KEY.to_string(),
            value: serde_json::Value::String(value.to_string()),
        }
    }

    #[test]
    fn the_last_activity_wins_and_an_unknown_one_is_reported() {
        let tags = vec![tag("plan"), tag("code"), tag("nonsense")];
        assert_eq!(of(&tags).map(|a| a.key), Some("code"));
        assert_eq!(unknown(&tags), vec!["nonsense".to_string()]);
        assert_eq!(of(&[]), None);
    }

    #[test]
    fn a_bare_marker_is_not_an_activity() {
        let tags = vec![Tag {
            key: KEY.to_string(),
            value: serde_json::Value::Bool(true),
        }];
        assert_eq!(of(&tags), None);
        assert!(unknown(&tags).is_empty(), "a marker is not a typo either");
    }
}
