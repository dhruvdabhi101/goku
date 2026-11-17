use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::commit::{plan_frame, CaptureFacts, FramePlan};
use crate::error::RecorderError;
use crate::filter::SkipReason;
use crate::imagebuf::{encode_jpeg, RawImage};
use crate::platform::{self, resolve_scope, InputBurst, InputWatch, Platform};
use crate::redact::redact_text;
use crate::store::ReadStore;
use crate::store::Store;
use crate::triggers::consider;
use crate::types::{
    Config, Degradation, DiagnoseReport, FocusSnapshot, Frame, FrameId, FrameSummary,
    PermissionKind, PermissionReport, PermissionState, PixelPolicy, PixelRect, RecentFramesQuery,
    RecorderEvent, SecretHit, Status, TextMode, TextSegment, TriggerKind, WalkBudget,
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static SLOT: Mutex<Option<u64>> = Mutex::new(None);

const COMMAND_WAIT: Duration = Duration::from_millis(50);
const POLL_EVERY: Duration = Duration::from_millis(200);
const FOCUS_SETTLE: Duration = Duration::from_millis(150);
const CLICK_SETTLE: Duration = Duration::from_millis(80);
const THIN_TEXT_CHARS: usize = 16;

pub struct Recorder {
    id: u64,
    config: Config,
    gate: Mutex<Life>,
    gate_cv: Condvar,
    degradations: Arc<Mutex<Vec<Degradation>>>,
    paused: Arc<AtomicBool>,
    subscribers: Arc<Mutex<Vec<SyncSender<RecorderEvent>>>>,
}

enum Life {
    Idle,
    Starting,
    StopRequested,
    Running {
        tx: SyncSender<Command>,
        join: Option<JoinHandle<()>>,
    },
}

enum Command {
    Capture {
        trigger: TriggerKind,
        reply: SyncSender<Result<FrameId, RecorderError>>,
    },
    Permissions {
        reply: SyncSender<PermissionReport>,
    },
    Request {
        kind: PermissionKind,
        reply: SyncSender<Result<(), RecorderError>>,
    },
    OpenSettings {
        kind: PermissionKind,
        reply: SyncSender<Result<(), RecorderError>>,
    },
    Stop,
}

impl Recorder {
    pub fn new(config: Config) -> Result<Self, RecorderError> {
        Ok(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            config,
            gate: Mutex::new(Life::Idle),
            gate_cv: Condvar::new(),
            degradations: Arc::new(Mutex::new(Vec::new())),
            paused: Arc::new(AtomicBool::new(false)),
            subscribers: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn start(&self) -> Result<(), RecorderError> {
        {
            let mut life = lock(&self.gate);
            if !matches!(*life, Life::Idle) {
                return Err(RecorderError::AlreadyRunning);
            }
            let mut slot = lock(&SLOT);
            if slot.is_some() {
                return Err(RecorderError::AlreadyRunning);
            }
            *slot = Some(self.id);
            *life = Life::Starting;
        }
        let spawned = self.spawn_worker();
        let mut life = lock(&self.gate);
        match (&*life, spawned) {
            (Life::Starting, Ok((tx, handle))) => {
                *life = Life::Running {
                    tx,
                    join: Some(handle),
                };
                Ok(())
            }
            (Life::StopRequested, Ok((tx, handle))) => {
                drop(life);
                let _ = tx.send(Command::Stop);
                let _ = handle.join();
                self.release_slot();
                *lock(&self.gate) = Life::Idle;
                self.gate_cv.notify_all();
                Err(RecorderError::NotRunning)
            }
            (_, Err(err)) => {
                self.release_slot();
                *life = Life::Idle;
                self.gate_cv.notify_all();
                Err(err)
            }
            _ => {
                self.release_slot();
                *life = Life::Idle;
                self.gate_cv.notify_all();
                Err(RecorderError::AlreadyRunning)
            }
        }
    }

    pub fn stop(&self) -> Result<(), RecorderError> {
        let mut life = lock(&self.gate);
        match std::mem::replace(&mut *life, Life::Idle) {
            Life::Idle => Err(RecorderError::NotRunning),
            Life::Starting => {
                *life = Life::StopRequested;
                loop {
                    life = self
                        .gate_cv
                        .wait(life)
                        .unwrap_or_else(|err| err.into_inner());
                    if matches!(*life, Life::Idle) {
                        break;
                    }
                }
                Ok(())
            }
            Life::StopRequested => {
                *life = Life::StopRequested;
                Ok(())
            }
            Life::Running { tx, join } => {
                drop(life);
                let _ = tx.send(Command::Stop);
                if let Some(handle) = join {
                    let _ = handle.join();
                }
                self.release_slot();
                self.paused.store(false, Ordering::Release);
                self.emit(RecorderEvent::Stopped);
                Ok(())
            }
        }
    }

    pub fn pause(&self) -> Result<(), RecorderError> {
        self.require_running()?;
        self.paused.store(true, Ordering::Release);
        self.emit(RecorderEvent::Paused);
        Ok(())
    }

    pub fn resume(&self) -> Result<(), RecorderError> {
        self.require_running()?;
        self.paused.store(false, Ordering::Release);
        self.emit(RecorderEvent::Resumed);
        Ok(())
    }

    pub fn capture_now(&self) -> Result<FrameId, RecorderError> {
        let tx = self.command_tx()?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        tx.send(Command::Capture {
            trigger: TriggerKind::Manual,
            reply: reply_tx,
        })
        .map_err(|_| RecorderError::NotRunning)?;
        match reply_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(RecorderError::CaptureTimeout),
            Err(RecvTimeoutError::Disconnected) => Err(RecorderError::NotRunning),
        }
    }

    pub fn status(&self) -> Result<Status, RecorderError> {
        let (frame_count, session_id, last_trigger) = match ReadStore::open(&self.config.data_dir) {
            Ok(read) => read.summary()?,
            Err(_) => (0, None, None),
        };
        Ok(Status {
            running: self.is_running(),
            paused: self.paused.load(Ordering::Acquire),
            session_id,
            frame_count,
            last_trigger,
            degradations: self.degradations(),
            os: std::env::consts::OS.to_string(),
        })
    }

    pub fn recent_frames(
        &self,
        query: RecentFramesQuery,
    ) -> Result<Vec<FrameSummary>, RecorderError> {
        match ReadStore::open(&self.config.data_dir) {
            Ok(read) => read.recent(&query),
            Err(_) => Ok(Vec::new()),
        }
    }

    pub fn frame(&self, id: FrameId) -> Result<Frame, RecorderError> {
        ReadStore::open(&self.config.data_dir)
            .map_err(|_| RecorderError::StoreFailed("no database".into()))?
            .frame(id)
    }

    pub fn frame_image(&self, id: FrameId) -> Result<Vec<u8>, RecorderError> {
        let read = ReadStore::open(&self.config.data_dir)
            .map_err(|_| RecorderError::StoreFailed("no database".into()))?;
        read.image_bytes(id)
    }

    pub fn diagnose(&self) -> Result<DiagnoseReport, RecorderError> {
        let permissions = self.permissions()?;
        let probe = self.config.data_dir.as_path().join(".diag");
        let data_dir_writable = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&probe)
            .map(|_file| std::fs::remove_file(&probe).is_ok())
            .unwrap_or(false);
        let sample = redact_text("sk-abcdefghijklmnopqrstuvwxyz012345");
        Ok(DiagnoseReport {
            permissions,
            degradations: self.degradations(),
            data_dir_writable,
            redaction_self_test: !sample.text.contains("sk-abcdefghijklmnopqrstuvwxyz012345"),
            os: std::env::consts::OS.to_string(),
            running: self.is_running(),
            paused: self.paused.load(Ordering::Acquire),
        })
    }

    pub fn subscribe(&self) -> Receiver<RecorderEvent> {
        let (tx, rx) = mpsc::sync_channel(32);
        lock(&self.subscribers).push(tx);
        rx
    }

    pub fn permissions(&self) -> Result<PermissionReport, RecorderError> {
        if let Ok(tx) = self.command_tx() {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            tx.send(Command::Permissions { reply: reply_tx })
                .map_err(|_| RecorderError::NotRunning)?;
            return reply_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| RecorderError::CaptureTimeout);
        }
        Ok(platform::connect()?.permissions())
    }

    pub fn request_permission(&self, kind: PermissionKind) -> Result<(), RecorderError> {
        self.platform_call(
            |platform| platform.request(kind),
            |tx, reply| Command::Request { kind, reply },
        )
    }

    pub fn open_settings(&self, kind: PermissionKind) -> Result<(), RecorderError> {
        self.platform_call(
            |platform| platform.open_settings(kind),
            |tx, reply| {
                let _ = tx;
                Command::OpenSettings { kind, reply }
            },
        )
    }

    pub fn degradations(&self) -> Vec<Degradation> {
        lock(&self.degradations).clone()
    }

    fn spawn_worker(&self) -> Result<(SyncSender<Command>, JoinHandle<()>), RecorderError> {
        let platform = platform::connect()?;
        let store = Store::open(self.config.data_dir.clone())?;
        store.retain(self.config.max_age_days, self.config.max_bytes)?;
        let (tx, rx) = mpsc::sync_channel(16);
        let config = self.config.clone();
        let degradations = Arc::clone(&self.degradations);
        let paused = Arc::clone(&self.paused);
        let subscribers = Arc::clone(&self.subscribers);
        lock(&self.degradations).clear();
        self.paused.store(false, Ordering::Release);
        let handle = thread::Builder::new()
            .name("goku-recorder".into())
            .spawn(move || {
                worker_loop(
                    platform,
                    store,
                    config,
                    rx,
                    degradations,
                    paused,
                    subscribers,
                )
            })
            .map_err(|err| RecorderError::Message(err.to_string()))?;
        Ok((tx, handle))
    }

    fn command_tx(&self) -> Result<SyncSender<Command>, RecorderError> {
        let life = lock(&self.gate);
        match &*life {
            Life::Running { tx, .. } => Ok(tx.clone()),
            _ => Err(RecorderError::NotRunning),
        }
    }

    fn is_running(&self) -> bool {
        matches!(*lock(&self.gate), Life::Running { .. })
    }

    fn require_running(&self) -> Result<(), RecorderError> {
        if self.is_running() {
            Ok(())
        } else {
            Err(RecorderError::NotRunning)
        }
    }

    fn release_slot(&self) {
        let mut slot = lock(&SLOT);
        if *slot == Some(self.id) {
            *slot = None;
        }
    }

    fn emit(&self, event: RecorderEvent) {
        let mut subs = lock(&self.subscribers);
        subs.retain(|tx| match tx.try_send(event.clone()) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => true,
            Err(mpsc::TrySendError::Disconnected(_)) => false,
        });
    }

    fn platform_call(
        &self,
        idle: impl FnOnce(&dyn Platform) -> Result<(), RecorderError>,
        running: impl FnOnce(SyncSender<Command>, SyncSender<Result<(), RecorderError>>) -> Command,
    ) -> Result<(), RecorderError> {
        if let Ok(tx) = self.command_tx() {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            tx.send(running(tx.clone(), reply_tx))
                .map_err(|_| RecorderError::NotRunning)?;
            return reply_rx
                .recv_timeout(Duration::from_secs(15))
                .map_err(|_| RecorderError::CaptureTimeout)?;
        }
        idle(platform::connect()?.as_ref())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

fn worker_loop(
    platform: Box<dyn Platform>,
    store: Store,
    config: Config,
    rx: Receiver<Command>,
    degradations: Arc<Mutex<Vec<Degradation>>>,
    paused: Arc<AtomicBool>,
    subscribers: Arc<Mutex<Vec<SyncSender<RecorderEvent>>>>,
) {
    let mut worker = Worker::new(platform, store, config, degradations, paused, subscribers);
    loop {
        match rx.recv_timeout(COMMAND_WAIT) {
            Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Command::Capture { trigger, reply }) => {
                let result = worker.capture(trigger);
                if let Ok(id) = &result {
                    worker.emit(RecorderEvent::Frame {
                        id: id.0,
                        trigger: trigger.as_str().to_string(),
                        image_status: "stored".into(),
                    });
                }
                let _ = reply.send(result);
            }
            Ok(Command::Permissions { reply }) => {
                let report = worker.platform.permissions();
                let _ = reply.send(report);
            }
            Ok(Command::Request { kind, reply }) => {
                let _ = reply.send(worker.platform.request(kind));
            }
            Ok(Command::OpenSettings { kind, reply }) => {
                let _ = reply.send(worker.platform.open_settings(kind));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if worker.paused.load(Ordering::Acquire) {
            continue;
        }
        worker.drain_input();
        worker.poll_focus();
        worker.fire_settled();
    }
    if let Some(mut watch) = worker.watch.take() {
        watch.stop();
    }
    let _ = worker.store.finish();
}

struct Armed {
    kind: TriggerKind,
    at: Instant,
}

struct Worker {
    platform: Box<dyn Platform>,
    store: Store,
    config: Config,
    degradations: Arc<Mutex<Vec<Degradation>>>,
    paused: Arc<AtomicBool>,
    subscribers: Arc<Mutex<Vec<SyncSender<RecorderEvent>>>>,
    watch: Option<Box<dyn InputWatch>>,
    input_rx: Option<Receiver<InputBurst>>,
    pending: Option<TriggerKind>,
    last_fire: Option<Instant>,
    last_focus: Option<crate::types::FocusIdentity>,
    last_focus_poll: Instant,
    last_activity: Instant,
    click_at: Option<Instant>,
    key_at: Option<Instant>,
    scroll_at: Option<Instant>,
    focus_armed: Option<Armed>,
}

impl Worker {
    fn new(
        platform: Box<dyn Platform>,
        store: Store,
        config: Config,
        degradations: Arc<Mutex<Vec<Degradation>>>,
        paused: Arc<AtomicBool>,
        subscribers: Arc<Mutex<Vec<SyncSender<RecorderEvent>>>>,
    ) -> Self {
        let (watch, input_rx) = open_watch(platform.as_ref(), &config, &degradations);
        let now = Instant::now();
        Self {
            platform,
            store,
            config,
            degradations,
            paused,
            subscribers,
            watch,
            input_rx,
            pending: None,
            last_fire: None,
            last_focus: None,
            last_focus_poll: now,
            last_activity: now,
            click_at: None,
            key_at: None,
            scroll_at: None,
            focus_armed: None,
        }
    }

    fn capture(&mut self, trigger: TriggerKind) -> Result<FrameId, RecorderError> {
        let mut found = Vec::new();
        let result = run_capture(
            self.platform.as_ref(),
            &self.store,
            &self.config,
            trigger,
            &mut found,
        );
        let mut slot = lock(&self.degradations);
        for item in found {
            if !slot.contains(&item) {
                slot.push(item);
            }
        }
        result
    }

    fn emit(&self, event: RecorderEvent) {
        let mut subs = lock(&self.subscribers);
        subs.retain(|tx| match tx.try_send(event.clone()) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => true,
            Err(mpsc::TrySendError::Disconnected(_)) => false,
        });
    }

    fn note(&mut self, kind: TriggerKind) {
        let elapsed = self
            .last_fire
            .map(|at| at.elapsed().as_millis() as u64)
            .unwrap_or(u64::MAX);
        let decision = consider(
            self.pending,
            kind,
            elapsed,
            self.config.triggers.min_interval_ms,
        );
        self.pending = decision.pending;
        if let Some(kind) = decision.fire {
            self.last_fire = Some(Instant::now());
            self.pending = None;
            let _ = self.capture(kind);
        }
    }

    fn drain_input(&mut self) {
        let Some(rx) = self.input_rx.as_ref() else {
            return;
        };
        while let Ok(burst) = rx.try_recv() {
            self.last_activity = Instant::now();
            match burst {
                InputBurst::Click if self.config.triggers.click => {
                    self.click_at = Some(Instant::now());
                }
                InputBurst::Key if self.config.triggers.typing_quiet_ms.is_some() => {
                    self.key_at = Some(Instant::now());
                }
                InputBurst::Scroll if self.config.triggers.scroll_quiet_ms.is_some() => {
                    self.scroll_at = Some(Instant::now());
                }
                _ => {}
            }
        }
    }

    fn poll_focus(&mut self) {
        if !self.config.triggers.app_switch && !self.config.triggers.window_focus {
            return;
        }
        if self.last_focus_poll.elapsed() < POLL_EVERY {
            return;
        }
        self.last_focus_poll = Instant::now();
        let Ok(focus) = self.platform.focus() else {
            return;
        };
        let next = focus.identity;
        if let Some(prev) = self.last_focus.clone() {
            let kind = if prev.pid != next.pid && self.config.triggers.app_switch {
                Some(TriggerKind::AppSwitch)
            } else if self.config.triggers.window_focus && !prev.same_window(&next) {
                Some(TriggerKind::WindowFocus)
            } else {
                None
            };
            if let Some(kind) = kind {
                self.last_activity = Instant::now();
                self.focus_armed = Some(Armed {
                    kind,
                    at: Instant::now() + FOCUS_SETTLE,
                });
            }
        }
        self.last_focus = Some(next);
    }

    fn fire_settled(&mut self) {
        if self.click_at.is_some_and(|at| at.elapsed() >= CLICK_SETTLE) {
            self.click_at = None;
            self.note(TriggerKind::Click);
        }
        if let Some(quiet) = self.config.triggers.typing_quiet_ms {
            if self
                .key_at
                .is_some_and(|at| at.elapsed() >= Duration::from_millis(quiet))
            {
                self.key_at = None;
                self.note(TriggerKind::TypingPause);
            }
        }
        if let Some(quiet) = self.config.triggers.scroll_quiet_ms {
            if self
                .scroll_at
                .is_some_and(|at| at.elapsed() >= Duration::from_millis(quiet))
            {
                self.scroll_at = None;
                self.note(TriggerKind::ScrollStop);
            }
        }
        if self
            .focus_armed
            .as_ref()
            .is_some_and(|armed| armed.at <= Instant::now())
        {
            let kind = self.focus_armed.take().map(|armed| armed.kind);
            if let Some(kind) = kind {
                self.note(kind);
            }
        }
        if let Some(idle_ms) = self.config.triggers.idle_ms {
            let since_fire = self
                .last_fire
                .map(|at| at.elapsed().as_millis() as u64)
                .unwrap_or(u64::MAX);
            if self.last_activity.elapsed() >= Duration::from_millis(idle_ms)
                && since_fire >= idle_ms
            {
                self.note(TriggerKind::Idle);
            }
        }
    }
}

fn open_watch(
    platform: &dyn Platform,
    config: &Config,
    degradations: &Mutex<Vec<Degradation>>,
) -> (Option<Box<dyn InputWatch>>, Option<Receiver<InputBurst>>) {
    let want = config.triggers.click
        || config.triggers.typing_quiet_ms.is_some()
        || config.triggers.scroll_quiet_ms.is_some();
    if !want {
        return (None, None);
    }
    let (raw_tx, raw_rx) = mpsc::channel();
    let (bounded_tx, bounded_rx) = mpsc::sync_channel(8);
    thread::Builder::new()
        .name("goku-input".into())
        .spawn(move || {
            while let Ok(burst) = raw_rx.recv() {
                match bounded_tx.try_send(burst) {
                    Ok(()) => {}
                    Err(mpsc::TrySendError::Full(_)) => {}
                    Err(mpsc::TrySendError::Disconnected(_)) => break,
                }
            }
        })
        .map_err(|err| RecorderError::Message(err.to_string()))
        .ok();
    match platform.try_watch_input(raw_tx) {
        Ok(watch) => (Some(watch), Some(bounded_rx)),
        Err(_) => {
            let mut slot = lock(degradations);
            if !slot.contains(&Degradation::InputHooksUnavailable) {
                slot.push(Degradation::InputHooksUnavailable);
            }
            (None, None)
        }
    }
}

struct Extracted {
    browser_url: Option<String>,
    text: String,
    hits: Vec<SecretHit>,
    labels: Vec<String>,
    forced: Vec<PixelRect>,
    reliable: bool,
}

fn run_capture(
    platform: &dyn Platform,
    store: &Store,
    config: &Config,
    trigger: TriggerKind,
    degradations: &mut Vec<Degradation>,
) -> Result<FrameId, RecorderError> {
    let before = platform.focus()?;
    let (scope, fell_back) = resolve_scope(config.capture.scope, platform.supported_scopes());
    if fell_back {
        degradations.push(Degradation::ScopeFallback {
            requested: config.capture.scope,
            used: scope,
        });
    }
    if config.exclude_own_app && before.identity.pid == std::process::id() {
        return Err(RecorderError::Skipped(SkipReason::OwnApp));
    }
    config
        .filter
        .check_window(&before)
        .map_err(RecorderError::Skipped)?;
    let secure = config.pause_on_secure_input && platform.secure_input();
    let mut permission_missing = platform.permissions().screen == PermissionState::Denied;
    let mut raw = None;
    let mut pixels_ok = false;
    if !secure && !permission_missing {
        match platform.capture(&before, scope, &config.capture) {
            Ok(image) => {
                raw = Some(image);
                pixels_ok = true;
            }
            Err(RecorderError::PermissionMissing) => permission_missing = true,
            Err(_) => {}
        }
    }
    let extracted = if secure {
        Extracted {
            browser_url: None,
            text: String::new(),
            hits: Vec::new(),
            labels: Vec::new(),
            forced: Vec::new(),
            reliable: true,
        }
    } else {
        extract_text(platform, config, &before, raw.as_ref(), degradations)
    };
    config
        .filter
        .check_url(extracted.browser_url.as_deref())
        .map_err(RecorderError::Skipped)?;
    let after = platform.focus()?;
    if !same_capture(&before, &after) {
        return Err(RecorderError::capture("focus changed during capture"));
    }
    let mut hits = extracted.hits;
    let mut labels = extracted.labels;
    if !extracted.reliable
        && extracted.text.trim().is_empty()
        && matches!(config.pixels, PixelPolicy::SecretRegions { .. })
    {
        hits.push(SecretHit {
            label: "UNVERIFIED".into(),
            start: 0,
            end: 0,
            region: None,
        });
        if !labels.iter().any(|label| label == "UNVERIFIED") {
            labels.push("UNVERIFIED".into());
        }
    }
    let facts = CaptureFacts {
        before: before.identity.clone(),
        after: after.identity.clone(),
        pixels_ok,
        denied: false,
        secure_input: secure,
        permission_missing,
        text: extracted.text,
        hits,
        labels,
    };
    let plan = plan_frame(&facts, raw, config.pixels, &extracted.forced)?;
    if matches!(plan, FramePlan::Discard { .. }) {
        return Err(RecorderError::capture("focus changed during capture"));
    }
    let jpeg = match &plan {
        FramePlan::Row {
            image: Some(image), ..
        } => encode_jpeg(image, config.capture.jpeg_quality)?,
        _ => Vec::new(),
    };
    store.commit(&plan, trigger, &after, &jpeg)
}

fn extract_text(
    platform: &dyn Platform,
    config: &Config,
    focus: &FocusSnapshot,
    raw: Option<&RawImage>,
    degradations: &mut Vec<Degradation>,
) -> Extracted {
    let mut segments = Vec::new();
    let mut browser_url = None;
    let mut document_path = None;
    let mut ax_ok = false;
    let mut ocr_ok = false;
    if config.text_mode != TextMode::OcrOnly {
        match platform.accessibility(focus, WalkBudget::default()) {
            Ok(ax) => {
                ax_ok = true;
                browser_url = ax.browser_url;
                document_path = ax.document_path;
                segments = ax.segments;
            }
            Err(_) => push_deg(degradations, Degradation::AccessibilityUnavailable),
        }
    }
    let ax_chars = non_ws_chars(&segments);
    if needs_ocr(config.text_mode, ax_ok, ax_chars) {
        if let Some(image) = raw {
            match platform.ocr(image) {
                Ok(ocr) => {
                    ocr_ok = true;
                    segments.extend(ocr.segments);
                }
                Err(_) => push_deg(degradations, Degradation::OcrUnavailable),
            }
        }
    }
    let (flat, pieces) = join_text(&segments);
    let mut redacted = redact_text(&flat);
    attach_regions(&flat, &mut redacted.hits, &segments, &pieces);
    merge_extra(
        &focus.window_title,
        &mut redacted.hits,
        &mut redacted.labels,
    );
    if let Some(url) = browser_url.as_deref() {
        merge_extra(url, &mut redacted.hits, &mut redacted.labels);
    }
    if let Some(path) = document_path.as_deref() {
        merge_extra(path, &mut redacted.hits, &mut redacted.labels);
    }
    if password_missing_box(&segments) {
        redacted.hits.push(SecretHit {
            label: "PASSWORD".into(),
            start: 0,
            end: 0,
            region: None,
        });
        if !redacted.labels.iter().any(|label| label == "PASSWORD") {
            redacted.labels.push("PASSWORD".into());
        }
    }
    let attempted_ocr = needs_ocr(config.text_mode, ax_ok, non_ws_chars(&segments));
    let reliable = match config.text_mode {
        TextMode::OcrOnly => ocr_ok,
        TextMode::Both => ax_ok || ocr_ok,
        TextMode::AccessibilityFirst => {
            ax_ok || (attempted_ocr && ocr_ok) || (ax_ok && !attempted_ocr)
        }
    };
    Extracted {
        browser_url,
        text: redacted.text,
        hits: redacted.hits,
        labels: redacted.labels,
        forced: password_rects(&segments),
        reliable: if config.text_mode == TextMode::AccessibilityFirst {
            ax_ok || ocr_ok
        } else {
            reliable
        },
    }
}

fn push_deg(list: &mut Vec<Degradation>, item: Degradation) {
    if !list.contains(&item) {
        list.push(item);
    }
}

fn same_capture(before: &FocusSnapshot, after: &FocusSnapshot) -> bool {
    before.identity.same_window(&after.identity)
        && before.window_title == after.window_title
        && before.rect == after.rect
}

fn needs_ocr(mode: TextMode, ax_ok: bool, ax_chars: usize) -> bool {
    match mode {
        TextMode::OcrOnly | TextMode::Both => true,
        TextMode::AccessibilityFirst => !ax_ok || ax_chars < THIN_TEXT_CHARS,
    }
}

fn non_ws_chars(segments: &[TextSegment]) -> usize {
    segments
        .iter()
        .map(|seg| seg.text.chars().filter(|ch| !ch.is_whitespace()).count())
        .sum()
}

struct Piece {
    start: usize,
    end: usize,
    segment: usize,
}

fn join_text(segments: &[TextSegment]) -> (String, Vec<Piece>) {
    let mut out = String::new();
    let mut pieces = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let start = out.len();
        out.push_str(&seg.text);
        pieces.push(Piece {
            start,
            end: out.len(),
            segment: i,
        });
    }
    (out, pieces)
}

fn attach_regions(
    source: &str,
    hits: &mut [SecretHit],
    segments: &[TextSegment],
    pieces: &[Piece],
) {
    for hit in hits {
        if source.get(hit.start..hit.end).is_none() || hit.start >= hit.end {
            hit.region = None;
            continue;
        }
        let mut union = None;
        let mut saw = false;
        let mut missing = false;
        for piece in pieces {
            if piece.start >= hit.end || hit.start >= piece.end {
                continue;
            }
            saw = true;
            match segments[piece.segment].bbox.filter(|rect| !rect.is_empty()) {
                Some(rect) => union = Some(union_rect(union, rect)),
                None => missing = true,
            }
        }
        hit.region = if !saw || missing { None } else { union };
    }
}

fn union_rect(acc: Option<PixelRect>, next: PixelRect) -> PixelRect {
    let Some(acc) = acc else {
        return next;
    };
    let x0 = acc.x.min(next.x);
    let y0 = acc.y.min(next.y);
    let x1 =
        (i64::from(acc.x) + i64::from(acc.width)).max(i64::from(next.x) + i64::from(next.width));
    let y1 =
        (i64::from(acc.y) + i64::from(acc.height)).max(i64::from(next.y) + i64::from(next.height));
    PixelRect {
        x: x0,
        y: y0,
        width: (x1 - i64::from(x0)).max(0) as u32,
        height: (y1 - i64::from(y0)).max(0) as u32,
    }
}

fn password_missing_box(segments: &[TextSegment]) -> bool {
    segments.iter().any(|seg| {
        let role = seg.role.to_ascii_lowercase();
        (role.contains("secure") || role.contains("password"))
            && seg.bbox.filter(|rect| !rect.is_empty()).is_none()
    })
}

fn merge_extra(source: &str, hits: &mut Vec<SecretHit>, labels: &mut Vec<String>) {
    if source.is_empty() {
        return;
    }
    let extra = redact_text(source);
    for mut hit in extra.hits {
        hit.region = None;
        if !labels.contains(&hit.label) {
            labels.push(hit.label.clone());
        }
        hits.push(hit);
    }
}

fn password_rects(segments: &[TextSegment]) -> Vec<PixelRect> {
    segments
        .iter()
        .filter_map(|seg| {
            let role = seg.role.to_ascii_lowercase();
            if role.contains("secure") || role.contains("password") {
                seg.bbox.filter(|rect| !rect.is_empty())
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::mpsc::Sender;

    use crate::imagebuf::RawImage;
    use crate::platform::{InputBurst, InputWatch};
    use crate::types::{
        AxSnapshot, CaptureOptions, CaptureScope, FocusIdentity, ImageStatus, OcrSnapshot,
        OnUnmappable, PermissionKind, PermissionReport, PermissionState, PixelPolicy, PixelRect,
        TextSource, WindowRect,
    };

    const KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz012345";

    struct FakeWatch;
    impl InputWatch for FakeWatch {
        fn stop(&mut self) {}
    }

    struct FakePlatform {
        scopes: Vec<CaptureScope>,
        focuses: Vec<FocusSnapshot>,
        focus_calls: AtomicUsize,
        capture_calls: AtomicUsize,
        used_scope: Mutex<Option<CaptureScope>>,
        ax: Mutex<Result<AxSnapshot, RecorderError>>,
        ocr: Mutex<Result<OcrSnapshot, RecorderError>>,
        secure: bool,
        screen: PermissionState,
        hooks_ok: bool,
    }

    impl FakePlatform {
        fn stable(focus: FocusSnapshot, ax: AxSnapshot) -> Self {
            Self {
                scopes: vec![
                    CaptureScope::FocusedApp,
                    CaptureScope::FocusedWindow,
                    CaptureScope::DisplayCrop,
                ],
                focuses: vec![focus],
                focus_calls: AtomicUsize::new(0),
                capture_calls: AtomicUsize::new(0),
                used_scope: Mutex::new(None),
                ax: Mutex::new(Ok(ax)),
                ocr: Mutex::new(Err(RecorderError::OcrFailed("not used".into()))),
                secure: false,
                screen: PermissionState::Granted,
                hooks_ok: false,
            }
        }
    }

    impl Platform for FakePlatform {
        fn os_name(&self) -> &'static str {
            "test"
        }
        fn supported_scopes(&self) -> &[CaptureScope] {
            &self.scopes
        }
        fn permissions(&self) -> PermissionReport {
            PermissionReport {
                screen: self.screen,
                accessibility: PermissionState::Granted,
                input: PermissionState::Granted,
                can_record: self.screen != PermissionState::Denied,
                degradations: vec![],
                restart_required: false,
            }
        }
        fn request(&self, _kind: PermissionKind) -> Result<(), RecorderError> {
            Ok(())
        }
        fn open_settings(&self, _kind: PermissionKind) -> Result<(), RecorderError> {
            Ok(())
        }
        fn focus(&self) -> Result<FocusSnapshot, RecorderError> {
            let n = self.focus_calls.fetch_add(1, AtomicOrdering::Relaxed);
            self.focuses
                .get(n.min(self.focuses.len() - 1))
                .cloned()
                .ok_or(RecorderError::NoFocusedWindow)
        }
        fn capture(
            &self,
            _focus: &FocusSnapshot,
            scope: CaptureScope,
            _opts: &CaptureOptions,
        ) -> Result<RawImage, RecorderError> {
            self.capture_calls.fetch_add(1, AtomicOrdering::Relaxed);
            *self.used_scope.lock().unwrap() = Some(scope);
            RawImage::new(2, 2, [9u8, 9, 9, 255].repeat(4))
        }
        fn accessibility(
            &self,
            _focus: &FocusSnapshot,
            _budget: WalkBudget,
        ) -> Result<AxSnapshot, RecorderError> {
            self.ax.lock().unwrap().clone()
        }
        fn ocr(&self, _image: &RawImage) -> Result<OcrSnapshot, RecorderError> {
            self.ocr.lock().unwrap().clone()
        }
        fn secure_input(&self) -> bool {
            self.secure
        }
        fn try_watch_input(
            &self,
            _tx: Sender<InputBurst>,
        ) -> Result<Box<dyn InputWatch>, RecorderError> {
            if self.hooks_ok {
                Ok(Box::new(FakeWatch))
            } else {
                Err(RecorderError::Message("hooks unavailable".into()))
            }
        }
    }

    fn temp_config() -> (std::path::PathBuf, Config) {
        static N: AtomicUsize = AtomicUsize::new(1);
        let n = N.fetch_add(1, AtomicOrdering::Relaxed);
        let path = std::env::temp_dir().join(format!("goku-engine-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let mut config = Config::new(&path).unwrap();
        config.pixels = PixelPolicy::SecretRegions {
            on_unmappable: OnUnmappable::WithholdImage,
        };
        (path, config)
    }

    fn focus(window: u64, app: &str, title: &str) -> FocusSnapshot {
        FocusSnapshot {
            identity: FocusIdentity {
                pid: 42,
                window_id: window,
                app_id: "com.example.app".into(),
            },
            app_name: app.into(),
            window_title: title.into(),
            rect: WindowRect {
                x: 0,
                y: 0,
                width: 80,
                height: 40,
                scale: 2,
            },
            display_id: 1,
        }
    }

    fn seg(text: &str, bbox: Option<PixelRect>) -> TextSegment {
        TextSegment {
            text: text.into(),
            role: "AXStaticText".into(),
            bbox,
            source: TextSource::Accessibility,
        }
    }

    fn ax(segments: Vec<TextSegment>) -> AxSnapshot {
        AxSnapshot {
            segments,
            browser_url: None,
            document_path: None,
            truncated: false,
            node_count: 1,
        }
    }

    fn box_at() -> PixelRect {
        PixelRect {
            x: 1,
            y: 2,
            width: 8,
            height: 4,
        }
    }

    #[test]
    fn attach_regions_maps_each_span_to_its_own_segment() {
        let secret = KEY;
        let first = box_at();
        let second = PixelRect {
            x: 40,
            y: 8,
            width: 12,
            height: 6,
        };
        let segments = vec![
            seg(&format!("left {secret}"), Some(first)),
            seg(&format!("right {secret}"), Some(second)),
        ];
        let (joined, pieces) = join_text(&segments);
        let mut hits = redact_text(&joined).hits;
        attach_regions(&joined, &mut hits, &segments, &pieces);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].region, Some(first));
        assert_eq!(hits[1].region, Some(second));

        let mut spanning = hits[0].clone();
        spanning.start = 0;
        spanning.end = joined.len();
        attach_regions(
            &joined,
            std::slice::from_mut(&mut spanning),
            &segments,
            &pieces,
        );
        assert!(spanning.region.unwrap().width > first.width);

        let mut unboxed = segments.clone();
        unboxed[1].bbox = None;
        let (joined, pieces) = join_text(&unboxed);
        let mut hits = redact_text(&joined).hits;
        let mut across = hits[0].clone();
        across.start = 0;
        across.end = joined.len();
        attach_regions(
            &joined,
            std::slice::from_mut(&mut across),
            &unboxed,
            &pieces,
        );
        assert!(across.region.is_none());
        let _ = hits;
    }

    #[test]
    fn ocr_runs_when_accessibility_is_missing_or_thin_and_always_for_both() {
        assert!(needs_ocr(TextMode::AccessibilityFirst, false, 100));
        assert!(needs_ocr(TextMode::AccessibilityFirst, true, 4));
        assert!(!needs_ocr(TextMode::AccessibilityFirst, true, 40));
        assert!(needs_ocr(TextMode::OcrOnly, true, 40));
        assert!(needs_ocr(TextMode::Both, true, 40));
    }

    #[test]
    fn a_mapped_secret_stores_redacted_text_and_a_jpeg() {
        let (_path, config) = temp_config();
        let store = Store::open(config.data_dir.clone()).unwrap();
        let platform = FakePlatform::stable(
            focus(1, "Notes", "scratch"),
            ax(vec![seg(&format!("token {KEY}"), Some(box_at()))]),
        );
        let mut deg = Vec::new();
        let id = run_capture(&platform, &store, &config, TriggerKind::Manual, &mut deg).unwrap();
        let row = &store.recent_frames(1).unwrap()[0];
        assert_eq!(row.id, id);
        assert!(!row.text.contains(KEY));
        assert!(row.text.contains("[REDACTED:OPENAI_KEY]"));
        assert_eq!(row.image_status, ImageStatus::Ready.as_str());
        assert!(store.frame_path(id).unwrap().as_str().ends_with(".jpg"));
        assert!(deg.is_empty());
    }

    #[test]
    fn an_unmapped_secret_withholds_the_image() {
        let (_path, config) = temp_config();
        let store = Store::open(config.data_dir.clone()).unwrap();
        let platform = FakePlatform::stable(focus(1, "Notes", "scratch"), ax(vec![seg(KEY, None)]));
        let id = run_capture(
            &platform,
            &store,
            &config,
            TriggerKind::Manual,
            &mut Vec::new(),
        )
        .unwrap();
        let row = &store.recent_frames(1).unwrap()[0];
        assert_eq!(row.id, id);
        assert!(!row.text.contains(KEY));
        assert_eq!(row.image_status, "withheld_redaction");
        assert!(row.image_path.is_none());
        assert!(store.frame_path(id).is_err());
    }

    #[test]
    fn a_secret_only_in_the_title_is_redacted_and_withholds_the_image() {
        let (_path, config) = temp_config();
        let store = Store::open(config.data_dir.clone()).unwrap();
        let platform = FakePlatform::stable(
            focus(1, "Notes", "password=hunter2"),
            ax(vec![seg("just a note", Some(box_at()))]),
        );
        run_capture(
            &platform,
            &store,
            &config,
            TriggerKind::Manual,
            &mut Vec::new(),
        )
        .unwrap();
        let row = &store.recent_frames(1).unwrap()[0];
        assert_eq!(row.image_status, "withheld_redaction");
        assert!(!row.window_title.contains("hunter2"));
        assert!(row.window_title.contains("password=[REDACTED:PASSWORD]"));
        assert!(!row.text.contains("hunter2"));
    }

    #[test]
    fn a_focus_change_discards_the_frame() {
        let (_path, config) = temp_config();
        let store = Store::open(config.data_dir.clone()).unwrap();
        let mut platform = FakePlatform::stable(focus(1, "Notes", "a"), ax(vec![]));
        platform.focuses.push(focus(2, "Notes", "b"));
        let err = run_capture(
            &platform,
            &store,
            &config,
            TriggerKind::Manual,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(matches!(err, RecorderError::CaptureFailed(_)));
        assert!(store.recent_frames(10).unwrap().is_empty());
    }

    #[test]
    fn a_denylisted_app_is_not_captured() {
        let (_path, config) = temp_config();
        let store = Store::open(config.data_dir.clone()).unwrap();
        let platform = FakePlatform::stable(focus(1, "1Password", "vault"), ax(vec![]));
        let result = run_capture(
            &platform,
            &store,
            &config,
            TriggerKind::AppSwitch,
            &mut Vec::new(),
        );
        assert_eq!(result, Err(RecorderError::Skipped(SkipReason::AppExcluded)));
        assert_eq!(platform.capture_calls.load(AtomicOrdering::Relaxed), 0);
        assert!(store.recent_frames(10).unwrap().is_empty());
    }

    #[test]
    fn an_excluded_url_drops_the_frame_after_the_walk() {
        let (_path, mut config) = temp_config();
        config.filter.exclude_urls = vec!["bank.com".into()];
        let store = Store::open(config.data_dir.clone()).unwrap();
        let mut snapshot = ax(vec![seg(
            "balance and account details for the user",
            Some(box_at()),
        )]);
        snapshot.browser_url = Some("https://online.bank.com/home".into());
        let platform = FakePlatform::stable(focus(1, "Safari", "Bank"), snapshot);
        let result = run_capture(
            &platform,
            &store,
            &config,
            TriggerKind::AppSwitch,
            &mut Vec::new(),
        );
        assert_eq!(result, Err(RecorderError::Skipped(SkipReason::UrlExcluded)));
        assert!(store.recent_frames(10).unwrap().is_empty());
    }

    #[test]
    fn focused_app_on_a_window_only_platform_records_the_fallback() {
        let (_path, config) = temp_config();
        let store = Store::open(config.data_dir.clone()).unwrap();
        let mut platform = FakePlatform::stable(
            focus(1, "Notes", "a"),
            ax(vec![seg(
                "hello there, this is enough text",
                Some(box_at()),
            )]),
        );
        platform.scopes = vec![CaptureScope::FocusedWindow, CaptureScope::DisplayCrop];
        let mut deg = Vec::new();
        run_capture(&platform, &store, &config, TriggerKind::Manual, &mut deg).unwrap();
        assert_eq!(
            deg,
            vec![Degradation::ScopeFallback {
                requested: CaptureScope::FocusedApp,
                used: CaptureScope::FocusedWindow,
            }]
        );
        assert_eq!(
            *platform.used_scope.lock().unwrap(),
            Some(CaptureScope::FocusedWindow)
        );
    }

    #[test]
    fn thin_accessibility_text_falls_back_to_ocr() {
        let (_path, config) = temp_config();
        let store = Store::open(config.data_dir.clone()).unwrap();
        let mut platform =
            FakePlatform::stable(focus(1, "Notes", "a"), ax(vec![seg("ok", Some(box_at()))]));
        *platform.ocr.lock().unwrap() = Ok(OcrSnapshot {
            segments: vec![TextSegment {
                text: format!("canvas {KEY}"),
                role: "line".into(),
                bbox: Some(box_at()),
                source: TextSource::Ocr,
            }],
        });
        run_capture(
            &platform,
            &store,
            &config,
            TriggerKind::Manual,
            &mut Vec::new(),
        )
        .unwrap();
        let row = &store.recent_frames(1).unwrap()[0];
        assert!(!row.text.contains(KEY));
        assert!(row.text.contains("[REDACTED:OPENAI_KEY]"));
        assert_eq!(row.image_status, "ready");
    }

    #[test]
    fn missing_input_hooks_degrade_without_failing() {
        let (_path, mut config) = temp_config();
        config.triggers.click = true;
        let platform = FakePlatform::stable(focus(1, "Notes", "a"), ax(vec![]));
        let slot = Mutex::new(Vec::new());
        let (watch, rx) = open_watch(&platform, &config, &slot);
        assert!(watch.is_none());
        assert!(rx.is_none());
        assert_eq!(
            lock(&slot).as_slice(),
            &[Degradation::InputHooksUnavailable]
        );
    }

    #[test]
    fn capture_now_and_stop_before_start_are_not_running() {
        let (_path, config) = temp_config();
        let recorder = Recorder::new(config).unwrap();
        assert_eq!(
            recorder.capture_now().unwrap_err(),
            RecorderError::NotRunning
        );
        assert_eq!(recorder.stop().unwrap_err(), RecorderError::NotRunning);
    }

    fn start_lock() -> MutexGuard<'static, ()> {
        static START_LOCK: Mutex<()> = Mutex::new(());
        START_LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }

    #[test]
    fn idle_stop_does_not_release_a_running_recorder() {
        let _guard = start_lock();
        let (_path, config) = temp_config();
        let running = Recorder::new(config.clone()).unwrap();
        running.start().unwrap();
        let idle = Recorder::new(config).unwrap();
        assert_eq!(idle.stop().unwrap_err(), RecorderError::NotRunning);
        assert_eq!(idle.start().unwrap_err(), RecorderError::AlreadyRunning);
        running.stop().unwrap();
        idle.start().unwrap();
        idle.stop().unwrap();
    }

    #[test]
    fn a_second_start_returns_already_running() {
        let _guard = start_lock();
        let (_path, config) = temp_config();
        let recorder = Recorder::new(config.clone()).unwrap();
        recorder.start().unwrap();
        assert_eq!(recorder.start().unwrap_err(), RecorderError::AlreadyRunning);
        let other = Recorder::new(config).unwrap();
        assert_eq!(other.start().unwrap_err(), RecorderError::AlreadyRunning);
        recorder.stop().unwrap();
        assert_eq!(recorder.stop().unwrap_err(), RecorderError::NotRunning);
        recorder.start().unwrap();
        recorder.stop().unwrap();
    }
}
