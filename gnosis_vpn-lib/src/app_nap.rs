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

#[cfg(not(target_os = "macos"))]
mod noop {
    /// No-op token on non-macOS platforms.
    pub struct ActivityToken;

    pub fn disable(_reason: &str) -> ActivityToken {
        ActivityToken
    }
}

#[cfg(target_os = "macos")]
pub use macos::{ActivityToken, disable};
#[cfg(not(target_os = "macos"))]
pub use noop::{ActivityToken, disable};
