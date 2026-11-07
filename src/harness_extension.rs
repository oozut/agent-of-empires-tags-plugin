//! A harness-side extension, shipped by the plugin and installed only when the
//! user says so twice.
//!
//! A launch-time hook can fill a header value once per process, because a
//! harness reads its config once and caches what it finds there. A harness with
//! an in-process extension hook can do better: the extension runs inside the
//! agent and re-reads this plugin's live file before each model request, so a
//! tag edited while a session runs reaches the next request.
//!
//! That is a real grant, not a delivery detail: `aoe plugin install` approves a
//! plugin, and this decides that a second program runs *inside the agent
//! process*. So it takes two things, and neither alone is enough:
//!
//! * **intent** — the user lists the harness in the `harness_extension` setting;
//! * **a receipt** — the user read this build's disclosure and approved *this*
//!   file, which is recorded here with its SHA-256 and the disclosure version.
//!
//! The receipt is durable on purpose: it lives in the app dir, so a plugin
//! update, a reinstall, a pi upgrade or a config edit leaves it working. It is
//! invalidated by exactly three things — a change to the artefact's bytes, a
//! bumped [`DISCLOSURE_VERSION`], or the user revoking it. Keeping it alive
//! across every other change is what makes "kept for longer" honest without
//! turning consent into a rubber stamp.
//!
//! Nothing here names a harness: the row in `adapters.toml` says which artefact
//! it loads and where that artefact goes, so a second harness is data, a
//! separate approval and a separate hash.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::adapters::Adapter;
use crate::resolver::ResolvedHeader;

/// The setting the user writes to say which harnesses may get an extension.
pub const SETTING: &str = "harness_extension";

/// The receipt file, in the app dir next to `config.toml`.
pub const RECEIPT_FILE: &str = "harness-extension.toml";

/// Which disclosure the user approved. Bump it when the wording in
/// [`disclosure`] changes in a way a user should see again — every receipt with
/// an older number stops being enough, and the file the user approves is the
/// same file.
pub const DISCLOSURE_VERSION: u32 = 1;

/// The default file name inside the harness's extension directory. A row may
/// override it; the name is ours so it can be recognised and removed.
pub const DEFAULT_FILE: &str = "aoe-llm-tags.ts";

/// The variable that tells an extension where this session's live file is.
/// A row may override it; the extension only ever reads the path it is given.
pub const DEFAULT_LIVE_ENV: &str = "AOE_LLM_TAGS_LIVE";

/// The live file's name inside `sessions/<id>/`.
pub const LIVE_FILE: &str = "live.json";

/// Everything a harness needs, once the two conditions are met.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub harness: String,
    /// The artefact in the plugin tree, as shipped.
    pub artifact: PathBuf,
    pub sha256: String,
    /// Directory inside the session's config copy (`extensions` for pi).
    pub subdir: String,
    /// Our file inside that directory.
    pub file: String,
    /// The variable the hook exports so the extension finds [`LIVE_FILE`].
    pub env_var: String,
    /// `sessions/<id>/live.json`.
    pub live: PathBuf,
}

/// Why a harness does not get an extension. Every variant is a different
/// sentence in `doctor`, because "sends nothing dynamic" must never be a
/// silent no-op.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// The adapter row has no in-harness hook at all.
    NoArtifact { harness: String },
    /// The artefact exists but the user never asked for it.
    NoIntent { harness: String },
    /// There is nowhere to put it: this harness is served through its own
    /// environment or a single file, not a session config directory.
    NoDirectory { harness: String, mode: String },
    /// The receipt names a file this build does not ship.
    MissingArtifact {
        harness: String,
        path: PathBuf,
        why: String,
    },
    /// Intent, but no approval for what would be written.
    NoReceipt { harness: String, sha256: String },
    /// Approved, but the shipped file changed since.
    HashMismatch {
        harness: String,
        approved: String,
        shipped: String,
    },
    /// Approved, but the wording the user agreed to changed since.
    DisclosureMismatch {
        harness: String,
        approved: u32,
        current: u32,
    },
}

impl Refusal {
    /// A stable machine-readable reason (`--json`, tests, the page).
    pub fn reason(&self) -> &'static str {
        match self {
            Refusal::NoArtifact { .. } => "no_artifact",
            Refusal::NoIntent { .. } => "no_intent",
            Refusal::NoDirectory { .. } => "no_directory",
            Refusal::MissingArtifact { .. } => "missing_artifact",
            Refusal::NoReceipt { .. } => "no_receipt",
            Refusal::HashMismatch { .. } => "hash_mismatch",
            Refusal::DisclosureMismatch { .. } => "disclosure_mismatch",
        }
    }

    /// Whether a launch should say something about this. The two "nothing was
    /// asked for" cases are the normal state of most rows and stay quiet;
    /// everything else means the user asked and something is in the way.
    pub fn worth_a_warning(&self) -> bool {
        !matches!(self, Refusal::NoArtifact { .. } | Refusal::NoIntent { .. })
    }

    pub fn harness(&self) -> &str {
        match self {
            Refusal::NoArtifact { harness }
            | Refusal::NoIntent { harness }
            | Refusal::NoDirectory { harness, .. }
            | Refusal::MissingArtifact { harness, .. }
            | Refusal::NoReceipt { harness, .. }
            | Refusal::HashMismatch { harness, .. }
            | Refusal::DisclosureMismatch { harness, .. } => harness,
        }
    }

    /// What the user or the operator is told. One sentence, with the verb that
    /// fixes it where there is one.
    pub fn detail(&self) -> String {
        match self {
            Refusal::NoArtifact { harness } => {
                format!("{harness} has no in-process extension hook: launch-time tags only")
            }
            Refusal::NoIntent { harness } => format!(
                "{harness} supports an extension, but it is not in `{SETTING}`: launch-time tags only"
            ),
            Refusal::NoDirectory { harness, mode } => format!(
                "{harness} is delivered through {mode}, which has no config directory to add an \
                 extension to: launch-time tags only"
            ),
            Refusal::MissingArtifact { path, why, .. } => format!(
                "the extension artefact {} is not in the plugin tree: {why}",
                path.display()
            ),
            Refusal::NoReceipt { harness, sha256 } => format!(
                "{harness} extension not approved: run `aoe-llm-tags extension enable {harness}` \
                 and read the disclosure (sha256 {sha256})"
            ),
            Refusal::HashMismatch {
                harness,
                approved,
                shipped,
            } => format!(
                "the {harness} extension changed since you approved it (approved {approved}, \
                 shipped {shipped}): re-approve with `aoe-llm-tags extension enable {harness}`"
            ),
            Refusal::DisclosureMismatch {
                harness,
                approved,
                current,
            } => format!(
                "the {harness} extension disclosure changed since you approved it (v{approved}, \
                 now v{current}): re-approve with `aoe-llm-tags extension enable {harness}`"
            ),
        }
    }
}

/// One harness's approval, as stored.
#[derive(Clone, Debug, PartialEq)]
pub struct Receipt {
    pub harness: String,
    pub sha256: String,
    pub disclosure_version: u32,
    /// Informational: when the approval happened, in UTC.
    pub accepted_at: String,
    /// Informational: which plugin release asked. Never a validity condition.
    pub accepted_plugin: String,
    /// Informational: which harness version was on the machine. Never a
    /// condition either — a harness upgrade is not a reason to re-ask.
    pub tested_harness: String,
}

/// The receipt file. Empty when nothing was ever approved.
pub fn receipt_path(app_dir: &Path) -> PathBuf {
    app_dir.join(RECEIPT_FILE)
}

/// Every receipt, keyed by harness. A broken file is a warning and an empty
/// map, never a reason to refuse a launch.
pub fn receipts(app_dir: &Path) -> (BTreeMap<String, Receipt>, Vec<String>) {
    let path = receipt_path(app_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return (BTreeMap::new(), vec![]),
        Err(err) => {
            return (
                BTreeMap::new(),
                vec![format!("{} could not be read: {err}", path.display())],
            )
        }
    };
    let table = match raw.parse::<toml::Table>() {
        Ok(table) => table,
        Err(err) => {
            return (
                BTreeMap::new(),
                vec![format!("{} could not be parsed: {err}", path.display())],
            )
        }
    };
    let mut out = BTreeMap::new();
    let mut warnings = Vec::new();
    let Some(rows) = table.get("extension").and_then(toml::Value::as_table) else {
        return (out, warnings);
    };
    for (harness, value) in rows {
        let Some(row) = value.as_table() else {
            warnings.push(format!(
                "{}: [extension.{harness}] is not a table; ignoring it",
                path.display()
            ));
            continue;
        };
        let text = |key: &str| {
            row.get(key)
                .and_then(toml::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let sha256 = text("sha256");
        if !valid_sha(&sha256) {
            warnings.push(format!(
                "{}: [extension.{harness}] has no usable sha256; treating it as unapproved",
                path.display()
            ));
            continue;
        }
        let disclosure_version = row
            .get("disclosure_version")
            .and_then(toml::Value::as_integer)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        out.insert(
            harness.clone(),
            Receipt {
                harness: harness.clone(),
                sha256,
                disclosure_version,
                accepted_at: text("accepted_at"),
                accepted_plugin: text("accepted_plugin"),
                tested_harness: text("tested_harness"),
            },
        );
    }
    (out, warnings)
}

/// Record an approval. `sha256` must be the digest of the file the user was
/// shown; the caller reads it with [`artifact_sha`], so a receipt can never
/// name bytes that are not the ones this build ships.
pub fn accept(
    app_dir: &Path,
    harness: &str,
    sha256: &str,
    harness_version: &str,
) -> Result<Receipt, String> {
    if !valid_sha(sha256) {
        return Err(format!("{sha256:?} is not a sha256 digest"));
    }
    let (mut all, _) = receipts(app_dir);
    let receipt = Receipt {
        harness: harness.to_string(),
        sha256: sha256.to_ascii_lowercase(),
        disclosure_version: DISCLOSURE_VERSION,
        accepted_at: now_rfc3339(),
        accepted_plugin: plugin_version(),
        tested_harness: harness_version.to_string(),
    };
    all.insert(harness.to_string(), receipt.clone());
    crate::private::create_dir(app_dir).map_err(|err| format!("{}: {err}", app_dir.display()))?;
    crate::private::write_atomic(&receipt_path(app_dir), &render_receipts(&all)).map_err(
        |err| {
            format!(
                "{} could not be written: {err}",
                receipt_path(app_dir).display()
            )
        },
    )?;
    Ok(receipt)
}

/// Forget one harness's approval. Returns whether there was one.
pub fn revoke(app_dir: &Path, harness: &str) -> Result<bool, String> {
    let (mut all, _) = receipts(app_dir);
    if all.remove(harness).is_none() {
        return Ok(false);
    }
    let path = receipt_path(app_dir);
    if all.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(true);
    }
    crate::private::write_atomic(&path, &render_receipts(&all))
        .map_err(|err| format!("{} could not be written: {err}", path.display()))?;
    Ok(true)
}

fn render_receipts(all: &BTreeMap<String, Receipt>) -> String {
    let mut out = String::from(
        "# Approvals for harness-side extensions (aoe-llm-tags).\n\
         # Written when a user reads the disclosure and enables an extension.\n\
         # Deleting a table revokes that harness; nothing else invalidates one.\n",
    );
    for (harness, receipt) in all {
        out.push_str(&format!(
            "\n[extension.{harness}]\nsha256 = {:?}\ndisclosure_version = {}\n\
             accepted_at = {:?}\naccepted_plugin = {:?}\ntested_harness = {:?}\n",
            receipt.sha256,
            receipt.disclosure_version,
            receipt.accepted_at,
            receipt.accepted_plugin,
            receipt.tested_harness
        ));
    }
    out
}

/// The artefact source this build carries for a row, if the row names one.
///
/// Compiled in ([`crate::artifacts`]), so "the file the user approved" is a
/// property of the binary and a session copy needs no lookup outside it.
pub fn embedded(adapter: &Adapter) -> Option<&'static str> {
    if adapter.extension_artifact.is_empty() {
        return None;
    }
    crate::artifacts::bytes(&adapter.extension_artifact)
}

/// The shipped artefact's path, for messages. Empty when the row names none.
pub fn artifact_path(adapter: &Adapter) -> PathBuf {
    PathBuf::from(&adapter.extension_artifact)
}

/// The digest of the artefact this build would write.
pub fn artifact_sha(adapter: &Adapter) -> Result<String, Refusal> {
    let harness = adapter.agent.clone();
    if adapter.extension_artifact.is_empty() {
        return Err(Refusal::NoArtifact { harness });
    }
    match embedded(adapter) {
        Some(source) => Ok(crate::sha256::hex(source.as_bytes())),
        None => Err(Refusal::MissingArtifact {
            harness,
            path: artifact_path(adapter),
            why: "this build does not carry it".to_string(),
        }),
    }
}

/// The name our file takes inside the harness's extension directory.
pub fn file_name(adapter: &Adapter) -> String {
    if adapter.extension_filename.is_empty() {
        DEFAULT_FILE.to_string()
    } else {
        adapter.extension_filename.clone()
    }
}

/// The directory inside the session config copy (`extensions` for a harness
/// that loads `<config>/extensions/*.ts`).
pub fn subdir(adapter: &Adapter) -> String {
    if adapter.extension_subdir.is_empty() {
        "extensions".to_string()
    } else {
        adapter.extension_subdir.clone()
    }
}

fn live_env(adapter: &Adapter) -> String {
    if adapter.extension_env.is_empty() {
        DEFAULT_LIVE_ENV.to_string()
    } else {
        adapter.extension_env.clone()
    }
}

/// What this build would write for a row, ignoring whether the user asked for
/// it: the file, its digest and where it would go. `extension enable` prints
/// this as the disclosure, so the text a user approves is derived from the same
/// place the writer uses.
pub fn proposal(adapter: &Adapter, app_dir: &Path, session_id: &str) -> Result<Plan, Refusal> {
    let harness = adapter.agent.clone();
    if adapter.extension_artifact.is_empty() {
        return Err(Refusal::NoArtifact { harness });
    }
    if adapter.write_mode != crate::overlay::MODE_OVERLAY_DIR {
        return Err(Refusal::NoDirectory {
            harness,
            mode: if adapter.write_mode.is_empty() {
                crate::overlay::MODE_NONE.to_string()
            } else {
                adapter.write_mode.clone()
            },
        });
    }
    let sha256 = artifact_sha(adapter)?;
    Ok(Plan {
        harness,
        artifact: artifact_path(adapter),
        sha256,
        subdir: subdir(adapter),
        file: file_name(adapter),
        env_var: live_env(adapter),
        live: live_path(app_dir, session_id),
    })
}

/// The full decision for one row: allowed (with the plan), or the one reason it
/// is not. Checked in the order a user would fix them.
pub fn plan(
    adapter: &Adapter,
    wanted: &[String],
    app_dir: &Path,
    session_id: &str,
) -> Result<Plan, Refusal> {
    let harness = adapter.agent.clone();
    if adapter.extension_artifact.is_empty() {
        return Err(Refusal::NoArtifact { harness });
    }
    if !wanted.iter().any(|name| name == &harness) {
        return Err(Refusal::NoIntent { harness });
    }
    let plan = proposal(adapter, app_dir, session_id)?;
    let (all, _) = receipts(app_dir);
    let Some(receipt) = all.get(&harness) else {
        return Err(Refusal::NoReceipt {
            harness,
            sha256: plan.sha256,
        });
    };
    if receipt.sha256 != plan.sha256 {
        return Err(Refusal::HashMismatch {
            harness,
            approved: receipt.sha256.clone(),
            shipped: plan.sha256,
        });
    }
    if receipt.disclosure_version != DISCLOSURE_VERSION {
        return Err(Refusal::DisclosureMismatch {
            harness,
            approved: receipt.disclosure_version,
            current: DISCLOSURE_VERSION,
        });
    }
    Ok(plan)
}

/// `sessions/<id>/live.json` — one file per session, whichever harness reads it.
pub fn live_path(app_dir: &Path, session_id: &str) -> PathBuf {
    app_dir
        .join("sessions")
        .join(crate::overlay::sanitize(session_id))
        .join(LIVE_FILE)
}

/// What an extension is allowed to send, written for it to re-read.
///
/// The values are the ones the launch already baked into the config copy, so a
/// live file can only ever repeat a decision this plugin made — it never mints a
/// tag of its own. `enabled` is what a revoke flips, so a running session stops
/// sending dynamic values on its next request instead of at the next launch.
pub fn write_live(
    app_dir: &Path,
    session_id: &str,
    resolved: &[ResolvedHeader],
    enabled: bool,
) -> Result<PathBuf, String> {
    let path = live_path(app_dir, session_id);
    let targets: Vec<serde_json::Value> = resolved
        .iter()
        .map(|item| {
            serde_json::json!({
                "kind": "header",
                "name": item.header,
                "value": item.value,
            })
        })
        .collect();
    let body = serde_json::json!({
        "session_id": session_id,
        "generated_at": now_rfc3339(),
        "enabled": enabled,
        "targets": targets,
    });
    if let Some(parent) = path.parent() {
        crate::private::create_dir(parent)
            .map_err(|err| format!("{} could not be created: {err}", parent.display()))?;
    }
    crate::private::write_atomic(&path, &format!("{body}\n"))
        .map_err(|err| format!("{} could not be written: {err}", path.display()))?;
    Ok(path)
}

/// The text a user approves, before anything is written. It names the harness,
/// the file and the digest, and it says what the code does *and* what it does
/// not — the version they approve is pinned by [`DISCLOSURE_VERSION`].
pub fn disclosure(plan: &Plan, harness_version: &str) -> String {
    format!(
        "{harness} — per-request hook\n\
         \n\
         Enabling this puts one extra file in each {harness} session's private config copy:\n\
         <session config>/{subdir}/{file}. {harness} loads it inside the agent process, with the\n\
         agent's own permissions. It is what lets a tag change reach the next request instead of\n\
         the next launch, and what lets the session report what it is doing — running tests,\n\
         writing code.\n\
         \n\
         It does: read one JSON file this plugin writes (live.json: the session's tags and its\n\
         current activity) and set the {harness} request headers from it on each model request.\n\
         It does not: read your prompts, transcript, files or credentials; open a socket; spawn a\n\
         process; or write anything outside that session folder.\n\
         To undo: set `{SETTING} = []`, or run `aoe-llm-tags extension disable {harness}`. A\n\
         session that is already running stops sending dynamic tags on its next request.\n\
         \n\
         plugin {plugin} · disclosure v{version} · sha256 {sha}\n\
         requires {harness} with an in-process request hook (yours: {harness_version})",
        harness = plan.harness,
        subdir = plan.subdir,
        file = plan.file,
        sha = plan.sha256,
        plugin = plugin_version(),
        version = DISCLOSURE_VERSION,
        harness_version = if harness_version.is_empty() {
            "unknown"
        } else {
            harness_version
        },
    )
}

/// The plugin's own version, from the manifest compiled into the binary.
pub fn plugin_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn valid_sha(text: &str) -> bool {
    text.len() == 64 && text.chars().all(|ch| ch.is_ascii_hexdigit())
}

/// `YYYY-MM-DDTHH:MM:SSZ`, from the system clock. Hand-rolled like the rest of
/// this crate's small primitives: a timestamp here is informational, and a date
/// crate for one line is not worth the dependency.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 → `(year, month, day)` (Howard Hinnant's civil_from_days).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_day_renders_as_a_date() {
        // 2026-11-07 is 20764 days after the epoch.
        assert_eq!(civil_from_days(20_764), (2026, 11, 7));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn a_short_or_non_hex_digest_is_not_a_receipt() {
        assert!(!valid_sha(""));
        assert!(!valid_sha("abc"));
        assert!(!valid_sha(&"z".repeat(64)));
        assert!(valid_sha(&"a".repeat(64)));
    }

    #[test]
    fn the_disclosure_names_the_file_the_digest_and_how_to_undo_it() {
        let plan = Plan {
            harness: "example".into(),
            artifact: PathBuf::from("harness/example.ts"),
            sha256: "b".repeat(64),
            subdir: "extensions".into(),
            file: "aoe-llm-tags.ts".into(),
            env_var: DEFAULT_LIVE_ENV.into(),
            live: PathBuf::from("/tmp/live.json"),
        };
        let text = disclosure(&plan, "1.2.3");
        for needle in [
            "example",
            "extensions/aoe-llm-tags.ts",
            &"b".repeat(64),
            "extension disable example",
            "does not: read your prompts",
            "v1",
            "1.2.3",
        ] {
            assert!(
                text.contains(needle),
                "the disclosure omits {needle:?}: {text}"
            );
        }
    }

    #[test]
    fn a_receipt_round_trips_through_the_file() {
        let dir = std::env::temp_dir().join(format!("aoe-ext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::private::create_dir(&dir).expect("app dir");
        let sha = "c".repeat(64);
        accept(&dir, "example", &sha, "9.9.9").expect("accept");
        let (all, warnings) = receipts(&dir);
        assert!(warnings.is_empty(), "{warnings:?}");
        let receipt = all.get("example").expect("a receipt");
        assert_eq!(receipt.sha256, sha);
        assert_eq!(receipt.disclosure_version, DISCLOSURE_VERSION);
        assert_eq!(receipt.tested_harness, "9.9.9");
        assert!(receipt.accepted_at.ends_with('Z'));
        // A second harness is a second approval, not a re-use of the first.
        accept(&dir, "other", &"d".repeat(64), "").expect("accept other");
        let (all, _) = receipts(&dir);
        assert_eq!(all.len(), 2);
        assert!(revoke(&dir, "example").expect("revoke"));
        assert!(!revoke(&dir, "example").expect("revoke twice"));
        let (all, _) = receipts(&dir);
        assert_eq!(all.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_receipt_file_is_a_warning_not_a_crash() {
        let dir = std::env::temp_dir().join(format!("aoe-ext-broken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::private::create_dir(&dir).expect("app dir");
        std::fs::write(receipt_path(&dir), "not = [valid").expect("write");
        let (all, warnings) = receipts(&dir);
        assert!(all.is_empty());
        assert_eq!(warnings.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
