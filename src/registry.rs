//! Per-project plugin settings, read straight from the host's project registry.
//!
//! aoe stores a `per_project` setting in the registry entry of the project it was
//! set for: `<app_dir>/projects.json` (global) and
//! `<app_dir>/profiles/<name>/projects.json` (profile, which shadows a global
//! entry for the same path), under
//! `overrides.plugin_settings["<plugin id>"]["<key>"]`.
//!
//! The worker has a host RPC for this (`config.get {key, project_path}`, like
//! `feature_tag`), but the launch hook runs outside the host in a `$SHELL -c`
//! with a two-second budget and no socket, so it reads the file the host writes.
//! Both paths land in the same settings table before anything else looks at it,
//! so a value means the same thing whether a launch or the worker read it.
//!
//! Failure behaviour is the hook's: a file that cannot be read or parsed is a
//! warning and no overrides, never an error. A session with no project path, or
//! one inside no registered project, simply inherits the global value.

use std::collections::BTreeMap;
use std::path::Path;

use crate::settings::PLUGIN_ID;

/// The plugin's per-project keys this build consumes. A key not listed here is
/// not read: the registry holds other plugins' settings too, and this plugin
/// must not act on a key it does not implement.
pub const KEYS: [&str; 4] = [
    "activity_family",
    "activity_families",
    "request_tag_header",
    "request_tag_header_name",
];

/// The registry path for one scope.
pub fn registry_path(app_dir: &Path, profile: &str) -> Vec<std::path::PathBuf> {
    let mut paths = vec![app_dir.join("projects.json")];
    if !profile.trim().is_empty() {
        paths.push(
            app_dir
                .join("profiles")
                .join(crate::overlay::sanitize(profile.trim()))
                .join("projects.json"),
        );
    }
    paths
}

/// This project's values for [`KEYS`], profile last so it wins.
pub fn project_settings(
    app_dir: &Path,
    profile: &str,
    project_path: &str,
) -> (BTreeMap<String, serde_json::Value>, Vec<String>) {
    let mut values = BTreeMap::new();
    let mut warnings = Vec::new();
    if project_path.trim().is_empty() {
        return (values, warnings);
    }
    let wanted = canonical(project_path);
    for path in registry_path(app_dir, profile) {
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                warnings.push(format!(
                    "project settings: cannot read {} ({err}); using the global value",
                    path.display()
                ));
                continue;
            }
        };
        let projects: Vec<serde_json::Value> = match serde_json::from_str(&raw) {
            Ok(projects) => projects,
            Err(err) => {
                warnings.push(format!(
                    "project settings: {} is not a JSON array of projects ({err}); ignoring it",
                    path.display()
                ));
                continue;
            }
        };
        let Some(entry) = projects.iter().find(|entry| {
            entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|path| canonical(path) == wanted)
        }) else {
            continue;
        };
        let Some(settings) = entry
            .get("overrides")
            .and_then(|overrides| overrides.get("plugin_settings"))
            .and_then(|plugins| plugins.get(PLUGIN_ID))
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        for key in KEYS {
            if let Some(value) = settings.get(key) {
                values.insert(key.to_string(), value.clone());
            }
        }
    }
    (values, warnings)
}

/// The canonical form of a path, or the path as written when it cannot be
/// resolved (a project directory the user has since moved still matches the
/// registry by its recorded spelling, which is the best answer available).
fn canonical(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "registry-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, project: &str, value: &str) {
        std::fs::write(
            dir,
            format!(
                r#"[{{"name":"p","path":{project:?},"overrides":{{"plugin_settings":{{"{PLUGIN_ID}":{value}}}}}}}]"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn the_profile_entry_shadows_the_global_one() {
        let dir = temp();
        let project = dir.join("repo");
        std::fs::create_dir_all(&project).unwrap();
        let global = dir.join("projects.json");
        write(
            &global,
            project.to_str().unwrap(),
            r#"{"activity_family":"global"}"#,
        );
        let profile = dir.join("profiles/main/projects.json");
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        write(
            &profile,
            project.to_str().unwrap(),
            r#"{"activity_family":"profile"}"#,
        );

        let (values, warnings) = project_settings(&dir, "main", project.to_str().unwrap());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            values.get("activity_family"),
            Some(&serde_json::json!("profile"))
        );
    }

    #[test]
    fn the_header_choice_is_one_of_the_keys_a_project_can_set() {
        // The launch hook has no host RPC, so a per-project choice reaches it
        // through this list or not at all.
        assert!(KEYS.contains(&"request_tag_header"));
        assert!(KEYS.contains(&"request_tag_header_name"));
        let dir = temp();
        let project = dir.join("repo");
        std::fs::create_dir_all(&project).unwrap();
        write(
            &dir.join("projects.json"),
            project.to_str().unwrap(),
            r#"{"request_tag_header":"databricks"}"#,
        );
        let (values, _warnings) = project_settings(&dir, "", project.to_str().unwrap());
        assert_eq!(
            values.get("request_tag_header"),
            Some(&serde_json::json!("databricks"))
        );
    }

    #[test]
    fn a_path_that_resolves_to_the_registered_one_matches() {
        let dir = temp();
        let project = dir.join("repo");
        std::fs::create_dir_all(&project).unwrap();
        // Register the canonical path, ask with a path containing `..`.
        let canonical = std::fs::canonicalize(&project).unwrap();
        write(
            &dir.join("projects.json"),
            canonical.to_str().unwrap(),
            r#"{"activity_family":"via-canonical"}"#,
        );
        let asked = project.join("..").join("repo");
        let (values, _) = project_settings(&dir, "", asked.to_str().unwrap());
        assert_eq!(
            values.get("activity_family"),
            Some(&serde_json::json!("via-canonical"))
        );
    }

    #[test]
    fn a_malformed_registry_is_a_warning_and_nothing_else() {
        let dir = temp();
        std::fs::write(dir.join("projects.json"), "not json").unwrap();
        let (values, warnings) = project_settings(&dir, "", "/tmp");
        assert!(values.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("not a JSON array"), "{warnings:?}");
    }

    #[test]
    fn no_project_path_reads_nothing() {
        let dir = temp();
        let (values, warnings) = project_settings(&dir, "", "");
        assert!(values.is_empty());
        assert!(warnings.is_empty());
    }
}
