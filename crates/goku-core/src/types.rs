use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::RecorderError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureScope {
    FocusedApp,
    FocusedWindow,
    DisplayCrop,
}

impl CaptureScope {
    pub fn parse(s: &str) -> Result<Self, RecorderError> {
        match s {
            "focusedApp" => Ok(Self::FocusedApp),
            "focusedWindow" => Ok(Self::FocusedWindow),
            "displayCrop" => Ok(Self::DisplayCrop),
            other => Err(RecorderError::config(format!(
                "unknown capture scope {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextMode {
    AccessibilityFirst,
    OcrOnly,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnUnmappable {
    WithholdImage,
    RedactWholeImage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelPolicy {
    Off,
    SecretRegions { on_unmappable: OnUnmappable },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    AppSwitch,
    WindowFocus,
    Click,
    TypingPause,
    ScrollStop,
    Idle,
    Manual,
}

impl TriggerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppSwitch => "app_switch",
            Self::WindowFocus => "window_focus",
            Self::Click => "click",
            Self::TypingPause => "typing_pause",
            Self::ScrollStop => "scroll_stop",
            Self::Idle => "idle",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageStatus {
    Ready,
    WithheldRedaction,
    WithheldPermission,
    WithheldSecureInput,
    WithheldDenylist,
    WithheldCapture,
}

impl ImageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::WithheldRedaction => "withheld_redaction",
            Self::WithheldPermission => "withheld_permission",
            Self::WithheldSecureInput => "withheld_secure_input",
            Self::WithheldDenylist => "withheld_denylist",
            Self::WithheldCapture => "withheld_capture",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardReason {
    FocusIncoherent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    Screen,
    Accessibility,
    Input,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    Granted,
    Denied,
    NotNeeded,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Degradation {
    ScopeFallback {
        requested: CaptureScope,
        used: CaptureScope,
    },
    InputHooksUnavailable,
    AccessibilityUnavailable,
    OcrUnavailable,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionReport {
    pub screen: PermissionState,
    pub accessibility: PermissionState,
    pub input: PermissionState,
    pub can_record: bool,
    pub degradations: Vec<Degradation>,
    pub restart_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionId(String);

impl SessionId {
    pub fn parse(raw: &str) -> Result<Self, RecorderError> {
        let ok = raw.len() >= 8
            && raw.len() <= 64
            && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !raw.starts_with('-')
            && !raw.ends_with('-')
            && !raw.contains("--");
        if !ok {
            return Err(RecorderError::config(
                "session id must be 8-64 url-safe characters",
            ));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn fresh() -> Self {
        static N: AtomicU64 = AtomicU64::new(1);
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        Self(format!("{ms:x}-{n:x}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FrameId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativePath(String);

impl RelativePath {
    pub fn parse(raw: &str) -> Result<Self, RecorderError> {
        if raw.is_empty() || raw.starts_with('/') || raw.starts_with('\\') || raw.contains(':') {
            return Err(RecorderError::config("image path must be relative"));
        }
        let bad = raw
            .split(['/', '\\'])
            .any(|part| part.is_empty() || part == "." || part == "..");
        if bad {
            return Err(RecorderError::config("image path must not contain . or .."));
        }
        Ok(Self(raw.replace('\\', "/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir(PathBuf);

impl DataDir {
    pub fn create(path: impl AsRef<Path>) -> Result<Self, RecorderError> {
        let path = path.as_ref();
        std::fs::create_dir_all(path).map_err(|e| RecorderError::config(e.to_string()))?;
        let canon = path
            .canonicalize()
            .map_err(|e| RecorderError::config(e.to_string()))?;
        Ok(Self(canon))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn resolve(&self, rel: &RelativePath) -> Result<PathBuf, RecorderError> {
        let root_meta =
            std::fs::symlink_metadata(&self.0).map_err(|e| RecorderError::config(e.to_string()))?;
        if root_meta.file_type().is_symlink() {
            return Err(RecorderError::config("data dir is a symlink"));
        }
        let mut cur = self.0.clone();
        for part in rel.as_str().split('/') {
            if part.is_empty() || part == "." || part == ".." {
                return Err(RecorderError::config("resolved path escapes data dir"));
            }
            cur.push(part);
            if !cur.starts_with(&self.0) {
                return Err(RecorderError::config("resolved path escapes data dir"));
            }
            if cur.exists() {
                let meta = std::fs::symlink_metadata(&cur)
                    .map_err(|e| RecorderError::config(e.to_string()))?;
                if meta.file_type().is_symlink() {
                    return Err(RecorderError::config("path contains a symlink"));
                }
            }
        }
        Ok(cur)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusIdentity {
    pub pid: u32,
    pub window_id: u64,
    pub app_id: String,
}

impl FocusIdentity {
    pub fn same_window(&self, other: &Self) -> bool {
        if self.pid != other.pid {
            return false;
        }
        if self.window_id != 0 && other.window_id != 0 {
            return self.window_id == other.window_id;
        }
        self.app_id == other.app_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusSnapshot {
    pub identity: FocusIdentity,
    pub app_name: String,
    pub window_title: String,
    pub rect: WindowRect,
    pub display_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSegment {
    pub text: String,
    pub role: String,
    pub bbox: Option<PixelRect>,
    pub source: TextSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextSource {
    Accessibility,
    Ocr,
    None,
}

impl TextSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accessibility => "accessibility",
            Self::Ocr => "ocr",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxSnapshot {
    pub segments: Vec<TextSegment>,
    pub browser_url: Option<String>,
    pub document_path: Option<String>,
    pub truncated: bool,
    pub node_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrSnapshot {
    pub segments: Vec<TextSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretHit {
    pub label: String,
    pub start: usize,
    pub end: usize,
    pub region: Option<PixelRect>,
}

impl SecretHit {
    pub fn mapped(&self) -> bool {
        self.region.is_some_and(|r| !r.is_empty())
    }
}

#[derive(Debug, Clone)]
pub struct TriggerConfig {
    pub app_switch: bool,
    pub window_focus: bool,
    pub click: bool,
    pub typing_quiet_ms: Option<u64>,
    pub scroll_quiet_ms: Option<u64>,
    pub idle_ms: Option<u64>,
    pub min_interval_ms: u64,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            app_switch: true,
            window_focus: true,
            click: false,
            typing_quiet_ms: None,
            scroll_quiet_ms: None,
            idle_ms: Some(30_000),
            min_interval_ms: 750,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CaptureOptions {
    pub scope: CaptureScope,
    pub jpeg_quality: u8,
    pub max_width: u32,
    pub include_cursor: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            scope: CaptureScope::FocusedApp,
            jpeg_quality: 80,
            max_width: 1920,
            include_cursor: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: DataDir,
    pub capture: CaptureOptions,
    pub triggers: TriggerConfig,
    pub text_mode: TextMode,
    pub pixels: PixelPolicy,
    pub filter: crate::filter::CaptureFilter,
    pub exclude_own_app: bool,
    pub pause_on_secure_input: bool,
    pub max_age_days: u32,
    pub max_bytes: u64,
}

impl Config {
    pub fn new(data_dir: impl AsRef<Path>) -> Result<Self, RecorderError> {
        let data_dir = DataDir::create(data_dir)?;
        Ok(Self {
            data_dir,
            capture: CaptureOptions::default(),
            triggers: TriggerConfig::default(),
            text_mode: TextMode::AccessibilityFirst,
            pixels: PixelPolicy::SecretRegions {
                on_unmappable: OnUnmappable::WithholdImage,
            },
            filter: crate::filter::CaptureFilter::with_default_excludes(),
            exclude_own_app: true,
            pause_on_secure_input: true,
            max_age_days: 7,
            max_bytes: 2 * 1024 * 1024 * 1024,
        })
    }
}

#[derive(Debug, Clone)]
pub struct WalkBudget {
    pub timeout_ms: u64,
    pub max_nodes: u32,
    pub max_depth: u32,
    pub max_chars: u32,
}

impl Default for WalkBudget {
    fn default() -> Self {
        Self {
            timeout_ms: 250,
            max_nodes: 5000,
            max_depth: 35,
            max_chars: 50_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub running: bool,
    pub paused: bool,
    pub session_id: Option<String>,
    pub frame_count: u64,
    pub last_trigger: Option<String>,
    pub degradations: Vec<Degradation>,
    pub os: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentFramesQuery {
    pub limit: u32,
    pub before: Option<i64>,
    pub session_id: Option<String>,
    pub app: Option<String>,
    pub trigger: Option<String>,
}

impl Default for RecentFramesQuery {
    fn default() -> Self {
        Self {
            limit: 50,
            before: None,
            session_id: None,
            app: None,
            trigger: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameSummary {
    pub id: FrameId,
    pub session_id: String,
    pub captured_at: i64,
    pub trigger: String,
    pub app_name: String,
    pub window_title: String,
    pub image_status: String,
    pub image_available: bool,
    pub text_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub id: FrameId,
    pub session_id: String,
    pub captured_at: i64,
    pub trigger: String,
    pub app_name: String,
    pub app_bundle_id: String,
    pub window_title: String,
    pub image_path: Option<String>,
    pub image_width: Option<u32>,
    pub image_height: Option<u32>,
    pub image_status: String,
    pub text: String,
    pub text_source: Option<String>,
    pub redaction_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnoseReport {
    pub permissions: PermissionReport,
    pub degradations: Vec<Degradation>,
    pub data_dir_writable: bool,
    pub redaction_self_test: bool,
    pub os: String,
    pub running: bool,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecorderEvent {
    Frame {
        id: i64,
        trigger: String,
        image_status: String,
    },
    Paused,
    Resumed,
    Stopped,
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_rejects_a_symlink_component() {
        let root = std::env::temp_dir().join(format!(
            "goku-resolve-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dir = DataDir::create(&root).unwrap();
        let outside = std::env::temp_dir().join(format!("goku-out-{}.jpg", std::process::id()));
        std::fs::write(&outside, b"keep").unwrap();
        let link = dir.as_path().join("linked.jpg");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            let rel = RelativePath::parse("linked.jpg").unwrap();
            assert!(dir.resolve(&rel).is_err());
            assert!(outside.exists());
        }
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn relative_path_rejects_escape() {
        assert!(RelativePath::parse("../x").is_err());
        assert!(RelativePath::parse("/tmp/x").is_err());
        assert!(RelativePath::parse("sessions/a/f.jpg").is_ok());
    }

    #[test]
    fn same_window_ignores_title() {
        let a = FocusIdentity {
            pid: 1,
            window_id: 9,
            app_id: "com.apple.TextEdit".into(),
        };
        let b = a.clone();
        assert!(a.same_window(&b));
        let other = FocusIdentity {
            window_id: 10,
            ..a.clone()
        };
        assert!(!a.same_window(&other));
    }
}
