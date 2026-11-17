use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use axuielement::ax_attribute::{attributes, subroles};
use axuielement::{is_process_trusted, system_wide, AXError, AXUIElement};
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::access::ScreenCaptureAccess;
use core_graphics::event::{
    CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult,
};
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_foundation::{NSArray, NSData, NSDictionary};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
};
use screencapturekit::cg::CGRect;
use screencapturekit::screenshot_manager::{CGImageExt, SCScreenshotManager};
use screencapturekit::shareable_content::{
    SCDisplay, SCRunningApplication, SCShareableContent, SCWindow,
};
use screencapturekit::stream::configuration::SCStreamConfiguration;
use screencapturekit::stream::content_filter::SCContentFilter;
use screencapturekit::utils::error::SCError;

use crate::error::RecorderError;
use crate::imagebuf::RawImage;
use crate::platform::{InputBurst, InputWatch, Platform};
use crate::types::{
    AxSnapshot, CaptureOptions, CaptureScope, FocusSnapshot, OcrSnapshot, PermissionKind,
    PermissionReport, PermissionState, PixelRect, TextSegment, TextSource, WalkBudget, WindowRect,
};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(4);
const FOCUS_TIMEOUT: Duration = Duration::from_secs(4);
const OCR_TIMEOUT: Duration = Duration::from_secs(2);
const AX_MESSAGE_TIMEOUT_SECS: f32 = 0.25;
const MAX_CAPTURE_EDGE: u32 = 8192;
const FRAME_TOLERANCE: f64 = 1.0;

const AX_WINDOW_NUMBER: &str = "AXWindowNumber";
const AX_BUNDLE_IDENTIFIER: &str = "AXBundleIdentifier";
const AX_WEB_AREA: &str = "AXWebArea";

static FOCUS_FLIGHT: AtomicBool = AtomicBool::new(false);
static CAPTURE_FLIGHT: AtomicBool = AtomicBool::new(false);
static AX_FLIGHT: AtomicBool = AtomicBool::new(false);
static OCR_FLIGHT: AtomicBool = AtomicBool::new(false);

pub struct MacPlatform;

impl MacPlatform {
    pub fn new() -> Self {
        Self
    }
}

impl Platform for MacPlatform {
    fn os_name(&self) -> &'static str {
        "macos"
    }

    fn supported_scopes(&self) -> &[CaptureScope] {
        &[
            CaptureScope::FocusedApp,
            CaptureScope::FocusedWindow,
            CaptureScope::DisplayCrop,
        ]
    }

    fn permissions(&self) -> PermissionReport {
        let screen = if ScreenCaptureAccess::default().preflight() {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        };
        let accessibility = if is_process_trusted() {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        };
        let input = if listen_event_access() {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        };
        PermissionReport {
            can_record: screen == PermissionState::Granted,
            screen,
            accessibility,
            input,
            degradations: vec![],
            restart_required: false,
        }
    }

    fn request(&self, kind: PermissionKind) -> Result<(), RecorderError> {
        match kind {
            PermissionKind::Screen => {
                ScreenCaptureAccess::default().request();
            }
            PermissionKind::Accessibility => {
                let _ = axuielement::is_process_trusted_with_prompt();
            }
            PermissionKind::Input => {
                request_listen_event_access();
            }
        }
        Ok(())
    }

    fn open_settings(&self, kind: PermissionKind) -> Result<(), RecorderError> {
        let pane = match kind {
            PermissionKind::Screen => "Privacy_ScreenCapture",
            PermissionKind::Accessibility => "Privacy_Accessibility",
            PermissionKind::Input => "Privacy_ListenEvent",
        };
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
        std::process::Command::new("/usr/bin/open")
            .arg(url)
            .status()
            .map_err(|err| RecorderError::Message(format!("open settings failed: {err}")))?;
        Ok(())
    }

    fn focus(&self) -> Result<FocusSnapshot, RecorderError> {
        run_bounded(
            "goku-focus",
            &FOCUS_FLIGHT,
            FOCUS_TIMEOUT,
            RecorderError::AxFailed("focus timed out".into()),
            read_focus,
        )
    }

    fn capture(
        &self,
        focus: &FocusSnapshot,
        scope: CaptureScope,
        opts: &CaptureOptions,
    ) -> Result<RawImage, RecorderError> {
        if !ScreenCaptureAccess::default().preflight() {
            return Err(RecorderError::PermissionMissing);
        }
        let focus = focus.clone();
        let opts = opts.clone();
        run_bounded(
            "goku-capture",
            &CAPTURE_FLIGHT,
            CAPTURE_TIMEOUT,
            RecorderError::CaptureTimeout,
            move || capture_window(&focus, scope, &opts),
        )
    }

    fn accessibility(
        &self,
        focus: &FocusSnapshot,
        budget: WalkBudget,
    ) -> Result<AxSnapshot, RecorderError> {
        if !is_process_trusted() {
            return Err(RecorderError::AxFailed(
                "accessibility permission is missing".into(),
            ));
        }
        let focus = focus.clone();
        let wait = Duration::from_millis(budget.timeout_ms.max(1));
        run_bounded(
            "goku-ax",
            &AX_FLIGHT,
            wait,
            RecorderError::AxFailed("accessibility walk timed out".into()),
            move || walk_window(&focus, &budget),
        )
    }

    fn ocr(&self, image: &RawImage) -> Result<OcrSnapshot, RecorderError> {
        let image = image.clone();
        run_bounded(
            "goku-ocr",
            &OCR_FLIGHT,
            OCR_TIMEOUT,
            RecorderError::OcrFailed("ocr timed out".into()),
            move || recognize(&image),
        )
    }

    fn secure_input(&self) -> bool {
        unsafe { IsSecureEventInputEnabled() }
    }

    fn try_watch_input(
        &self,
        tx: Sender<InputBurst>,
    ) -> Result<Box<dyn InputWatch>, RecorderError> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        thread::Builder::new()
            .name("goku-input".into())
            .spawn(move || run_input_tap(tx, stop_for_thread, ready_tx))
            .map_err(|err| RecorderError::Message(format!("input watch failed: {err}")))?;
        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => Ok(Box::new(TapWatch { stop })),
            Ok(Err(err)) => Err(err),
            Err(_) => Err(RecorderError::Message(
                "input event tap did not start".into(),
            )),
        }
    }
}

struct TapWatch {
    stop: Arc<AtomicBool>,
}

impl InputWatch for TapWatch {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for TapWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct FlightPermit(&'static AtomicBool);

impl Drop for FlightPermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn run_bounded<T, F>(
    name: &str,
    flight: &'static AtomicBool,
    timeout: Duration,
    on_timeout: RecorderError,
    work: F,
) -> Result<T, RecorderError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, RecorderError> + Send + 'static,
{
    let deadline = Instant::now() + timeout;
    let permit = match acquire(flight, deadline) {
        Ok(permit) => permit,
        Err(()) => return Err(on_timeout),
    };
    let (tx, rx) = mpsc::channel();
    let spawned = thread::Builder::new().name(name.into()).spawn(move || {
        let _permit = permit;
        let _ = tx.send(work());
    });
    if let Err(err) = spawned {
        return Err(RecorderError::Message(err.to_string()));
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(remaining) {
        Ok(result) => result,
        Err(_) => Err(on_timeout),
    }
}

fn acquire(flight: &'static AtomicBool, deadline: Instant) -> Result<FlightPermit, ()> {
    loop {
        if flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Ok(FlightPermit(flight));
        }
        if Instant::now() >= deadline {
            return Err(());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn listen_event_access() -> bool {
    unsafe { CGPreflightListenEventAccess() }
}

fn request_listen_event_access() {
    unsafe {
        CGRequestListenEventAccess();
    }
}

fn read_focus() -> Result<FocusSnapshot, RecorderError> {
    if !is_process_trusted() {
        return Err(RecorderError::AxFailed(
            "accessibility permission is missing".into(),
        ));
    }
    let system =
        system_wide().ok_or_else(|| RecorderError::AxFailed("no system element".into()))?;
    let _ = system.set_timeout(AX_MESSAGE_TIMEOUT_SECS);
    let app = system
        .focused_application()
        .map_err(ax_err)?
        .ok_or_else(|| RecorderError::AxFailed("no focused application".into()))?;
    let _ = app.set_timeout(AX_MESSAGE_TIMEOUT_SECS);
    let pid = app.pid().map_err(ax_err)?;
    if pid < 0 {
        return Err(RecorderError::AxFailed(
            "focused application has no pid".into(),
        ));
    }
    let window = system
        .focused_window()
        .map_err(ax_err)?
        .or_else(|| {
            app.element_attribute(attributes::AX_FOCUSED_WINDOW_ATTRIBUTE)
                .ok()
                .flatten()
        })
        .or_else(|| {
            app.element_attribute(attributes::AX_MAIN_WINDOW_ATTRIBUTE)
                .ok()
                .flatten()
        })
        .ok_or(RecorderError::NoFocusedWindow)?;
    let _ = window.set_timeout(AX_MESSAGE_TIMEOUT_SECS);
    let (origin, size) = element_frame(&window)?;
    if size.width <= 0.0 || size.height <= 0.0 {
        return Err(RecorderError::NoFocusedWindow);
    }
    let app_id = app
        .string_attribute(AX_BUNDLE_IDENTIFIER)
        .ok()
        .flatten()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| format!("pid:{pid}"));
    let app_name = app
        .string_attribute(attributes::AX_TITLE_ATTRIBUTE)
        .ok()
        .flatten()
        .unwrap_or_default();
    let window_title = window
        .string_attribute(attributes::AX_TITLE_ATTRIBUTE)
        .ok()
        .flatten()
        .unwrap_or_default();
    let width = size.width.round() as u32;
    let height = size.height.round() as u32;
    let rect = WindowRect {
        x: origin.x.round() as i32,
        y: origin.y.round() as i32,
        width,
        height,
        scale: 1,
    };
    let window_id = window
        .i64_attribute(AX_WINDOW_NUMBER)
        .ok()
        .flatten()
        .filter(|id| *id > 0)
        .map(|id| id as u64)
        .map(Ok)
        .unwrap_or_else(|| resolve_window_id(pid, &rect, &window_title))?;
    let scale = display_scale_at(origin.x, origin.y);
    Ok(FocusSnapshot {
        identity: crate::types::FocusIdentity {
            pid: pid as u32,
            window_id,
            app_id,
        },
        app_name,
        window_title,
        rect: WindowRect { scale, ..rect },
        display_id: display_id_at(origin.x, origin.y),
    })
}

fn element_frame(
    element: &AXUIElement,
) -> Result<(axuielement::AXPoint, axuielement::AXSize), RecorderError> {
    let origin = element
        .point_attribute(attributes::AX_POSITION_ATTRIBUTE)
        .map_err(ax_err)?
        .ok_or(RecorderError::NoFocusedWindow)?;
    let size = element
        .size_attribute(attributes::AX_SIZE_ATTRIBUTE)
        .map_err(ax_err)?
        .ok_or(RecorderError::NoFocusedWindow)?;
    Ok((origin, size))
}

fn ax_err(err: AXError) -> RecorderError {
    RecorderError::AxFailed(err.to_string())
}

fn resolve_window_id(pid: i32, rect: &WindowRect, title: &str) -> Result<u64, RecorderError> {
    if !ScreenCaptureAccess::default().preflight() {
        return Err(RecorderError::AxFailed(
            "focused window has no stable id".into(),
        ));
    }
    let content = SCShareableContent::create()
        .with_on_screen_windows_only(true)
        .get()
        .map_err(|err| match err {
            SCError::PermissionDenied(_) => RecorderError::PermissionMissing,
            other => RecorderError::AxFailed(other.to_string()),
        })?;
    let hints = content
        .windows()
        .iter()
        .filter(|window| {
            window
                .owning_application()
                .is_some_and(|owner| owner.process_id() == pid)
        })
        .map(window_hint)
        .collect::<Vec<_>>();
    match pick_window_id(&hints, rect, title) {
        WindowPick::One(id) => Ok(id),
        WindowPick::Missing => Err(RecorderError::NoFocusedWindow),
        WindowPick::Ambiguous => Err(RecorderError::AxFailed(
            "focused window is ambiguous".into(),
        )),
    }
}

fn capture_window(
    focus: &FocusSnapshot,
    scope: CaptureScope,
    opts: &CaptureOptions,
) -> Result<RawImage, RecorderError> {
    let content = SCShareableContent::create()
        .with_on_screen_windows_only(true)
        .get()
        .map_err(capture_err)?;
    let app = content
        .applications()
        .into_iter()
        .find(|app| app.process_id() == focus.identity.pid as i32)
        .ok_or_else(|| RecorderError::capture("focused application is not shareable"))?;
    let window = match_window(&content, focus, &app)?;
    let display = display_for(&content, focus, &window)?;
    let frame = window.frame();
    let filter = build_filter(scope, &display, &app, &window)?;
    let (width, height) = logical_size(&focus.rect)?;
    let mut config = SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_shows_cursor(opts.include_cursor)
        .with_captures_audio(false);
    if !matches!(scope, CaptureScope::FocusedWindow) {
        config.set_source_rect(window_source_rect(&display, frame));
    }
    let image = SCScreenshotManager::capture_image(&filter, &config).map_err(capture_err)?;
    bgra_to_raw(&image)
}

fn match_window(
    content: &SCShareableContent,
    focus: &FocusSnapshot,
    app: &SCRunningApplication,
) -> Result<SCWindow, RecorderError> {
    let owned: Vec<SCWindow> = content
        .windows()
        .into_iter()
        .filter(|window| {
            window
                .owning_application()
                .is_some_and(|owner| owner.process_id() == app.process_id())
        })
        .collect();
    if focus.identity.window_id != 0 {
        return owned
            .into_iter()
            .find(|window| u64::from(window.window_id()) == focus.identity.window_id)
            .ok_or_else(|| RecorderError::capture("focused window is not shareable"));
    }
    let hints = owned.iter().map(window_hint).collect::<Vec<_>>();
    let id = match pick_window_id(&hints, &focus.rect, &focus.window_title) {
        WindowPick::One(id) => id,
        WindowPick::Missing => {
            return Err(RecorderError::capture("focused window is not shareable"));
        }
        WindowPick::Ambiguous => {
            return Err(RecorderError::capture("focused window is ambiguous"));
        }
    };
    owned
        .into_iter()
        .find(|window| u64::from(window.window_id()) == id)
        .ok_or_else(|| RecorderError::capture("focused window is not shareable"))
}

fn display_for(
    content: &SCShareableContent,
    focus: &FocusSnapshot,
    window: &SCWindow,
) -> Result<SCDisplay, RecorderError> {
    let displays = content.displays();
    if focus.display_id != 0 {
        if let Some(display) = displays
            .iter()
            .find(|display| display.display_id() == focus.display_id)
        {
            return Ok(display.clone());
        }
    }
    let frame = window.frame();
    displays
        .into_iter()
        .find(|display| contains_center(display.frame(), frame))
        .ok_or_else(|| RecorderError::capture("no display contains the focused window"))
}

fn build_filter(
    scope: CaptureScope,
    display: &SCDisplay,
    app: &SCRunningApplication,
    window: &SCWindow,
) -> Result<SCContentFilter, RecorderError> {
    let filter = match scope {
        CaptureScope::FocusedWindow => SCContentFilter::create().with_window(window),
        CaptureScope::FocusedApp => SCContentFilter::create()
            .with_display(display)
            .with_including_applications(&[app], &[]),
        CaptureScope::DisplayCrop => SCContentFilter::create()
            .with_display(display)
            .with_excluding_windows(&[]),
    };
    filter
        .build()
        .map_err(|err| RecorderError::capture(err.to_string()))
}

fn window_source_rect(display: &SCDisplay, window: CGRect) -> CGRect {
    let frame = display.frame();
    CGRect::new(
        window.origin.x - frame.origin.x,
        window.origin.y - frame.origin.y,
        window.size.width,
        window.size.height,
    )
}

fn logical_size(rect: &WindowRect) -> Result<(u32, u32), RecorderError> {
    if rect.width == 0
        || rect.height == 0
        || rect.width > MAX_CAPTURE_EDGE
        || rect.height > MAX_CAPTURE_EDGE
    {
        return Err(RecorderError::capture("window size is not capturable"));
    }
    Ok((rect.width, rect.height))
}

fn bgra_to_raw(image: &screencapturekit::CGImage) -> Result<RawImage, RecorderError> {
    let width = u32::try_from(image.width())
        .map_err(|_| RecorderError::capture("image width overflows"))?;
    let height = u32::try_from(image.height())
        .map_err(|_| RecorderError::capture("image height overflows"))?;
    let bgra = image.bgra_data().map_err(capture_err)?;
    let mut rgba = bgra;
    let mut index = 0;
    while index + 3 < rgba.len() {
        rgba.swap(index, index + 2);
        index += 4;
    }
    RawImage::new(width, height, rgba)
}

fn capture_err(err: SCError) -> RecorderError {
    match err {
        SCError::PermissionDenied(_) => RecorderError::PermissionMissing,
        other => RecorderError::capture(other.to_string()),
    }
}

fn contains_center(display: CGRect, window: CGRect) -> bool {
    let cx = window.origin.x + window.size.width / 2.0;
    let cy = window.origin.y + window.size.height / 2.0;
    cx >= display.origin.x
        && cy >= display.origin.y
        && cx < display.origin.x + display.size.width
        && cy < display.origin.y + display.size.height
}

struct WindowHint {
    id: u64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    title: String,
}

enum WindowPick {
    One(u64),
    Missing,
    Ambiguous,
}

fn window_hint(window: &SCWindow) -> WindowHint {
    let frame = window.frame();
    WindowHint {
        id: u64::from(window.window_id()),
        x: frame.origin.x,
        y: frame.origin.y,
        width: frame.size.width,
        height: frame.size.height,
        title: window.title().unwrap_or_default(),
    }
}

fn frame_close(x: f64, y: f64, width: f64, height: f64, rect: &WindowRect) -> bool {
    (x - f64::from(rect.x)).abs() <= FRAME_TOLERANCE
        && (y - f64::from(rect.y)).abs() <= FRAME_TOLERANCE
        && (width - f64::from(rect.width)).abs() <= FRAME_TOLERANCE
        && (height - f64::from(rect.height)).abs() <= FRAME_TOLERANCE
}

fn pick_window_id(hints: &[WindowHint], rect: &WindowRect, title: &str) -> WindowPick {
    let mut hits: Vec<&WindowHint> = hints
        .iter()
        .filter(|hint| hint.id != 0 && frame_close(hint.x, hint.y, hint.width, hint.height, rect))
        .collect();
    if hits.len() > 1 && !title.is_empty() {
        let titled: Vec<&WindowHint> = hits
            .iter()
            .copied()
            .filter(|hint| hint.title == title)
            .collect();
        if !titled.is_empty() {
            hits = titled;
        }
    }
    match hits.as_slice() {
        [one] => WindowPick::One(one.id),
        [] => WindowPick::Missing,
        _ => WindowPick::Ambiguous,
    }
}

struct Walk {
    segments: Vec<TextSegment>,
    browser_url: Option<String>,
    document_path: Option<String>,
    node_count: u32,
    chars: u32,
    truncated: bool,
    unmapped_secure: bool,
    deadline: Instant,
    budget: WalkBudget,
    window: PixelRect,
}

fn walk_window(focus: &FocusSnapshot, budget: &WalkBudget) -> Result<AxSnapshot, RecorderError> {
    let system =
        system_wide().ok_or_else(|| RecorderError::AxFailed("no system element".into()))?;
    let _ = system.set_timeout(AX_MESSAGE_TIMEOUT_SECS);
    let app = system
        .focused_application()
        .map_err(ax_err)?
        .ok_or_else(|| RecorderError::AxFailed("no focused application".into()))?;
    let _ = app.set_timeout(AX_MESSAGE_TIMEOUT_SECS);
    if app.pid().map_err(ax_err)? != focus.identity.pid as i32 {
        return Err(RecorderError::AxFailed(
            "focused application changed before the walk".into(),
        ));
    }
    let window = app
        .element_attribute(attributes::AX_FOCUSED_WINDOW_ATTRIBUTE)
        .map_err(ax_err)?
        .or_else(|| {
            app.element_attribute(attributes::AX_MAIN_WINDOW_ATTRIBUTE)
                .ok()
                .flatten()
        })
        .ok_or(RecorderError::NoFocusedWindow)?;
    let _ = window.set_timeout(AX_MESSAGE_TIMEOUT_SECS);
    same_ax_window(&window, focus)?;
    let window_rect = window_point_rect(focus);
    let mut walk = Walk {
        segments: Vec::new(),
        browser_url: None,
        document_path: None,
        node_count: 0,
        chars: 0,
        truncated: false,
        unmapped_secure: false,
        deadline: Instant::now() + Duration::from_millis(budget.timeout_ms),
        budget: budget.clone(),
        window: window_rect,
    };
    walk.visit(&window, 0);
    if walk.browser_url.is_none() {
        walk.browser_url = window
            .string_attribute(attributes::AX_DOCUMENT_ATTRIBUTE)
            .ok()
            .flatten()
            .filter(|value| value.starts_with("http://") || value.starts_with("https://"));
    }
    if walk.document_path.is_none() {
        walk.document_path = window
            .string_attribute(attributes::AX_DOCUMENT_ATTRIBUTE)
            .ok()
            .flatten()
            .filter(|value| value.starts_with("file:"));
    }
    if walk.unmapped_secure {
        return Err(RecorderError::AxFailed("secure field has no bounds".into()));
    }
    Ok(AxSnapshot {
        segments: walk.segments,
        browser_url: walk.browser_url,
        document_path: walk.document_path,
        truncated: walk.truncated,
        node_count: walk.node_count,
    })
}

impl Walk {
    fn expired(&self) -> bool {
        self.node_count >= self.budget.max_nodes || Instant::now() >= self.deadline
    }

    fn visit(&mut self, element: &AXUIElement, depth: u32) {
        if self.expired() {
            self.truncated = true;
            return;
        }
        self.node_count += 1;
        let _ = element.set_timeout(AX_MESSAGE_TIMEOUT_SECS);
        let role = element
            .string_attribute(attributes::AX_ROLE_ATTRIBUTE)
            .ok()
            .flatten()
            .unwrap_or_default();
        let subrole = element
            .string_attribute(attributes::AX_SUBROLE_ATTRIBUTE)
            .ok()
            .flatten()
            .unwrap_or_default();
        self.note_url(element, &role);
        self.note_document(element);
        self.note_text(element, &role, &subrole);
        if depth >= self.budget.max_depth || self.expired() {
            self.truncated = depth >= self.budget.max_depth || self.expired();
            return;
        }
        let children = element.children().unwrap_or_default();
        for child in children {
            if self.expired() {
                self.truncated = true;
                return;
            }
            self.visit(&child, depth + 1);
        }
    }

    fn note_url(&mut self, element: &AXUIElement, role: &str) {
        if self.browser_url.is_some() || role != AX_WEB_AREA {
            return;
        }
        self.browser_url = element
            .string_attribute(attributes::AXURL_ATTRIBUTE)
            .ok()
            .flatten()
            .or_else(|| {
                element
                    .string_attribute(attributes::AX_DOCUMENT_ATTRIBUTE)
                    .ok()
                    .flatten()
            })
            .filter(|value| !value.is_empty());
    }

    fn note_document(&mut self, element: &AXUIElement) {
        if self.document_path.is_some() {
            return;
        }
        self.document_path = element
            .string_attribute(attributes::AX_DOCUMENT_ATTRIBUTE)
            .ok()
            .flatten()
            .filter(|value| value.starts_with("file:"));
    }

    fn note_text(&mut self, element: &AXUIElement, role: &str, subrole: &str) {
        let secure = role == subroles::AX_SECURE_TEXT_FIELD_SUBROLE
            || subrole == subroles::AX_SECURE_TEXT_FIELD_SUBROLE
            || role == "AXSecureTextField";
        if secure {
            let bbox = match element_frame(element) {
                Ok((origin, size)) => {
                    clip_to_window(origin.x, origin.y, size.width, size.height, self.window)
                }
                Err(_) => {
                    let cover = cover_window(self.window);
                    if cover.is_empty() {
                        self.unmapped_secure = true;
                        return;
                    }
                    Some(cover)
                }
            };
            let Some(bbox) = bbox.filter(|rect| !rect.is_empty()) else {
                return;
            };
            self.segments.push(TextSegment {
                text: String::new(),
                role: subroles::AX_SECURE_TEXT_FIELD_SUBROLE.into(),
                bbox: Some(bbox),
                source: TextSource::Accessibility,
            });
            return;
        }
        let bbox = element_bbox(element, self.window);
        let text = element
            .string_attribute(attributes::AX_VALUE_ATTRIBUTE)
            .ok()
            .flatten()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                element
                    .string_attribute(attributes::AX_TITLE_ATTRIBUTE)
                    .ok()
                    .flatten()
                    .filter(|value| !value.trim().is_empty())
            })
            .or_else(|| {
                element
                    .string_attribute(attributes::AX_DESCRIPTION_ATTRIBUTE)
                    .ok()
                    .flatten()
                    .filter(|value| !value.trim().is_empty())
            });
        let Some(text) = text else {
            return;
        };
        if self.chars >= self.budget.max_chars {
            self.truncated = true;
            return;
        }
        let room = (self.budget.max_chars - self.chars) as usize;
        let text = if text.len() > room {
            self.truncated = true;
            text.chars().take(room).collect()
        } else {
            text
        };
        self.chars += text.chars().count() as u32;
        self.segments.push(TextSegment {
            text,
            role: role.to_string(),
            bbox,
            source: TextSource::Accessibility,
        });
    }
}

fn same_ax_window(window: &AXUIElement, focus: &FocusSnapshot) -> Result<(), RecorderError> {
    if let Some(id) = window
        .i64_attribute(AX_WINDOW_NUMBER)
        .ok()
        .flatten()
        .filter(|id| *id > 0)
    {
        if focus.identity.window_id != 0 && id as u64 != focus.identity.window_id {
            return Err(RecorderError::AxFailed(
                "focused window changed before the walk".into(),
            ));
        }
        return Ok(());
    }
    let (origin, size) = element_frame(window)?;
    if frame_close(origin.x, origin.y, size.width, size.height, &focus.rect) {
        Ok(())
    } else {
        Err(RecorderError::AxFailed(
            "focused window changed before the walk".into(),
        ))
    }
}

fn window_point_rect(focus: &FocusSnapshot) -> PixelRect {
    PixelRect {
        x: focus.rect.x,
        y: focus.rect.y,
        width: focus.rect.width,
        height: focus.rect.height,
    }
}

fn cover_window(window: PixelRect) -> PixelRect {
    PixelRect {
        x: 0,
        y: 0,
        width: window.width,
        height: window.height,
    }
}

fn element_bbox(element: &AXUIElement, window: PixelRect) -> Option<PixelRect> {
    let origin = element
        .point_attribute(attributes::AX_POSITION_ATTRIBUTE)
        .ok()
        .flatten()?;
    let size = element
        .size_attribute(attributes::AX_SIZE_ATTRIBUTE)
        .ok()
        .flatten()?;
    clip_to_window(origin.x, origin.y, size.width, size.height, window)
}

fn clip_to_window(x: f64, y: f64, width: f64, height: f64, window: PixelRect) -> Option<PixelRect> {
    if !x.is_finite()
        || !y.is_finite()
        || !width.is_finite()
        || !height.is_finite()
        || width <= 0.0
        || height <= 0.0
    {
        return None;
    }
    let left = (x - f64::from(window.x)).floor() as i64;
    let top = (y - f64::from(window.y)).floor() as i64;
    let right = (x + width - f64::from(window.x)).ceil() as i64;
    let bottom = (y + height - f64::from(window.y)).ceil() as i64;
    let clip_left = left.max(0);
    let clip_top = top.max(0);
    let clip_right = right.min(i64::from(window.width));
    let clip_bottom = bottom.min(i64::from(window.height));
    if clip_right <= clip_left || clip_bottom <= clip_top {
        return None;
    }
    Some(PixelRect {
        x: i32::try_from(clip_left).ok()?,
        y: i32::try_from(clip_top).ok()?,
        width: u32::try_from(clip_right - clip_left).ok()?,
        height: u32::try_from(clip_bottom - clip_top).ok()?,
    })
}

fn recognize(image: &RawImage) -> Result<OcrSnapshot, RecorderError> {
    let jpeg = jpeg_bytes(image)?;
    let data = NSData::with_bytes(&jpeg);
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Fast);
    request.setUsesLanguageCorrection(false);
    let as_request: Retained<VNRequest> =
        Retained::into_super(Retained::into_super(request.clone()));
    let requests = NSArray::from_retained_slice(&[as_request]);
    let options: Retained<NSDictionary<objc2_vision::VNImageOption, objc2::runtime::AnyObject>> =
        NSDictionary::dictionary();
    let handler = VNImageRequestHandler::initWithData_options(
        <VNImageRequestHandler as AnyThread>::alloc(),
        &data,
        &options,
    );
    handler
        .performRequests_error(&requests)
        .map_err(|err| RecorderError::OcrFailed(err.localizedDescription().to_string()))?;
    let width = image.width;
    let height = image.height;
    let mut segments = Vec::new();
    if let Some(results) = request.results() {
        for observation in results.iter() {
            let candidates = observation.topCandidates(1);
            let Some(candidate) = candidates.iter().next() else {
                continue;
            };
            let text = candidate.string().to_string();
            if text.trim().is_empty() {
                continue;
            }
            let rect = unsafe { observation.boundingBox() };
            segments.push(TextSegment {
                text,
                role: "ocr".into(),
                bbox: Some(normalized_box(rect, width, height)),
                source: TextSource::Ocr,
            });
        }
    }
    Ok(OcrSnapshot { segments })
}

fn jpeg_bytes(image: &RawImage) -> Result<Vec<u8>, RecorderError> {
    let mut out = Vec::new();
    let rgb: Vec<u8> = image
        .rgba
        .chunks_exact(4)
        .flat_map(|px| px[..3].iter().copied())
        .collect();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80);
    encoder
        .encode(
            &rgb,
            image.width,
            image.height,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|err| RecorderError::OcrFailed(err.to_string()))?;
    Ok(out)
}

fn normalized_box(rect: objc2_core_foundation::CGRect, width: u32, height: u32) -> PixelRect {
    let x = (rect.origin.x * f64::from(width)).round() as i32;
    let y_top = (1.0 - rect.origin.y - rect.size.height) * f64::from(height);
    PixelRect {
        x,
        y: y_top.round() as i32,
        width: (rect.size.width * f64::from(width)).round().max(0.0) as u32,
        height: (rect.size.height * f64::from(height)).round().max(0.0) as u32,
    }
}

fn run_input_tap(
    tx: Sender<InputBurst>,
    stop: Arc<AtomicBool>,
    ready: Sender<Result<(), RecorderError>>,
) {
    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::ListenOnly,
        vec![
            CGEventType::KeyDown,
            CGEventType::LeftMouseDown,
            CGEventType::RightMouseDown,
            CGEventType::OtherMouseDown,
            CGEventType::ScrollWheel,
        ],
        move |_proxy, kind, _event| {
            let burst = match kind {
                CGEventType::KeyDown => Some(InputBurst::Key),
                CGEventType::ScrollWheel => Some(InputBurst::Scroll),
                CGEventType::LeftMouseDown
                | CGEventType::RightMouseDown
                | CGEventType::OtherMouseDown => Some(InputBurst::Click),
                _ => None,
            };
            if let Some(burst) = burst {
                let _ = tx.send(burst);
            }
            CallbackResult::Keep
        },
    );
    let tap = match tap {
        Ok(tap) => tap,
        Err(()) => {
            let _ = ready.send(Err(RecorderError::Message(
                "input event tap could not be created".into(),
            )));
            return;
        }
    };
    let source = match tap.mach_port().create_runloop_source(0) {
        Ok(source) => source,
        Err(()) => {
            let _ = ready.send(Err(RecorderError::Message(
                "input event tap has no run loop source".into(),
            )));
            return;
        }
    };
    let loop_ = CFRunLoop::get_current();
    loop_.add_source(&source, unsafe { kCFRunLoopCommonModes });
    tap.enable();
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::Relaxed) {
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, 0);
        }
    }
    drop(source);
    drop(loop_);
}

fn display_id_at(x: f64, y: f64) -> u32 {
    let mut display = 0_u32;
    let mut count = 0_u32;
    let err = unsafe { CGGetDisplaysWithPoint(CGPoint { x, y }, 1, &mut display, &mut count) };
    if err != 0 || count == 0 {
        0
    } else {
        display
    }
}

fn display_scale_at(x: f64, y: f64) -> u32 {
    let mut display = 0_u32;
    let mut count = 0_u32;
    let found = unsafe { CGGetDisplaysWithPoint(CGPoint { x, y }, 1, &mut display, &mut count) };
    if found != 0 || count == 0 {
        return 1;
    }
    let mode = unsafe { CGDisplayCopyDisplayMode(display) };
    if mode.is_null() {
        return 1;
    }
    let pixels = unsafe { CGDisplayModeGetPixelWidth(mode) };
    let points = unsafe { CGDisplayModeGetWidth(mode) };
    unsafe { CGDisplayModeRelease(mode) };
    if points <= 0 {
        1
    } else {
        ((pixels as f64 / points as f64).round() as u32).max(1)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn CGGetDisplaysWithPoint(point: CGPoint, max: u32, displays: *mut u32, count: *mut u32)
        -> i32;
    fn CGDisplayCopyDisplayMode(display: u32) -> *mut std::ffi::c_void;
    fn CGDisplayModeGetPixelWidth(mode: *mut std::ffi::c_void) -> usize;
    fn CGDisplayModeGetWidth(mode: *mut std::ffi::c_void) -> usize;
    fn CGDisplayModeRelease(mode: *mut std::ffi::c_void);
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *const std::ffi::c_void;
    fn CFRunLoopRunInMode(
        mode: *const std::ffi::c_void,
        seconds: f64,
        return_after_source: i32,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_are_the_three_mac_scopes() {
        let platform = MacPlatform::new();
        assert_eq!(
            platform.supported_scopes(),
            &[
                CaptureScope::FocusedApp,
                CaptureScope::FocusedWindow,
                CaptureScope::DisplayCrop,
            ]
        );
    }

    #[test]
    fn permissions_read_status_without_prompting() {
        let report = MacPlatform::new().permissions();
        assert_eq!(report.can_record, report.screen == PermissionState::Granted);
        assert!(matches!(
            report.screen,
            PermissionState::Granted | PermissionState::Denied
        ));
    }

    #[test]
    fn logical_size_keeps_point_dimensions() {
        let rect = WindowRect {
            x: 10,
            y: 20,
            width: 4000,
            height: 2000,
            scale: 2,
        };
        assert_eq!(logical_size(&rect).unwrap(), (4000, 2000));
    }

    #[test]
    fn clip_uses_window_points_not_scale() {
        let window = PixelRect {
            x: 100,
            y: 200,
            width: 80,
            height: 40,
        };
        let inside = clip_to_window(110.0, 210.0, 10.0, 8.0, window).unwrap();
        assert_eq!(
            inside,
            PixelRect {
                x: 10,
                y: 10,
                width: 10,
                height: 8
            }
        );
        let partial = clip_to_window(90.0, 190.0, 20.0, 20.0, window).unwrap();
        assert_eq!(
            partial,
            PixelRect {
                x: 0,
                y: 0,
                width: 10,
                height: 10
            }
        );
        assert!(clip_to_window(0.0, 0.0, 10.0, 10.0, window).is_none());
        let covered = cover_window(window);
        assert_eq!(covered.width, window.width);
        assert_eq!(covered.height, window.height);
        assert!(!covered.is_empty());
    }

    #[test]
    fn pick_requires_a_unique_frame() {
        let rect = WindowRect {
            x: 10,
            y: 20,
            width: 100,
            height: 80,
            scale: 2,
        };
        let hints = [
            WindowHint {
                id: 7,
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 80.0,
                title: "Notes".into(),
            },
            WindowHint {
                id: 8,
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 300.0,
                title: "Notes".into(),
            },
        ];
        assert!(matches!(
            pick_window_id(&hints, &rect, "Notes"),
            WindowPick::One(7)
        ));
        let tied = [
            WindowHint {
                id: 7,
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 80.0,
                title: "A".into(),
            },
            WindowHint {
                id: 9,
                x: 10.2,
                y: 20.2,
                width: 100.0,
                height: 80.0,
                title: "B".into(),
            },
        ];
        assert!(matches!(
            pick_window_id(&tied, &rect, ""),
            WindowPick::Ambiguous
        ));
        assert!(matches!(
            pick_window_id(&tied, &rect, "A"),
            WindowPick::One(7)
        ));
    }

    #[test]
    fn bgra_swap_is_rgba() {
        let mut bytes = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let mut index = 0;
        while index + 3 < bytes.len() {
            bytes.swap(index, index + 2);
            index += 4;
        }
        assert_eq!(bytes, vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    #[ignore]
    fn live_focus_reads_a_window() {
        let focus = MacPlatform::new().focus().expect("focus");
        assert!(focus.rect.width > 0);
        assert!(focus.rect.height > 0);
    }
}
