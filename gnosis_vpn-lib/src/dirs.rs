use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

pub use gnosis_vpn_ipc::dirs::{UserDirs, user_dirs};

/// Root's runtime files (socket, lock, crash-recovery state): `/run` per FHS 3.0, `/var/run` on macOS which has no `/run`.
#[cfg(target_os = "linux")]
pub const RUNTIME_DIR: &str = "/run/gnosisvpn";
#[cfg(target_os = "macos")]
pub const RUNTIME_DIR: &str = "/var/run/gnosisvpn";

pub fn runtime_file(name: &str) -> PathBuf {
    Path::new(RUNTIME_DIR).join(name)
}

/// Creates every missing dir from `tree_root` down to `path` 0700; dirs above `tree_root` get the default mode.
pub fn ensure_private_dir(tree_root: &Path, path: &Path) -> std::io::Result<()> {
    if !path.starts_with(tree_root) {
        return Err(std::io::Error::other(format!(
            "{} is outside {}",
            path.display(),
            tree_root.display()
        )));
    }
    if let Some(parent) = tree_root.parent() {
        std::fs::create_dir_all(parent)?;
    }
    DirBuilder::new().mode(0o700).recursive(true).create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dir_from_the_tree_root_down_is_created_with_mode_0700() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().join("outer").join("root");
        let path = root.join("middle").join("inner");
        ensure_private_dir(&root, &path).expect("create");
        for dir in [&root, &root.join("middle"), &path] {
            let mode = std::fs::metadata(dir).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", dir.display());
        }
        // Creating it again is a no-op, not an error.
        ensure_private_dir(&root, &path).expect("idempotent");
    }

    #[test]
    fn a_path_outside_the_tree_root_is_an_error() {
        let tmp = tempfile::tempdir().expect("temp dir");
        assert!(ensure_private_dir(&tmp.path().join("a"), &tmp.path().join("b")).is_err());
        assert!(!tmp.path().join("b").exists());
    }
}
