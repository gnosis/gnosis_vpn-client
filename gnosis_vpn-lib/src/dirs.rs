use serde::{Deserialize, Serialize};

use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// The per-user directories the worker keeps its state in; root computes them, the worker creates them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserDirs {
    pub data: PathBuf,
    pub cache: PathBuf,
    pub logs: PathBuf,
}

/// Root's runtime files (socket, lock, crash-recovery state): `/run` per FHS 3.0, `/var/run` on macOS which has no `/run`.
#[cfg(target_os = "linux")]
pub const RUNTIME_DIR: &str = "/run/gnosisvpn";
#[cfg(target_os = "macos")]
pub const RUNTIME_DIR: &str = "/var/run/gnosisvpn";

pub fn runtime_file(name: &str) -> PathBuf {
    Path::new(RUNTIME_DIR).join(name)
}

/// Pure function of home and platform; root never creates or chowns these dirs (symlink chown = root hole).
#[cfg(target_os = "linux")]
pub fn user_dirs(home: &Path) -> UserDirs {
    UserDirs {
        data: home.join(".local/share/gnosisvpn"),
        cache: home.join(".cache/gnosisvpn"),
        logs: home.join(".local/state/gnosisvpn"),
    }
}

#[cfg(target_os = "macos")]
pub fn user_dirs(home: &Path) -> UserDirs {
    UserDirs {
        data: home.join("Library/Application Support/GnosisVPN"),
        cache: home.join("Library/Caches/GnosisVPN"),
        logs: home.join("Library/Logs/GnosisVPN"),
    }
}

impl UserDirs {
    /// Identity of one network; an empty dir means a fresh identity.
    pub fn network_data(&self, network: &str) -> PathBuf {
        self.data.join("networks").join(network)
    }

    pub fn network_cache(&self, network: &str) -> PathBuf {
        self.cache.join("networks").join(network)
    }
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_follows_the_xdg_layout() {
        let dirs = user_dirs(Path::new("/home/alice"));
        assert_eq!(dirs.data, PathBuf::from("/home/alice/.local/share/gnosisvpn"));
        assert_eq!(dirs.cache, PathBuf::from("/home/alice/.cache/gnosisvpn"));
        assert_eq!(dirs.logs, PathBuf::from("/home/alice/.local/state/gnosisvpn"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_follows_the_library_layout() {
        let dirs = user_dirs(Path::new("/Users/alice"));
        assert_eq!(
            dirs.data,
            PathBuf::from("/Users/alice/Library/Application Support/GnosisVPN")
        );
        assert_eq!(dirs.cache, PathBuf::from("/Users/alice/Library/Caches/GnosisVPN"));
        assert_eq!(dirs.logs, PathBuf::from("/Users/alice/Library/Logs/GnosisVPN"));
    }

    #[test]
    fn network_state_is_keyed_by_network_name() {
        let dirs = UserDirs {
            data: PathBuf::from("/srv/gvpn"),
            cache: PathBuf::from("/srv/gvpn/cache"),
            logs: PathBuf::from("/srv/gvpn/logs"),
        };
        assert_eq!(
            dirs.network_data("jura-dev"),
            PathBuf::from("/srv/gvpn/networks/jura-dev")
        );
        assert_eq!(
            dirs.network_cache("jura-dev"),
            PathBuf::from("/srv/gvpn/cache/networks/jura-dev")
        );
    }

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
