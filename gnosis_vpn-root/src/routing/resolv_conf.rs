//! Manages `/etc/resolv.conf` directly where no resolver manager owns it, mirroring Mullvad's static-file DNS module.

use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

pub(super) const RESOLV_CONF: &str = "/etc/resolv.conf";
pub(super) const BACKUP: &str = "/etc/resolv.conf.gnosisvpn-backup";
const MARKER: &str =
    "# nameserver lines set by gnosisvpn while connected; original in /etc/resolv.conf.gnosisvpn-backup";

/// One tunnel per process, so a global keeps `dns::set`/`dns::restore` free of handles.
static WATCH: Mutex<Option<Watch>> = Mutex::new(None);

struct Watch {
    _watcher: notify::RecommendedWatcher,
    cancel: CancellationToken,
}

/// Point the host resolver at `servers` until [`restore`]; false when the file cannot be read or written.
pub(super) fn apply(servers: &[&str]) -> bool {
    let servers: Vec<String> = servers.iter().map(|s| s.to_string()).collect();
    // A leftover backup means root died mid-connection: the backup is the real original, not the current file.
    let original = match read_optional(BACKUP) {
        Ok(Some(backup)) => backup,
        Ok(None) => match read_optional(RESOLV_CONF) {
            Ok(content) => content.unwrap_or_default(),
            Err(e) => {
                tracing::warn!(%e, path = RESOLV_CONF, "cannot read resolv.conf (continuing)");
                return false;
            }
        },
        Err(e) => {
            tracing::warn!(%e, path = BACKUP, "cannot read resolv.conf backup (continuing)");
            return false;
        }
    };
    if let Err(e) = write_backup(&original) {
        tracing::warn!(%e, path = BACKUP, "cannot write resolv.conf backup (continuing)");
        return false;
    }
    if let Err(e) = std::fs::write(RESOLV_CONF, with_nameservers(&original, &servers)) {
        tracing::warn!(%e, path = RESOLV_CONF, "cannot write resolv.conf (continuing)");
        // A backup left behind would pose as the original on the next connect.
        restore_backup();
        return false;
    }
    start_watching(servers);
    true
}

/// Stop re-applying and hand the original file back.
pub(super) fn restore() -> bool {
    stop_watching();
    restore_backup()
}

pub(super) fn backup_exists() -> bool {
    Path::new(BACKUP).exists()
}

/// Write the backup back and delete it; true when no backup is left, including when there was none.
pub(super) fn restore_backup() -> bool {
    let original = match read_optional(BACKUP) {
        Ok(Some(content)) => content,
        Ok(None) => return true,
        Err(e) => {
            tracing::warn!(%e, path = BACKUP, "cannot read resolv.conf backup (continuing)");
            return false;
        }
    };
    if let Err(e) = std::fs::write(RESOLV_CONF, original) {
        tracing::warn!(%e, path = RESOLV_CONF, "cannot restore resolv.conf (continuing)");
        return false;
    }
    if let Err(e) = std::fs::remove_file(BACKUP) {
        tracing::warn!(%e, path = BACKUP, "cannot remove resolv.conf backup (continuing)");
        return false;
    }
    true
}

/// Atomic, so a crash mid-write cannot leave a partial backup for the sweep to restore.
fn write_backup(content: &str) -> std::io::Result<()> {
    let tmp = format!("{BACKUP}.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, BACKUP)
}

fn read_optional(path: &str) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// `original` with only its `nameserver` lines replaced by `servers`; idempotent.
pub(super) fn with_nameservers(original: &str, servers: &[String]) -> String {
    let mut out = String::new();
    for line in original.lines() {
        if is_nameserver_line(line) || line == MARKER {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(MARKER);
    out.push('\n');
    for server in servers {
        out.push_str("nameserver ");
        out.push_str(server);
        out.push('\n');
    }
    out
}

/// The nameserver addresses listed in a resolv.conf text, in order.
pub(super) fn nameservers(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|line| is_nameserver_line(line))
        .filter_map(|line| line.split_whitespace().nth(1))
        .map(str::to_string)
        .collect()
}

fn is_nameserver_line(line: &str) -> bool {
    line.split_whitespace().next() == Some("nameserver")
}

/// Re-apply after an external rewrite; watches directories because NetworkManager replaces the file by rename.
fn start_watching(servers: Vec<String>) {
    stop_watching();
    let targets = watched_paths();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && event.paths.iter().any(|path| targets.contains(path))
        {
            let _ = event_tx.send(());
        }
    });
    let mut watcher = match watcher {
        Ok(watcher) => watcher,
        Err(e) => {
            tracing::warn!(%e, "cannot watch resolv.conf; external rewrites will not be re-applied");
            return;
        }
    };
    for dir in watched_dirs() {
        if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            tracing::warn!(%e, dir = %dir.display(), "cannot watch resolv.conf directory (continuing)");
        }
    }

    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => return,
                Some(()) = event_rx.recv() => {
                    // A rewrite arrives as a burst (tmp file, rename, chmod); settle first.
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    while event_rx.try_recv().is_ok() {}
                    // Holding the lock serializes with `restore`, which cancels under it before restoring.
                    let Ok(_watch) = WATCH.lock() else {
                        return;
                    };
                    if task_cancel.is_cancelled() {
                        return;
                    }
                    reapply(&servers);
                }
            }
        }
    });
    if let Ok(mut watch) = WATCH.lock() {
        *watch = Some(Watch {
            _watcher: watcher,
            cancel,
        });
    }
}

fn stop_watching() {
    let Ok(mut watch) = WATCH.lock() else {
        return;
    };
    if let Some(watch) = watch.take() {
        watch.cancel.cancel();
    }
}

fn reapply(servers: &[String]) {
    let current = match read_optional(RESOLV_CONF) {
        Ok(content) => content.unwrap_or_default(),
        Err(e) => {
            tracing::warn!(%e, path = RESOLV_CONF, "cannot re-read resolv.conf (continuing)");
            return;
        }
    };
    // Our own write, or a rewrite that kept the tunnel servers: nothing to do.
    if nameservers(&current) == servers {
        return;
    }
    tracing::info!("resolv.conf was rewritten by another program - re-applying tunnel DNS");
    // The newcomer is what the host wants once we disconnect, so it becomes the backup.
    if let Err(e) = write_backup(&current) {
        tracing::warn!(%e, path = BACKUP, "cannot update resolv.conf backup (continuing)");
        return;
    }
    if let Err(e) = std::fs::write(RESOLV_CONF, with_nameservers(&current, servers)) {
        tracing::warn!(%e, path = RESOLV_CONF, "cannot re-apply resolv.conf (continuing)");
    }
}

/// The symlink and its target, both of which a rewrite may touch.
fn watched_paths() -> HashSet<PathBuf> {
    let mut paths = HashSet::from([PathBuf::from(RESOLV_CONF)]);
    if let Ok(target) = std::fs::canonicalize(RESOLV_CONF) {
        paths.insert(target);
    }
    paths
}

fn watched_dirs() -> HashSet<PathBuf> {
    watched_paths()
        .into_iter()
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn servers(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn replaces_nameservers_and_keeps_everything_else() {
        let original = "# Generated by NetworkManager\nsearch lan\nnameserver 192.168.1.1\noptions edns0\n";
        let rewritten = with_nameservers(original, &servers(&["1.1.1.1", "8.8.8.8"]));
        assert_eq!(
            rewritten,
            format!(
                "# Generated by NetworkManager\nsearch lan\noptions edns0\n{MARKER}\nnameserver 1.1.1.1\nnameserver 8.8.8.8\n"
            )
        );
    }

    #[test]
    fn rewriting_twice_is_stable() {
        let once = with_nameservers("nameserver 10.0.0.1\n", &servers(&["1.1.1.1"]));
        let twice = with_nameservers(&once, &servers(&["1.1.1.1"]));
        assert_eq!(once, twice);
    }

    #[test]
    fn empty_original_gets_only_the_tunnel_servers() {
        assert_eq!(
            with_nameservers("", &servers(&["1.1.1.1"])),
            format!("{MARKER}\nnameserver 1.1.1.1\n")
        );
    }

    #[test]
    fn nameservers_are_parsed_in_order_and_tolerate_indentation() {
        let content =
            "search lan\n  nameserver 10.0.0.1\nnameserver\t10.0.0.2 # trailing\n#nameserver 10.0.0.3\nnameserverx 1\n";
        assert_eq!(nameservers(content), servers(&["10.0.0.1", "10.0.0.2"]));
    }
}
