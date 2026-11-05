//! What the plugin writes, only this user can read.
//!
//! Two of its own files can carry a secret: a session's generated harness config
//! holds whatever the user's config holds (a provider `apiKey` included, because
//! that is what the harness needs to authenticate), and a session's
//! `overrides.toml` holds the literal token the user gave that session. Neither is
//! a public artefact, so neither inherits the umask: directories are created
//! `0700` and files `0600`, whatever mode the file they were built from has.
//!
//! The mode is the whole guarantee — the plugin cannot strip a key the harness
//! needs — so it is applied on every write, not only on the first one: a session
//! dir left behind by an older build (or a restored backup) is tightened too.

use std::path::Path;

/// Create `dir` (and the parents this call creates) private to this user.
///
/// A path that is already there is left in place and only its mode is fixed: the
/// session dirs belong to the plugin, the app dir above them belongs to aoe. A
/// symlinked session dir is not followed and not chmodded — the file written
/// inside it is `0600` regardless, and rewriting somebody's link is not this
/// plugin's business.
pub fn create_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_dir() && meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Replace `path` with `text`: same-directory temporary file, then a rename, so a
/// reader never sees half a file.
///
/// The temporary is created anew (`create_new`) at `0600` after any leftover is
/// removed, so a symlink planted at that name cannot redirect the write, and the
/// rename publishes a regular file the plugin owns. `path` itself is replaced
/// rather than followed, for the same reason.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let tmp = temp_for(path);
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(text.as_bytes())?;
    drop(file);
    // `mode` only applies when the file is created; anything else that produced
    // this name is not trusted to have produced 0600.
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)
}

/// `models.json.aoe-llm-tags.tmp` — beside its target, because `rename` is only
/// atomic within one directory, and named so a leftover says who made it.
fn temp_for(path: &Path) -> std::path::PathBuf {
    match path.file_name() {
        Some(name) => path.with_file_name(format!("{}.aoe-llm-tags.tmp", name.to_string_lossy())),
        None => path.with_extension("aoe-llm-tags.tmp"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn dir(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("llm-tags-private-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn a_created_dir_is_private_even_with_a_permissive_umask() {
        let root = dir("create");
        let nested = root.join("sessions").join("abc").join("pi");
        create_dir(&nested).expect("create");
        for path in [&root, &root.join("sessions"), &nested] {
            let mode = std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "{} is {mode:o}", path.display());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_dir_left_open_by_an_older_build_is_tightened() {
        let root = dir("tighten");
        std::fs::create_dir_all(&root).expect("create");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        create_dir(&root).expect("create again");
        let mode = std::fs::metadata(&root)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_written_file_is_private_and_replaces_what_was_there() {
        let root = dir("write");
        create_dir(&root).expect("create");
        let file = root.join("models.json");
        std::fs::write(&file, "old\n").expect("source");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        write_atomic(&file, "new\n").expect("write");
        assert_eq!(std::fs::read_to_string(&file).expect("read"), "new\n");
        let mode = std::fs::metadata(&file)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        assert!(
            !root.join("models.json.aoe-llm-tags.tmp").exists(),
            "the temporary is renamed away, not left behind"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_link_where_the_temporary_goes_cannot_redirect_the_write() {
        let root = dir("symlink");
        create_dir(&root).expect("create");
        let target = root.join("elsewhere.json");
        std::fs::write(&target, "keep\n").expect("target");
        // A symlink at the temporary's name: a plain `write` would follow it and
        // truncate the file it points at.
        std::os::unix::fs::symlink(&target, root.join("models.json.aoe-llm-tags.tmp"))
            .expect("symlink");

        write_atomic(&root.join("models.json"), "mine\n").expect("write");
        assert_eq!(
            std::fs::read_to_string(&target).expect("target"),
            "keep\n",
            "the link's target was written through"
        );
        assert!(!root.join("models.json.aoe-llm-tags.tmp").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
