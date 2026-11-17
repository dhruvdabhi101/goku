use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::core::{Interface, BOOL};
use windows::Foundation::IMemoryBufferReference;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapBufferAccessMode, BitmapPixelFormat, SoftwareBitmap,
};
use windows::Media::Ocr::OcrEngine;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{
    IMemoryBufferByteAccess, RoInitialize, RO_INIT_SINGLETHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationElement,
    IUIAutomationTreeWalker, IUIAutomationValuePattern, TreeScope, TreeScope_Children,
    TreeScope_Element, UIA_BoundingRectanglePropertyId, UIA_ControlTypePropertyId,
    UIA_IsPasswordPropertyId, UIA_NamePropertyId, UIA_ValuePatternId, UIA_ValueValuePropertyId,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG,
    MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
    WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_XBUTTONDOWN,
};

use crate::error::RecorderError;
use crate::imagebuf::RawImage;
use crate::platform::{InputBurst, InputWatch, Platform};
use crate::types::{
    AxSnapshot, CaptureOptions, CaptureScope, FocusSnapshot, OcrSnapshot, PermissionKind,
    PermissionReport, PermissionState, PixelRect, TextSegment, TextSource, WalkBudget, WindowRect,
};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(4);
const AX_DEPTH_CAP: u32 = 35;
const AX_NODE_CAP: u32 = 2000;
const AX_TIME_CAP: Duration = Duration::from_millis(250);
const STOP_MESSAGE: u32 = WM_QUIT;
const ACCESS_DENIED: &str = "0x80070005";

static SCREEN_DENIED: AtomicBool = AtomicBool::new(false);
static HOOK_TX: Mutex<Option<Sender<InputBurst>>> = Mutex::new(None);
static MOUSE_HOOK: Mutex<Option<isize>> = Mutex::new(None);
static KEY_HOOK: Mutex<Option<isize>> = Mutex::new(None);

pub struct WinPlatform;

impl WinPlatform {
    pub fn new() -> Self {
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        Self
    }
}

struct HookWatch {
    thread: Option<u32>,
    join: Option<JoinHandle<()>>,
}

impl InputWatch for HookWatch {
    fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = unsafe { PostThreadMessageW(thread, STOP_MESSAGE, WPARAM(0), LPARAM(0)) };
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for HookWatch {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Platform for WinPlatform {
    fn os_name(&self) -> &'static str {
        "windows"
    }

    fn supported_scopes(&self) -> &[CaptureScope] {
        &[CaptureScope::FocusedWindow, CaptureScope::DisplayCrop]
    }

    fn permissions(&self) -> PermissionReport {
        let screen = if SCREEN_DENIED.load(Ordering::Relaxed) {
            PermissionState::Denied
        } else {
            PermissionState::NotNeeded
        };
        PermissionReport {
            screen,
            accessibility: PermissionState::NotNeeded,
            input: PermissionState::NotNeeded,
            can_record: screen != PermissionState::Denied,
            degradations: vec![],
            restart_required: false,
        }
    }

    fn request(&self, kind: PermissionKind) -> Result<(), RecorderError> {
        match kind {
            PermissionKind::Screen | PermissionKind::Accessibility | PermissionKind::Input => {
                Ok(())
            }
        }
    }

    fn open_settings(&self, _kind: PermissionKind) -> Result<(), RecorderError> {
        Err(RecorderError::Message(
            "windows has no permission pane for capture, accessibility, or input".into(),
        ))
    }

    fn focus(&self) -> Result<FocusSnapshot, RecorderError> {
        read_focus()
    }

    fn capture(
        &self,
        focus: &FocusSnapshot,
        scope: CaptureScope,
        opts: &CaptureOptions,
    ) -> Result<RawImage, RecorderError> {
        let (used, _) = crate::platform::resolve_scope(scope, self.supported_scopes());
        match capture_one(focus, used, opts.include_cursor) {
            Err(err) if access_denied(&err) => {
                SCREEN_DENIED.store(true, Ordering::Relaxed);
                Err(RecorderError::PermissionMissing)
            }
            other => other,
        }
    }

    fn accessibility(
        &self,
        focus: &FocusSnapshot,
        budget: WalkBudget,
    ) -> Result<AxSnapshot, RecorderError> {
        hwnd_from(focus.identity.window_id)?;
        let window_id = focus.identity.window_id;
        let timeout = Duration::from_millis(budget.timeout_ms.max(AX_TIME_CAP.as_millis() as u64));
        let (tx, rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("goku-uia".into())
            .spawn(move || {
                let result = hwnd_from(window_id).and_then(|hwnd| walk_window(hwnd, budget));
                tx.send(result).ok()
            })
            .map_err(|err| RecorderError::AxFailed(err.to_string()))?;
        let result = rx.recv_timeout(timeout).unwrap_or(Ok(AxSnapshot {
            segments: Vec::new(),
            browser_url: None,
            document_path: None,
            truncated: true,
            node_count: 0,
        }));
        let _ = worker.join();
        result
    }

    fn ocr(&self, image: &RawImage) -> Result<OcrSnapshot, RecorderError> {
        recognize(image).map_err(|err| RecorderError::OcrFailed(err.to_string()))
    }

    fn secure_input(&self) -> bool {
        false
    }

    fn try_watch_input(
        &self,
        tx: Sender<InputBurst>,
    ) -> Result<Box<dyn InputWatch>, RecorderError> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("goku-hooks".into())
            .spawn(move || hook_thread(tx, ready_tx))
            .map_err(|err| RecorderError::Message(err.to_string()))?;
        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(thread)) => Ok(Box::new(HookWatch {
                thread: Some(thread),
                join: Some(join),
            })),
            Ok(Err(err)) => {
                let _ = join.join();
                Err(err)
            }
            Err(_) => {
                let _ = join.join();
                Err(RecorderError::Message(
                    "input hook thread did not start".into(),
                ))
            }
        }
    }
}

fn read_focus() -> Result<FocusSnapshot, RecorderError> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return Err(RecorderError::NoFocusedWindow);
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return Err(RecorderError::NoFocusedWindow);
    }
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }
        .map_err(|err| RecorderError::capture(err.to_string()))?;
    let width = rect.right.saturating_sub(rect.left).max(0) as u32;
    let height = rect.bottom.saturating_sub(rect.top).max(0) as u32;
    if width == 0 || height == 0 {
        return Err(RecorderError::NoFocusedWindow);
    }
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    Ok(FocusSnapshot {
        identity: crate::types::FocusIdentity {
            pid,
            window_id: hwnd.0 as usize as u64,
            app_id: exe_path(pid).unwrap_or_default(),
        },
        app_name: app_name_from_path(&exe_path(pid).unwrap_or_default()),
        window_title: window_title(hwnd),
        rect: WindowRect {
            x: rect.left,
            y: rect.top,
            width,
            height,
            scale: ((dpi as u64) * 100 / 96) as u32,
        },
        display_id: monitor.0 as u32,
    })
}

fn exe_path(pid: u32) -> Result<String, RecorderError> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|err| RecorderError::capture(err.to_string()))?;
    let path = query_image_path(handle);
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
    path
}

fn query_image_path(handle: HANDLE) -> Result<String, RecorderError> {
    let mut words = vec![0u16; 1024];
    let mut len = words.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(words.as_mut_ptr()),
            &mut len,
        )
    }
    .map_err(|err| RecorderError::capture(err.to_string()))?;
    Ok(String::from_utf16_lossy(&words[..len as usize]))
}

fn app_name_from_path(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

fn window_title(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut words = vec![0u16; len as usize + 1];
    let wrote = unsafe { GetWindowTextW(hwnd, &mut words) };
    if wrote <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&words[..wrote as usize])
}

fn hwnd_from(window_id: u64) -> Result<HWND, RecorderError> {
    let hwnd = HWND(window_id as usize as *mut _);
    if hwnd.0.is_null() {
        return Err(RecorderError::NoFocusedWindow);
    }
    Ok(hwnd)
}

fn capture_one(
    focus: &FocusSnapshot,
    scope: CaptureScope,
    include_cursor: bool,
) -> Result<RawImage, RecorderError> {
    let _ = unsafe { RoInitialize(RO_INIT_SINGLETHREADED) };
    let hwnd = hwnd_from(focus.identity.window_id)?;
    let item = match scope {
        CaptureScope::DisplayCrop => capture_item_for_monitor(hwnd)?,
        CaptureScope::FocusedWindow | CaptureScope::FocusedApp => capture_item_for_window(hwnd)?,
    };
    let size = item
        .Size()
        .map_err(|err| RecorderError::capture(err.to_string()))?;
    if size.Width <= 0 || size.Height <= 0 {
        return Err(RecorderError::capture("capture item has no pixels"));
    }
    let (d3d, device) = capture_device()?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        1,
        size,
    )
    .map_err(feature_or_capture)?;
    let session = pool
        .CreateCaptureSession(&item)
        .map_err(|err| RecorderError::capture(err.to_string()))?;
    if let Ok(required) = session.IsBorderRequired() {
        if required {
            let _ = session.SetIsBorderRequired(false);
        }
    }
    let _ = session.SetIsCursorCaptureEnabled(include_cursor);
    session
        .StartCapture()
        .map_err(|err| RecorderError::capture(err.to_string()))?;
    let frame = next_frame(&pool)?;
    let image = frame_to_rgba(&d3d, &frame);
    let _ = frame.Close();
    let _ = session.Close();
    let _ = pool.Close();
    let image = image?;
    if scope == CaptureScope::DisplayCrop {
        crop_to_window(image, focus)
    } else {
        Ok(image)
    }
}

fn capture_item_for_window(hwnd: HWND) -> Result<GraphicsCaptureItem, RecorderError> {
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|err| RecorderError::capture(err.to_string()))?;
    unsafe { interop.CreateForWindow(hwnd) }.map_err(|err| RecorderError::capture(err.to_string()))
}

fn capture_item_for_monitor(hwnd: HWND) -> Result<GraphicsCaptureItem, RecorderError> {
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.0.is_null() {
        return Err(RecorderError::capture("window is not on a monitor"));
    }
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|err| RecorderError::capture(err.to_string()))?;
    unsafe { interop.CreateForMonitor(monitor) }
        .map_err(|err| RecorderError::capture(err.to_string()))
}

fn capture_device() -> Result<(ID3D11Device, IDirect3DDevice), RecorderError> {
    let fail = |err: windows::core::Error| RecorderError::capture(err.to_string());
    let mut d3d = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut d3d),
            None,
            None,
        )
    }
    .map_err(fail)?;
    let d3d: ID3D11Device =
        d3d.ok_or_else(|| RecorderError::capture("Direct3D 11 device is unavailable"))?;
    let dxgi: IDXGIDevice = d3d.cast().map_err(fail)?;
    let device = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .and_then(|inspectable| inspectable.cast::<IDirect3DDevice>())
        .map_err(fail)?;
    Ok((d3d, device))
}

fn next_frame(
    pool: &Direct3D11CaptureFramePool,
) -> Result<windows::Graphics::Capture::Direct3D11CaptureFrame, RecorderError> {
    let start = Instant::now();
    while start.elapsed() < CAPTURE_TIMEOUT {
        if let Ok(frame) = pool.TryGetNextFrame() {
            return Ok(frame);
        }
        thread::sleep(Duration::from_millis(15));
    }
    Err(RecorderError::CaptureTimeout)
}

fn frame_to_rgba(
    d3d: &ID3D11Device,
    frame: &windows::Graphics::Capture::Direct3D11CaptureFrame,
) -> Result<RawImage, RecorderError> {
    let fail = |err: windows::core::Error| RecorderError::capture(err.to_string());
    let access: IDirect3DDxgiInterfaceAccess = frame
        .Surface()
        .and_then(|surface| surface.cast())
        .map_err(fail)?;
    let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(fail)?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;
    let mut staging = None;
    unsafe { d3d.CreateTexture2D(&desc, None, Some(&mut staging)) }.map_err(fail)?;
    let staging =
        staging.ok_or_else(|| RecorderError::capture("staging texture is unavailable"))?;
    let context = unsafe { d3d.GetImmediateContext() }.map_err(fail)?;
    unsafe { context.CopyResource(&staging, &texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.map_err(fail)?;
    let width = desc.Width as usize;
    let height = desc.Height as usize;
    let pitch = mapped.RowPitch as usize;
    let mut rgba = Vec::with_capacity(width * height * 4);
    if !mapped.pData.is_null() && pitch >= width * 4 {
        let source =
            unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, pitch * height) };
        for row in source.chunks_exact(pitch) {
            for bgra in row[..width * 4].chunks_exact(4) {
                rgba.extend_from_slice(&[bgra[2], bgra[1], bgra[0], 255]);
            }
        }
    }
    unsafe { context.Unmap(&staging, 0) };
    RawImage::new(desc.Width, desc.Height, rgba)
}

fn crop_to_window(image: RawImage, focus: &FocusSnapshot) -> Result<RawImage, RecorderError> {
    let monitor = HMONITOR(focus.display_id as isize as *mut _);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..MONITORINFO::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Err(RecorderError::capture("monitor rect is unavailable"));
    }
    let origin_x = focus.rect.x - info.rcMonitor.left;
    let origin_y = focus.rect.y - info.rcMonitor.top;
    crop_rgba(
        &image,
        PixelRect {
            x: origin_x,
            y: origin_y,
            width: focus.rect.width,
            height: focus.rect.height,
        },
    )
}

fn crop_rgba(image: &RawImage, rect: PixelRect) -> Result<RawImage, RecorderError> {
    let x0 = rect.x.max(0) as u32;
    let y0 = rect.y.max(0) as u32;
    let x1 = ((rect.x as i64) + rect.width as i64).clamp(0, image.width as i64) as u32;
    let y1 = ((rect.y as i64) + rect.height as i64).clamp(0, image.height as i64) as u32;
    if x1 <= x0 || y1 <= y0 {
        return Err(RecorderError::capture("window crop is empty"));
    }
    let width = x1 - x0;
    let height = y1 - y0;
    let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for y in y0..y1 {
        let start = ((y * image.width + x0) * 4) as usize;
        let end = start + (width as usize) * 4;
        rgba.extend_from_slice(&image.rgba[start..end]);
    }
    RawImage::new(width, height, rgba)
}

fn walk_window(hwnd: HWND, budget: WalkBudget) -> Result<AxSnapshot, RecorderError> {
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let result = walk_window_com(hwnd, budget);
    unsafe { CoUninitialize() };
    result
}

fn walk_window_com(hwnd: HWND, budget: WalkBudget) -> Result<AxSnapshot, RecorderError> {
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .map_err(|err| RecorderError::AxFailed(err.to_string()))?;
    let root = unsafe { automation.ElementFromHandle(hwnd) }
        .map_err(|err| RecorderError::AxFailed(err.to_string()))?;
    let cache = unsafe { automation.CreateCacheRequest() }
        .map_err(|err| RecorderError::AxFailed(err.to_string()))?;
    for property in [
        UIA_NamePropertyId,
        UIA_ControlTypePropertyId,
        UIA_BoundingRectanglePropertyId,
        UIA_IsPasswordPropertyId,
        UIA_ValueValuePropertyId,
    ] {
        unsafe { cache.AddProperty(property) }
            .map_err(|err| RecorderError::AxFailed(err.to_string()))?;
    }
    unsafe { cache.AddPattern(UIA_ValuePatternId) }.ok();
    unsafe { cache.SetTreeScope(TreeScope(TreeScope_Element.0 | TreeScope_Children.0)) }.ok();
    let walker = unsafe { automation.ControlViewWalker() }
        .map_err(|err| RecorderError::AxFailed(err.to_string()))?;
    let deadline = Instant::now() + AX_TIME_CAP.min(Duration::from_millis(budget.timeout_ms));
    let depth_cap = budget.max_depth.min(AX_DEPTH_CAP);
    let node_cap = budget.max_nodes.min(AX_NODE_CAP);
    let mut segments = Vec::new();
    let mut node_count = 0u32;
    let mut truncated = false;
    let mut chars = 0u32;
    let mut browser_url = None;
    let mut document_path = None;
    visit(
        &walker,
        &cache,
        &root,
        0,
        depth_cap,
        node_cap,
        budget.max_chars,
        deadline,
        &mut segments,
        &mut node_count,
        &mut chars,
        &mut truncated,
        &mut browser_url,
        &mut document_path,
    );
    Ok(AxSnapshot {
        segments,
        browser_url,
        document_path,
        truncated,
        node_count,
    })
}

fn visit(
    walker: &IUIAutomationTreeWalker,
    cache: &IUIAutomationCacheRequest,
    element: &IUIAutomationElement,
    depth: u32,
    depth_cap: u32,
    node_cap: u32,
    char_cap: u32,
    deadline: Instant,
    segments: &mut Vec<TextSegment>,
    node_count: &mut u32,
    chars: &mut u32,
    truncated: &mut bool,
    browser_url: &mut Option<String>,
    document_path: &mut Option<String>,
) {
    if Instant::now() >= deadline || *node_count >= node_cap || depth > depth_cap {
        *truncated = true;
        return;
    }
    *node_count += 1;
    push_segment(
        element,
        segments,
        chars,
        char_cap,
        browser_url,
        document_path,
    );
    if depth == depth_cap {
        *truncated = true;
        return;
    }
    let mut child = unsafe { walker.GetFirstChildElementBuildCache(element, cache) }.ok();
    while let Some(current) = child {
        if Instant::now() >= deadline || *node_count >= node_cap {
            *truncated = true;
            return;
        }
        visit(
            walker,
            cache,
            &current,
            depth + 1,
            depth_cap,
            node_cap,
            char_cap,
            deadline,
            segments,
            node_count,
            chars,
            truncated,
            browser_url,
            document_path,
        );
        child = unsafe { walker.GetNextSiblingElementBuildCache(&current, cache) }.ok();
    }
}

fn push_segment(
    element: &IUIAutomationElement,
    segments: &mut Vec<TextSegment>,
    chars: &mut u32,
    char_cap: u32,
    browser_url: &mut Option<String>,
    document_path: &mut Option<String>,
) {
    let password = unsafe { element.CurrentIsPassword() }
        .unwrap_or(BOOL(0))
        .as_bool();
    let role = control_role(element);
    let value = if password {
        String::new()
    } else {
        element_text(element)
    };
    if role == "document" {
        if value.starts_with("http://") || value.starts_with("https://") {
            *browser_url = Some(value.clone());
        } else if value.contains('\\') || value.contains('/') {
            *document_path = Some(value.clone());
        }
    }
    if value.is_empty() && !password {
        return;
    }
    if *chars >= char_cap {
        return;
    }
    *chars = chars.saturating_add(value.len() as u32);
    segments.push(TextSegment {
        text: value,
        role,
        bbox: element_rect(element),
        source: TextSource::Accessibility,
    });
}

fn element_text(element: &IUIAutomationElement) -> String {
    if let Ok(pattern) =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
    {
        if let Ok(value) = unsafe { pattern.CurrentValue() } {
            let text = value.to_string();
            if !text.is_empty() {
                return text;
            }
        }
    }
    unsafe { element.CurrentName() }
        .map(|name| name.to_string())
        .unwrap_or_default()
}

fn element_rect(element: &IUIAutomationElement) -> Option<PixelRect> {
    let rect = unsafe { element.CurrentBoundingRectangle() }.ok()?;
    let width = rect.right.saturating_sub(rect.left);
    let height = rect.bottom.saturating_sub(rect.top);
    if width <= 0 || height <= 0 {
        return None;
    }
    Some(PixelRect {
        x: rect.left,
        y: rect.top,
        width: width as u32,
        height: height as u32,
    })
}

fn control_role(element: &IUIAutomationElement) -> String {
    match unsafe { element.CurrentControlType() }
        .map(|id| id.0)
        .unwrap_or(0)
    {
        50004 => "edit".into(),
        50005 => "hyperlink".into(),
        50020 => "text".into(),
        50030 => "document".into(),
        50032 => "window".into(),
        other if other == 0 => String::new(),
        other => other.to_string(),
    }
}

fn recognize(image: &RawImage) -> Result<OcrSnapshot, RecorderError> {
    let _ = unsafe { RoInitialize(RO_INIT_SINGLETHREADED) };
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|err| RecorderError::OcrFailed(err.to_string()))?;
    let bitmap = software_bitmap(image)?;
    let op = engine
        .RecognizeAsync(&bitmap)
        .map_err(|err| RecorderError::OcrFailed(err.to_string()))?;
    let result = op
        .join()
        .map_err(|err| RecorderError::OcrFailed(format!("language missing or {err}")))?;
    let lines = result
        .Lines()
        .map_err(|err| RecorderError::OcrFailed(err.to_string()))?;
    let mut segments = Vec::new();
    for line in lines {
        let words = line
            .Words()
            .map_err(|err| RecorderError::OcrFailed(err.to_string()))?;
        for word in words {
            let text = word
                .Text()
                .map(|value| value.to_string())
                .unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            let rect = word.BoundingRect().ok();
            segments.push(TextSegment {
                text,
                role: "ocr".into(),
                bbox: rect.map(pixel_rect),
                source: TextSource::Ocr,
            });
        }
    }
    Ok(OcrSnapshot { segments })
}

fn software_bitmap(image: &RawImage) -> Result<SoftwareBitmap, RecorderError> {
    let bitmap = SoftwareBitmap::CreateWithAlpha(
        BitmapPixelFormat::Rgba8,
        image.width as i32,
        image.height as i32,
        BitmapAlphaMode::Straight,
    )
    .map_err(|err| RecorderError::OcrFailed(err.to_string()))?;
    let buffer = bitmap
        .LockBuffer(BitmapBufferAccessMode::ReadWrite)
        .map_err(|err| RecorderError::OcrFailed(err.to_string()))?;
    let reference: IMemoryBufferReference = buffer
        .CreateReference()
        .map_err(|err| RecorderError::OcrFailed(err.to_string()))?;
    let bytes: IMemoryBufferByteAccess = reference
        .cast()
        .map_err(|err| RecorderError::OcrFailed(err.to_string()))?;
    let mut ptr = std::ptr::null_mut();
    let mut capacity = 0u32;
    unsafe { bytes.GetBuffer(&mut ptr, &mut capacity) }
        .map_err(|err| RecorderError::OcrFailed(err.to_string()))?;
    let plane = buffer
        .GetPlaneDescription(0)
        .map_err(|err| RecorderError::OcrFailed(err.to_string()))?;
    copy_rgba(image, ptr, capacity, plane.StartIndex, plane.Stride);
    let _ = buffer.Close();
    Ok(bitmap)
}

fn copy_rgba(image: &RawImage, ptr: *mut u8, capacity: u32, start: i32, stride: i32) {
    if ptr.is_null() || start < 0 || stride <= 0 {
        return;
    }
    let start = start as usize;
    let stride = stride as usize;
    let row_bytes = (image.width as usize) * 4;
    for y in 0..image.height as usize {
        let src = y * row_bytes;
        let dst = start + y * stride;
        if dst + row_bytes > capacity as usize {
            break;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(image.rgba.as_ptr().add(src), ptr.add(dst), row_bytes);
        }
    }
}

fn pixel_rect(rect: windows::Foundation::Rect) -> PixelRect {
    PixelRect {
        x: rect.X as i32,
        y: rect.Y as i32,
        width: rect.Width.max(0.0) as u32,
        height: rect.Height.max(0.0) as u32,
    }
}

fn hook_thread(tx: Sender<InputBurst>, ready: Sender<Result<u32, RecorderError>>) {
    let thread = unsafe { GetCurrentThreadId() };
    {
        let mut slot = HOOK_TX.lock().unwrap_or_else(|err| err.into_inner());
        *slot = Some(tx);
    }
    let mouse = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0) };
    let key = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_hook), None, 0) };
    let (mouse, key) = match (mouse, key) {
        (Ok(mouse), Ok(key)) => (mouse, key),
        (mouse, key) => {
            if let Ok(hook) = mouse {
                let _ = unsafe { UnhookWindowsHookEx(hook) };
            }
            if let Ok(hook) = key {
                let _ = unsafe { UnhookWindowsHookEx(hook) };
            }
            let _ = ready.send(Err(RecorderError::Message(
                "low-level input hook failed".into(),
            )));
            return;
        }
    };
    {
        let mut mouse_slot = MOUSE_HOOK.lock().unwrap_or_else(|err| err.into_inner());
        *mouse_slot = Some(mouse.0 as isize);
        let mut key_slot = KEY_HOOK.lock().unwrap_or_else(|err| err.into_inner());
        *key_slot = Some(key.0 as isize);
    }
    let _ = ready.send(Ok(thread));
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        if msg.message == STOP_MESSAGE {
            break;
        }
        let _ = unsafe { TranslateMessage(&msg) };
        unsafe { DispatchMessageW(&msg) };
    }
    let _ = unsafe { UnhookWindowsHookEx(mouse) };
    let _ = unsafe { UnhookWindowsHookEx(key) };
    let mut slot = HOOK_TX.lock().unwrap_or_else(|err| err.into_inner());
    *slot = None;
}

unsafe extern "system" fn mouse_hook(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    if code >= 0 {
        let message = wparam.0 as u32;
        if matches!(
            message,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN | WM_MOUSEWHEEL
        ) {
            let burst = if message == WM_MOUSEWHEEL {
                InputBurst::Scroll
            } else {
                InputBurst::Click
            };
            send_burst(burst);
        }
        let _ = lparam.0 as *const MSLLHOOKSTRUCT;
    }
    let hook = hook_handle(&MOUSE_HOOK);
    unsafe { CallNextHookEx(hook, code, wparam, lparam) }
}

unsafe extern "system" fn key_hook(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    if code >= 0 && matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if !info
            .flags
            .contains(windows::Win32::UI::WindowsAndMessaging::LLKHF_UP)
        {
            send_burst(InputBurst::Key);
        }
    }
    let hook = hook_handle(&KEY_HOOK);
    unsafe { CallNextHookEx(hook, code, wparam, lparam) }
}

fn hook_handle(slot: &Mutex<Option<isize>>) -> Option<HHOOK> {
    let value = slot.lock().ok()?.as_ref().copied()?;
    Some(HHOOK(value as *mut _))
}

fn send_burst(burst: InputBurst) {
    let guard = HOOK_TX.lock().unwrap_or_else(|err| err.into_inner());
    if let Some(tx) = guard.as_ref() {
        let _ = tx.send(burst);
    }
}

fn access_denied(err: &RecorderError) -> bool {
    err.to_string().to_ascii_lowercase().contains(ACCESS_DENIED)
}

fn feature_or_capture(err: windows::core::Error) -> RecorderError {
    RecorderError::capture(err.to_string())
}
