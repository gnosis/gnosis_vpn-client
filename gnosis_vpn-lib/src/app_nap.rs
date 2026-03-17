/// Prevents macOS App Nap from throttling this process.
///
/// On macOS, when the GUI app is backgrounded, the OS may apply App Nap to
/// daemon child processes, throttling timers and CPU. This module calls
/// `[NSProcessInfo beginActivityWithOptions:reason:]` to opt out.
///
/// On non-macOS platforms, this is a zero-cost no-op.

#[cfg(target_os = "macos")]
mod macos {
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

    /// Opaque token that keeps the activity assertion alive.
    /// The assertion is released when this token is dropped.
    pub struct ActivityToken {
        _activity: objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>,
    }

    // SAFETY: The activity token returned by `beginActivityWithOptions:reason:` is a
    // refcounted Cocoa object that is safe to hold across threads. It is only used as
    // a lifetime anchor — no methods are called on it after creation.
    unsafe impl Send for ActivityToken {}
    unsafe impl Sync for ActivityToken {}

    /// Disables App Nap for the current process.
    ///
    /// Returns an [`ActivityToken`] that must be kept alive for the duration needed.
    /// Dropping the token re-enables App Nap.
    pub fn disable(reason: &str) -> ActivityToken {
        let info = NSProcessInfo::processInfo();
        let reason = NSString::from_str(reason);
        let activity =
            info.beginActivityWithOptions_reason(NSActivityOptions::UserInitiatedAllowingIdleSystemSleep, &reason);
        tracing::info!("App Nap disabled");
        ActivityToken { _activity: activity }
    }
}

#[cfg(target_os = "linux")]
mod noop {
    /// No-op token on non-macOS platforms.
    pub struct ActivityToken;

    pub fn disable(_reason: &str) -> ActivityToken {
        ActivityToken
    }
}

#[cfg(target_os = "macos")]
pub use macos::{ActivityToken, disable};
#[cfg(target_os = "linux")]
pub use noop::{ActivityToken, disable};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disable_returns_valid_token() {
        let _token = disable("test: App Nap prevention");
        // On macOS this exercises the real FFI path;
        // on other platforms it exercises the no-op path.
    }
}
