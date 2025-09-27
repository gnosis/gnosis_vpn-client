#[derive(Debug, thiserror::Error)]
pub enum HoprError {
    #[error("Construction error: {0}")]
    Construction(String),

    #[error("HoprLib error: {0}")]
    HoprLib(#[from] edgli::hopr_lib::errors::HoprLibError),

    // --- session errors ---
    #[error("Session error: {0}")]
    Session(String),

    #[error("Listen host already used")]
    ListenHostAlreadyUsed,

    #[error("Session not found")]
    SessionNotFound,

    #[error("Session failed to adjust: {0}")]
    SessionNotAdjusted(String),

    #[error("Ambiguous client for session")]
    SessionAmbiguousClient,
}
