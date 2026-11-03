//! Where the worker keeps each user's state; user interfaces read the same layout.
use serde::{Deserialize, Serialize};

use std::path::{Path, PathBuf};

/// The per-user directories the worker keeps its state in; root computes them, the worker creates them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserDirs {
    pub data: PathBuf,
    pub cache: PathBuf,
    pub logs: PathBuf,
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
    /// Identity and PIX pool state of one network; an empty dir means a fresh identity.
    pub fn network_data(&self, network: &str) -> PathBuf {
        self.data.join("networks").join(network)
    }

    pub fn network_cache(&self, network: &str) -> PathBuf {
        self.cache.join("networks").join(network)
    }

    pub fn pix_state(&self, network: &str) -> PathBuf {
        self.network_data(network).join("pix")
    }
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
        assert_eq!(
            dirs.pix_state("jura-dev"),
            PathBuf::from("/srv/gvpn/networks/jura-dev/pix")
        );
    }
}
