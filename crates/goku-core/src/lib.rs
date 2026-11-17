mod commit;
mod engine;
mod error;
mod filter;
mod imagebuf;
mod platform;
mod redact;
mod store;
mod triggers;
mod types;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

pub use commit::{plan_frame, CaptureFacts, FramePlan};
pub use engine::Recorder;
pub use error::RecorderError;
pub use filter::{CaptureFilter, SkipReason};
pub use imagebuf::{encode_jpeg, prepare_image, PersistableImage, RawImage};
pub use platform::{connect, resolve_scope, InputBurst, InputWatch, Platform};
pub use redact::{redact_text, Redaction};
pub use types::*;
