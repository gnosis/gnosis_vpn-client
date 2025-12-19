#[derive(Debug, thiserror::Error)]
pub enum HoprError {
    #[error("Construction error: {0}")]
    Construction(String),

    #[error("HoprLib error: {0}")]
    HoprLib(#[from] edgli::hopr_lib::errors::HoprLibError),

    // --- channel errors ---
    #[error("Channel error: {0}")]
    Channel(String),

    // --- session errors ---
    #[error("Session error: {0}")]
    Session(String),

    #[error("Session not found")]
    SessionNotFound,

    #[error("Session failed to adjust: {0}")]
    SessionNotAdjusted(String),

    #[error("Ambiguous client for session")]
    SessionAmbiguousClient,

    #[error("Failed to extract telemetry: {0}")]
    Telemetry(String),

    #[error("No ticket price available")]
    NoTicketPrice,

    #[error("Failed to start telemetry reactor: {0}")]
    TelemetryReactorStart(String),
}
