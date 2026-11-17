use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RecorderError {
    #[error("this build does not support capture on this operating system")]
    UnsupportedPlatform,
    #[error("screen recording permission is missing")]
    PermissionMissing,
    #[error("a recorder is already running in this process")]
    AlreadyRunning,
    #[error("the recorder is not running")]
    NotRunning,
    #[error("no focused window")]
    NoFocusedWindow,
    #[error("capture failed: {0}")]
    CaptureFailed(String),
    #[error("capture timed out")]
    CaptureTimeout,
    #[error("accessibility walk failed: {0}")]
    AxFailed(String),
    #[error("ocr failed: {0}")]
    OcrFailed(String),
    #[error("store failed: {0}")]
    StoreFailed(String),
    #[error("disk is full")]
    DiskFull,
    #[error("invalid config: {0}")]
    Config(String),
    #[error("capture skipped: {0}")]
    Skipped(crate::filter::SkipReason),
    #[error("{0}")]
    Message(String),
}

impl RecorderError {
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    pub fn capture(msg: impl Into<String>) -> Self {
        Self::CaptureFailed(msg.into())
    }
}
