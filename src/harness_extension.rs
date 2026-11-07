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

/// The user's home directory, as a user-scope row's `~` resolves.
///
/// One place, so the disclosure, the writer and `doctor` cannot disagree about
/// which directory `~/.claude/...` means.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// The receipt file, in the app dir next to `config.toml`.
pub const RECEIPT_FILE: &str = "harness-extension.toml";

/// Which disclosure the user approved. Bump it when the wording in
/// [`disclosure`] changes in a way a user should see again — every receipt with
/// an older number stops being enough, and the file the user approves is the
/// same file.
pub const DISCLOSURE_VERSION: u32 = 3;

/// The default file name inside the harness's extension directory. A row may
/// override it; the name is ours so it can be recognised and removed.
pub const DEFAULT_FILE: &str = "aoe-llm-tags.ts";

/// The variable that tells an extension where this session's live file is.
/// A row may override it; the extension only ever reads the path it is given.
pub const DEFAULT_LIVE_ENV: &str = "AOE_LLM_TAGS_LIVE";

/// The live file's name inside `sessions/<id>/`.
pub const LIVE_FILE: &str = "live.json";

/// The file name a skill is read from (the harness's convention, not ours).
pub const SKILL_FILE: &str = "SKILL.md";

/// Our skill's directory name, when a row does not name one.
pub const DEFAULT_SKILL_DIR: &str = "aoe-activity";

/// The skills root inside a session's config copy.
pub const DEFAULT_SKILL_SUBDIR: &str = "skills";

/// The always-loaded instruction file, when a row does not name one.
pub const DEFAULT_APPEND_FILE: &str = "APPEND_SYSTEM.md";

/// How a skill this plugin owns announces itself in AoE's managed store, so an
/// update can refresh its own copy and never overwrite one a person edited.
pub const MANAGED_MARKER: &str = "<!-- aoe-llm-tags: managed skill -->";

/// The two lines that delimit what this plugin owns inside a file that is
/// otherwise the user's (`~/.claude/CLAUDE.md` and friends). Everything outside
/// them is left byte-for-byte alone, and removal deletes exactly the span.
pub const BLOCK_BEGIN: &str = "<!-- aoe-llm-tags:begin -->";
pub const BLOCK_END: &str = "<!-- aoe-llm-tags:end -->";

/// Where a row's carriers go.
///
/// Most harnesses have no per-session config directory, so "the session's own
/// activity" can only reach them through their own config under the user's
/// home. That is a different grant — we write inside a file the user owns — and
/// it is disclosed as such, not folded into the session-copy wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    /// Into the harness's per-session config copy; nothing of the user's own
    /// config is touched and the copy is rebuilt on every launch.
    Session,
    /// Into the harness's own config under the user's home, behind a marker
    /// block for the instruction file and in our own skill directory.
    User,
}

impl Carrier {
    pub fn name(self) -> &'static str {
        match self {
            Carrier::Session => "session",
            Carrier::User => "user",
        }
    }
}

/// The three carriers of one row, placed for one scope.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bundle {
    /// The in-process hook. Only a session copy can carry one.
    pub hook: Option<Placement>,
    pub skill: Option<Placement>,
    pub instructions: Option<Placement>,
}

impl Bundle {
    pub fn parts(&self) -> impl Iterator<Item = &Placement> {
        self.hook
            .iter()
            .chain(self.skill.iter())
            .chain(self.instructions.iter())
    }

    pub fn is_empty(&self) -> bool {
        self.hook.is_none() && self.skill.is_none() && self.instructions.is_none()
    }
}

/// Everything a harness needs, once the two conditions are met.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub harness: String,
    pub sha256: String,
    /// Which scope the carriers were placed for.
    pub carrier: Carrier,
    /// The in-process hook, when the row ships one (session scope only).
    pub hook: Option<Placement>,
    /// The skill written for the harness, when the row ships one.
    pub skill: Option<Placement>,
    /// Instructions for the always-loaded context, when the row ships them.
    pub instructions: Option<Placement>,
    /// The variable the hook exports so the extension finds [`LIVE_FILE`].
    pub env_var: String,
    /// `sessions/<id>/live.json`.
    pub live: PathBuf,
    /// The mirror in AoE's managed skill store (`<app dir>/skills/<dir>/`).
    pub managed: Option<Placement>,
}

impl Plan {
    pub fn parts(&self) -> impl Iterator<Item = &Placement> {
        self.hook
            .iter()
            .chain(self.skill.iter())
            .chain(self.instructions.iter())
    }
}

/// One file of the bundle, and where it goes.
///
/// `subdir` is relative to the scope's base: the session's config copy for
/// [`Carrier::Session`], the user's home for [`Carrier::User`]. So a session
/// skill is `skills/aoe-activity/SKILL.md`, a user skill is
/// `.claude/skills/aoe-activity/SKILL.md`, and an appended instruction file
/// sits at that base's root. Empty `file` never happens: the artefact's own name
/// is the fallback.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    /// Path in the plugin tree, for messages and the digest.
    pub artifact: PathBuf,
    pub subdir: String,
    pub file: String,
    /// The path as the disclosure and `doctor` spell it
    /// (`<session config>/extensions/aoe-llm-tags.ts`, `~/.claude/CLAUDE.md`).
    pub display: String,
}

impl Placement {
    /// Where this lands, given the scope's base directory.
    pub fn target(&self, dir: &Path) -> PathBuf {
        if self.subdir.is_empty() {
            dir.join(&self.file)
        } else {
            dir.join(&self.subdir).join(&self.file)
        }
    }

    /// The artefact as shipped, or `None` when this build does not carry it.
    pub fn source(&self) -> Option<&'static str> {
        crate::artifacts::bytes(&self.artifact.to_string_lossy())
    }
}

/// Why a harness does not get an extension. Every variant is a different
/// sentence in `doctor`, because "sends nothing dynamic" must never be a
/// silent no-op.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// The adapter row has no in-harness hook at all.
    NoArtifact { harness: String },
    /// The row has nowhere to put either carrier: no hook, no skill root, no
    /// instruction file.
    NoDestination { harness: String },
    /// A user-scope row, with no home directory to resolve `~` against.
    NoHome { harness: String },
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
            Refusal::NoDestination { .. } => "no_destination",
            Refusal::NoHome { .. } => "no_home",
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
        !matches!(
            self,
            Refusal::NoArtifact { .. } | Refusal::NoDestination { .. } | Refusal::NoIntent { .. }
        )
    }

    pub fn harness(&self) -> &str {
        match self {
            Refusal::NoArtifact { harness }
            | Refusal::NoDestination { harness }
            | Refusal::NoHome { harness }
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
            Refusal::NoDestination { harness } => format!(
                "{harness} has nowhere this plugin can put an activity carrier (no in-process \
                 hook, no skill root and no always-loaded instruction file): launch-time tags only"
            ),
            Refusal::NoHome { harness } => format!(
                "{harness} keeps its carriers in your own config, and no home directory is known \
                 to resolve `~` against: launch-time tags only"
            ),
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
    home: Option<&Path>,
    adapter: &Adapter,
    sha256: &str,
    harness_version: &str,
) -> Result<Receipt, String> {
    let harness = adapter.agent.as_str();
    if !valid_sha(sha256) {
        return Err(format!("{sha256:?} is not a sha256 digest"));
    }
    // The carriers first: an approval that could not be written is not an
    // approval, and a receipt for files that are not there would make the next
    // launch claim a delivery it never made.
    if carrier(adapter) == Some(Carrier::User) {
        let home = home.ok_or_else(|| "no home directory to write to".to_string())?;
        install_user(home, adapter)?;
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
    // The approval is the consent, so this is where the skill joins AoE's own
    // skill list: nothing appears on the Skills page before somebody said yes.
    let _ = install_managed_skill(app_dir, adapter);
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
pub fn revoke(app_dir: &Path, home: Option<&Path>, adapter: &Adapter) -> Result<bool, String> {
    let harness = adapter.agent.as_str();
    // The managed entry is the discoverable copy, so it goes with the approval.
    let _ = remove_managed_skill(app_dir, adapter);
    // And so does anything this approval put in the user's own config. A
    // revoke that left a skill behind would be a revocation in name only.
    if let Some(home) = home {
        if carrier(adapter) == Some(Carrier::User) {
            let _ = remove_user(home, adapter);
        }
    }
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

/// Which scope a row's carriers go to, or `None` when it has no place for any.
///
/// `None` is a real answer for a harness with no hook, no skill root and no
/// always-loaded instruction file: it gets launch-time tags and nothing else,
/// and [`Refusal::NoDestination`] says so rather than inventing a path.
pub fn carrier(adapter: &Adapter) -> Option<Carrier> {
    match adapter.carrier_scope.trim() {
        "session" => Some(Carrier::Session),
        "user" => Some(Carrier::User),
        _ if !adapter.extension_artifact.is_empty() => Some(Carrier::Session),
        _ if !adapter.user_skill_root.is_empty() || !adapter.user_instructions.is_empty() => {
            Some(Carrier::User)
        }
        _ => None,
    }
}

/// Every file a row carries, placed for `carrier`.
pub fn bundle(adapter: &Adapter, carrier: Carrier) -> Bundle {
    let mut out = Bundle::default();
    match carrier {
        Carrier::Session => {
            if !adapter.extension_artifact.is_empty() {
                out.hook = Some(Placement {
                    artifact: artifact_path(adapter),
                    subdir: subdir(adapter),
                    file: file_name(adapter),
                    display: format!(
                        "<session config>/{}/{}",
                        subdir(adapter),
                        file_name(adapter)
                    ),
                });
            }
            if !adapter.skill_artifact.is_empty() {
                out.skill = Some(Placement {
                    artifact: PathBuf::from(&adapter.skill_artifact),
                    subdir: skills_dir(adapter),
                    file: SKILL_FILE.to_string(),
                    display: format!("<session config>/{}/{}", skills_dir(adapter), SKILL_FILE),
                });
            }
            if !adapter.append_artifact.is_empty() {
                out.instructions = Some(Placement {
                    artifact: PathBuf::from(&adapter.append_artifact),
                    subdir: String::new(),
                    file: append_file(adapter),
                    display: format!("<session config>/{}", append_file(adapter)),
                });
            }
        }
        Carrier::User => {
            if let (false, Some(dir)) = (adapter.skill_artifact.is_empty(), user_skill_dir(adapter))
            {
                out.skill = Some(Placement {
                    artifact: PathBuf::from(&adapter.skill_artifact),
                    subdir: dir.clone(),
                    file: SKILL_FILE.to_string(),
                    display: format!("~/{dir}/{SKILL_FILE}"),
                });
            }
            if let (false, Some((parent, name))) = (
                adapter.append_artifact.is_empty(),
                split_user_path(&adapter.user_instructions),
            ) {
                out.instructions = Some(Placement {
                    artifact: PathBuf::from(&adapter.append_artifact),
                    subdir: parent.clone(),
                    file: name.clone(),
                    display: if parent.is_empty() {
                        format!("~/{name}")
                    } else {
                        format!("~/{parent}/{name}")
                    },
                });
            }
        }
    }
    out
}

/// The skill's directory under the user's home (`.example/skills/aoe-activity`).
fn user_skill_dir(adapter: &Adapter) -> Option<String> {
    let root = home_relative(&adapter.user_skill_root)?;
    Some(format!("{root}/{}", skill_dir(adapter)))
}

/// `~/.example/AGENTS.md` → `(".example", "AGENTS.md")`.
fn split_user_path(spec: &str) -> Option<(String, String)> {
    let rest = home_relative(spec)?;
    Some(match rest.rsplit_once('/') {
        Some((parent, name)) => (parent.to_string(), name.to_string()),
        None => (String::new(), rest.to_string()),
    })
}

/// A row's `~/...` spec as a home-relative path.
///
/// Only `~`-relative specs are usable: an absolute path in an adapter row would
/// write outside the one place a user-scope approval is allowed to touch, and
/// `..` is how a path escapes the home it names.
fn home_relative(spec: &str) -> Option<String> {
    let rest = spec.trim().strip_prefix("~/")?;
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() || rest.split('/').any(|part| part == ".." || part.is_empty()) {
        return None;
    }
    Some(rest.to_string())
}

/// The artefact source this build carries for a row's hook, if it names one.
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

/// The digest a receipt pins: every file of the bundle, in a stable order.
///
/// Hashing the bundle rather than only the extension means a build that adds or
/// rewords the skill asks for a fresh approval — the user approved a set of
/// files that run in their agent, and a set that grew is a different set.
pub fn bundle_sha(adapter: &Adapter, carrier: Carrier) -> Result<String, Refusal> {
    let harness = adapter.agent.clone();
    let bundle = bundle(adapter, carrier);
    if bundle.is_empty() {
        return Err(Refusal::NoDestination { harness });
    }
    // Where each file goes is part of what the user approved: moving a skill
    // into a different directory writes into a different place in their config,
    // so it is a different grant even when the bytes are identical.
    let mut outline = format!("carrier:{}\n", carrier.name());
    for placement in bundle.parts() {
        let name = placement.artifact.to_string_lossy().into_owned();
        let Some(source) = placement.source() else {
            return Err(Refusal::MissingArtifact {
                harness,
                path: placement.artifact.clone(),
                why: "this build does not carry it".to_string(),
            });
        };
        outline.push_str(&format!(
            "{}\n{name}\n{}\n",
            placement.display,
            crate::sha256::hex(source.as_bytes())
        ));
    }
    Ok(crate::sha256::hex(outline.as_bytes()))
}

/// Every file a row's bundle consists of for a session copy, hook first.
pub fn placements(adapter: &Adapter) -> Vec<Placement> {
    let bundle = bundle(adapter, Carrier::Session);
    bundle.parts().cloned().collect()
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

/// The skills root inside a session's config copy (`skills`).
pub fn skill_root(adapter: &Adapter) -> String {
    if adapter.skill_subdir.is_empty() {
        DEFAULT_SKILL_SUBDIR.to_string()
    } else {
        adapter.skill_subdir.clone()
    }
}

/// Our skill's directory name (`aoe-activity`), i.e. `skills/<this>/SKILL.md`.
pub fn skill_dir(adapter: &Adapter) -> String {
    if adapter.skill_dir.is_empty() {
        DEFAULT_SKILL_DIR.to_string()
    } else {
        adapter.skill_dir.clone()
    }
}

/// The skill's subdir inside the config copy: `skills/aoe-activity`.
fn skills_dir(adapter: &Adapter) -> String {
    format!("{}/{}", skill_root(adapter), skill_dir(adapter))
}

/// The always-loaded file's name for a row, when it ships one.
pub fn instructions_name(adapter: &Adapter) -> Option<String> {
    (!adapter.append_artifact.is_empty()).then(|| append_file(adapter))
}

fn append_file(adapter: &Adapter) -> String {
    if adapter.append_filename.is_empty() {
        DEFAULT_APPEND_FILE.to_string()
    } else {
        adapter.append_filename.clone()
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
pub fn proposal(
    adapter: &Adapter,
    app_dir: &Path,
    session_id: &str,
    home: Option<&Path>,
) -> Result<Plan, Refusal> {
    let harness = adapter.agent.clone();
    let Some(carrier) = carrier(adapter) else {
        return Err(Refusal::NoDestination { harness });
    };
    if carrier == Carrier::User && home.is_none() {
        return Err(Refusal::NoHome { harness });
    }
    // A session copy needs a directory this plugin owns; a row delivered through
    // a single generated file has none, and putting files beside it would write
    // into the harness's own config instead.
    if carrier == Carrier::Session && adapter.write_mode != crate::overlay::MODE_OVERLAY_DIR {
        return Err(Refusal::NoDirectory {
            harness,
            mode: if adapter.write_mode.is_empty() {
                crate::overlay::MODE_NONE.to_string()
            } else {
                adapter.write_mode.clone()
            },
        });
    }
    let bundle = bundle(adapter, carrier);
    let sha256 = bundle_sha(adapter, carrier)?;
    Ok(Plan {
        harness,
        sha256,
        carrier,
        // The managed mirror is the same file under the app dir, so it is part
        // of what the user sees, not a separate grant.
        managed: bundle.skill.as_ref().map(|_| Placement {
            artifact: PathBuf::from(&adapter.skill_artifact),
            subdir: format!("skills/{}", skill_dir(adapter)),
            file: SKILL_FILE.to_string(),
            display: format!("<app dir>/skills/{}/", skill_dir(adapter)),
        }),
        hook: bundle.hook,
        skill: bundle.skill,
        instructions: bundle.instructions,
        env_var: live_env(adapter),
        live: live_path(app_dir, session_id),
    })
}

/// The skill's bytes as written, marker included.
///
/// The marker is what lets a later release recognise its own copy in AoE's
/// managed store: a skill a person edited (or adopted and rewrote) does not
/// carry it, so it is never overwritten.
pub fn skill_text(adapter: &Adapter) -> Option<String> {
    let source = crate::artifacts::bytes(&adapter.skill_artifact)?;
    Some(format!("{}\n{MANAGED_MARKER}\n", source.trim_end()))
}

/// The always-loaded reminder's bytes as written (no marker: the block is the
/// marker).
pub fn instructions_text(adapter: &Adapter) -> Option<&'static str> {
    crate::artifacts::bytes(&adapter.append_artifact)
}

/// Write one carrier into the user's own config: the skill as its own directory,
/// the reminder as a marker block inside a file that stays theirs.
///
/// Returns the paths written. A skill directory somebody else owns is never
/// overwritten, and a file we cannot read is not reworded blind: both are an
/// error, so the approval fails loudly instead of half-applying.
pub fn install_user(home: &Path, adapter: &Adapter) -> Result<Vec<PathBuf>, String> {
    let bundle = bundle(adapter, Carrier::User);
    let mut written = Vec::new();
    if let Some(skill) = &bundle.skill {
        let path = skill.target(home);
        if path.symlink_metadata().is_ok() && !owned_skill(&path) {
            return Err(format!(
                "{} exists and is not ours (it has no `{MANAGED_MARKER}` marker); leaving it alone",
                path.display()
            ));
        }
        let Some(text) = skill_text(adapter) else {
            return Err(format!(
                "{} is not carried by this build",
                skill.artifact.display()
            ));
        };
        write_carrier(&path, &text)?;
        written.push(path);
    }
    if let Some(instructions) = &bundle.instructions {
        let path = instructions.target(home);
        let Some(source) = instructions_text(adapter) else {
            return Err(format!(
                "{} is not carried by this build",
                instructions.artifact.display()
            ));
        };
        let existing = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(format!("{} could not be read: {err}", path.display())),
        };
        write_carrier(&path, &upsert_block(&existing, source))?;
        written.push(path);
    }
    Ok(written)
}

/// Take the carriers back out of the user's own config. Returns the paths it
/// changed: our skill directory, and the reminder's file when the block was the
/// only thing in it (or the file itself when it was not).
pub fn remove_user(home: &Path, adapter: &Adapter) -> Result<Vec<PathBuf>, String> {
    let bundle = bundle(adapter, Carrier::User);
    let mut touched = Vec::new();
    if let Some(skill) = &bundle.skill {
        let path = skill.target(home);
        if path.symlink_metadata().is_ok() && owned_skill(&path) {
            if let Some(dir) = path.parent() {
                std::fs::remove_dir_all(dir)
                    .map_err(|err| format!("{} could not be removed: {err}", dir.display()))?;
                touched.push(path);
            }
        }
    }
    if let Some(instructions) = &bundle.instructions {
        let path = instructions.target(home);
        let Ok(existing) = std::fs::read_to_string(&path) else {
            return Ok(touched);
        };
        let rest = strip_block(&existing);
        if rest.trim().is_empty() {
            std::fs::remove_file(&path)
                .map_err(|err| format!("{} could not be removed: {err}", path.display()))?;
            touched.push(path);
        } else if rest != existing {
            write_carrier(&path, &format!("{}\n", rest.trim_end()))?;
            touched.push(path);
        }
    }
    Ok(touched)
}

/// Whether the file at `path` is this plugin's own skill copy: it carries the
/// marker. A missing file counts as ours (there is nothing to lose), a file
/// without the marker does not.
fn owned_skill(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(text) => text.contains(MANAGED_MARKER),
        Err(_) => true,
    }
}

fn write_carrier(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            crate::private::create_dir(parent)
                .map_err(|err| format!("{} could not be created: {err}", parent.display()))?;
        }
    }
    crate::private::write_atomic(path, text)
        .map_err(|err| format!("{} could not be written: {err}", path.display()))
}

/// Our reminder wrapped in its markers.
pub fn block(source: &str) -> String {
    format!("{BLOCK_BEGIN}\n{}\n{BLOCK_END}", source.trim())
}

/// The byte range our block occupies in `text`, markers included.
fn block_span(text: &str) -> Option<(usize, usize)> {
    let start = text.find(BLOCK_BEGIN)?;
    let end = text[start..].find(BLOCK_END)? + start + BLOCK_END.len();
    Some((start, end))
}

/// Put our block into a file that is otherwise the user's: replace the previous
/// one in place, else append it, so a second approval is idempotent and a file
/// with their own text everywhere else is never reworded.
pub fn upsert_block(existing: &str, source: &str) -> String {
    let ours = block(source);
    match block_span(existing) {
        Some((start, end)) => format!("{}{ours}{}", &existing[..start], &existing[end..]),
        None => {
            let theirs = existing.trim_end();
            if theirs.is_empty() {
                format!("{ours}\n")
            } else {
                format!("{theirs}\n\n{ours}\n")
            }
        }
    }
}

/// Remove our block, leaving the rest of the file as it was.
pub fn strip_block(text: &str) -> String {
    let Some((start, end)) = block_span(text) else {
        return text.to_string();
    };
    let head = text[..start].trim_end();
    let tail = text[end..].trim_start_matches('\n');
    match (head.is_empty(), tail.is_empty()) {
        (_, true) => head.to_string(),
        (true, false) => tail.to_string(),
        (false, false) => format!("{head}\n\n{tail}"),
    }
}

/// `<app dir>/skills/<skill dir>/SKILL.md` — AoE's own managed skills store,
/// which is what its Skills page lists.
pub fn managed_skill_dir(app_dir: &Path, adapter: &Adapter) -> PathBuf {
    app_dir.join("skills").join(skill_dir(adapter))
}

pub fn managed_skill_path(app_dir: &Path, adapter: &Adapter) -> PathBuf {
    managed_skill_dir(app_dir, adapter).join(SKILL_FILE)
}

/// Whether the managed copy on disk is this plugin's, i.e. safe to rewrite.
///
/// Absent counts as ours (nothing to lose); present without the marker does
/// not, because that is a person's skill and a plugin does not get to replace
/// it silently.
pub fn managed_skill_is_ours(app_dir: &Path, adapter: &Adapter) -> bool {
    match std::fs::read_to_string(managed_skill_path(app_dir, adapter)) {
        Ok(text) => text.contains(MANAGED_MARKER),
        Err(_) => true,
    }
}

/// Put the skill in AoE's managed store so it appears on the Skills page and
/// can be read, edited or removed like any other AoE skill.
///
/// Returns whether it wrote. A copy somebody else owns is left alone and
/// reported, because the session copy still delivers the skill — the managed
/// entry is the discoverable one, not the one the agent loads.
pub fn install_managed_skill(app_dir: &Path, adapter: &Adapter) -> Result<bool, String> {
    let Some(text) = skill_text(adapter) else {
        return Ok(false);
    };
    let dir = managed_skill_dir(app_dir, adapter);
    if !managed_skill_is_ours(app_dir, adapter) {
        return Ok(false);
    }
    crate::private::create_dir(&dir)
        .map_err(|err| format!("{} could not be created: {err}", dir.display()))?;
    let path = dir.join(SKILL_FILE);
    crate::private::write_atomic(&path, &text)
        .map_err(|err| format!("{} could not be written: {err}", path.display()))?;
    Ok(true)
}

/// Remove the managed copy, if it is still ours. Returns whether it went.
pub fn remove_managed_skill(app_dir: &Path, adapter: &Adapter) -> Result<bool, String> {
    let dir = managed_skill_dir(app_dir, adapter);
    if dir.symlink_metadata().is_err() {
        return Ok(false);
    }
    if !managed_skill_is_ours(app_dir, adapter) {
        return Ok(false);
    }
    std::fs::remove_dir_all(&dir)
        .map_err(|err| format!("{} could not be removed: {err}", dir.display()))?;
    Ok(true)
}

/// The full decision for one row: allowed (with the plan), or the one reason it
/// is not. Checked in the order a user would fix them.
pub fn plan(
    adapter: &Adapter,
    wanted: &[String],
    app_dir: &Path,
    session_id: &str,
    home: Option<&Path>,
) -> Result<Plan, Refusal> {
    let harness = adapter.agent.clone();
    if carrier(adapter).is_none() {
        return Err(Refusal::NoDestination { harness });
    }
    if !wanted.iter().any(|name| name == &harness) {
        return Err(Refusal::NoIntent { harness });
    }
    let plan = proposal(adapter, app_dir, session_id, home)?;
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
    // The worker rewrites this file whenever the session's tags change, and the
    // extension re-reads it when its mtime moves. A write that would not change
    // what it says is therefore worth skipping: it saves a read per request and
    // keeps `generated_at` meaning "when the values last changed".
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if let Ok(existing) = serde_json::from_str::<serde_json::Value>(&existing) {
            if existing.get("enabled") == body.get("enabled")
                && existing.get("targets") == body.get("targets")
            {
                return Ok(path);
            }
        }
    }
    if let Some(parent) = path.parent() {
        crate::private::create_dir(parent)
            .map_err(|err| format!("{} could not be created: {err}", parent.display()))?;
    }
    crate::private::write_atomic(&path, &format!("{body}\n"))
        .map_err(|err| format!("{} could not be written: {err}", path.display()))?;
    Ok(path)
}

/// Keep a running session's live file in step with what the plugin resolves now.
///
/// A session that was launched with an approved extension has a live file; every
/// other session has none, so this is a no-op for them. This is the whole point
/// of the file: a tag edited while the session runs reaches the *next request*
/// without a relaunch, and a revoke flips `enabled` so the same session stops
/// sending dynamic values on its next request instead of at its next launch.
pub fn refresh_live(
    app_dir: &Path,
    session_id: &str,
    adapter: Option<&crate::adapters::Adapter>,
    intent: &[String],
    resolved: &[ResolvedHeader],
    home: Option<&Path>,
) -> Option<Result<PathBuf, String>> {
    // No file means this session never got an extension: writing one now would
    // invent a delivery the launch did not set up.
    live_path(app_dir, session_id).exists().then(|| {
        let enabled =
            adapter.is_some_and(|adapter| plan(adapter, intent, app_dir, session_id, home).is_ok());
        write_live(app_dir, session_id, resolved, enabled)
    })
}

/// The text a user approves, before anything is written. It names the harness,
/// the files and the digest, and it says what the code does *and* what it does
/// not — the version they approve is pinned by [`DISCLOSURE_VERSION`].
///
/// Two shapes, because the grant is two different grants: a session copy is
/// private to that session, while a user-scope row writes inside the user's own
/// config. Folding them into one sentence would describe neither.
pub fn disclosure(plan: &Plan, harness_version: &str) -> String {
    let files = disclosed_paths(plan);
    let harness = plan.harness.as_str();
    let version = if harness_version.is_empty() {
        "unknown"
    } else {
        harness_version
    };
    // What the row actually carries decides the words: a row with no in-process
    // hook must not claim per-request delivery, and a row with no skill must not
    // claim to install one. A disclosure that describes the wrong bundle would
    // be a false statement the user approves.
    let carries = |placement: &Option<Placement>| placement.is_some();
    let hook = carries(&plan.hook);
    let skill = carries(&plan.skill);
    let reminder = carries(&plan.instructions);
    let managed = plan.managed.is_some();
    let source = if skill {
        "from the skill"
    } else {
        "following the reminder"
    };
    let only_managed = if managed {
        ", that session's own settings and the managed skill listed there"
    } else {
        " and that session's own settings"
    };
    let footer = format!(
        "plugin {plugin} · disclosure v{version} · sha256 {sha}\n{requires}",
        plugin = plugin_version(),
        version = DISCLOSURE_VERSION,
        sha = plan.sha256,
        requires = match plan.carrier {
            Carrier::Session => format!(
                "requires {harness} with an in-process request hook (yours: {version})",
            ),
            Carrier::User => format!(
                "requires nothing of {harness}: these are files it already reads (yours: {version})",
            ),
        },
    );
    match plan.carrier {
        Carrier::Session => {
            let carried = match (hook, skill, reminder) {
                (true, true, _) => "per-request hook and activity skill".to_string(),
                (true, false, _) => "per-request hook".to_string(),
                (false, true, _) => "activity skill, in each session's config copy".to_string(),
                (false, false, _) => "activity reminder, in each session's config copy".to_string(),
            };
            // A session copy is private to the session: say so, because that is
            // the difference from a user-scope grant.
            let preamble = if hook {
                format!(
                    "Approving this lets the plugin add, to each {harness} session's private \
                     config copy,\nthe files below. {harness} loads them inside the agent process, \
                     with the agent's own\npermissions. They are what lets a tag change reach the \
                     next request instead of the\nnext launch, and what lets the session say what \
                     it is doing — running tests, writing\ncode — without waiting for someone at \
                     the dashboard."
                )
            } else {
                format!(
                    "Approving this lets the plugin add, to each {harness} session's private \
                     config copy,\nthe files below. Nothing of yours is touched: that copy is \
                     built for the session alone.\nThey are what lets the session say what it is \
                     doing — running tests, writing code —\nwithout waiting for someone at the \
                     dashboard."
                )
            };
            let does = if hook {
                format!(
                    "read one JSON file this plugin writes (live.json: the session's tags and its\n\
                     current activity) and set the {harness} request headers from it on each model \
                     request;\nand, {source}, run `aoe-llm-tags activity <phase>` when the agent \
                     judges the\nwork has moved on — that command only writes this session's own \
                     tag."
                )
            } else {
                format!(
                    "let the agent keep the session's activity current: {source}, run\n\
                     `aoe-llm-tags activity <phase>` when the agent judges the work has moved \
                     on — that\ncommand only writes this session's own tag, which the dashboard \
                     shows and which the next\nlaunch's headers carry."
                )
            };
            // Only a row with a hook can stop sending mid-session; the others say
            // nothing about a running process, because nothing there re-reads us.
            let running = if hook {
                " A session that is\nalready running stops sending dynamic tags on its next request."
            } else {
                " A session that is already\nrunning keeps the value it launched with."
            };
            format!(
                "{harness} — {carried}\n\
                 \n\
                 {preamble}\n\
                 \n\
                 {files}\
                 \n\
                 It does: {does}\n\
                 It does not: read your prompts, transcript, files or credentials; send anything\n\
                 outside the session's requests; open a socket; or write anywhere except the files\n\
                 above{only_managed}.\n\
                 To undo: run `aoe-llm-tags extension disable {harness}`, which removes every path\n\
                 above, and set `{SETTING} = []` so new sessions do not use it.{running}\n\
                 \n\
                 {footer}"
            )
        }
        Carrier::User => {
            let carried = match (skill, reminder) {
                (true, true) => "activity skill and reminder, in your own config",
                (true, false) => "activity skill, in your own config",
                _ => "activity reminder, in your own config",
            };
            let reach = match (skill, reminder) {
                (true, true) => {
                    "the skill is\na directory of its own, and the reminder is a block \
                                 between two marker lines inside a\nfile that stays yours. \
                                 Everything outside those markers — your own rules, your own\n\
                                 notes — is left byte for byte as it is, and removing the block is \
                                 how a disable undoes\nit."
                }
                (true, false) => {
                    "the skill is\na directory of its own and nothing else of yours is \
                                   touched."
                }
                _ => {
                    "the reminder is a\nblock between two marker lines inside a file that stays \
                      yours. Everything outside those\nmarkers — your own rules, your own notes — \
                      is left byte for byte as it is, and removing\nthe block is how a disable \
                      undoes it."
                }
            };
            let does = match (skill, reminder) {
                (true, true) => format!(
                    "put the activity skill where {harness} looks for skills, and a short \
                     reminder in\nthe file it reads into every conversation, so the session states \
                     what it is doing\n(running tests, writing code) on its own; and, from the \
                     skill, run\n`aoe-llm-tags activity <phase>` when the agent judges the work \
                     has moved on — that\ncommand only writes this session's own tag, which the \
                     dashboard shows and which the next\nlaunch's headers carry."
                ),
                (true, false) => format!(
                    "put the activity skill where {harness} looks for skills, so the session \
                     states what\nit is doing (running tests, writing code) on its own; and, from \
                     the skill, run\n`aoe-llm-tags activity <phase>` when the agent judges the \
                     work has moved on — that\ncommand only writes this session's own tag, which \
                     the dashboard shows and which the next\nlaunch's headers carry."
                ),
                _ => format!(
                    "put a short reminder in the file {harness} reads into every conversation, so \
                     the\nsession states what it is doing (running tests, writing code) on its own; \
                     and, following\nthat reminder, run `aoe-llm-tags activity <phase>` when the \
                     agent judges the work has\nmoved on — that command only writes this session's \
                     own tag, which the dashboard shows and\nwhich the next launch's headers \
                     carry."
                ),
            };
            let undo = match (skill, reminder) {
                (true, true) => {
                    "deletes the skill\ndirectory and the block between the markers, \
                                 leaving the rest of each file as it was,"
                }
                (true, false) => "deletes the skill\ndirectory,",
                _ => {
                    "removes the block between\nthe markers, leaving the rest of the file as it \
                      was,"
                }
            };
            format!(
                "{harness} — {carried}\n\
                 \n\
                 {harness} has no per-session config copy this plugin can write into, so approving \
                 this\nadds the files below to your own {harness} configuration under your home \
                 directory.\nThat is a wider reach than a session copy, and it is worth reading \
                 twice: {reach}\n\
                 \n\
                 {files}\
                 \n\
                 It does: {does}\n\
                 It does not: read your prompts, transcript, files or credentials; edit any line\n\
                 outside its own marker block; send anything to {harness} or anywhere else; open a\n\
                 socket; change a request that is already running; or write anywhere except the \
                 files\nabove{only_managed}.\n\
                 Because {harness} has no in-process request hook here, an activity change \
                 reaches\n{harness} at the next launch. The badge, the pane and the page change at \
                 once.\n\
                 To undo: run `aoe-llm-tags extension disable {harness}`, which {undo}\n\
                 and set `{SETTING} = []` so new sessions do not use it.\n\
                 \n\
                 {footer}"
            )
        }
    }
}

/// The disclosed paths, one per line, annotations in a column: the disclosure is
/// read, not parsed, and a ragged list is harder to check than an aligned one.
fn disclosed_paths(plan: &Plan) -> String {
    let mut files = String::new();
    for placement in plan.parts() {
        let note = match (&plan.hook, &plan.skill, &plan.instructions) {
            (Some(hook), _, _) if hook == placement => "the hook itself",
            (_, Some(skill), _) if skill == placement => {
                "a skill that tells the agent to keep the activity current"
            }
            _ => "the same reminder, in the always-loaded instructions",
        };
        files.push_str(&line(&placement.display, note));
    }
    if let Some(managed) = &plan.managed {
        files.push_str(&line(
            &managed.display,
            "the same skill in AoE's own skill list, so it shows on",
        ));
        files.push_str(&format!(
            "{:width$}the Skills page and can be read, edited or deleted there\n",
            "",
            width = 2 + 50 + 2,
        ));
    }
    files
}

/// One disclosed path with its annotation in a column.
fn line(path: &str, note: &str) -> String {
    format!("  {path:<50}  {note}\n")
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

    /// A row that carries the bundle in a session's config copy. No artefact
    /// path here names a harness: those live in `adapters.toml`.
    fn session_row() -> Adapter {
        Adapter {
            agent: "example".into(),
            extension_artifact: "harness/skills/aoe-activity/SKILL.md".into(),
            extension_subdir: "extensions".into(),
            extension_filename: "aoe-llm-tags.ts".into(),
            skill_artifact: "harness/skills/aoe-activity/SKILL.md".into(),
            skill_dir: "aoe-activity".into(),
            skill_subdir: "skills".into(),
            append_artifact: "harness/activity.append.md".into(),
            append_filename: "APPEND_SYSTEM.md".into(),
            carrier_scope: "session".into(),
            write_mode: crate::overlay::MODE_OVERLAY_DIR.into(),
            ..Adapter::default()
        }
    }

    /// A row whose harness has no session copy, so the carriers go under the
    /// user's own home behind a marker block.
    fn user_row() -> Adapter {
        Adapter {
            agent: "example".into(),
            skill_artifact: "harness/skills/aoe-activity/SKILL.md".into(),
            skill_dir: "aoe-activity".into(),
            append_artifact: "harness/activity.append.md".into(),
            carrier_scope: "user".into(),
            user_skill_root: "~/.example/skills".into(),
            user_instructions: "~/.example/AGENTS.md".into(),
            ..Adapter::default()
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aoe-ext-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::private::create_dir(&dir).expect("temp dir");
        dir
    }

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
    fn the_session_disclosure_names_the_files_the_digest_and_how_to_undo_it() {
        let plan = Plan {
            harness: "example".into(),
            sha256: "b".repeat(64),
            carrier: Carrier::Session,
            hook: Some(Placement {
                artifact: PathBuf::from("harness/hook.ts"),
                subdir: "extensions".into(),
                file: "aoe-llm-tags.ts".into(),
                display: "<session config>/extensions/aoe-llm-tags.ts".into(),
            }),
            skill: None,
            instructions: None,
            env_var: DEFAULT_LIVE_ENV.into(),
            live: PathBuf::from("/tmp/live.json"),
            managed: None,
        };
        let text = disclosure(&plan, "1.2.3");
        for needle in [
            "example",
            "<session config>/extensions/aoe-llm-tags.ts",
            &"b".repeat(64),
            "extension disable example",
            "does not: read your prompts",
            "v3",
            "1.2.3",
        ] {
            assert!(
                text.contains(needle),
                "the disclosure omits {needle:?}: {text}"
            );
        }
    }

    /// The user-scope grant is a different one, and the text has to say so: it
    /// writes inside a file the user owns, and only between two markers.
    #[test]
    fn the_user_disclosure_says_it_writes_inside_the_users_own_config() {
        let mut adapter = user_row();
        adapter.agent = "example".into();
        let plan = proposal(
            &adapter,
            Path::new("/tmp/app"),
            "s",
            Some(Path::new("/home/x")),
        )
        .expect("a plan");
        let text = disclosure(&plan, "");
        for needle in [
            "~/.example/skills/aoe-activity/SKILL.md",
            "~/.example/AGENTS.md",
            "marker",
            "your own config",
            "next launch",
            "extension disable example",
        ] {
            assert!(
                text.contains(needle),
                "the user disclosure omits {needle:?}: {text}"
            );
        }
        // It must not claim a per-request delivery this harness cannot make.
        assert!(
            !text.contains("re-reads"),
            "the user disclosure promises a per-request read: {text}"
        );
    }

    #[test]
    fn a_receipt_round_trips_through_the_file() {
        let dir = tmp("receipt");
        let home = tmp("receipt-home");
        let sha = "c".repeat(64);
        accept(&dir, Some(&home), &session_row(), &sha, "9.9.9").expect("accept");
        let (all, warnings) = receipts(&dir);
        assert!(warnings.is_empty(), "{warnings:?}");
        let receipt = all.get("example").expect("a receipt");
        assert_eq!(receipt.sha256, sha);
        assert_eq!(receipt.disclosure_version, DISCLOSURE_VERSION);
        assert_eq!(receipt.tested_harness, "9.9.9");
        assert!(receipt.accepted_at.ends_with('Z'));
        // A second harness is a second approval, not a re-use of the first.
        let mut other = session_row();
        other.agent = "other".into();
        accept(&dir, Some(&home), &other, &"d".repeat(64), "").expect("accept other");
        let (all, _) = receipts(&dir);
        assert_eq!(all.len(), 2);
        assert!(revoke(&dir, Some(&home), &session_row()).expect("revoke"));
        assert!(!revoke(&dir, Some(&home), &session_row()).expect("revoke twice"));
        let (all, _) = receipts(&dir);
        assert_eq!(all.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_broken_receipt_file_is_a_warning_not_a_crash() {
        let dir = tmp("broken");
        std::fs::write(receipt_path(&dir), "not = [valid").expect("write");
        let (all, warnings) = receipts(&dir);
        assert!(all.is_empty());
        assert_eq!(warnings.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_user_bundle_spells_the_paths_under_the_home() {
        let placed = bundle(&user_row(), Carrier::User);
        let skill = placed.skill.expect("a skill");
        assert_eq!(skill.display, "~/.example/skills/aoe-activity/SKILL.md");
        assert_eq!(skill.subdir, ".example/skills/aoe-activity");
        let instructions = placed.instructions.expect("a reminder");
        assert_eq!(instructions.display, "~/.example/AGENTS.md");
        assert_eq!(instructions.subdir, ".example");
        assert_eq!(instructions.file, "AGENTS.md");
        // A session copy of the same row is a different placement, so it is a
        // different grant.
        let session = bundle(&user_row(), Carrier::Session);
        assert_eq!(
            session.skill.unwrap().display,
            "<session config>/skills/aoe-activity/SKILL.md"
        );
    }

    #[test]
    fn a_row_with_nowhere_to_put_anything_is_refused_by_name() {
        let row = Adapter {
            agent: "nothing".into(),
            ..Adapter::default()
        };
        assert_eq!(carrier(&row), None);
        let refusal = plan(&row, &["nothing".to_string()], Path::new("/tmp"), "s", None)
            .expect_err("no destination");
        assert_eq!(refusal.reason(), "no_destination");
        assert!(refusal.detail().contains("nothing"));
    }

    #[test]
    fn approving_writes_the_user_carriers_and_revoking_takes_them_back_out() {
        let home = tmp("user-home");
        let theirs = home.join(".example").join("AGENTS.md");
        crate::private::create_dir(theirs.parent().unwrap()).expect("dir");
        std::fs::write(&theirs, "# my own rules\n\nbe brief\n").expect("write");

        let written = install_user(&home, &user_row()).expect("install");
        assert_eq!(written.len(), 2);
        let text = std::fs::read_to_string(&theirs).expect("read");
        assert!(text.starts_with("# my own rules\n\nbe brief\n"), "{text}");
        assert!(
            text.contains(BLOCK_BEGIN) && text.contains(BLOCK_END),
            "{text}"
        );
        assert!(text.contains("aoe-llm-tags"), "{text}");
        let skill = home.join(".example/skills/aoe-activity/SKILL.md");
        assert!(std::fs::read_to_string(&skill)
            .expect("skill")
            .contains(MANAGED_MARKER));

        // A second approval is the same bytes, not a second block.
        install_user(&home, &user_row()).expect("install again");
        assert_eq!(
            std::fs::read_to_string(&theirs).expect("read"),
            text,
            "a second approval reworded or duplicated the file"
        );

        let touched = remove_user(&home, &user_row()).expect("remove");
        assert_eq!(touched.len(), 2);
        assert_eq!(
            std::fs::read_to_string(&theirs).expect("read"),
            "# my own rules\n\nbe brief\n",
            "the user's own text did not come back byte for byte"
        );
        assert!(skill.symlink_metadata().is_err(), "the skill stayed behind");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_file_only_the_block_was_in_is_deleted_on_removal() {
        let home = tmp("new-file-home");
        install_user(&home, &user_row()).expect("install");
        let path = home.join(".example/AGENTS.md");
        assert!(path.is_file());
        remove_user(&home, &user_row()).expect("remove");
        assert!(
            path.symlink_metadata().is_err(),
            "an empty reminder file was left behind"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_skill_dir_somebody_else_wrote_is_never_overwritten() {
        let home = tmp("someone-elses-home");
        let skill = home.join(".example/skills/aoe-activity/SKILL.md");
        crate::private::create_dir(skill.parent().unwrap()).expect("dir");
        std::fs::write(&skill, "---\nname: aoe-activity\n---\nmine\n").expect("write");
        let err = install_user(&home, &user_row()).expect_err("refused");
        assert!(err.contains("not ours"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&skill).expect("read"),
            "---\nname: aoe-activity\n---\nmine\n"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_digest_covers_where_the_files_go_not_only_what_they_say() {
        let one = bundle_sha(&user_row(), Carrier::User).expect("sha");
        let mut moved = user_row();
        moved.user_instructions = "~/.example/CLAUDE.md".into();
        let two = bundle_sha(&moved, Carrier::User).expect("sha");
        assert_ne!(
            one, two,
            "moving the reminder to another file is a different grant"
        );
        // And the same row twice is the same digest.
        assert_eq!(one, bundle_sha(&user_row(), Carrier::User).expect("sha"));
        // A session-scope approval is not a user-scope one.
        assert_ne!(one, bundle_sha(&user_row(), Carrier::Session).expect("sha"));
    }
}
