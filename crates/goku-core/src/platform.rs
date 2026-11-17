use std::sync::mpsc::Sender;

use crate::error::RecorderError;
use crate::imagebuf::RawImage;
use crate::types::{
    AxSnapshot, CaptureOptions, CaptureScope, FocusSnapshot, OcrSnapshot, PermissionKind,
    PermissionReport, WalkBudget,
};

pub enum InputBurst {
    Click,
    Key,
    Scroll,
}

pub trait Platform: Send {
    fn os_name(&self) -> &'static str;
    fn supported_scopes(&self) -> &[CaptureScope];
    fn permissions(&self) -> PermissionReport;
    fn request(&self, kind: PermissionKind) -> Result<(), RecorderError>;
    fn open_settings(&self, kind: PermissionKind) -> Result<(), RecorderError>;
    fn focus(&self) -> Result<FocusSnapshot, RecorderError>;
    fn capture(
        &self,
        focus: &FocusSnapshot,
        scope: CaptureScope,
        opts: &CaptureOptions,
    ) -> Result<RawImage, RecorderError>;
    fn accessibility(
        &self,
        focus: &FocusSnapshot,
        budget: WalkBudget,
    ) -> Result<AxSnapshot, RecorderError>;
    fn ocr(&self, image: &RawImage) -> Result<OcrSnapshot, RecorderError>;
    fn secure_input(&self) -> bool;
    fn try_watch_input(&self, tx: Sender<InputBurst>)
        -> Result<Box<dyn InputWatch>, RecorderError>;
}

pub trait InputWatch: Send {
    fn stop(&mut self);
}

pub fn resolve_scope(requested: CaptureScope, supported: &[CaptureScope]) -> (CaptureScope, bool) {
    if supported.contains(&requested) {
        return (requested, false);
    }
    if supported.contains(&CaptureScope::FocusedWindow) {
        return (CaptureScope::FocusedWindow, true);
    }
    (requested, true)
}

pub fn connect() -> Result<Box<dyn Platform>, RecorderError> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(crate::macos::MacPlatform::new()))
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(crate::windows::WinPlatform::new()))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err(RecorderError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_style_support_falls_back_from_focused_app() {
        let supported = [CaptureScope::FocusedWindow, CaptureScope::DisplayCrop];
        let (used, fell) = resolve_scope(CaptureScope::FocusedApp, &supported);
        assert_eq!(used, CaptureScope::FocusedWindow);
        assert!(fell);
    }

    #[test]
    fn macos_style_support_keeps_focused_app() {
        let supported = [
            CaptureScope::FocusedApp,
            CaptureScope::FocusedWindow,
            CaptureScope::DisplayCrop,
        ];
        let (used, fell) = resolve_scope(CaptureScope::FocusedApp, &supported);
        assert_eq!(used, CaptureScope::FocusedApp);
        assert!(!fell);
    }
}
